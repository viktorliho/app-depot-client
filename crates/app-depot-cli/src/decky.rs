use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use eframe::egui;
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Cursor},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};

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
        let downloaded = library
            .cached_releases(profile, &app.id)?
            .iter()
            .any(|release| downloaded_releases.contains(&release.value.id));
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
    let image_error =
        |error: image::ImageError| ApiError::Protocol(format!("Artwork decode: {error}"));
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(LibraryError::Io)?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(72 * 1024 * 1024);
    limits.max_image_width = Some(item.width);
    limits.max_image_height = Some(item.height);
    reader.limits(limits);
    let mut rgba = reader.decode().map_err(image_error)?.into_rgba8();
    if rgba.dimensions() != (item.width, item.height) {
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
    Ok(json!({"revision":revision,"png":STANDARD.encode(png.into_inner())}))
}

pub(super) fn run_decky(options: &Options) -> Result<(), CliError> {
    if options.positional.len() == 2 && options.positional[1] == "discover" {
        let results = app_depot_core::discovery::discover(Duration::from_secs(3))
            .map_err(|_| CliError::Decky("discovery_unavailable"))?;
        return Ok(print_json(&results)?);
    }
    let mut library = open_library(options)?;
    let args = &options.positional;
    match args.as_slice() {
        [_, action] if action == "status" => print_json(&json!({
            "profiles": library.profiles(false)?.iter().map(public_profile).collect::<Vec<_>>(),
            "library": selected_library_paths(options)?.content,
            "usage": library.storage_usage()?,
            "version": env!("CARGO_PKG_VERSION"),
        }))?,
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
        &choice.declaration_revision,
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
        &choice.declaration_revision,
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
    let mut choices = candidates(&library, &profile, app)?;
    let name = library
        .cached_apps(&profile)?
        .into_iter()
        .find(|row| row.value.id == app)
        .map_or_else(|| "App Depot".into(), |row| row.value.name);
    let saved = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Vec<String>>(&bytes).ok());
    let selected = saved
        .as_ref()
        .and_then(|saved| {
            choices
                .iter()
                .position(|choice| choice_key(choice) == *saved)
        })
        .or_else(|| (saved.is_none() && choices.len() == 1).then_some(0));
    if let Some(index) = selected {
        let choice = choices.swap_remove(index);
        if matches!(choice.status.as_str(), "Ready" | "PreparationRequired") {
            return supervise(&mut library, &choice, &stop);
        }
        choices = vec![choice];
    }
    drop(library);
    let paths = selected_library_paths(options)?;
    let window = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([720., 420.]),
        ..Default::default()
    };
    let completed = std::sync::Arc::new(std::sync::Mutex::new(None));
    let mut state = PlayUi {
        name,
        path,
        paths,
        profile: profile.clone(),
        app: app.into(),
        auto_start: selected.is_some(),
        choices,
        index: 0,
        worker: None,
        started: None,
        phase: String::new(),
        error: None,
        active: None,
        completed: completed.clone(),
        stop: stop.clone(),
        gamepad: gilrs::Gilrs::new().ok(),
        initial_progress: None,
    };
    eframe::run_native(
        "App Depot · Play",
        window,
        Box::new(move |context| {
            context.egui_ctx.set_pixels_per_point(1.2);
            if state.auto_start
                && let Err(error) = state.start()
            {
                state.error = Some(error.to_string());
            }
            Ok(Box::new(state))
        }),
    )
    .map_err(|error| LibraryError::Invalid(format!("Could not open Play window: {error}")))?;
    let selected = completed.lock().unwrap().take();
    if let Some(choice) = selected {
        let mut library = open_library(options)?;
        supervise(&mut library, &choice, &stop)?;
    }
    Ok(())
}

