//! Desktop commands and snapshots. All network, SQLite, hashing and image work stays here.

use app_depot_core::{
    ServerProfileId,
    api::ApiClient,
    launch::LaunchCandidate,
    library::{Library, LibraryPaths, LocalIssue, Profile},
    managed::{Installation, StorageUsage},
    models::{App, Artifact, Association, DeclarationSnapshot, Release},
    secrets::{NativeSecrets, SecretStore},
    transfer::{DownloadJob, TransferObservation, TransferPhase, VerifiedFile},
    tree_download::{TreeFileProgress, VerifiedTree},
};
use eframe::egui;
use image::ImageReader;
use std::{
    collections::{HashMap, HashSet},
    io::Cursor,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArtUse {
    Grid,
    Detail,
}

#[derive(Clone)]
pub struct AssociationView {
    pub association: Association,
    pub artifact: Option<Artifact>,
}

#[derive(Clone)]
pub struct ReleaseView {
    pub release: Release,
    pub associations: Vec<AssociationView>,
    pub declaration: Option<DeclarationSnapshot>,
}

#[derive(Clone)]
pub struct AppView {
    pub app: App,
    pub releases: Vec<ReleaseView>,
    pub stale: bool,
}

#[derive(Clone, Default)]
pub struct Snapshot {
    pub selected: Option<ServerProfileId>,
    pub profiles: Vec<Profile>,
    pub apps: Vec<AppView>,
    pub jobs: Vec<DownloadJob>,
    pub job_context: HashMap<String, (String, String, String)>,
    pub files: Vec<VerifiedFile>,
    pub trees: Vec<VerifiedTree>,
    pub local_issues: Vec<LocalIssue>,
    pub installations: Vec<Installation>,
    pub launches: Vec<LaunchCandidate>,
    #[cfg(windows)]
    pub update_occupancy_clear: Option<bool>,
    pub library_data: PathBuf,
    pub library_content: PathBuf,
}

#[derive(Clone)]
pub enum Command {
    Refresh(Option<ServerProfileId>),
    Sync(ServerProfileId),
    Connect {
        name: String,
        url: String,
        allow_http: bool,
        token: Option<String>,
        persist: bool,
        attempt: u64,
    },
    CancelConnect(u64),
    RenameProfile {
        id: ServerProfileId,
        name: String,
    },
    MoveProfile {
        id: ServerProfileId,
        url: String,
        allow_http: bool,
    },
    ArchiveProfile(ServerProfileId),
    SetToken {
        id: ServerProfileId,
        token: String,
        persist: bool,
    },
    RemoveToken(ServerProfileId),
    Download {
        profile: ServerProfileId,
        release: String,
        association: String,
        kind: String,
    },
    Pause(String),
    Resume(String),
    Retry(String),
    MoveToTop(String),
    Cancel(String),
    TreeFiles(String),
    Artwork {
        profile: ServerProfileId,
        app: String,
        usage: ArtUse,
    },
    Reveal(PathBuf),
    StorageUsage,
    RemoveInstallation {
        id: String,
        complete: bool,
    },
    VerifyInstallation(String),
    #[cfg(windows)]
    InstallArtifact(String),
    RepairInstallation(String),
    RemoveArtifact {
        profile: ServerProfileId,
        association: String,
    },
    RemoveApp {
        profile: ServerProfileId,
        app: String,
        complete: bool,
    },
    RepairSource {
        profile: ServerProfileId,
        association: String,
    },
    Play {
        profile: ServerProfileId,
        release: String,
        association: String,
        entry: String,
        declaration_revision: String,
    },
}

pub(crate) const WINDOWS_INSTALL_ONLY: bool = cfg!(windows);
pub(crate) const WINDOWS_INSTALL_NOTICE: &str = "Windows supports File and Tree installation, native Play and removal of verified working copies. Repair is unavailable.";

impl Command {
    fn blocked_on_windows(&self) -> bool {
        match self {
            Self::Download { kind, .. } => !matches!(kind.as_str(), "File" | "Tree"),
            Self::RemoveInstallation { complete, .. } => *complete,
            Self::Reveal(_)
            | Self::StorageUsage
            | Self::RepairInstallation(_)
            | Self::RemoveArtifact { .. }
            | Self::RemoveApp { .. }
            | Self::RepairSource { .. } => true,
            _ => false,
        }
    }
}

pub enum Event {
    Snapshot(Box<Snapshot>),
    StorageUsage(StorageUsage),
    Jobs {
        profile: ServerProfileId,
        jobs: Vec<DownloadJob>,
        context: Option<HashMap<String, (String, String, String)>>,
    },
    JobChanged,
    CatalogChanged(ServerProfileId),
    TreeFiles {
        job: String,
        files: Vec<TreeFileProgress>,
    },
    Transfer {
        job: String,
        observation: TransferObservation,
        at: Instant,
    },
    Artwork {
        profile: ServerProfileId,
        app: String,
        usage: ArtUse,
        binding: String,
        image: egui::ColorImage,
    },
    ArtworkMissing {
        profile: ServerProfileId,
        app: String,
        usage: ArtUse,
    },
    Notice(String),
    Error(String),
    LocalOperation {
        id: String,
        phase: Option<String>,
    },
    #[cfg(not(windows))]
    Integrity(app_depot_core::managed::InstallationIntegrity),
    SelectProfile(ServerProfileId),
    Launch {
        profile: ServerProfileId,
        association: String,
        entry: String,
        state: String,
        message: String,
    },
    SyncState {
        profile: ServerProfileId,
        running: bool,
    },
    RefreshError {
        profile: ServerProfileId,
        message: String,
    },
    ConnectionResult {
        connected: bool,
        needs_token: bool,
        message: String,
    },
}

enum Background {
    DownloadFinished {
        id: String,
        result: Result<(), String>,
    },
    Synced {
        profile: ServerProfileId,
        result: Result<(), String>,
        retryable: bool,
        rate_limited: bool,
    },
    Connected {
        attempt: u64,
        name: String,
        url: String,
        allow_http: bool,
        token: Option<String>,
        persist: bool,
        result: Result<(), app_depot_core::api::ApiError>,
    },
}

const REFRESH_INTERVAL: Duration = Duration::from_secs(600);
struct RefreshState {
    running: bool,
    pending_force: bool,
    failures: u32,
    next: Instant,
    blocked: bool,
}
impl RefreshState {
    fn new() -> Self {
        Self {
            running: false,
            pending_force: false,
            failures: 0,
            next: Instant::now(),
            blocked: false,
        }
    }
    fn finish(&mut self, success: bool, retryable: bool, rate_limited: bool, jitter: u64) {
        self.running = false;
        if success {
            self.failures = 0;
            self.blocked = false;
            self.next = Instant::now() + REFRESH_INTERVAL;
        } else if retryable {
            self.failures = self.failures.saturating_add(1);
            let seconds = (5_u64 << self.failures.min(7))
                .min(600)
                .max(if rate_limited { 60 } else { 0 })
                + jitter;
            self.next = Instant::now() + Duration::from_secs(seconds);
        } else {
            self.blocked = true;
        }
    }
    fn credential_changed(&mut self) -> bool {
        if self.running {
            self.pending_force = true;
            false
        } else {
            self.blocked = false;
            self.failures = 0;
            true
        }
    }
}
fn refresh_due(
    state: &RefreshState,
    last_sync: Option<i64>,
    at: Instant,
    epoch: i64,
    force: bool,
) -> bool {
    !state.running
        && (force
            || (!state.blocked
                && at >= state.next
                && !last_sync.is_some_and(|last| {
                    state.failures == 0
                        && epoch.saturating_sub(last) < REFRESH_INTERVAL.as_millis() as i64
                })))
}

pub struct Worker {
    pub commands: Sender<Command>,
    pub events: Receiver<Event>,
    cancelled_connect: Arc<AtomicU64>,
}

struct ArtTask {
    profile: Profile,
    token: Option<String>,
    app: String,
    usage: ArtUse,
}

impl Worker {
    pub fn start(paths: LibraryPaths, ctx: egui::Context) -> Self {
        let (commands, rx) = mpsc::channel();
        let (tx, events) = mpsc::channel();
        let cancelled_connect = Arc::new(AtomicU64::new(0));
        let cancelled = cancelled_connect.clone();
        thread::spawn(move || run(paths, rx, tx, ctx, cancelled));
        Self {
            commands,
            events,
            cancelled_connect,
        }
    }
    pub fn send(&self, command: Command) {
        if let Command::CancelConnect(attempt) = &command {
            self.cancelled_connect.store(*attempt, Ordering::SeqCst);
        }
        let _ = self.commands.send(command);
    }
}

fn send(tx: &Sender<Event>, ctx: &egui::Context, event: Event) {
    if tx.send(event).is_ok() {
        ctx.request_repaint();
    }
}

fn send_jobs(
    lib: &Library,
    profile: &ServerProfileId,
    with_context: bool,
    tx: &Sender<Event>,
    ctx: &egui::Context,
) -> Result<(), String> {
    let jobs = lib.jobs(None).map_err(|e| e.to_string())?;
    let context = if with_context {
        let mut context = HashMap::new();
        for job in &jobs {
            if let Some(value) = lib
                .display_context_for_association(&job.profile_id, &job.association_id)
                .map_err(|e| e.to_string())?
            {
                context.insert(job.id.clone(), value);
            }
        }
        Some(context)
    } else {
        None
    };
    send(
        tx,
        ctx,
        Event::Jobs {
            profile: profile.clone(),
            jobs,
            context,
        },
    );
    Ok(())
}

fn client(profile: &Profile, memory: &HashMap<String, String>) -> Result<ApiClient, String> {
    let token = match memory.get(&profile.id.0) {
        Some(value) => Some(value.clone()),
        None => profile
            .credential_ref
            .as_deref()
            .map(|reference| NativeSecrets.get(reference))
            .transpose()
            .map_err(|e| e.to_string())?,
    };
    ApiClient::new(
        &profile.origin,
        token,
        profile.origin.starts_with("http://"),
    )
    .map_err(|e| e.to_string())
}

pub(crate) fn snapshot(
    lib: &Library,
    selected: Option<&ServerProfileId>,
) -> Result<Snapshot, String> {
    let mut result = Snapshot {
        selected: selected.cloned(),
        profiles: lib.profiles(true).map_err(|e| e.to_string())?,
        library_data: lib.paths.metadata.clone(),
        library_content: lib.paths.content.clone(),
        ..Snapshot::default()
    };
    result.jobs = lib.jobs(None).map_err(|e| e.to_string())?;
    for job in &result.jobs {
        if let Some(context) = lib
            .display_context_for_association(&job.profile_id, &job.association_id)
            .map_err(|e| e.to_string())?
        {
            result.job_context.insert(job.id.clone(), context);
        }
    }
    if let Some(id) = selected {
        for row in lib.cached_apps(id).map_err(|e| e.to_string())? {
            let mut releases = Vec::new();
            for release in lib
                .cached_releases(id, &row.value.id)
                .map_err(|e| e.to_string())?
            {
                let mut associations = Vec::new();
                for association in lib
                    .cached_associations(id, &release.value.id)
                    .map_err(|e| e.to_string())?
                {
                    let artifact = lib
                        .cached_artifact(id, &association.value.artifact_id)
                        .map_err(|e| e.to_string())?
                        .map(|v| v.value);
                    associations.push(AssociationView {
                        association: association.value,
                        artifact,
                    });
                }
                releases.push(ReleaseView {
                    declaration: lib
                        .cached_snapshot(id, &release.value.id)
                        .map_err(|e| e.to_string())?
                        .map(|v| v.value),
                    release: release.value,
                    associations,
                });
            }
            result.apps.push(AppView {
                app: row.value,
                releases,
                stale: row.stale,
            });
        }
        let (files, file_issues) = lib.inspect_files(Some(id)).map_err(|e| e.to_string())?;
        result.files = files;
        result.local_issues.extend(file_issues);
        result.installations = lib.installations(Some(id)).map_err(|e| e.to_string())?;
        let (trees, tree_issues) = lib.inspect_trees(Some(id)).map_err(|e| e.to_string())?;
        result.trees = trees;
        result.local_issues.extend(tree_issues);

        for app in &result.apps {
            for release in &app.releases {
                if release.declaration.is_some() {
                    result.launches.extend(
                        lib.launch_candidates(id, &release.release.id)
                            .map_err(|e| e.to_string())?,
                    );
                }
            }
        }
    }
    #[cfg(windows)]
    {
        result.update_occupancy_clear = lib
            .windows_active_processes()
            .ok()
            .map(|processes| processes.is_empty());
    }
    Ok(result)
}

#[expect(
    clippy::too_many_arguments,
    reason = "one worker operation needs library, command, and event channels"
)]
fn operation(
    lib: &mut Library,
    selected: &mut Option<ServerProfileId>,
    memory: &mut HashMap<String, String>,
    paths: &LibraryPaths,
    cmd: Command,
    tx: &Sender<Event>,
    art_tx: &Sender<ArtTask>,
    ctx: &egui::Context,
) -> Result<bool, String> {
    if WINDOWS_INSTALL_ONLY && cmd.blocked_on_windows() {
        return Err(WINDOWS_INSTALL_NOTICE.into());
    }
    let jobs_only = matches!(
        &cmd,
        Command::Download { .. }
            | Command::Pause(_)
            | Command::Resume(_)
            | Command::Retry(_)
            | Command::MoveToTop(_)
            | Command::Cancel(_)
    );
    match cmd {
        Command::Refresh(id) => {
            *selected = id;
        }
        Command::RenameProfile { id, name } => {
            lib.rename_profile(&id, &name).map_err(|e| e.to_string())?
        }
        Command::MoveProfile {
            id,
            url,
            allow_http,
        } => {
            let profile = lib
                .replace_origin(&id, &url, allow_http)
                .map_err(|e| e.to_string())?;
            memory.remove(&id.0);
            *selected = Some(profile.id.clone());
            send(tx, ctx, Event::SelectProfile(profile.id));
        }
        Command::ArchiveProfile(id) => {
            let profile = lib.profile(&id).map_err(|e| e.to_string())?;
            if let Some(reference) = profile.credential_ref {
                NativeSecrets
                    .delete(&reference)
                    .map_err(|e| e.to_string())?;
                lib.set_credential_ref(&id, None)
                    .map_err(|e| e.to_string())?;
            }
            memory.remove(&id.0);
            lib.archive_profile(&id).map_err(|e| e.to_string())?;
            send(
                tx,
                ctx,
                Event::Notice(
                    "Profile archived; cached metadata and verified content retained".into(),
                ),
            );
        }
        Command::SetToken { id, token, persist } => {
            let profile = lib.profile(&id).map_err(|e| e.to_string())?;
            if profile.archived {
                return Err("cannot edit archived profile".into());
            }
            let candidate = ApiClient::new(
                &profile.origin,
                Some(token.clone()),
                profile.origin.starts_with("http://"),
            )
            .map_err(|e| e.to_string())?;
            let caps = candidate
                .ensure_read_compatible()
                .map_err(|e| e.to_string())?;
            candidate
                .list_apps(caps.page_size().map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            if persist {
                let mut secrets = NativeSecrets;
                if let Some(reference) = profile.credential_ref {
                    secrets
                        .replace(&reference, &token)
                        .map_err(|e| format!("Secure storage is unavailable: {e}. Choose session-only access or restore the credential service."))?;
                } else {
                    let reference = secrets.put(&token).map_err(|e| format!("Secure storage is unavailable: {e}. Choose session-only access or restore the credential service."))?;
                    if let Err(e) = lib.set_credential_ref(&id, Some(&reference)) {
                        let _ = secrets.delete(&reference);
                        return Err(e.to_string());
                    }
                }
                memory.remove(&id.0);
                send(
                    tx,
                    ctx,
                    Event::Notice("Token saved in native secure storage".into()),
                );
            } else {
                ApiClient::new(
                    &profile.origin,
                    Some(token.clone()),
                    profile.origin.starts_with("http://"),
                )
                .map_err(|e| e.to_string())?;
                memory.insert(id.0.clone(), token);
                send(
                    tx,
                    ctx,
                    Event::Notice("Session token set; it will be forgotten on exit".into()),
                );
            }
        }
        Command::RemoveToken(id) => {
            let profile = lib.profile(&id).map_err(|e| e.to_string())?;
            if let Some(reference) = profile.credential_ref {
                NativeSecrets
                    .delete(&reference)
                    .map_err(|e| e.to_string())?;
                lib.set_credential_ref(&id, None)
                    .map_err(|e| e.to_string())?;
            }
            memory.remove(&id.0);
            send(tx, ctx, Event::Notice("Profile token removed".into()));
        }
        Command::Sync(_) | Command::Connect { .. } | Command::CancelConnect(_) => {
            unreachable!("handled by worker scheduler")
        }
        Command::Download {
            profile,
            release,
            association,
            kind,
        } => {
            let p = lib.profile(&profile).map_err(|e| e.to_string())?;
            let api = client(&p, memory)?;
            let job = match kind.as_str() {
                "File" => lib.queue_file(&api, &profile, &release, &association),
                "Tree" => lib.queue_tree(&api, &profile, &release, &association),
                _ => return Err("unsupported Artifact kind".into()),
            }
            .map_err(|e| e.to_string())?;
            if job.state == "Queued" {
                lib.move_job_to_top(&job.id).map_err(|e| e.to_string())?;
            }
        }
        Command::Pause(id) => {
            lib.pause_job(&id).map_err(|e| e.to_string())?;
        }
        Command::Cancel(id) => {
            lib.cancel_job(&id).map_err(|e| e.to_string())?;
        }
        Command::Resume(id) | Command::Retry(id) => {
            lib.retry_job(&id).map_err(|e| e.to_string())?;
        }
        Command::MoveToTop(id) => {
            lib.move_job_to_top(&id).map_err(|e| e.to_string())?;
        }
        Command::TreeFiles(id) => {
            let files = lib.tree_files(&id).map_err(|e| e.to_string())?;
            send(tx, ctx, Event::TreeFiles { job: id, files });
            return Ok(false);
        }
        Command::Artwork {
            profile,
            app,
            usage,
        } => {
            let p = lib.profile(&profile).map_err(|e| e.to_string())?;
            if p.archived {
                send(
                    tx,
                    ctx,
                    Event::ArtworkMissing {
                        profile,
                        app,
                        usage,
                    },
                );
                return Ok(false);
            }
            art_tx
                .send(ArtTask {
                    token: memory.get(&profile.0).cloned(),
                    profile: p,
                    app,
                    usage,
                })
                .map_err(|_| "artwork worker unavailable".to_owned())?;
            return Ok(false);
        }
        Command::Reveal(path) => {
            if !path.exists() {
                return Err("local content no longer exists".into());
            }
            let folder = if path.is_dir() {
                path
            } else {
                path.parent()
                    .ok_or("invalid local content path")?
                    .to_path_buf()
            };
            if cfg!(target_os = "windows") {
                std::process::Command::new("explorer").arg(&folder).spawn()
            } else if cfg!(target_os = "macos") {
                std::process::Command::new("open").arg(&folder).spawn()
            } else {
                std::process::Command::new("xdg-open").arg(&folder).spawn()
            }
            .map_err(|e| e.to_string())?;
            return Ok(false);
        }
        Command::StorageUsage => {
            let paths = paths.clone();
            let tx = tx.clone();
            let ctx = ctx.clone();
            thread::spawn(
                move || match Library::open(paths).and_then(|lib| lib.storage_usage()) {
                    Ok(usage) => send(&tx, &ctx, Event::StorageUsage(usage)),
                    Err(error) => send(&tx, &ctx, Event::Error(error.to_string())),
                },
            );
            return Ok(false);
        }
        Command::RemoveInstallation { id, complete } => {
            send(
                tx,
                ctx,
                Event::LocalOperation {
                    id: id.clone(),
                    phase: Some("Checking active processes".into()),
                },
            );
            let paths = paths.clone();
            let tx = tx.clone();
            let ctx = ctx.clone();
            thread::spawn(move || {
                let report = |phase: &str| {
                    send(
                        &tx,
                        &ctx,
                        Event::LocalOperation {
                            id: id.clone(),
                            phase: Some(phase.into()),
                        },
                    )
                };
                let result = Library::open(paths).and_then(|mut lib| {
                    lib.remove_installation_observed(&id, complete, &mut |phase| report(phase))
                });
                match result {
                    Ok(())=>send(&tx,&ctx,Event::Notice(if complete{"Managed application data and local download reference removed"}else{"Writable application copy removed; verified download and managed data kept"}.into())),
                    Err(error)=>send(&tx,&ctx,Event::Error(error.to_string())),
                }
                send(&tx, &ctx, Event::LocalOperation { id, phase: None });
                send(&tx, &ctx, Event::JobChanged);
            });
            return Ok(false);
        }
        #[cfg(windows)]
        Command::InstallArtifact(id) => {
            let paths = paths.clone();
            let tx = tx.clone();
            let ctx = ctx.clone();
            thread::spawn(move || {
                send(
                    &tx,
                    &ctx,
                    Event::LocalOperation {
                        id: id.clone(),
                        phase: Some("Installing verified Artifact".into()),
                    },
                );
                match Library::open(paths).and_then(|mut lib| lib.install_windows_artifact(&id)) {
                    Ok(_) => send(&tx, &ctx, Event::Notice("Artifact installed".into())),
                    Err(error) => send(&tx, &ctx, Event::Error(error.to_string())),
                }
                send(&tx, &ctx, Event::LocalOperation { id, phase: None });
                send(&tx, &ctx, Event::JobChanged);
            });
            return Ok(false);
        }
        Command::VerifyInstallation(id) => {
            let paths = paths.clone();
            let tx = tx.clone();
            let ctx = ctx.clone();
            thread::spawn(move || {
                send(
                    &tx,
                    &ctx,
                    Event::LocalOperation {
                        id: id.clone(),
                        phase: Some("Comparing installed files with the published Artifact".into()),
                    },
                );
                #[cfg(not(windows))]
                match Library::open(paths).and_then(|lib| lib.verify_installation(&id)) {
                    Ok(report) => send(&tx, &ctx, Event::Integrity(report)),
                    Err(error) => send(&tx, &ctx, Event::Error(error.to_string())),
                }
                #[cfg(windows)]
                match Library::open(paths).and_then(|lib| lib.verify_windows_installation(&id)) {
                    Ok(()) => send(
                        &tx,
                        &ctx,
                        Event::Notice("Installed payload matches its Artifact".into()),
                    ),
                    Err(error) => send(&tx, &ctx, Event::Error(error.to_string())),
                }
                send(&tx, &ctx, Event::LocalOperation { id, phase: None });
            });
            return Ok(false);
        }
        Command::RepairInstallation(id) => {
            let paths = paths.clone();
            let tx = tx.clone();
            let ctx = ctx.clone();
            thread::spawn(move || {
                send(
                    &tx,
                    &ctx,
                    Event::LocalOperation {
                        id: id.clone(),
                        phase: Some(
                            "Preserving modified installation and restoring original files".into(),
                        ),
                    },
                );
                match Library::open(paths).and_then(|mut lib| lib.repair_installation(&id)) {
                    Ok((_, backup)) => send(
                        &tx,
                        &ctx,
                        Event::Notice(backup.map_or(
                            "Installation restored from verified local content".into(),
                            |backup| {
                                format!(
                                    "Installation restored. Previous files kept at {}",
                                    backup.display()
                                )
                            },
                        )),
                    ),
                    Err(error) => send(&tx, &ctx, Event::Error(error.to_string())),
                }
                send(&tx, &ctx, Event::LocalOperation { id, phase: None });
                send(&tx, &ctx, Event::JobChanged);
            });
            return Ok(false);
        }
        Command::RemoveArtifact {
            profile,
            association,
        } => {
            send(
                tx,
                ctx,
                Event::LocalOperation {
                    id: association.clone(),
                    phase: Some("Removing local download and unreferenced content".into()),
                },
            );
            let paths = paths.clone();
            let tx = tx.clone();
            let ctx = ctx.clone();
            thread::spawn(move || {
                let result = Library::open(paths)
                    .and_then(|mut lib| lib.remove_local_artifact(&profile, &association));
                match result {
                    Ok(()) => send(
                        &tx,
                        &ctx,
                        Event::Notice("Selected local download removed".into()),
                    ),
                    Err(error) => send(&tx, &ctx, Event::Error(error.to_string())),
                };
                send(
                    &tx,
                    &ctx,
                    Event::LocalOperation {
                        id: association,
                        phase: None,
                    },
                );
                send(&tx, &ctx, Event::JobChanged);
            });
            return Ok(false);
        }
        Command::RemoveApp {
            profile,
            app,
            complete,
        } => {
            send(
                tx,
                ctx,
                Event::LocalOperation {
                    id: app.clone(),
                    phase: Some("Removing application items".into()),
                },
            );
            let paths = paths.clone();
            let tx = tx.clone();
            let ctx = ctx.clone();
            thread::spawn(move || {
                let report = |index: usize, total: usize| {
                    send(
                        &tx,
                        &ctx,
                        Event::LocalOperation {
                            id: app.clone(),
                            phase: Some(format!("Removing item {index} of {total}")),
                        },
                    )
                };
                let result = Library::open(paths).and_then(|mut lib| {
                    lib.remove_local_app_observed(&profile, &app, complete, &mut |index, total| {
                        report(index, total)
                    })
                });
                match result {
                    Ok(count) => send(
                        &tx,
                        &ctx,
                        Event::Notice(format!(
                            "Processed {count} local application item(s); Server catalog and account remain"
                        )),
                    ),
                    Err(error) => send(&tx, &ctx, Event::Error(error.to_string())),
                };
                send(
                    &tx,
                    &ctx,
                    Event::LocalOperation {
                        id: app,
                        phase: None,
                    },
                );
                send(&tx, &ctx, Event::JobChanged);
            });
            return Ok(false);
        }
        Command::RepairSource {
            profile,
            association,
        } => {
            let paths = paths.clone();
            let tx = tx.clone();
            let ctx = ctx.clone();
            thread::spawn(move || {
                let result = Library::open(paths)
                    .and_then(|mut lib| lib.repair_verified_tree(&profile, &association));
                match result {
                    Ok((_, quarantine)) => {
                        let detail = quarantine
                            .map_or("No previous directory was present.".into(), |p| {
                                format!("Previous files are preserved at {}", p.display())
                            });
                        send(
                            &tx,
                            &ctx,
                            Event::Notice(format!(
                                "Verified source restored from local objects. {detail}"
                            )),
                        );
                    }
                    Err(error) => send(&tx, &ctx, Event::Error(error.to_string())),
                }
                send(&tx, &ctx, Event::JobChanged);
            });
            return Ok(false);
        }
        Command::Play {
            profile,
            release,
            association,
            entry,
            declaration_revision,
        } => {
            static ACTIVE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
            let active = ACTIVE.get_or_init(|| Mutex::new(HashSet::new()));
            let key = format!("{}:{association}:{entry}", profile.0);
            if !active
                .lock()
                .map_err(|_| "launch state unavailable")?
                .insert(key.clone())
            {
                return Err("This program is already running or starting".into());
            }
            let paths = paths.clone();
            let tx = tx.clone();
            let ctx = ctx.clone();
            thread::spawn(move || {
                let announce = |state: &str, message: String| {
                    send(
                        &tx,
                        &ctx,
                        Event::Launch {
                            profile: profile.clone(),
                            association: association.clone(),
                            entry: entry.clone(),
                            state: state.into(),
                            message,
                        },
                    )
                };
                announce("Starting", "Checking local content before launch".into());
                let result = (|| -> Result<(), String> {
                    let mut lib = Library::open(paths).map_err(|e| e.to_string())?;
                    let candidate = lib
                        .launch_candidates(&profile, &release)
                        .map_err(|e| e.to_string())?
                        .into_iter()
                        .find(|c| c.association_id == association && c.entry_id == entry)
                        .ok_or("selected launch declaration is no longer cached")?;
                    if candidate.selection_revision() != declaration_revision {
                        return Err(
                            "Launch declaration changed; review the updated choice before playing"
                                .into(),
                        );
                    }
                    if candidate.status == "PreparationRequired" {
                        announce("Preparing", "Creating the writable application copy".into());
                        lib.prepare_portable(&profile, &association)
                            .map_err(|e| format!("Preparation failed: {e}"))?;
                        send(&tx, &ctx, Event::JobChanged);
                    } else if candidate.status != "Ready" {
                        return Err(format!("{}: {}", candidate.status, candidate.reason));
                    }
                    let mut child = lib
                        .play_declared(
                            &profile,
                            &release,
                            &association,
                            &entry,
                            &declaration_revision,
                        )
                        .map_err(|e| e.to_string())?;
                    let pid = child.id();
                    if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                        announce(
                            "Exited",
                            format!("Process {pid} exited immediately: {status}"),
                        );
                        return Ok(());
                    }
                    announce("Running", format!("Process {pid} started"));
                    let status = child.wait().map_err(|e| e.to_string())?;
                    announce("Exited", format!("Process {pid} exited: {status}"));
                    Ok(())
                })();
                if let Err(error) = result {
                    announce("Launch failed", error)
                }
                if let Some(active) = ACTIVE.get()
                    && let Ok(mut active) = active.lock()
                {
                    active.remove(&key);
                }
            });
            return Ok(false);
        }
    }
    if jobs_only {
        if let Some(profile) = selected.as_ref() {
            send_jobs(lib, profile, true, tx, ctx)?;
        }
        Ok(false)
    } else {
        Ok(true)
    }
}

