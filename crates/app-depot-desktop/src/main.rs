#![cfg_attr(windows, windows_subsystem = "windows")]
mod design;
mod manifest_ui;
mod preferences;

#[cfg(any(target_os = "linux", windows))]
mod updater;
mod views;
mod worker;

use app_depot_core::telemetry::LiveProgress;
use app_depot_core::{
    ServerProfileId,
    api::ApiClient,
    library::LibraryPaths,
    managed::{InstallationIntegrity, StorageUsage},
    models::{AppConvention, AssociationConvention, ReleaseConvention},
    transfer::DownloadJob,
};
use eframe::egui::{self, Color32, RichText, TextureHandle};
use preferences::{Preferences, Theme};
use std::{
    collections::{HashMap, HashSet},
    env,
    path::PathBuf,
    time::{Duration, Instant},
};
use worker::{AppView, ArtUse, Command, Event, Snapshot, Worker};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Catalog,
    Library,
    Downloads,
    Settings,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Confirmation {
    Archive,
    Move,
    RemoveToken,
    RemoveInstallation,
    RemoveArtifact,
    RemoveApp,
    RepairSource,
    RepairInstallation,
    Exit,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Setup {
    Closed,
    Url,
    Checking,
    Token,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    Servers,
    Storage,
    Appearance,
    Updates,
    Diagnostics,
}
#[derive(Clone, PartialEq, Eq, Hash)]
struct ArtKey {
    profile: String,
    app: String,
    usage: ArtUse,
}
struct ArtTexture {
    texture: TextureHandle,
    aspect: f32,
}

struct Desktop {
    #[cfg(target_os = "linux")]
    update_ack: Option<PathBuf>,
    #[cfg(target_os = "linux")]
    update_ack_armed: bool,
    app_icon: TextureHandle,
    #[cfg(any(target_os = "linux", windows))]
    updater: updater::Updater,
    worker: Worker,
    install_dialog: Option<manifest_ui::InstallDialog>,
    manifest_request: u64,
    standard_remove: Option<(ServerProfileId, String, String)>,
    snapshot: Snapshot,
    storage_usage: Option<StorageUsage>,
    active: Option<ServerProfileId>,
    screen: Screen,
    settings_tab: SettingsTab,
    selected_app: Option<String>,
    selected_job: Option<String>,
    show_history: bool,
    last_tree_details_refresh: Instant,
    tree_progress: HashMap<String, Vec<app_depot_core::tree_download::TreeFileProgress>>,
    live_progress: HashMap<String, LiveProgress>,
    launch_states: HashMap<String, (String, String)>,
    local_operations: HashMap<String, String>,
    integrity_reports: HashMap<String, InstallationIntegrity>,
    search: String,
    sort_desc: bool,
    prefs: Preferences,
    preferences_root: PathBuf,
    message: Option<(String, bool, Instant)>,
    textures: HashMap<ArtKey, ArtTexture>,
    art_requested: HashSet<ArtKey>,
    setup: Setup,
    setup_error: Option<String>,
    profile_name: String,
    profile_url: String,
    edit_name: String,
    edit_url: String,
    edit_allow_http: bool,
    allow_http: bool,
    token: String,
    token_persist: bool,
    token_visible: bool,
    confirmation: Option<Confirmation>,
    remove_target: Option<(String, bool, String)>,
    remove_artifact_target: Option<(ServerProfileId, String, String)>,
    remove_app_target: Option<(ServerProfileId, String, String, bool)>,
    repair_target: Option<(ServerProfileId, String)>,
    repair_installation_target: Option<(String, String)>,
    exit_allowed: bool,
    syncing: HashSet<String>,
    sync_errors: HashMap<String, String>,
    connect_attempt: u64,
    last_activation: Instant,
    last_focus: bool,
}
impl Desktop {
    fn new(ctx: &egui::Context, paths: LibraryPaths) -> Self {
        let (prefs, error) = match preferences::load(&paths.metadata) {
            Ok(p) => (p, None),
            Err(e) => (
                Preferences::default(),
                Some(format!("Could not read desktop preferences: {e}")),
            ),
        };
        let root = paths.metadata.clone();
        let icon = eframe::icon_data::from_png_bytes(include_bytes!(
            "../../../assets/io.github.viktorliho.AppDepot-64.png"
        ))
        .expect("bundled application icon is valid PNG");
        let app_icon = ctx.load_texture(
            "app-depot-icon",
            egui::ColorImage::from(icon),
            egui::TextureOptions::LINEAR,
        );
        let this = Self {
            #[cfg(target_os = "linux")]
            update_ack: None,
            #[cfg(target_os = "linux")]
            update_ack_armed: false,
            app_icon,
            #[cfg(any(target_os = "linux", windows))]
            updater: updater::Updater::new(ctx),
            worker: Worker::start(paths, ctx.clone()),
            install_dialog: None,
            manifest_request: 0,
            standard_remove: None,
            snapshot: Snapshot::default(),
            storage_usage: None,
            active: None,
            screen: Screen::Catalog,
            settings_tab: SettingsTab::Servers,
            selected_app: None,
            selected_job: None,
            show_history: true,
            last_tree_details_refresh: Instant::now(),
            tree_progress: HashMap::new(),
            live_progress: HashMap::new(),
            launch_states: HashMap::new(),
            local_operations: HashMap::new(),
            integrity_reports: HashMap::new(),
            search: String::new(),
            sort_desc: false,
            prefs,
            preferences_root: root,
            message: None,
            textures: HashMap::new(),
            art_requested: HashSet::new(),
            setup: Setup::Closed,
            setup_error: error,
            profile_name: String::new(),
            profile_url: String::new(),
            edit_name: String::new(),
            edit_url: String::new(),
            edit_allow_http: false,
            allow_http: false,
            token: String::new(),
            token_persist: true,
            token_visible: false,
            confirmation: None,
            remove_target: None,
            remove_artifact_target: None,
            remove_app_target: None,
            repair_target: None,
            repair_installation_target: None,
            exit_allowed: false,
            syncing: HashSet::new(),
            sync_errors: HashMap::new(),
            connect_attempt: 0,
            last_activation: Instant::now(),
            last_focus: false,
        };
        this.apply_theme(ctx);
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        this
    }
    fn send(&self, command: Command) {
        self.worker.send(command);
    }
    fn profile(&self) -> Option<&app_depot_core::library::Profile> {
        self.snapshot
            .profiles
            .iter()
            .find(|p| self.active.as_ref() == Some(&p.id))
    }
    fn remember(&mut self) {
        if let Err(e) = preferences::save(&self.preferences_root, &self.prefs) {
            self.notice(format!("Could not save appearance preferences: {e}"), true);
        }
    }
    fn apply_theme(&self, ctx: &egui::Context) {
        let theme = match self.prefs.theme {
            Theme::System => egui::ThemePreference::System,
            Theme::Dark => egui::ThemePreference::Dark,
            Theme::Light => egui::ThemePreference::Light,
        };
        design::install(ctx, theme, self.prefs.ui_scale);
    }
    fn notice(&mut self, message: String, error: bool) {
        self.message = Some((message, error, Instant::now()));
    }
    fn begin_setup(&mut self) {
        self.profile_name.clear();
        self.profile_url.clear();
        self.token.clear();
        self.token_visible = false;
        self.allow_http = false;
        self.setup_error = None;
        self.setup = Setup::Url;
    }
    fn select(&mut self, id: ServerProfileId) {
        self.active = Some(id.clone());
        self.selected_app = None;
        self.selected_job = None;
        self.live_progress.clear();
        self.token.clear();
        self.token_visible = false;
        self.edit_name.clear();
        self.edit_url.clear();
        self.edit_allow_http = false;
        self.textures.clear();
        self.art_requested.clear();
        self.snapshot.apps.clear();
        self.snapshot.files.clear();
        self.snapshot.trees.clear();
        self.snapshot.local_issues.clear();
        self.snapshot.installations.clear();
        self.snapshot.standard.clear();
        self.snapshot.plans.clear();
        self.snapshot.prerequisites.clear();
        self.install_dialog = None;
        self.standard_remove = None;
        self.snapshot.jobs.clear();
        self.snapshot.launches.clear();
        self.launch_states.clear();
        self.prefs.last_profile = Some(id.0.clone());
        self.remember();
        self.send(Command::Refresh(Some(id)));
    }
    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.worker.events.try_recv() {
            match event {
                Event::Snapshot(s) => {
                    let selected = s.selected.clone();
                    #[cfg(windows)]
                    {
                        self.snapshot.update_occupancy_clear = s.update_occupancy_clear;
                    }
                    self.snapshot.profiles = s.profiles;
                    self.snapshot.library_data = s.library_data;
                    self.snapshot.library_content = s.library_content;
                    if self.active.is_none() {
                        let preferred = self
                            .prefs
                            .last_profile
                            .as_ref()
                            .and_then(|id| self.snapshot.profiles.iter().find(|p| &p.id.0 == id))
                            .map(|p| p.id.clone());
                        let fallback = self
                            .snapshot
                            .profiles
                            .iter()
                            .find(|p| !p.archived)
                            .map(|p| p.id.clone());
                        if let Some(id) = preferred.or(fallback) {
                            self.select(id.clone());
                            self.send(Command::Sync(id));
                        }
                    }
                    if selected.as_ref() == self.active.as_ref() {
                        self.snapshot.apps = s.apps;
                        self.snapshot.jobs = s.jobs;
                        self.snapshot.job_context = s.job_context;
                        self.snapshot.files = s.files;
                        self.snapshot.trees = s.trees;
                        self.snapshot.local_issues = s.local_issues;
                        self.snapshot.installations = s.installations;
                        self.snapshot.standard = s.standard;
                        self.snapshot.plans = s.plans;
                        self.snapshot.prerequisites = s.prerequisites;
                        self.snapshot.launches = s.launches;
                    }
                }
                Event::ManifestResolved { request, result } => {
                    if let Some(dialog) = self
                        .install_dialog
                        .as_mut()
                        .filter(|d| d.request == request)
                    {
                        match result {
                            Ok(plan) => dialog.set_plan(plan),
                            Err(error) => {
                                dialog.busy = false;
                                dialog.error = Some(error);
                            }
                        }
                    }
                }
                Event::StorageUsage(usage) => self.storage_usage = Some(usage),
                Event::Jobs {
                    profile,
                    jobs,
                    context,
                } => {
                    if self.active.as_ref() == Some(&profile) {
                        self.snapshot.jobs = jobs;
                        if let Some(context) = context {
                            self.snapshot.job_context = context;
                        }
                    }
                }
                Event::JobChanged => {
                    self.send(Command::Refresh(self.active.clone()));
                }
                Event::Launch {
                    profile,
                    association,
                    entry,
                    state,
                    message,
                } => {
                    if self.active.as_ref() == Some(&profile) {
                        self.launch_states
                            .insert(format!("{association}:{entry}"), (state, message));
                    }
                }
                Event::CatalogChanged(profile) => {
                    if self.active.as_ref() == Some(&profile) {
                        self.textures.clear();
                        self.art_requested.clear();
                    }
                    self.sync_errors.remove(&profile.0);
                }
                Event::TreeFiles { job, files } => {
                    self.tree_progress.insert(job, files);
                }
                Event::Transfer {
                    job,
                    observation,
                    at,
                } => {
                    self.live_progress
                        .entry(job)
                        .and_modify(|p| p.update(observation.clone(), at))
                        .or_insert_with(|| LiveProgress::new(observation, at));
                }
                Event::Artwork {
                    profile,
                    app,
                    usage,
                    binding,
                    image,
                } => {
                    if self.active.as_ref() == Some(&profile) {
                        let key = ArtKey {
                            profile: profile.0,
                            app,
                            usage,
                        };
                        let aspect = image.size[0] as f32 / image.size[1] as f32;
                        let texture = ctx.load_texture(
                            format!("{}:{}:{usage:?}:{binding}", key.profile, key.app),
                            image,
                            egui::TextureOptions::LINEAR,
                        );
                        self.textures.insert(key, ArtTexture { texture, aspect });
                    }
                }
                Event::ArtworkMissing {
                    profile,
                    app,
                    usage,
                } => {
                    self.art_requested.insert(ArtKey {
                        profile: profile.0,
                        app,
                        usage,
                    });
                }
                Event::Notice(s) => self.notice(s, false),
                Event::Error(s) => self.notice(s, true),
                Event::LocalOperation { id, phase } => {
                    if let Some(phase) = phase {
                        self.local_operations.insert(id, phase);
                    } else {
                        self.local_operations.remove(&id);
                    }
                }
                #[cfg(not(windows))]
                Event::Integrity(report) => {
                    self.integrity_reports
                        .insert(report.installation_id.clone(), report);
                }
                Event::SelectProfile(id) => {
                    self.setup = Setup::Closed;
                    self.setup_error = None;
                    self.profile_name.clear();
                    self.profile_url.clear();
                    self.token.clear();
                    self.screen = Screen::Catalog;
                    self.select(id);
                }
                Event::SyncState { profile, running } => {
                    if running {
                        self.syncing.insert(profile.0);
                    } else {
                        self.syncing.remove(&profile.0);
                    }
                }
                Event::RefreshError { profile, message } => {
                    self.sync_errors.insert(profile.0, message);
                }
                Event::ConnectionResult {
                    connected,
                    needs_token,
                    message,
                } => {
                    if connected {
                        self.setup = Setup::Closed;
                        self.setup_error = None;
                    } else {
                        self.setup = if needs_token {
                            Setup::Token
                        } else {
                            Setup::Url
                        };
                        self.setup_error = if needs_token
                            && self.token.is_empty()
                            && message == "This catalog requires a personal API token."
                        {
                            None
                        } else {
                            Some(message)
                        };
                    }
                }
            }
        }
    }
    fn request_art(
        &mut self,
        ui: &mut egui::Ui,
        app: &str,
        usage: ArtUse,
        size: egui::Vec2,
        name: &str,
    ) {
        let Some(id) = self.active.clone() else {
            return;
        };
        let key = ArtKey {
            profile: id.0.clone(),
            app: app.into(),
            usage,
        };
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        let backdrop = design::palette(ui).elevated;
        if let Some(art) = self.textures.get(&key) {
            ui.painter().rect_filled(rect, 11.0, backdrop);
            let fitted = if art.aspect > size.x / size.y {
                egui::vec2(size.x, size.x / art.aspect)
            } else {
                egui::vec2(size.y * art.aspect, size.y)
            };
            let image_rect = egui::Rect::from_center_size(rect.center(), fitted);
            ui.painter().image(
                art.texture.id(),
                image_rect,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            ui.painter().rect_filled(rect, 11.0, backdrop);
            let initials: String = name
                .split_whitespace()
                .take(2)
                .filter_map(|s| s.chars().next())
                .collect();
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                if initials.is_empty() { "A" } else { &initials },
                egui::FontId::proportional((size.y * 0.30).clamp(22.0, 48.0)),
                ui.visuals().weak_text_color(),
            );
            if self.art_requested.insert(key) && self.profile().is_some_and(|p| !p.archived) {
                self.send(Command::Artwork {
                    profile: id,
                    app: app.into(),
                    usage,
                });
            }
        }
    }
}
fn library_paths() -> Result<LibraryPaths, String> {
    if let Some(v) = env::var_os("APP_DEPOT_LIBRARY_DATA_DIR") {
        let metadata = PathBuf::from(v);
        let content = env::var_os("APP_DEPOT_LIBRARY_CONTENT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| metadata.clone());
        return Ok(LibraryPaths { metadata, content });
    }
    let mut p = LibraryPaths::default_for_user().map_err(|e| e.to_string())?;
    if let Some(v) = env::var_os("APP_DEPOT_LIBRARY_CONTENT_DIR") {
        p.content = PathBuf::from(v);
    }
    Ok(p)
}
fn main() -> eframe::Result {
    #[cfg(windows)]
    let _installation_guard = app_depot_core::windows_process::desktop_installation_guard()
        .map_err(|error| eframe::Error::AppCreation(Box::new(error)))?;
    #[cfg(windows)]
    app_depot_core::windows_process::supervisor_entrypoint();
    if env::args_os()
        .nth(1)
        .is_some_and(|argument| argument == "--version")
    {
        println!(
            "App Depot Desktop {} ({}, {})",
            env!("CARGO_PKG_VERSION"),
            env!("APP_DEPOT_BUILD_KIND"),
            env!("APP_DEPOT_SOURCE_REVISION")
        );
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    let update_ack = {
        let args: Vec<_> = env::args_os().collect();
        if args.len() == 3 && args[1] == "--appdepot-update-ack" {
            Some(PathBuf::from(&args[2]))
        } else {
            None
        }
    };
    let paths = match library_paths() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2)
        }
    };
    let options = eframe::NativeOptions {
        #[cfg(windows)]
        renderer: eframe::Renderer::Wgpu,
        #[cfg(windows)]
        wgpu_options: {
            let mut setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
            setup.instance_descriptor.backends = wgpu::Backends::DX12;
            eframe::WgpuConfiguration {
                wgpu_setup: setup.into(),
                ..Default::default()
            }
        },
        viewport: egui::ViewportBuilder::default()
            .with_title("App Depot")
            .with_app_id("io.github.viktorliho.AppDepot")
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!(
                    "../../../assets/io.github.viktorliho.AppDepot-64.png"
                ))
                .expect("bundled application icon is valid PNG"),
            )
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([760.0, 540.0])
            .with_resizable(true)
            .with_decorations(!cfg!(target_os = "linux")),
        ..Default::default()
    };
    eframe::run_native(
        "App Depot",
        options,
        Box::new(move |cc| {
            let desktop = Desktop::new(&cc.egui_ctx, paths);
            #[cfg(target_os = "linux")]
            let mut desktop = desktop;
            #[cfg(target_os = "linux")]
            if let Some(marker) = &update_ack
                && let (Some(image), Some(parent)) = (env::var_os("APPIMAGE"), marker.parent())
                && PathBuf::from(image).parent() == Some(parent)
                && marker
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(".appdepot-update-ack-"))
            {
                desktop.update_ack = Some(marker.clone());
            }
            Ok(Box::new(desktop))
        }),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::{
        PrimaryAction, catalog_item_visible, download_group, format_time, friendly_job_state,
        group_library_items, job_actions, launch_is_playable, primary_action, resize_direction,
        titlebar_command,
    };
    fn job(state: &str) -> DownloadJob {
        DownloadJob {
            id: "job".into(),
            profile_id: ServerProfileId("profile".into()),
            association_id: "association".into(),
            artifact_id: "adfile:sha256:a".into(),
            state: state.into(),
            expected_size: 10,
            progress: 3,
            etag: None,
            last_error: None,
            created_at: 0,
            updated_at: 0,
            attempts: 0,
            queue_rank: 0,
            auto_resume: false,
        }
    }
    #[test]
    fn job_actions_follow_durable_core_states() {
        assert!(matches!(
            job_actions(&job("Paused"))[0].1,
            Command::Resume(_)
        ));
        assert!(matches!(
            job_actions(&job("Failed"))[0].1,
            Command::Retry(_)
        ));
        assert!(job_actions(&job("Verifying")).is_empty());
    }
    #[test]
    fn retired_catalog_entry_is_hidden_only_after_complete_online_sync() {
        assert!(!catalog_item_visible(true, true));
        assert!(catalog_item_visible(true, false));
        assert!(catalog_item_visible(false, true));
    }
    #[test]
    fn unknown_json_keeps_large_integer_for_display() {
        let value: serde_json::Value =
            serde_json::from_str(r#"{"appDepot":{"future":18446744073709551616},"other":null}"#)
                .unwrap();
        let rendered = serde_json::to_string_pretty(&value).unwrap();
        assert!(rendered.contains("18446744073709551616"));
    }
    #[test]
    fn library_millisecond_timestamp_is_human_readable() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        assert_eq!(format_time(Some(now - 120_000)), "2 min ago");
    }
    #[test]
    fn offline_snapshot_reopens_without_server() {
        let dir = tempfile::tempdir().unwrap();
        let paths = LibraryPaths {
            metadata: dir.path().join("data"),
            content: dir.path().join("content"),
        };
        let mut lib = app_depot_core::library::Library::open(paths.clone()).unwrap();
        let first = lib.add_profile("One", "http://127.0.0.1:9", false).unwrap();
        let second = lib.add_profile("Two", "http://127.0.0.1:9", false).unwrap();
        drop(lib);
        let lib = app_depot_core::library::Library::open(paths).unwrap();
        let view = worker::snapshot(&lib, Some(&first.id)).unwrap();
        assert_eq!(view.profiles.len(), 2);
        assert_ne!(first.id, second.id);
        assert!(view.apps.is_empty());
    }
    #[test]
    fn completion_progress_is_not_premature() {
        assert_eq!(friendly_job_state("Verifying"), "Verifying");
        assert_eq!(friendly_job_state("Completed"), "Downloaded and verified");
        assert!(download_group("Downloading") < download_group("Queued"));
        assert!(download_group("Queued") < download_group("Completed"));
        assert_eq!(download_group("Cancelled"), 5);
    }
    #[test]
    fn compatible_play_is_separate_from_other_platforms() {
        assert!(launch_is_playable("Ready"));
        assert!(launch_is_playable("PreparationRequired"));
        assert!(!launch_is_playable("UnsupportedPlatform"));
        assert!(!launch_is_playable("NoLaunch"));
        assert!(!launch_is_playable("MissingTarget"));
    }
    #[test]
    fn library_groups_associations_without_merging_other_apps() {
        let item = |id: &str| {
            (
                "File",
                id.to_owned(),
                id.to_owned(),
                PathBuf::new(),
                Some(1),
                None,
            )
        };
        let groups = group_library_items(
            vec![item("a"), item("c"), item("b")],
            |association| Some(if association == "c" { "other" } else { "same" }.to_owned()),
            |association| association == "b",
        );
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "same");
        assert_eq!(
            groups[0]
                .1
                .iter()
                .map(|item| item.1.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "a"]
        );
        assert_eq!(groups[1].1[0].1, "c");
    }
    #[test]
    fn hero_never_chooses_among_multiple_launches() {
        assert_eq!(
            primary_action(0, 1, 1, false),
            PrimaryAction::ChooseArtifact
        );
        assert_eq!(primary_action(0, 1, 1, true), PrimaryAction::Offline);
        assert_eq!(primary_action(1, 1, 1, false), PrimaryAction::Play);
        assert_eq!(primary_action(1, 2, 2, false), PrimaryAction::Play);
        assert_eq!(primary_action(1, 1, 2, false), PrimaryAction::Play);
        assert_eq!(primary_action(2, 1, 2, false), PrimaryAction::ChooseLaunch);
    }
    #[test]
    fn titlebar_commands_follow_window_state() {
        assert_eq!(
            titlebar_command(0, false),
            egui::ViewportCommand::Minimized(true)
        );
        assert_eq!(
            titlebar_command(1, false),
            egui::ViewportCommand::Maximized(true)
        );
        assert_eq!(
            titlebar_command(1, true),
            egui::ViewportCommand::Maximized(false)
        );
        assert_eq!(titlebar_command(2, false), egui::ViewportCommand::Close);
    }

    #[test]
    fn resize_regions_cover_edges_and_corners_without_content() {
        use egui::ResizeDirection as D;
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(900.0, 650.0));
        for (pos, expected) in [
            ([2.0, 2.0], Some(D::NorthWest)),
            ([898.0, 2.0], Some(D::NorthEast)),
            ([2.0, 648.0], Some(D::SouthWest)),
            ([898.0, 648.0], Some(D::SouthEast)),
            ([450.0, 2.0], Some(D::North)),
            ([450.0, 648.0], Some(D::South)),
            ([2.0, 325.0], Some(D::West)),
            ([898.0, 325.0], Some(D::East)),
            ([450.0, 325.0], None),
            ([-1.0, 325.0], None),
        ] {
            assert_eq!(resize_direction(rect, egui::pos2(pos[0], pos[1])), expected);
        }
    }
}