struct PlayUi {
    name: String,
    path: PathBuf,
    paths: LibraryPaths,
    profile: ServerProfileId,
    app: String,
    choices: Vec<LaunchCandidate>,
    index: usize,
    worker: Option<Receiver<String>>,
    active: Option<std::process::Child>,
    auto_start: bool,
    completed: std::sync::Arc<std::sync::Mutex<Option<LaunchCandidate>>>,
    started: Option<Instant>,
    initial_progress: Option<u64>,
    stop: Arc<AtomicBool>,
    gamepad: Option<gilrs::Gilrs>,
    phase: String,
    error: Option<String>,
}

impl PlayUi {
    fn start(&mut self) -> Result<(), CliError> {
        let choice = &self.choices[self.index];
        let destination = self.path.parent().unwrap();
        fs::create_dir_all(destination).map_err(LibraryError::Io)?;
        // Distinct temp names allow two Play requests without sharing an open file.
        let tmp = self
            .path
            .with_extension(format!("{}.tmp", std::process::id()));
        let result = (|| -> io::Result<()> {
            use std::io::Write;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            serde_json::to_writer(&mut file, &choice_key(choice))?;
            file.flush()?;
            fs::rename(&tmp, &self.path)
        })();
        if let Err(error) = result {
            return Err(LibraryError::Io(error).into());
        }

        // Resume only after explicit Retry/Play. The shared library still arbitrates
        // the single transfer slot, and running jobs are never restarted here.
        let mut library = Library::open(self.paths.clone())?;
        library.reconcile_file_jobs()?;
        library.reconcile_tree_jobs()?;
        if let Some(job) = library.jobs(Some(&self.profile))?.into_iter().find(|job| {
            job.association_id == choice.association_id
                && job.artifact_id == choice.artifact_id
                && matches!(job.state.as_str(), "Paused" | "Failed")
        }) {
            library.retry_job(&job.id)?;
        }
        let mut child = Command::new(env::current_exe().map_err(LibraryError::Io)?)
            .arg("--library-data")
            .arg(&self.paths.metadata)
            .arg("--library-content")
            .arg(&self.paths.content)
            .args([
                "--profile",
                &self.profile.0,
                "decky-prepare",
                &self.app,
                &choice.release_id,
                &choice.association_id,
                &choice.entry_id,
                &choice.declaration_revision,
            ])
            .env("APP_DEPOT_DECKY_RPC", "1")
            .stderr(Stdio::piped())
            .spawn()
            .map_err(LibraryError::Io)?;
        let stderr = child.stderr.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        self.active = Some(child);
        self.worker = Some(receiver);
        self.started = Some(Instant::now());
        self.initial_progress = None;
        self.phase = "Connecting to Server…".into();
        self.error = None;
        Ok(())
    }

    fn cancel(&mut self) {
        if let Some(mut child) = self.active.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.worker = None;
    }

    fn progress(&self) -> Option<DownloadJob> {
        let library = Library::open(self.paths.clone()).ok()?;
        let choice = self.choices.get(self.index)?;
        library
            .active_association_job(&self.profile, &choice.association_id)
            .ok()
            .flatten()
    }
}

