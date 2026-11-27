use super::*;
mod gamescope;
mod handheld;
mod manifest;
use base64::{Engine, engine::general_purpose::STANDARD};
use eframe::egui;
use handheld::Session;
use serde_json::{Value, json};
use std::{
    fs,
    io::Cursor,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};

pub(super) fn launch_invocation() -> bool {
    let args = env::args_os().collect::<Vec<_>>();
    args.windows(2)
        .any(|pair| pair[0] == "decky" && pair[1] == "play")
        || (args.len() == 1 && PathBuf::from(&args[0]).ends_with("play"))
}

// A bounded diagnostic record. No command line, paths, Server details or child output.
pub(super) fn record_launch(stage: &str, code: Option<u8>) {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let Some(base) = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
    else {
        return;
    };
    let directory = base.join("app-depot-decky");
    let _ = (|| -> io::Result<()> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)?;
        if !fs::symlink_metadata(&directory)?.is_dir() {
            return Ok(());
        }
        let temporary = directory.join(format!(".launch-{}.tmp", std::process::id()));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        let result = (|| -> io::Result<()> {
            serde_json::to_writer(
                &mut file,
                &json!({
                    "stage":stage,"exitCode":code,"pid":std::process::id(),
                }),
            )?;
            file.flush()?;
            fs::rename(&temporary, directory.join("last-launch.json"))
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    })();
}

fn profile_client(library: &Library, id: &ServerProfileId) -> Result<ApiClient, CliError> {
    let profile = library.profile(id)?;
    if profile.archived {
        return Err(LibraryError::Invalid("Server profile is archived".into()).into());
    }
    Ok(ApiClient::new(
        &profile.origin,
        profile_token(&profile)?,
        profile.origin.starts_with("http://"),
    )?)
}

pub(super) fn profile_token(
    profile: &app_depot_core::library::Profile,
) -> Result<Option<String>, CliError> {
    profile
        .credential_ref
        .as_deref()
        .map(|reference| NativeSecrets.get(reference))
        .transpose()
        .map_err(Into::into)
}

pub(super) fn error_code(error: &CliError) -> &'static str {
    match error {
        CliError::Decky(code) => code,
        CliError::Secret(SecretError::Unavailable | SecretError::Missing) => {
            "secret_service_unavailable"
        }
        CliError::Secret(SecretError::Invalid) => "invalid_token",
        CliError::Api(error) | CliError::Library(LibraryError::Api(error)) => match error {
            ApiError::Authentication(_) => "authentication_required",
            ApiError::Authorization(_) => "catalog_forbidden",
            ApiError::Incompatible(_) | ApiError::Protocol(_) => "unsupported_protocol",
            ApiError::Config(message) if message.starts_with("remote HTTP requires") => {
                "http_consent_required"
            }
            ApiError::Config(message) if message == "invalid Bearer token" => "invalid_token",
            ApiError::Config(_) => "invalid_url",
            ApiError::Transport(details) => {
                let details = details.to_ascii_lowercase();
                if details.contains("timed out") || details.contains("timeout") {
                    "timeout"
                } else if details.contains("tls")
                    || details.contains("certificate")
                    || details.contains("ssl")
                {
                    "tls_failure"
                } else {
                    "server_unavailable"
                }
            }
            ApiError::Http { .. } => "server_error",
        },
        CliError::Library(LibraryError::Invalid(message)) => match message.as_str() {
            "local uninstall selection changed"
            | "local uninstall release owner changed"
            | "local uninstall association owner changed" => "uninstall_stale",
            "local uninstall blocked by an active operation"
            | "another library lifecycle operation is active"
            | "another application download holds the library slot"
            | "installation is running"
            | "application is running from this managed runtime"
            | "a download is active for this Association" => "uninstall_busy",
            _ => "internal",
        },
        _ => "internal",
    }
}

fn public_profile(profile: &app_depot_core::library::Profile) -> Value {
    json!({"id":profile.id,"origin":profile.origin,"name":profile.name,
        "authenticated":profile.credential_ref.is_some(),"status":profile.connection_status,
        "lastSync":profile.last_sync})
}

