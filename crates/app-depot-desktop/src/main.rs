#![cfg_attr(windows, windows_subsystem = "windows")]
mod preferences;
mod telemetry;
mod views;
mod worker;

use app_depot_core::{
    ServerProfileId,
    api::ApiClient,
    library::LibraryPaths,
    models::{AppConvention, AssociationConvention, EntryPointTarget, ReleaseConvention},
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
use telemetry::LiveProgress;
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
    Diagnostics,
}
#[derive(Clone, PartialEq, Eq, Hash)]
struct ArtKey {
    profile: String,
    app: String,
    usage: ArtUse,
}
struct ArtTexture {
    binding: String,
    texture: TextureHandle,
    aspect: f32,
}

struct Desktop {
    worker: Worker,
    snapshot: Snapshot,
    active: Option<ServerProfileId>,
    screen: Screen,
    settings_tab: SettingsTab,
    selected_app: Option<String>,
    selected_job: Option<String>,
    last_tree_details_refresh: Instant,
    tree_progress: HashMap<String, Vec<app_depot_core::tree_download::TreeFileProgress>>,
    live_progress: HashMap<String, LiveProgress>,
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
        let this = Self {
            worker: Worker::start(paths, ctx.clone()),
            snapshot: Snapshot::default(),
            active: None,
            screen: Screen::Catalog,
            settings_tab: SettingsTab::Servers,
            selected_app: None,
            selected_job: None,
            last_tree_details_refresh: Instant::now(),
            tree_progress: HashMap::new(),
            live_progress: HashMap::new(),
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
            exit_allowed: false,
            syncing: HashSet::new(),
            sync_errors: HashMap::new(),
            connect_attempt: 0,
            last_activation: Instant::now(),
            last_focus: false,
        };
        this.apply_theme(ctx);
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
        ctx.set_theme(theme);
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            let mut style = (*ctx.style_of(theme)).clone();
            style.spacing.item_spacing = egui::vec2(10.0, 9.0);
            style.spacing.button_padding = egui::vec2(13.0, 8.0);
            style.visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(10);
            style.visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(8);
            style.visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(8);
            style.visuals.widgets.active.corner_radius = egui::CornerRadius::same(8);
            ctx.set_style_of(theme, style);
        }
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
        self.snapshot.jobs.clear();
        self.prefs.last_profile = Some(id.0.clone());
        self.remember();
        self.send(Command::Refresh(Some(id)));
    }
    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.worker.events.try_recv() {
            match event {
                Event::Snapshot(s) => {
                    let selected = s.selected.clone();
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
                        self.snapshot.files = s.files;
                        self.snapshot.trees = s.trees;
                    }
                }
                Event::Jobs { profile, jobs } => {
                    if self.active.as_ref() == Some(&profile) {
                        self.snapshot.jobs = jobs;
                    }
                }
                Event::JobChanged => {
                    self.send(Command::Refresh(self.active.clone()));
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
                        self.textures.insert(
                            key,
                            ArtTexture {
                                binding,
                                texture,
                                aspect,
                            },
                        );
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
        if let Some(art) = self.textures.get(&key) {
            ui.painter()
                .rect_filled(rect, 11.0, ui.visuals().faint_bg_color);
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
            ui.interact(rect, ui.scope_id().with(app), egui::Sense::hover())
                .on_hover_text(format!("Artwork binding {}", art.binding));
        } else {
            ui.painter()
                .rect_filled(rect, 11.0, ui.visuals().faint_bg_color);
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
    let mut p = LibraryPaths::default_for_user().map_err(|e| e.to_string())?;
    if let Some(v) = env::var_os("APP_DEPOT_LIBRARY_DATA_DIR") {
        p.metadata = PathBuf::from(v);
        p.content = p.metadata.clone();
    }
    if let Some(v) = env::var_os("APP_DEPOT_LIBRARY_CONTENT_DIR") {
        p.content = PathBuf::from(v);
    }
    Ok(p)
}
fn main() -> eframe::Result {
    let paths = match library_paths() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2)
        }
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("App Depot")
            .with_inner_size([1120.0, 760.0])
            .with_min_inner_size([640.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "App Depot",
        options,
        Box::new(move |cc| Ok(Box::new(Desktop::new(&cc.egui_ctx, paths)))),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::{format_time, friendly_job_state, job_actions};
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
    }
}