fn artwork_loop(rx: Receiver<ArtTask>, tx: Sender<Event>, ctx: egui::Context) {
    for task in rx {
        let ArtTask {
            profile,
            token,
            app,
            usage,
        } = task;
        let id = profile.id.clone();
        let mut memory = HashMap::new();
        if let Some(token) = token {
            memory.insert(id.0.clone(), token);
        }
        let result = client(&profile, &memory).and_then(|api| {
            let items = match api.list_artwork(&app) {
                Ok(items) => items,
                Err(error) => return Err(error.to_string()),
            };
            let roles: &[&str] = match usage {
                ArtUse::Grid => &[
                    "steam.grid.horizontal",
                    "icon",
                    "steam.grid.vertical",
                    "logo",
                ],
                ArtUse::Detail => &["steam.hero", "steam.grid.horizontal", "icon", "logo"],
            };
            let item = roles
                .iter()
                .find_map(|role| items.iter().find(|item| item.role == *role));
            if let Some(item) = item {
                let image = api
                    .artwork_bytes(item)
                    .map_err(|e| e.to_string())
                    .and_then(|bytes| {
                        decode_image(&bytes, item.width, item.height, &item.mime_type)
                    });
                image.map(|image| Some((format!("{}:{}", item.id, item.digest), image)))
            } else {
                Ok(None)
            }
        });
        match result {
            Ok(Some((binding, image))) => send(
                &tx,
                &ctx,
                Event::Artwork {
                    profile: id,
                    app,
                    usage,
                    binding,
                    image,
                },
            ),
            Ok(None) => send(
                &tx,
                &ctx,
                Event::ArtworkMissing {
                    profile: id,
                    app,
                    usage,
                },
            ),
            Err(_) => {
                send(
                    &tx,
                    &ctx,
                    Event::ArtworkMissing {
                        profile: id,
                        app,
                        usage,
                    },
                );
            }
        }
    }
}