fn candidates(
    library: &Library,
    profile: &ServerProfileId,
    app: &str,
) -> Result<Vec<LaunchCandidate>, CliError> {
    let mut choices = Vec::new();
    for release in library.cached_releases(profile, app)? {
        if library
            .cached_snapshot(profile, &release.value.id)?
            .is_none()
        {
            continue;
        }
        choices.extend(
            library
                .launch_candidates(profile, &release.value.id)?
                .into_iter()
                .filter(|candidate| {
                    matches!(candidate.status.as_str(), "Ready" | "PreparationRequired")
                        || (!release.stale && candidate.status == "NotDownloaded")
                }),
        );
    }
    Ok(choices)
}

fn catalog(library: &Library, profile: &ServerProfileId) -> Result<Value, CliError> {
    let installs = library.installations(Some(profile))?;
    let mut downloaded_releases = std::collections::HashSet::new();
    let files = library.verified_files(Some(profile))?;
    let trees = library.verified_trees(Some(profile))?;
    for association in files
        .iter()
        .map(|file| &file.association_id)
        .chain(trees.iter().map(|tree| &tree.association_id))
    {
        if let Some(context) = library.release_for_association(profile, association)? {
            downloaded_releases.insert(context);
        }
    }
    let mut rows = Vec::new();
    for row in library.cached_apps(profile)? {
        let app = row.value;
        let selected = installs
            .iter()
            .filter(|item| item.app_id == app.id)
            .collect::<Vec<_>>();
        let mut downloaded = false;
        for release in library.cached_releases(profile, &app.id)? {
            let id = &release.value.id;
            if let Some(state) = library.release_installation(profile, id)? {
                downloaded |= state.active_generation.is_some();
            } else if downloaded_releases.contains(id) {
                downloaded |= !library.cached_snapshot(profile, id)?.is_some_and(|s| {
                    s.value
                        .release
                        .app_depot()
                        .is_some_and(|v| v.get("installation").is_some())
                });
            }
        }
        if !row.stale || !selected.is_empty() {
            rows.push(json!({"id":app.id,"name":app.name,"stale":row.stale,
                "downloaded":downloaded,"installations":selected}));
        }
    }
    Ok(json!(rows))
}

fn artwork(
    client: &ApiClient,
    app: &str,
    role: &str,
    downloaded: bool,
    known: &str,
) -> Result<Value, CliError> {
    let entries = client.list_artwork(app)?;
    let item = entries.iter().find(|item| item.role == role).or_else(|| {
        (role == "logo")
            .then(|| entries.iter().find(|item| item.role == "steam.logo"))
            .flatten()
    });
    let Some(item) = item else {
        return Ok(Value::Null);
    };
    let muted = !downloaded && matches!(role, "steam.grid.vertical" | "steam.grid.horizontal");
    let revision = format!("{}:{}:png1", item.digest, muted);
    if revision == known {
        return Ok(json!({"revision":revision}));
    }
    let bytes = client.artwork_bytes(item)?;
    let png = convert_grid(bytes, muted, item.width, item.height, true)?;
    Ok(json!({"revision":revision,"png":STANDARD.encode(png)}))
}

fn convert_grid(
    bytes: Vec<u8>,
    muted: bool,
    width: u32,
    height: u32,
    exact: bool,
) -> Result<Vec<u8>, CliError> {
    let image_error =
        |error: image::ImageError| ApiError::Protocol(format!("Artwork decode: {error}"));
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(LibraryError::Io)?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(72 * 1024 * 1024);
    limits.max_image_width = Some(width);
    limits.max_image_height = Some(height);
    reader.limits(limits);
    let mut rgba = reader.decode().map_err(image_error)?.into_rgba8();
    if exact && rgba.dimensions() != (width, height) {
        return Err(ApiError::Protocol("Artwork dimensions changed".into()).into());
    }
    if muted {
        for pixel in rgba.pixels_mut() {
            let gray =
                ((u16::from(pixel[0]) * 54 + u16::from(pixel[1]) * 183 + u16::from(pixel[2]) * 19)
                    >> 8) as u8;
            pixel.0[..3].fill(gray);
        }
    }
    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(rgba)
        .write_to(&mut png, image::ImageFormat::Png)
        .map_err(image_error)?;
    if png.get_ref().len() > 16 * 1024 * 1024 {
        return Err(ApiError::Protocol("Converted artwork exceeds size limit".into()).into());
    }
    Ok(png.into_inner())
}

