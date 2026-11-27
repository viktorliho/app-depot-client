//! Steam-session dialogs around the shared Standard installation lifecycle.
use super::*;
use app_depot_core::{
    compatibility,
    installation::{self, Plan, ResolveRequest},
};
use std::{io::Write, path::Path};

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

fn dialog(name: &str) -> Command {
    let mut cmd = Command::new("/usr/bin/zenity");
    cmd.arg(format!("--title={name}")).arg("--width=660");
    cmd
}

pub(super) fn show_error(name: &str, message: &str) {
    let _ = dialog(name)
        .args(["--error", "--no-markup"])
        .arg(format!("--text={message}"))
        .status();
}

fn confirm(name: &str, message: &str) -> Result<bool, CliError> {
    let status = dialog(name)
        .args(["--question", "--no-markup", "--default-cancel"])
        .arg(format!("--text={message}"))
        .status()
        .map_err(LibraryError::Io)?;
    Ok(status.success())
}

pub(super) fn first_play(
    lib: &mut Library,
    profile: &ServerProfileId,
    app: &str,
    name: &str,
    stop: &AtomicBool,
    choices: &[LaunchCandidate],
    selection_path: &Path,
) -> Result<bool, CliError> {
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
        return Ok(false);
    }
    let index = if releases.len() == 1 && choices.is_empty() {
        0
    } else {
        let mut cmd = dialog(name);
        cmd.args([
            "--list",
            "--text=Select release",
            "--column=Index",
            "--column=Release",
            "--hide-column=1",
        ]);
        if !choices.is_empty() {
            cmd.args(["other", "Existing launch options"]);
        }
        for (i, (r, _, _)) in releases.iter().enumerate() {
            cmd.arg(i.to_string())
                .arg(r.version.as_ref().unwrap_or(&r.id));
        }
        let output = cmd.output().map_err(LibraryError::Io)?;
        if !output.status.success() {
            return Ok(true);
        }
        let selection = String::from_utf8_lossy(&output.stdout);
        if selection.trim() == "other" {
            return Ok(false);
        }
        selection
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|i| *i < releases.len())
            .ok_or_else(|| invalid("Invalid release selection"))?
    };
    let (release, revision, manifest) = releases.swap_remove(index);
    let pending = lib
        .release_installation(profile, &release.id)?
        .and_then(|s| {
            s.generations
                .into_iter()
                .rev()
                .find(|g| g.state == "PrerequisitesPending")
        });
    let mut selections = installation::Selections::new();
    for group in manifest.options.into_iter().filter(|_| pending.is_none()) {
        let mut cmd = dialog(name);
        cmd.args([
            "--list",
            if group.multiple {
                "--checklist"
            } else {
                "--radiolist"
            },
            "--no-markup",
            "--column=Selected",
            "--column=Index",
            "--column=Choice",
            "--hide-column=2",
            "--print-column=2",
        ])
        .arg(format!("--text={}", group.name));
        for (i, item) in group.choices.iter().enumerate() {
            cmd.arg(if group.defaults.contains(&item.id) {
                "TRUE"
            } else {
                "FALSE"
            })
            .arg(i.to_string())
            .arg(&item.name);
        }
        let output = cmd.output().map_err(LibraryError::Io)?;
        if !output.status.success() {
            return Ok(true);
        }
        let ids = String::from_utf8_lossy(&output.stdout)
            .trim()
            .split('|')
            .filter(|v| !v.is_empty())
            .map(|v| {
                v.parse::<usize>()
                    .ok()
                    .and_then(|i| group.choices.get(i))
                    .map(|c| c.id.clone())
                    .ok_or_else(|| invalid("Invalid option selection"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        selections.insert(group.id, ids);
    }
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
    let mut review = format!("Install {name} using {}?\n", selected.display());
    for p in &prerequisites {
        review.push_str(&format!("\n{} · {} {:?}", p.name, p.path, p.arguments));
    }
    for note in plan.unresolved {
        review.push_str(&format!("\n{note}"));
    }
    if pending.is_some() {
        review.push_str("\nResume this installation and retry incomplete prerequisites?");
    }
    let mut prompt = dialog(name)
        .args([
            "--text-info",
            "--height=420",
            "--ok-label=Install",
            "--checkbox=I approve this installation and the required prerequisites",
        ])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(LibraryError::Io)?;
    prompt
        .stdin
        .take()
        .unwrap()
        .write_all(review.as_bytes())
        .map_err(LibraryError::Io)?;
    if !prompt.wait().map_err(LibraryError::Io)?.success() {
        return Ok(true);
    }
    let mut consent = Vec::new();
    for p in prerequisites {
        if !p.optional || confirm(name, &format!("Run optional prerequisite {}?", p.name))? {
            consent.push(p.id);
        }
    }
    if stop.load(Ordering::Relaxed) {
        return Ok(true);
    }
    let mut runtime = lib.bind_application_runtime(profile, &saved.id, Path::new(&selected))?;
    let mut window = dialog(name)
        .args([
            "--progress",
            "--pulsate",
            "--no-cancel",
            "--auto-close",
            "--text=Preparing application resources",
        ])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(LibraryError::Io)?;
    let mut pipe = window.stdin.take();
    let mut progress = |phase: &str| {
        if let Some(p) = &mut pipe {
            let _ = writeln!(p, "# {}", phase.replace(['\r', '\n'], " "));
        }
    };
    let installation = (|| -> Result<(), CliError> {
        let retry = pending.is_some();
        let generation = if let Some(g) = pending {
            Some(g)
        } else {
            lib.install_saved_plan_with_progress(profile, &saved.id, Some(&api), &mut progress)?
                .generations
                .into_iter()
                .rev()
                .find(|g| g.plan_id == saved.id && g.state == "PrerequisitesPending")
        };
        if let Some(g) = generation {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let round =
                lib.confirm_prerequisites(profile, &g.id, &saved.fingerprint, &consent, retry)?;
            lib.process_prerequisites_with_progress(
                profile,
                &g.id,
                &round,
                Some(&api),
                &mut runtime,
                &mut progress,
            )?;
        }
        Ok(())
    })();
    drop(pipe);
    let _ = window.kill();
    let _ = window.wait();
    installation?;
    if !stop.load(Ordering::Relaxed) {
        let choice = lib
            .launch_candidates(profile, &release.id)?
            .into_iter()
            .find(|c| c.status == "Ready")
            .ok_or_else(|| {
                invalid("Installation is incomplete. Play again to review and retry.")
            })?;
        save_choice(selection_path, &choice)?;
        supervise(lib, &choice, stop)?;
    }
    Ok(true)
}
