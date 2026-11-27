//! Steam-session dialogs around the shared Standard installation lifecycle.
use super::*;
use app_depot_core::{
    compatibility,
    installation::{self, Plan, ResolveRequest},
};
use std::path::Path;

fn invalid(message: &str) -> CliError {
    LibraryError::Invalid(message.into()).into()
}

pub(super) fn runtimes() -> Vec<PathBuf> {
    compatibility::discover()
        .into_iter()
        .map(|r| r.executable().to_owned())
        .collect()
}

pub(super) fn default_runtime(lib: &Library) -> Result<Option<PathBuf>, CliError> {
    match fs::read(lib.paths.metadata.join("decky-windows-runtime.json")) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).map_err(LibraryError::Json)?,
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(LibraryError::Io(e).into()),
    }
}

pub(super) fn set_runtime(lib: &Library, path: &str) -> Result<(), CliError> {
    if !runtimes().contains(&PathBuf::from(path)) {
        return Err(invalid("Runtime is unavailable"));
    }
    let temp = lib
        .paths
        .metadata
        .join(format!(".decky-runtime-{}.tmp", std::process::id()));
    let mut file = fs::File::create_new(&temp).map_err(LibraryError::Io)?;
    let result = (|| -> io::Result<()> {
        serde_json::to_writer(&mut file, path)?;
        file.sync_all()?;
        fs::rename(&temp, lib.paths.metadata.join("decky-windows-runtime.json"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    Ok(result.map_err(LibraryError::Io)?)
}

pub(super) fn first_play(
    lib: &mut Library,
    profile: &ServerProfileId,
    app: &str,
    name: &str,
    choices: &[LaunchCandidate],
    session: &Session,
) -> Result<Option<LaunchCandidate>, CliError> {
    let mut releases = Vec::new();
    for row in lib.cached_releases(profile, app)? {
        if row.stale || choices.iter().any(|c| c.release_id == row.value.id) {
            continue;
        }
        let Some(snapshot) = lib.cached_snapshot(profile, &row.value.id)? else {
            continue;
        };
        let doc = snapshot.value.release;
        if !doc
            .app_depot()
            .and_then(|v| v["platforms"].as_array())
            .is_some_and(|a| a.iter().any(|p| p == "windows/x86_64"))
        {
            continue;
        }
        if let Some(manifest) = installation::manifest(&doc)?
            && !manifest.recipe_payload
            && manifest.launch.is_some()
        {
            releases.push((row.value, doc.revision_id, manifest));
        }
    }
    if releases.is_empty() {
        return Ok(None);
    }
    let index = if releases.len() == 1 && choices.is_empty() {
        0
    } else {
        let mut options: Vec<_> = releases
            .iter()
            .map(|(r, _, _)| r.version.clone().unwrap_or_else(|| r.id.clone()))
            .collect();
        if !choices.is_empty() {
            options.push("Existing launch options".into());
        }
        let index = session.choose("Choose a release", "", options)?;
        if index == releases.len() {
            return Ok(None);
        }
        index
    };
    let (release, revision, manifest) = releases.swap_remove(index);
    let pending = lib
        .release_installation(profile, &release.id)?
        .and_then(|s| {
            s.generations
                .into_iter()
                .rev()
                .find(|g| matches!(g.state.as_str(), "Preparing" | "PrerequisitesPending"))
        });
    let mut selections = installation::Selections::new();
    for group in manifest.options.into_iter().filter(|_| pending.is_none()) {
        let ids = session.options(&group)?;
        selections.insert(group.id, ids);
    }
    session.phase("Resolving installation plan");
    let api = profile_client(lib, profile)?;
    let saved = if let Some(g) = &pending {
        lib.prerequisite_requirements(profile, &g.plan_id)?
    } else {
        lib.resolve_plan(
            profile,
            &api,
            &release.id,
            &ResolveRequest {
                platform: "windows/x86_64".into(),
                revision_id: Some(revision),
                selections,
            },
        )?
    };
    let plan: Plan = serde_json::from_value(saved.plan).map_err(LibraryError::Json)?;
    if plan.recipe.is_some()
        || plan.app_id != app
        || plan.platform != "windows/x86_64"
        || plan.launch.is_none()
    {
        return Err(invalid("The reviewed plan cannot launch this application"));
    }
    let selected = lib
        .application_runtime(profile, app)
        .ok()
        .map(|r| r.runtime.executable().to_owned())
        .or(default_runtime(lib)?)
        .ok_or_else(|| invalid("Select a Windows runtime in Decky Settings first"))?;
    let prerequisites = plan.prerequisites.unwrap_or_default();
    let mut consent = Vec::new();
    let mut sources: Vec<_> = plan
        .files
        .iter()
        .flat_map(|files| files.values())
        .map(|f| f.artifact_id.as_str())
        .collect();
    let mut components = Vec::new();
    for p in &prerequisites {
        if !p.optional
            || session.choose(
                &p.name,
                "Optional component",
                vec!["Skip".into(), "Include".into()],
            )? == 1
        {
            consent.push(p.id.clone());
            sources.push(&p.file.artifact_id);
            components.push(format!("{} · {} {:?}", p.name, p.path, p.arguments));
        }
    }
    let mut review = format!(
        "{}\nRuntime: {}\nComponents: {}",
        download_summary(lib, profile, &sources)?,
        selected.display(),
        if components.is_empty() {
            "None".into()
        } else {
            components.join("\n")
        }
    );
    for note in plan.unresolved {
        review.push_str(&format!("\n{note}"));
    }
    let action = if pending.is_some() {
        "Resume and retry incomplete components"
    } else {
        "Approve installation and play"
    };
    session.choose(&format!("Install {name}"), &review, vec![action.into()])?;
    session.check()?;
    session.activity.lock().unwrap().release = Some(release.id.clone());
    let mut runtime = lib.bind_application_runtime(profile, &saved.id, Path::new(&selected))?;
    let mut progress = |phase: &str| session.phase(phase);
    let retry = pending.is_some();
    let generation = if let Some(g) = pending.filter(|g| g.state == "PrerequisitesPending") {
        Some(g)
    } else {
        lib.install_saved_plan_with_progress(profile, &saved.id, Some(&api), &mut progress)?
            .generations
            .into_iter()
            .rev()
            .find(|g| g.plan_id == saved.id && g.state == "PrerequisitesPending")
    };
    if let Some(g) = generation {
        session.check()?;
        let round =
            lib.confirm_prerequisites(profile, &g.id, &saved.fingerprint, &consent, retry)?;
        use app_depot_core::prerequisite::{
            PrerequisiteExecutor, PrerequisiteOutcome, PrerequisiteRequest,
        };
        struct Cancellable<'a> {
            runtime: &'a mut compatibility::ApplicationRuntime,
            session: &'a Session,
        }
        impl PrerequisiteExecutor for Cancellable<'_> {
            fn execute(&mut self, request: &PrerequisiteRequest<'_>) -> PrerequisiteOutcome {
                if self.session.check().is_err() {
                    PrerequisiteOutcome::Cancelled
                } else {
                    self.runtime.execute(request)
                }
            }
        }
        let result = lib.process_prerequisites_with_progress(
            profile,
            &g.id,
            &round,
            Some(&api),
            &mut Cancellable {
                runtime: &mut runtime,
                session,
            },
            &mut progress,
        )?;
        if let Some(error) = result
            .generations
            .iter()
            .find(|value| value.id == g.id)
            .and_then(|g| g.last_error.as_deref())
        {
            return Err(invalid(error));
        }
    }
    session.check()?;
    let choice = lib
        .launch_candidates(profile, &release.id)?
        .into_iter()
        .find(|c| c.status == "Ready")
        .ok_or_else(|| {
            invalid("Installation is incomplete. Retry to review the pending components.")
        })?;
    Ok(Some(choice))
}