pub(super) fn run_decky(options: &Options) -> Result<(), CliError> {
    if options
        .positional
        .get(1)
        .is_some_and(|action| action == "play")
        && gamescope::display_unavailable()
    {
        return Err(CliError::Decky("steam_game_display_unavailable"));
    }
    if options.positional.len() == 2 && options.positional[1] == "discover" {
        let results = app_depot_core::discovery::discover(Duration::from_secs(3))
            .map_err(|_| CliError::Decky("discovery_unavailable"))?;
        return Ok(print_json(&results)?);
    }
    if options
        .positional
        .get(1)
        .is_some_and(|action| action == "mute-grid")
        && options.positional.len() == 2
    {
        let mut bytes = Vec::new();
        io::stdin()
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(LibraryError::Io)?;
        if bytes.len() > 16 * 1024 * 1024 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(CliError::Decky("invalid_artwork"));
        }
        return Ok(print_json(
            &json!({"png":STANDARD.encode(convert_grid(bytes, true, 8192, 8192, false)?)}),
        )?);
    }
    let mut library = open_library(options)?;
    let args = &options.positional;
    match args.as_slice() {
        [_, action] if action == "status" => print_json(&json!({
            "profiles": library.profiles(false)?.iter().map(public_profile).collect::<Vec<_>>(),
            "library": selected_library_paths(options)?.content,
            "usage": library.storage_usage()?,
            "runtimes": manifest::runtimes(),
            "windowsRuntime": manifest::default_runtime(&library)?,
            "version": env!("CARGO_PKG_VERSION"),
        }))?,
        [_, action, path] if action == "set-runtime" => {
            manifest::set_runtime(&library, path)?;
            print_json(&json!(true))?;
        }
        [_, action] if action == "installed" => {
            let mut rows = Vec::new();
            for profile in library.profiles(false)? {
                for app in library.cached_apps(&profile.id)? {
                    let app = app.value;
                    if let Some(item) = library.local_uninstall_preview(&profile.id, &app.id)? {
                        rows.push(json!({"id":app.id,"name":app.name,"profile":profile.id,
                            "profileName":profile.name,"origin":profile.origin,"revision":item.revision,
                            "states":item.states,"runtimeBytes":item.runtime_bytes,
                            "downloadBytes":item.download_bytes,"blocked":item.blocked}));
                    }
                }
            }
            print_json(&rows)?;
        }
        [_, action, profile, app, revision] if action == "uninstall" => {
            let profile = ServerProfileId(profile.clone());
            library.profile(&profile)?;
            if !library
                .cached_apps(&profile)?
                .iter()
                .any(|row| row.value.id == *app)
            {
                return Err(CliError::Decky("uninstall_stale"));
            }
            let reclaimed = library
                .uninstall_local_app(&profile, app, revision)
                .map_err(|error| {
                    let error = CliError::from(error);
                    if error_code(&error) == "internal" {
                        CliError::Decky("uninstall_incomplete")
                    } else {
                        error
                    }
                })?;
            print_json(&json!({"removed":true,"cleanupComplete":reclaimed}))?;
        }
        [_, action, origin] if action == "test" => {
            let mut input = String::new();
            io::stdin()
                .take(16_385)
                .read_to_string(&mut input)
                .map_err(LibraryError::Io)?;
            if input.len() > 16_384 {
                return Err(SecretError::Invalid.into());
            }
            let authenticated = !input.is_empty();
            let client = ApiClient::new(
                origin,
                (!input.is_empty()).then_some(input),
                options.allow_insecure_http,
            )?;
            let probe = || {
                let caps = client.ensure_read_compatible().map_err(|e| match e {
                    ApiError::Http { status: 404, .. } => {
                        ApiError::Incompatible("Capabilities route absent".into())
                    }
                    error => error,
                })?;
                client.list_apps(caps.page_size().map_err(ApiError::Incompatible)?)
            };
            let apps = probe().map_err(|error| match error {
                ApiError::Authentication(_) if authenticated => CliError::Decky("invalid_token"),
                ApiError::Authentication(_) => CliError::Decky("anonymous_reading_disabled"),
                error => error.into(),
            })?;
            print_json(&json!({"connected":true,"origin":client.origin(),
                "guest":!authenticated,"appCount":apps.len()}))?;
        }
        [_, action, id] if action == "sync" || action == "catalog" => {
            let profile = ServerProfileId(id.clone());
            if action == "sync" {
                let client = profile_client(&library, &profile)?;
                library.sync(&profile, &client)?;
            }
            print_json(&catalog(&library, &profile)?)?;
        }
        [_, action, id, app, role, state, known] if action == "artwork" => {
            let client = profile_client(&library, &ServerProfileId(id.clone()))?;
            print_json(&artwork(&client, app, role, state == "color", known)?)?;
        }
        [_, action, id, app] if action == "play" => return play(options, library, id, app),
        _ => return Err(ApiError::Config("Invalid Decky operation".into()).into()),
    }
    Ok(())
}

