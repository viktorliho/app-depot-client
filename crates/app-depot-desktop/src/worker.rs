//! Desktop commands and snapshots. All network, SQLite, hashing and image work stays here.

use app_depot_core::{
    ServerProfileId,
    api::ApiClient,
    library::{Library, LibraryPaths, Profile},
    models::{App, Artifact, Association, DeclarationSnapshot, Release},
    secrets::{NativeSecrets, SecretStore},
    transfer::{DownloadJob, VerifiedFile},
    tree_download::{TreeFileProgress, VerifiedTree},
};
use eframe::egui;
use image::ImageReader;
use std::{
    collections::HashMap,
    io::Cursor,
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
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
    pub profiles: Vec<Profile>,
    pub apps: Vec<AppView>,
    pub jobs: Vec<DownloadJob>,
    pub files: Vec<VerifiedFile>,
    pub trees: Vec<VerifiedTree>,
    pub library_data: PathBuf,
    pub library_content: PathBuf,
}

#[derive(Clone, Debug)]
pub enum Command {
    Refresh(Option<ServerProfileId>),
    Sync(ServerProfileId),
    Check(ServerProfileId),
    AddProfile {
        name: String,
        url: String,
        allow_http: bool,
    },
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
    Cancel(String),
    TreeFiles(String),
    Artwork {
        profile: ServerProfileId,
        app: String,
        usage: ArtUse,
    },
    Reveal(PathBuf),
}

pub enum Event {
    Snapshot(Snapshot),
    Jobs(Vec<DownloadJob>),
    TreeFiles {
        job: String,
        files: Vec<TreeFileProgress>,
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
    SelectProfile(ServerProfileId),
    Synced,
}

pub struct Worker {
    pub commands: Sender<Command>,
    pub events: Receiver<Event>,
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
        thread::spawn(move || run(paths, rx, tx, ctx));
        Self { commands, events }
    }
}

fn send(tx: &Sender<Event>, ctx: &egui::Context, event: Event) {
    if tx.send(event).is_ok() {
        ctx.request_repaint();
    }
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
        profiles: lib.profiles(true).map_err(|e| e.to_string())?,
        library_data: lib.paths.metadata.clone(),
        library_content: lib.paths.content.clone(),
        ..Snapshot::default()
    };
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
        result.jobs = lib.jobs(Some(id)).map_err(|e| e.to_string())?;
        result.files = lib.verified_files(Some(id)).map_err(|e| e.to_string())?;
        result.trees = lib.verified_trees(Some(id)).map_err(|e| e.to_string())?;
    }
    Ok(result)
}