fn decode_image(
    bytes: &[u8],
    width: u32,
    height: u32,
    mime: &str,
) -> Result<egui::ColorImage, String> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let expected = match mime {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        "image/webp" => image::ImageFormat::WebP,
        _ => return Err("unsupported artwork MIME type".into()),
    };
    if reader.format() != Some(expected) {
        return Err("artwork encoding differs from binding MIME type".into());
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|e| e.to_string())?;
    if image.width() != width
        || image.height() != height
        || u64::from(width) * u64::from(height) > 16_000_000
    {
        return Err("artwork dimensions differ from binding".into());
    }
    let rgba = image.to_rgba8();
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        rgba.as_raw(),
    ))
}

fn start_job(
    paths: LibraryPaths,
    profile: Profile,
    memory_token: Option<String>,
    job: DownloadJob,
    tx: Sender<Event>,
    ctx: egui::Context,
    background: Sender<Background>,
) {
    thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            let mut lib = Library::open(paths).map_err(|e| e.to_string())?;
            let mut memory = HashMap::new();
            if let Some(token) = memory_token {
                memory.insert(profile.id.0.clone(), token);
            }
            let api = client(&profile, &memory)?;
            let mut last_sent = Instant::now() - Duration::from_secs(1);
            let mut observe = |observation: TransferObservation| {
                let at = Instant::now();
                let important = matches!(
                    observation.phase,
                    TransferPhase::PreparingInventory
                        | TransferPhase::PreparingLocalFiles
                        | TransferPhase::SavingVerifiedContent
                ) || (observation.phase == TransferPhase::Verifying
                    && observation.files_done == observation.files_total);
                let interval = if observation.phase == TransferPhase::Downloading {
                    Duration::from_millis(150)
                } else if observation.phase == TransferPhase::WaitingForServer {
                    Duration::from_secs(2)
                } else {
                    Duration::from_millis(500)
                };
                if important || at.duration_since(last_sent) >= interval {
                    last_sent = at;
                    send(
                        &tx,
                        &ctx,
                        Event::Transfer {
                            job: job.id.clone(),
                            observation,
                            at,
                        },
                    );
                }
            };
            if job.artifact_id.starts_with("adtree:") {
                lib.run_tree_job_observed(&api, &job.id, None, &mut observe)
                    .map_err(|e| e.to_string())?;
            } else {
                lib.run_file_job_observed(&api, &job.id, None, &mut observe)
                    .map_err(|e| e.to_string())?;
            }
            if !WINDOWS_INSTALL_ONLY
                && lib.job(&job.id).map_err(|e| e.to_string())?.state == "Completed"
            {
                let release: Option<String> = lib
                    .release_for_association(&job.profile_id, &job.association_id)
                    .map_err(|e| e.to_string())?;
                if let Some(release) = release {
                    let ready = lib
                        .launch_candidates(&job.profile_id, &release)
                        .map_err(|e| e.to_string())?
                        .iter()
                        .any(|c| {
                            c.association_id == job.association_id
                                && c.status == "PreparationRequired"
                        });
                    if ready {
                        send(
                            &tx,
                            &ctx,
                            Event::Notice("Preparing the downloaded application for Play".into()),
                        );
                        lib.prepare_portable(&job.profile_id, &job.association_id)
                            .map_err(|e| format!("Automatic preparation failed: {e}"))?;
                    }
                }
            }
            Ok(())
        })();
        let _ = background.send(Background::DownloadFinished {
            id: job.id.clone(),
            result,
        });
        send(&tx, &ctx, Event::JobChanged);
    });
}