fn choice_key(choice: &LaunchCandidate) -> Vec<&str> {
    vec![
        &choice.release_id,
        &choice.association_id,
        &choice.entry_id,
        choice.selection_revision(),
    ]
}

fn selection_path(options: &Options, profile: &str, app: &str) -> Result<PathBuf, CliError> {
    let valid = |id: &str| id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-');
    if !valid(profile) || !valid(app) {
        return Err(ApiError::Config("Invalid shortcut identity".into()).into());
    }
    Ok(selected_library_paths(options)?
        .metadata
        .join("decky")
        .join(format!("{profile}-{app}.json")))
}

fn supervise(
    library: &mut Library,
    choice: &LaunchCandidate,
    stop: &AtomicBool,
) -> Result<(), CliError> {
    let ready = library
        .launch_candidates(&choice.profile_id, &choice.release_id)?
        .into_iter()
        .find(|current| {
            choice_key(current) == choice_key(choice) && current.artifact_id == choice.artifact_id
        })
        .ok_or_else(|| {
            LibraryError::Invalid("Launch selection changed. Open the shortcut again.".into())
        })?;
    prepare_selected(library, &ready)?;
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    let mut child = library.play_declared(
        &choice.profile_id,
        &choice.release_id,
        &choice.association_id,
        &choice.entry_id,
        choice.selection_revision(),
    )?;
    let mut termination = None;
    loop {
        if let Some(status) = child.try_wait().map_err(LibraryError::Io)? {
            return if status.success() {
                Ok(())
            } else {
                Err(CliError::Child(status))
            };
        }
        if stop.load(Ordering::Relaxed) {
            if let Some(sent) = termination {
                if Instant::now().duration_since(sent) > Duration::from_secs(3) {
                    let _ = child.kill();
                }
            } else {
                // The process has not been reaped, so its PID cannot be reused here.
                unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
                termination = Some(Instant::now());
            }
        }
        std::thread::sleep(Duration::from_millis(80));
    }
}