fn status(error: &app_depot_core::api::ApiError) -> &'static str {
    use app_depot_core::api::ApiError;
    match error {
        ApiError::Transport(_) => "unreachable",
        ApiError::Authentication(_) => "authentication-required",
        ApiError::Authorization(_) => "forbidden",
        ApiError::Incompatible(_) => "incompatible",
        ApiError::Protocol(_) => "protocol-error",
        _ => "http-error",
    }
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
    match cmd {
        Command::Refresh(id) => {
            *selected = id;
        }
        Command::AddProfile {
            name,
            url,
            allow_http,
        } => {
            let profile = lib
                .add_profile(&name, &url, allow_http)
                .map_err(|e| e.to_string())?;
            *selected = Some(profile.id.clone());
            send(tx, ctx, Event::SelectProfile(profile.id));
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
            if persist {
                let mut secrets = NativeSecrets;
                if let Some(reference) = profile.credential_ref {
                    secrets
                        .replace(&reference, &token)
                        .map_err(|e| e.to_string())?;
                } else {
                    let reference = secrets.put(&token).map_err(|e| e.to_string())?;
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
        Command::Check(id) => {
            let profile = lib.profile(&id).map_err(|e| e.to_string())?;
            let api = client(&profile, memory)?;
            match api.capabilities() {
                Ok(caps) => {
                    lib.connection_result(&id, Some(&caps), "connected")
                        .map_err(|e| e.to_string())?;
                    let issues = caps.incompatibilities();
                    send(
                        tx,
                        ctx,
                        Event::Notice(if issues.is_empty() {
                            "Server connected".into()
                        } else {
                            format!(
                                "Server connected with compatibility limits: {}",
                                issues.join(", ")
                            )
                        }),
                    );
                }
                Err(e) => {
                    lib.connection_result(&id, None, status(&e))
                        .map_err(|e| e.to_string())?;
                    return Err(e.to_string());
                }
            }
        }
        Command::Sync(id) => {
            let profile = lib.profile(&id).map_err(|e| e.to_string())?;
            let api = client(&profile, memory)?;
            let summary = lib.sync(&id, &api).map_err(|e| e.to_string())?;
            send(
                tx,
                ctx,
                Event::Notice(format!(
                    "Synchronized {} apps, {} releases and {} associations",
                    summary.apps, summary.releases, summary.associations
                )),
            );
            send(tx, ctx, Event::Synced);
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
            start_job(
                paths.clone(),
                p,
                memory.get(&profile.0).cloned(),
                job,
                tx.clone(),
                ctx.clone(),
            );
        }
        Command::Pause(id) => {
            lib.pause_job(&id).map_err(|e| e.to_string())?;
        }
        Command::Cancel(id) => {
            lib.cancel_job(&id).map_err(|e| e.to_string())?;
        }
        Command::Resume(id) | Command::Retry(id) => {
            let job = lib.retry_job(&id).map_err(|e| e.to_string())?;
            let p = lib.profile(&job.profile_id).map_err(|e| e.to_string())?;
            start_job(
                paths.clone(),
                p,
                memory.get(&job.profile_id.0).cloned(),
                job,
                tx.clone(),
                ctx.clone(),
            );
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
            let status = if cfg!(target_os = "windows") {
                std::process::Command::new("explorer").arg(&folder).status()
            } else if cfg!(target_os = "macos") {
                std::process::Command::new("open").arg(&folder).status()
            } else {
                std::process::Command::new("xdg-open").arg(&folder).status()
            }
            .map_err(|e| e.to_string())?;
            if !status.success() {
                return Err("file manager could not open the folder".into());
            }
            return Ok(false);
        }
    }
    Ok(true)
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
                    "steam.grid.vertical",
                    "icon",
                    "logo",
                    "steam.grid.horizontal",
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
            Err(error) => {
                send(
                    &tx,
                    &ctx,
                    Event::ArtworkMissing {
                        profile: id,
                        app,
                        usage,
                    },
                );
                if !error.contains("HTTP 404") && !error.starts_with("transport:") {
                    send(
                        &tx,
                        &ctx,
                        Event::Notice(format!("Artwork unavailable: {error}")),
                    );
                }
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
) {
    thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            let mut lib = Library::open(paths).map_err(|e| e.to_string())?;
            let mut memory = HashMap::new();
            if let Some(token) = memory_token {
                memory.insert(profile.id.0.clone(), token);
            }
            let api = client(&profile, &memory)?;
            if job.artifact_id.starts_with("adtree:") {
                lib.run_tree_job(&api, &job.id, None)
                    .map_err(|e| e.to_string())?;
            } else {
                lib.run_file_job(&api, &job.id, None)
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => send(
                &tx,
                &ctx,
                Event::Notice(format!("Download job {} finished", job.id)),
            ),
            Err(e) => send(
                &tx,
                &ctx,
                Event::Error(format!("Download job {}: {e}", job.id)),
            ),
        }
        send(&tx, &ctx, Event::Synced);
    });
}

fn run(paths: LibraryPaths, rx: Receiver<Command>, tx: Sender<Event>, ctx: egui::Context) {
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
            {
                send(&tx, &ctx, Event::Error(format!("Job recovery: {e}")));
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
    if let Ok(value) = snapshot(&lib, None) {
        send(&tx, &ctx, Event::Snapshot(value));
    }
    loop {
        match rx.recv_timeout(Duration::from_millis(650)) {
            Ok(cmd) => match operation(
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
                            Ok(value) => send(&tx, &ctx, Event::Snapshot(value)),
                            Err(e) => send(&tx, &ctx, Event::Error(e)),
                        }
                    }
                }
                Err(e) => {
                    send(&tx, &ctx, Event::Error(e));
                    if let Ok(value) = snapshot(&lib, selected.as_ref()) {
                        send(&tx, &ctx, Event::Snapshot(value));
                    }
                }
            },
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(id) = selected.as_ref()
                    && let Ok(jobs) = lib.jobs(Some(id))
                {
                    send(&tx, &ctx, Event::Jobs(jobs));
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