fn run(
    paths: LibraryPaths,
    rx: Receiver<Command>,
    tx: Sender<Event>,
    ctx: egui::Context,
    cancelled_connect: Arc<AtomicU64>,
) {
    let (background_tx, background_rx) = mpsc::channel::<Background>();
    let (art_tx, art_rx) = mpsc::channel();
    thread::spawn({
        let tx = tx.clone();
        let ctx = ctx.clone();
        move || artwork_loop(art_rx, tx, ctx)
    });
    let mut lib = match Library::open(paths.clone()) {
        Ok(mut lib) => {
            if let Err(e) = lib
                .reconcile_file_jobs()
                .and_then(|_| lib.reconcile_tree_jobs())
                .and_then(|_| lib.restore_preempted_jobs())
            {
                send(&tx, &ctx, Event::Error(format!("Job recovery: {e}")));
            }
            #[cfg(windows)]
            if let Err(error) = lib.reconcile_windows_installations() {
                send(
                    &tx,
                    &ctx,
                    Event::Error(format!("Installation recovery: {error}")),
                );
            }
            lib
        }
        Err(e) => {
            send(
                &tx,
                &ctx,
                Event::Error(format!("Cannot open local library: {e}")),
            );
            return;
        }
    };
    let mut selected = None;
    let mut memory = HashMap::new();
    let mut refreshes: HashMap<String, RefreshState> = HashMap::new();
    let mut pending_connect: Option<u64> = None;
    let mut active_download: Option<String> = None;
    let mut next_download_try = Instant::now();
    if let Ok(value) = snapshot(&lib, None) {
        send(&tx, &ctx, Event::Snapshot(Box::new(value)));
    }
    #[cfg(windows)]
    let mut previous_processes = Vec::new();
    #[cfg(windows)]
    let mut next_process_poll = Instant::now();
    loop {
        #[cfg(windows)]
        if Instant::now() >= next_process_poll {
            next_process_poll = Instant::now() + Duration::from_secs(1);
            if let Ok(processes) = lib.windows_active_processes()
                && processes != previous_processes
            {
                previous_processes = processes;
                if let Ok(value) = snapshot(&lib, selected.as_ref()) {
                    send(&tx, &ctx, Event::Snapshot(Box::new(value)));
                }
            }
        }
        while let Ok(done) = background_rx.try_recv() {
            match done {
                Background::DownloadFinished { id, result } => {
                    if active_download.as_deref() == Some(&id) {
                        active_download = None;
                    }
                    if let Err(error) = result {
                        if error.contains("another application download holds the library slot") {
                            next_download_try = Instant::now() + Duration::from_secs(1);
                        } else {
                            let _ = lib.fail_queued_job(&id, &error);
                            send(&tx, &ctx, Event::Error(format!("Download: {error}")));
                        }
                    }
                    if let Err(error) = lib.restore_preempted_jobs() {
                        send(&tx, &ctx, Event::Error(error.to_string()));
                    }
                    send(&tx, &ctx, Event::JobChanged);
                }
                Background::Synced {
                    profile,
                    result,
                    retryable,
                    rate_limited,
                } => {
                    let jitter = profile
                        .0
                        .bytes()
                        .fold(0_u64, |sum, b| sum.wrapping_add(u64::from(b)))
                        % 7;
                    let state = refreshes
                        .entry(profile.0.clone())
                        .or_insert_with(RefreshState::new);
                    let pending_force = state.pending_force;
                    state.pending_force = false;
                    state.finish(result.is_ok(), retryable, rate_limited, jitter);
                    send(
                        &tx,
                        &ctx,
                        Event::SyncState {
                            profile: profile.clone(),
                            running: false,
                        },
                    );
                    if result.is_ok() {
                        send(&tx, &ctx, Event::CatalogChanged(profile.clone()));
                    }
                    if let Err(error) = result
                        && selected.as_ref() == Some(&profile)
                    {
                        send(
                            &tx,
                            &ctx,
                            Event::RefreshError {
                                profile: profile.clone(),
                                message: error,
                            },
                        );
                    }
                    if let Ok(value) = snapshot(&lib, selected.as_ref()) {
                        send(&tx, &ctx, Event::Snapshot(Box::new(value)));
                    }
                    if pending_force {
                        schedule_sync(
                            &paths,
                            &lib,
                            &memory,
                            &mut refreshes,
                            &profile,
                            true,
                            &background_tx,
                            &tx,
                            &ctx,
                        );
                    }
                }
                Background::Connected {
                    attempt,
                    name,
                    url,
                    allow_http,
                    token,
                    persist,
                    result,
                } => {
                    if pending_connect != Some(attempt)
                        || cancelled_connect.load(Ordering::SeqCst) == attempt
                    {
                        continue;
                    }
                    pending_connect = None;
                    match result {
                        Ok(()) => {
                            let credential_requested = token.is_some();
                            let created = (|| -> Result<Profile, String> {
                                let reference = if persist {
                                    token
                                        .as_ref()
                                        .map(|value| {
                                            NativeSecrets.put(value).map_err(|e| e.to_string())
                                        })
                                        .transpose()?
                                } else {
                                    None
                                };
                                let profile = match lib.add_profile(&name, &url, allow_http) {
                                    Ok(profile) => profile,
                                    Err(error) => {
                                        if let Some(reference) = reference {
                                            let _ = NativeSecrets.delete(&reference);
                                        }
                                        return Err(error.to_string());
                                    }
                                };
                                if let Some(reference) = reference {
                                    lib.set_credential_ref(&profile.id, Some(&reference))
                                        .map_err(|e| e.to_string())?;
                                } else if let Some(token) = token {
                                    memory.insert(profile.id.0.clone(), token);
                                }
                                lib.profile(&profile.id).map_err(|e| e.to_string())
                            })();
                            match created {
                                Ok(profile) => {
                                    selected = Some(profile.id.clone());
                                    send(&tx, &ctx, Event::SelectProfile(profile.id.clone()));
                                    send(
                                        &tx,
                                        &ctx,
                                        Event::ConnectionResult {
                                            connected: true,
                                            needs_token: false,
                                            message: "Server connected. Loading catalog…".into(),
                                        },
                                    );
                                    if let Ok(value) = snapshot(&lib, selected.as_ref()) {
                                        send(&tx, &ctx, Event::Snapshot(Box::new(value)));
                                    }
                                    schedule_sync(
                                        &paths,
                                        &lib,
                                        &memory,
                                        &mut refreshes,
                                        &profile.id,
                                        true,
                                        &background_tx,
                                        &tx,
                                        &ctx,
                                    );
                                }
                                Err(error) => send(
                                    &tx,
                                    &ctx,
                                    Event::ConnectionResult {
                                        connected: false,
                                        needs_token: credential_requested,
                                        message: if credential_requested && persist {
                                            format!(
                                                "Could not save this credential securely: {error}. Choose Session only or make the system credential service available."
                                            )
                                        } else {
                                            error
                                        },
                                    },
                                ),
                            }
                        }
                        Err(error) => {
                            let needs_token = matches!(
                                error,
                                app_depot_core::api::ApiError::Authentication(_)
                                    | app_depot_core::api::ApiError::Authorization(_)
                            );
                            send(
                                &tx,
                                &ctx,
                                Event::ConnectionResult {
                                    connected: false,
                                    needs_token,
                                    message: if matches!(
                                        error,
                                        app_depot_core::api::ApiError::Authorization(_)
                                    ) {
                                        "This account cannot browse the catalog. Use a token with catalog access or ask the Server administrator for permission.".into()
                                    } else if needs_token {
                                        if token.is_some() {
                                            "The token was not accepted. Check it or create a new personal API token on the Server Account page.".into()
                                        } else {
                                            "This catalog requires a personal API token.".into()
                                        }
                                    } else {
                                        connection_guidance(&error)
                                    },
                                },
                            );
                        }
                    }
                }
            }
        }
        if let Some(current) = active_download.as_deref()
            && let (Ok(active), Ok(Some(next))) = (lib.job(current), lib.next_queued_job())
            && next.id != current
            && next.queue_rank > active.queue_rank
        {
            let _ = lib.preempt_job(current);
        }
        if active_download.is_none() && Instant::now() >= next_download_try {
            if let Err(error) = lib.restore_preempted_jobs() {
                send(&tx, &ctx, Event::Error(error.to_string()));
            }
            match lib.next_queued_job() {
                Ok(Some(job)) => match lib.profile(&job.profile_id) {
                    Ok(profile) => {
                        let token = memory.get(&job.profile_id.0).cloned();
                        active_download = Some(job.id.clone());
                        start_job(
                            paths.clone(),
                            profile,
                            token,
                            job,
                            tx.clone(),
                            ctx.clone(),
                            background_tx.clone(),
                        );
                    }
                    Err(error) => send(&tx, &ctx, Event::Error(error.to_string())),
                },
                Ok(None) => next_download_try = Instant::now() + Duration::from_secs(1),
                Err(error) => send(&tx, &ctx, Event::Error(error.to_string())),
            }
        }
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(Command::Connect {
                name,
                url,
                allow_http,
                token,
                persist,
                attempt,
            }) => {
                pending_connect = Some(attempt);
                let out = background_tx.clone();
                thread::spawn(move || {
                    let result = ApiClient::new(&url, token.clone(), allow_http).and_then(|api| {
                        let caps = api.ensure_read_compatible()?;
                        api.list_apps(
                            caps.page_size()
                                .map_err(app_depot_core::api::ApiError::Incompatible)?,
                        )?;
                        Ok(())
                    });
                    let _ = out.send(Background::Connected {
                        attempt,
                        name,
                        url,
                        allow_http,
                        token,
                        persist,
                        result,
                    });
                });
            }
            Ok(Command::CancelConnect(attempt)) => {
                if pending_connect == Some(attempt) {
                    pending_connect = None;
                }
            }
            Ok(Command::Sync(id)) => {
                schedule_sync(
                    &paths,
                    &lib,
                    &memory,
                    &mut refreshes,
                    &id,
                    true,
                    &background_tx,
                    &tx,
                    &ctx,
                );
            }
            Ok(cmd) => {
                let priority_change =
                    matches!(&cmd, Command::Download { .. } | Command::MoveToTop(_));
                let activate = match &cmd {
                    Command::Refresh(Some(id)) => Some(id.clone()),
                    _ => None,
                };
                let token_change = match &cmd {
                    Command::SetToken { id, .. } | Command::RemoveToken(id) => Some(id.clone()),
                    _ => None,
                };
                let succeeded = match operation(
                    &mut lib,
                    &mut selected,
                    &mut memory,
                    &paths,
                    cmd,
                    &tx,
                    &art_tx,
                    &ctx,
                ) {
                    Ok(refresh) => {
                        if refresh {
                            match snapshot(&lib, selected.as_ref()) {
                                Ok(value) => send(&tx, &ctx, Event::Snapshot(Box::new(value))),
                                Err(e) => send(&tx, &ctx, Event::Error(e)),
                            }
                        }
                        true
                    }
                    Err(e) => {
                        send(&tx, &ctx, Event::Error(e));
                        if let Ok(value) = snapshot(&lib, selected.as_ref()) {
                            send(&tx, &ctx, Event::Snapshot(Box::new(value)));
                        }
                        false
                    }
                };
                if let Some(id) = token_change.filter(|_| succeeded) {
                    let state = refreshes
                        .entry(id.0.clone())
                        .or_insert_with(RefreshState::new);
                    if state.credential_changed() {
                        schedule_sync(
                            &paths,
                            &lib,
                            &memory,
                            &mut refreshes,
                            &id,
                            true,
                            &background_tx,
                            &tx,
                            &ctx,
                        );
                    }
                }
                if priority_change
                    && succeeded
                    && let Some(current) = active_download.as_deref()
                    && let (Ok(active), Ok(Some(next))) = (lib.job(current), lib.next_queued_job())
                    && next.id != current
                    && next.queue_rank > active.queue_rank
                {
                    let _ = lib.preempt_job(current);
                }
                if priority_change && succeeded {
                    next_download_try = Instant::now();
                }
                if let Some(id) = activate {
                    schedule_sync(
                        &paths,
                        &lib,
                        &memory,
                        &mut refreshes,
                        &id,
                        false,
                        &background_tx,
                        &tx,
                        &ctx,
                    );
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(id) = selected.as_ref() {
                    let _ = send_jobs(&lib, id, false, &tx, &ctx);
                }
                if let Some(id) = selected.as_ref() {
                    schedule_sync(
                        &paths,
                        &lib,
                        &memory,
                        &mut refreshes,
                        id,
                        false,
                        &background_tx,
                        &tx,
                        &ctx,
                    );
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn schedule_sync(
    paths: &LibraryPaths,
    lib: &Library,
    memory: &HashMap<String, String>,
    states: &mut HashMap<String, RefreshState>,
    id: &ServerProfileId,
    force: bool,
    out: &Sender<Background>,
    events: &Sender<Event>,
    ctx: &egui::Context,
) {
    let Ok(profile) = lib.profile(id) else { return };
    if profile.archived {
        return;
    }
    let state = states.entry(id.0.clone()).or_insert_with(RefreshState::new);
    if !refresh_due(
        state,
        profile.last_sync,
        Instant::now(),
        unix_now_millis(),
        force,
    ) {
        return;
    }
    if force {
        state.blocked = false;
        state.failures = 0;
    }
    state.running = true;
    send(
        events,
        ctx,
        Event::SyncState {
            profile: id.clone(),
            running: true,
        },
    );
    let paths = paths.clone();
    let id = id.clone();
    let token = memory.get(&id.0).cloned();
    let out = out.clone();
    thread::spawn(move || {
        let result = (|| -> Result<(), app_depot_core::library::LibraryError> {
            let mut library = Library::open(paths)?;
            let mut memory = HashMap::new();
            if let Some(token) = token {
                memory.insert(id.0.clone(), token);
            }
            let api = client(&profile, &memory)
                .map_err(app_depot_core::library::LibraryError::Invalid)?;
            library.sync(&id, &api)?;
            Ok(())
        })();
        let retryable = matches!(
            &result,
            Err(app_depot_core::library::LibraryError::Api(
                app_depot_core::api::ApiError::Transport(_)
                    | app_depot_core::api::ApiError::Http {
                        status: 429 | 500..=599,
                        ..
                    }
            ))
        );
        let rate_limited = matches!(
            &result,
            Err(app_depot_core::library::LibraryError::Api(
                app_depot_core::api::ApiError::Http { status: 429, .. }
            ))
        );
        let _ = out.send(Background::Synced {
            profile: id,
            result: result.map_err(|e| e.to_string()),
            retryable,
            rate_limited,
        });
    });
}

fn unix_now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn connection_guidance(error: &app_depot_core::api::ApiError) -> String {
    use app_depot_core::api::ApiError;
    match error {
        ApiError::Config(_) => "Enter the Server origin URL, without a path or credentials.".into(),
        ApiError::Transport(_) => "Could not reach this Server. Check its address and your network connection, then try again.".into(),
        ApiError::Authorization(_) => "This account cannot browse the catalog. Check its Server permissions.".into(),
        ApiError::Incompatible(_) => "This Server's API is not compatible with this client. Check the Server API version.".into(),
        ApiError::Protocol(_) => "The Server returned an invalid response. Check that this is an App Depot Server and try again.".into(),
        ApiError::Http { status: 429, .. } => "The Server is limiting requests. Wait a moment before trying again.".into(),
        ApiError::Http { status, .. } => format!("The Server could not complete this request (HTTP {status}). Try again later."),
        ApiError::Authentication(_) => "This catalog requires a personal API token.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refresh_policy_deduplicates_and_backs_off() {
        let mut state = RefreshState::new();
        let now = Instant::now();
        assert!(refresh_due(&state, None, now, 1000, false));
        state.running = true;
        assert!(!refresh_due(&state, None, now, 1000, true));
        state.finish(false, true, false, 3);
        assert!(!refresh_due(&state, None, Instant::now(), 1000, false));
        assert!(!state.blocked);
        state.finish(false, false, false, 0);
        assert!(state.blocked);
        assert!(!refresh_due(&state, None, Instant::now(), 1000, false));
        assert!(refresh_due(&state, None, Instant::now(), 1000, true));
        state.finish(true, false, false, 0);
        assert!(!refresh_due(
            &state,
            Some(1000),
            Instant::now(),
            1001,
            false
        ));
        assert!(refresh_due(&state, Some(1000), Instant::now(), 1001, true));
        let fresh = RefreshState::new();
        assert!(!refresh_due(
            &fresh,
            Some(1_000_000),
            Instant::now(),
            1_001_000,
            false
        ));
        assert!(refresh_due(
            &fresh,
            Some(1_000_000),
            Instant::now(),
            1_601_000,
            false
        ));
        let mut limited = RefreshState::new();
        limited.finish(false, true, true, 0);
        assert!(limited.next > Instant::now() + Duration::from_secs(59));
        let mut changing = RefreshState::new();
        changing.running = true;
        assert!(!changing.credential_changed());
        assert!(changing.pending_force);
        assert!(!refresh_due(&changing, None, Instant::now(), 0, true));
        changing.finish(false, false, false, 0);
        assert!(changing.pending_force);
        changing.pending_force = false;
        assert!(refresh_due(&changing, None, Instant::now(), 0, true));
    }
    #[test]
    fn malformed_and_oversized_artwork_is_rejected() {
        assert!(decode_image(b"not an image", 1, 1, "image/png").is_err());
        let encoded: &[u8] = &[
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 11, 73, 68, 65, 84, 120, 156, 99, 248, 207,
            192, 240, 31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66,
            96, 130,
        ];
        assert!(decode_image(encoded, 100_000, 100_000, "image/png").is_err());
        assert!(decode_image(encoded, 1, 1, "image/jpeg").is_err());
    }
    #[test]
    fn connection_errors_offer_distinct_recovery() {
        use app_depot_core::api::ApiError;
        assert!(connection_guidance(&ApiError::Transport("offline".into())).contains("network"));
        assert!(connection_guidance(&ApiError::Authorization(None)).contains("permissions"));
        assert!(
            connection_guidance(&ApiError::Incompatible("version".into())).contains("compatible")
        );
        assert!(
            connection_guidance(&ApiError::Protocol("bad".into())).contains("invalid response")
        );
    }
}