fn play(options: &Options, mut library: Library, id: &str, app: &str) -> Result<(), CliError> {
    let profile = ServerProfileId(id.into());
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(signal, stop.clone()).map_err(LibraryError::Io)?;
    }
    let path = selection_path(options, id, app)?;
    let choices = candidates(&library, &profile, app)?;
    let name = library
        .cached_apps(&profile)?
        .into_iter()
        .find(|row| row.value.id == app)
        .map_or_else(|| "App Depot".into(), |row| row.value.name);
    let saved = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Vec<String>>(&bytes).ok());
    let selected = saved.as_ref().and_then(|saved| {
        choices
            .iter()
            .position(|choice| choice_key(choice) == *saved)
    });
    let fast =
        selected.or_else(|| (choices.len() == 1 && choices[0].status == "Ready").then_some(0));
    if let Some(i) = fast
        && choices[i].status == "Ready"
    {
        return supervise(&mut library, &choices[i], &stop);
    }
    let worker_options = options.clone();
    let app = app.to_owned();
    let session_name = name.clone();
    let paths = library.paths.clone();
    let ui_profile = profile.clone();
    let choice = handheld::run(name, paths, ui_profile, stop.clone(), move |session| {
        let choices = candidates(&library, &profile, &app)?;
        // A cancellation may finish an atomic activation before control returns.
        if choices.len() == 1 && choices[0].status == "Ready" {
            return Ok(choices[0].clone());
        }
        let selected = saved.as_ref().and_then(|saved| {
            choices
                .iter()
                .position(|choice| choice_key(choice) == *saved)
        });
        let events = session.activity.clone();
        library.observe_transfers(Arc::new(move |id, event| {
            events.lock().unwrap().observe(id, event)
        }));
        if selected.is_none()
            && let Some(choice) = manifest::first_play(
                &mut library,
                &profile,
                &app,
                &session_name,
                &choices,
                session,
            )?
        {
            save_choice(&path, &choice)?;
            return Ok(choice);
        }
        let index = match selected {
            Some(i) => i,
            None if choices.len() == 1 => 0,
            None if choices.is_empty() => {
                return Err(LibraryError::Invalid(
                    "No supported launch is declared. Synchronize this application in Decky."
                        .into(),
                )
                .into());
            }
            None => session.choose(
                "Choose a launch option",
                "",
                choices
                    .iter()
                    .map(|c| {
                        format!(
                            "{} · {}",
                            c.label.as_ref().unwrap_or(&c.entry_id),
                            c.release_id
                        )
                    })
                    .collect(),
            )?,
        };
        let choice = &choices[index];
        if choice.status != "Ready" {
            let size = if choice.status == "PreparationRequired" {
                "Already downloaded".into()
            } else {
                download_summary(&library, &profile, &[&choice.artifact_id])?
            };
            session.choose(
                "Install application",
                &format!("{size}\nRuntime: Native Linux\nAdditional components: None"),
                vec!["Install and play".into()],
            )?;
            session.phase("Preparing installation");
            library.reconcile_file_jobs()?;
            library.reconcile_tree_jobs()?;
            if let Some(job) = library.jobs(Some(&profile))?.into_iter().find(|j| {
                j.association_id == choice.association_id
                    && j.artifact_id == choice.artifact_id
                    && matches!(j.state.as_str(), "Paused" | "Failed")
            }) {
                library.retry_job(&job.id)?;
            }
            let mut args = worker_options.clone();
            args.profile = Some(profile.clone());
            args.positional = [
                "decky-prepare",
                &app,
                &choice.release_id,
                &choice.association_id,
                &choice.entry_id,
                choice.selection_revision(),
            ]
            .map(str::to_owned)
            .into();
            first_play_in_library(&args, true, &mut library, &mut |s| session.phase(s))?;
        }
        save_choice(&path, choice)?;
        Ok(choice.clone())
    })?;
    if let Some(choice) = choice {
        supervise(&mut open_library(options)?, &choice, &stop)?;
    }
    Ok(())
}

fn download_summary(
    lib: &Library,
    profile: &ServerProfileId,
    artifacts: &[&str],
) -> Result<String, CliError> {
    let jobs = lib.jobs(Some(profile))?;
    let mut bytes = 0u64;
    let unique = artifacts
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    for artifact in unique {
        let matching = jobs.iter().filter(|j| j.artifact_id == artifact);
        let completed = matching.clone().any(|j| j.state == "Completed");
        if completed {
            continue;
        }
        let known = matching
            .clone()
            .map(|j| j.expected_size)
            .max()
            .filter(|v| *v > 0)
            .or(lib
                .cached_artifact(profile, artifact)?
                .and_then(|a| a.value.size));
        let size = match known {
            Some(size) => size,
            None if artifact.starts_with("adtree:") => {
                let api = profile_client(lib, profile)?;
                let page = api
                    .ensure_read_compatible()?
                    .page_size()
                    .map_err(ApiError::Incompatible)?;
                let (entries, _) = api.tree_inventory(artifact, page)?;
                entries
                    .into_iter()
                    .map(|e| match e {
                        app_depot_core::models::InventoryEntry::File { size, .. } => size,
                        _ => 0,
                    })
                    .sum()
            }
            None => return Ok("Download size unavailable".into()),
        };
        let checkpoint = matching
            .filter(|j| {
                matches!(
                    j.state.as_str(),
                    "Paused" | "Failed" | "Downloading" | "Queued"
                )
            })
            .map(|j| j.progress)
            .max()
            .unwrap_or(0);
        bytes = bytes.saturating_add(size.saturating_sub(checkpoint));
    }
    Ok(format!(
        "Download: up to {} · verified files are reused",
        handheld::bytes(bytes)
    ))
}
fn save_choice(path: &std::path::Path, choice: &LaunchCandidate) -> Result<(), CliError> {
    let destination = path.parent().unwrap();
    fs::create_dir_all(destination).map_err(LibraryError::Io)?;
    // Distinct temp names allow two Play requests without sharing an open file.
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| -> io::Result<()> {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        serde_json::to_writer(&mut file, &choice_key(choice))?;
        file.flush()?;
        fs::rename(&tmp, path)
    })();
    if let Err(error) = result {
        return Err(LibraryError::Io(error).into());
    }

    Ok(())
}