impl Drop for PlayUi {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl eframe::App for PlayUi {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        let (mut confirm, mut back, mut up, mut down) = (false, false, false, false);
        if let Some(gamepad) = &mut self.gamepad {
            while let Some(event) = gamepad.next_event() {
                use gilrs::{Button, EventType};
                match event.event {
                    EventType::ButtonPressed(Button::South, _) => confirm = true,
                    EventType::ButtonPressed(Button::East, _) => back = true,
                    EventType::ButtonPressed(Button::DPadUp, _) => up = true,
                    EventType::ButtonPressed(Button::DPadDown, _) => down = true,
                    _ => {}
                }
            }
        }
        ctx.request_repaint_after(Duration::from_millis(100));
        if self.stop.load(Ordering::Relaxed) {
            self.cancel();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if let Some(receiver) = &self.worker {
            for line in receiver.try_iter() {
                self.phase = if let Ok(data) = serde_json::from_str::<Value>(&line) {
                    match data["code"].as_str() {
                        Some("authentication_required" | "invalid_token") => "This server requires authentication to access its catalog.",
                        Some("secret_service_unavailable") => "Unlock Secret Service before retrying.",
                        Some("server_unavailable") => "The Server is unavailable. Check the connection and retry.",
                        Some("timeout") => "The Server did not respond in time. Try again.",
                        Some("tls_failure") => "The HTTPS certificate or TLS connection could not be verified.",
                        Some("catalog_forbidden") => "This profile cannot access the selected content.",
                        Some("unsupported_protocol") => "The Server protocol is not supported.",
                        _ => "App Depot could not prepare the application. Synchronize the Server profile and try again.",
                    }.into()
                } else {
                    line
                };
            }
            let exited = self
                .active
                .as_mut()
                .and_then(|child| child.try_wait().ok().flatten());
            if let Some(status) = exited {
                self.active.take();
                self.worker = None;
                if status.success() {
                    *self.completed.lock().unwrap() = Some(self.choices[self.index].clone());
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
                self.error = Some(format!("{} ({status})", self.phase));
            }
        }

        if ctx.input(|input| {
            input.viewport().close_requested() || input.key_pressed(egui::Key::Escape)
        }) {
            self.cancel();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        root.heading(&self.name);
        root.add_space(12.);
        if let Some(error) = &self.error {
            root.label(error);
        }
        let downloading = self.worker.is_some();
        if downloading {
            root.label(&self.phase);
            if let Some(job) = self.progress() {
                root.label(&job.state);
                if job.expected_size > 0 {
                    let ratio = job.progress as f32 / job.expected_size as f32;
                    root.add(egui::ProgressBar::new(ratio).show_percentage());
                    if let Some(started) = self.started {
                        let initial = *self.initial_progress.get_or_insert(job.progress);
                        let rate = job.progress.saturating_sub(initial) as f64
                            / started.elapsed().as_secs_f64().max(1.);
                        if rate > 0. && started.elapsed() > Duration::from_secs(3) {
                            root.label(format!(
                                "{:.1} MiB/s · {:.0}s remaining",
                                rate / 1048576.,
                                job.expected_size.saturating_sub(job.progress) as f64 / rate
                            ));
                        }
                    }
                } else {
                    root.spinner();
                }
            } else {
                root.spinner();
            }
            if root.button("Cancel").clicked() || back {
                self.cancel();
                self.error =
                    Some("Download interrupted. Retry resumes from a verified checkpoint.".into());
            }
            ctx.request_repaint_after(Duration::from_millis(250));
        } else if self.choices.is_empty() {
            root.label("No supported native Linux launch is declared for this App.");
        } else {
            if self.choices.len() > 1 {
                if down || ctx.input(|input| input.key_pressed(egui::Key::ArrowDown)) {
                    self.index = (self.index + 1) % self.choices.len();
                }
                if up || ctx.input(|input| input.key_pressed(egui::Key::ArrowUp)) {
                    self.index = (self.index + self.choices.len() - 1) % self.choices.len();
                }
                for (index, choice) in self.choices.iter().enumerate() {
                    let label = choice.label.as_deref().unwrap_or(&choice.entry_id);
                    if root
                        .selectable_label(
                            index == self.index,
                            format!("{label} · {} · {}", choice.release_id, choice.status),
                        )
                        .clicked()
                    {
                        self.index = index;
                    }
                }
            }
            let play = root.button(if self.error.is_some() {
                "Retry"
            } else {
                "Play"
            });
            if (play.clicked() || confirm || ctx.input(|input| input.key_pressed(egui::Key::Enter)))
                && let Err(error) = self.start()
            {
                self.error = Some(error.to_string());
            }
        }
        if root.button("Exit").clicked() || (back && !downloading) {
            self.cancel();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}
