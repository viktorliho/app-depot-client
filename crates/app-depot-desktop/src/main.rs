#![cfg_attr(windows, windows_subsystem = "windows")]
mod worker;

use app_depot_core::{
    ServerProfileId,
    library::LibraryPaths,
    models::{AppConvention, AssociationConvention, EntryPointTarget, ReleaseConvention},
    transfer::DownloadJob,
};
use eframe::egui::{self, Color32, RichText, TextureHandle};
use std::{
    collections::{HashMap, HashSet},
    env,
    path::PathBuf,
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
    Exit,
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
}

struct Desktop {
    worker: Worker,
    snapshot: Snapshot,
    active: Option<ServerProfileId>,
    screen: Screen,
    selected_app: Option<String>,
    selected_job: Option<String>,
    tree_progress: HashMap<String, Vec<app_depot_core::tree_download::TreeFileProgress>>,
    search: String,
    grid: bool,
    sort_desc: bool,
    dark: bool,
    message: Option<(String, bool)>,
    textures: HashMap<ArtKey, ArtTexture>,
    art_requested: HashSet<ArtKey>,
    profile_name: String,
    profile_url: String,
    edit_name: String,
    edit_url: String,
    allow_http: bool,
    token: String,
    token_persist: bool,
    confirmation: Option<Confirmation>,
    exit_allowed: bool,
}
impl Desktop {
    fn new(ctx: &egui::Context, paths: LibraryPaths) -> Self {
        Self {
            worker: Worker::start(paths, ctx.clone()),
            snapshot: Snapshot::default(),
            active: None,
            screen: Screen::Catalog,
            selected_app: None,
            selected_job: None,
            tree_progress: HashMap::new(),
            search: String::new(),
            grid: true,
            sort_desc: false,
            dark: true,
            message: None,
            textures: HashMap::new(),
            art_requested: HashSet::new(),
            profile_name: String::new(),
            profile_url: String::new(),
            edit_name: String::new(),
            edit_url: String::new(),
            allow_http: false,
            token: String::new(),
            token_persist: true,
            confirmation: None,
            exit_allowed: false,
        }
    }
    fn send(&self, cmd: Command) {
        let _ = self.worker.commands.send(cmd);
    }
    fn profile(&self) -> Option<&app_depot_core::library::Profile> {
        self.snapshot
            .profiles
            .iter()
            .find(|p| self.active.as_ref() == Some(&p.id))
    }
    fn select(&mut self, id: ServerProfileId) {
        self.active = Some(id.clone());
        self.selected_app = None;
        self.selected_job = None;
        self.textures.clear();
        self.art_requested.clear();
        self.send(Command::Refresh(Some(id)));
    }
    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.worker.events.try_recv() {
            match event {
                Event::Snapshot(s) => {
                    self.snapshot = s;
                    if self.active.is_none()
                        && let Some(id) = self
                            .snapshot
                            .profiles
                            .iter()
                            .find(|p| !p.archived)
                            .map(|p| p.id.clone())
                    {
                        self.select(id);
                    }
                }
                Event::Jobs(j) => self.snapshot.jobs = j,
                Event::TreeFiles { job, files } => {
                    self.tree_progress.insert(job, files);
                }
                Event::Artwork {
                    profile,
                    app,
                    usage,
                    binding,
                    image,
                } => {
                    let key = ArtKey {
                        profile: profile.0,
                        app,
                        usage,
                    };
                    let texture = ctx.load_texture(
                        format!("{}:{}:{usage:?}:{binding}", key.profile, key.app),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.textures.insert(key, ArtTexture { binding, texture });
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
                Event::Notice(s) => self.message = Some((s, false)),
                Event::Error(s) => self.message = Some((s, true)),
                Event::SelectProfile(id) => {
                    self.profile_name.clear();
                    self.profile_url.clear();
                    self.select(id);
                }
                Event::Synced => {
                    self.textures.clear();
                    self.art_requested.clear();
                    if let Some(id) = self.active.clone() {
                        self.send(Command::Refresh(Some(id)));
                    }
                }
            }
        }
    }
    fn art(&mut self, ui: &mut egui::Ui, app: &str, usage: ArtUse, size: egui::Vec2) {
        let Some(id) = self.active.clone() else {
            return;
        };
        let key = ArtKey {
            profile: id.0.clone(),
            app: app.into(),
            usage,
        };
        if let Some(a) = self.textures.get(&key) {
            ui.image((a.texture.id(), size))
                .on_hover_text(format!("Artwork binding {}", a.binding));
        } else {
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 8.0, ui.visuals().faint_bg_color);
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "No artwork",
                egui::FontId::proportional(13.0),
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
    fn nav(&mut self, ui: &mut egui::Ui) {
        ui.heading("APP DEPOT");
        ui.label(RichText::new("Reference client").weak());
        ui.add_space(20.0);
        for (screen, name) in [
            (Screen::Catalog, "Catalog"),
            (Screen::Library, "Library"),
            (Screen::Downloads, "Downloads"),
            (Screen::Settings, "Settings"),
        ] {
            if ui.selectable_label(self.screen == screen, name).clicked() {
                self.screen = screen;
            }
            ui.add_space(6.0);
        }
        ui.add_space(20.0);
        ui.separator();
        ui.label(RichText::new("SERVER PROFILE").small().weak());
        let label = self
            .profile()
            .map_or("Select profile", |p| p.name.as_str())
            .to_owned();
        let options: Vec<_> = self
            .snapshot
            .profiles
            .iter()
            .map(|p| (p.id.clone(), p.name.clone(), p.archived))
            .collect();
        egui::ComboBox::from_id_salt("profiles")
            .selected_text(label)
            .width(160.0)
            .show_ui(ui, |ui| {
                for (id, name, archived) in options {
                    if ui
                        .selectable_label(
                            self.active.as_ref() == Some(&id),
                            if archived {
                                format!("{name} · archived")
                            } else {
                                name
                            },
                        )
                        .clicked()
                    {
                        self.select(id);
                    }
                }
            });
        if let Some(p) = self.profile() {
            ui.label(
                RichText::new(if p.archived {
                    "Archived · offline"
                } else {
                    &p.connection_status
                })
                .small()
                .weak(),
            );
        }
    }
    fn catalog(&mut self, ui: &mut egui::Ui) {
        if let Some(id) = self.selected_app.clone() {
            self.details(ui, &id);
            return;
        }
        ui.horizontal(|ui| {
            ui.heading("Catalog");
            ui.add_space(12.0);
            ui.label(
                RichText::new(self.profile().map_or("No profile", |p| {
                    match (p.last_sync, p.connection_status.as_str()) {
                        (Some(_), "connected") => "Synchronized cache",
                        (Some(_), _) => "Offline / cached metadata",
                        (None, _) => "Not synchronized",
                    }
                }))
                .weak(),
            );
            if ui
                .add_enabled(
                    self.profile().is_some_and(|p| !p.archived),
                    egui::Button::new("Synchronize"),
                )
                .clicked()
                && let Some(id) = self.active.clone()
            {
                self.send(Command::Sync(id));
            }
        });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("Search name, slug or ID")
                    .desired_width(300.0),
            );
            ui.selectable_value(&mut self.grid, true, "Grid");
            ui.selectable_value(&mut self.grid, false, "Table");
            if !self.grid
                && ui
                    .button(if self.sort_desc {
                        "Name ↓"
                    } else {
                        "Name ↑"
                    })
                    .clicked()
            {
                self.sort_desc = !self.sort_desc;
            }
        });
        ui.separator();
        let q = self.search.to_lowercase();
        let mut apps: Vec<_> = self
            .snapshot
            .apps
            .iter()
            .filter(|a| {
                q.is_empty()
                    || a.app.name.to_lowercase().contains(&q)
                    || a.app.slug.to_lowercase().contains(&q)
                    || a.app.id.contains(&q)
            })
            .cloned()
            .collect();
        apps.sort_by_key(|a| a.app.name.to_lowercase());
        if self.sort_desc {
            apps.reverse();
        }
        if self.active.is_none() {
            empty(ui, "Add or select a server profile in Settings.");
            return;
        }
        if apps.is_empty() {
            empty(
                ui,
                if self.snapshot.apps.is_empty() {
                    "No cached Apps. Synchronize while connected."
                } else {
                    "No Apps match this search."
                },
            );
            return;
        }
        let columns = if self.grid {
            ((ui.available_width() / 205.0).floor() as usize).max(1)
        } else {
            1
        };
        let row_height = if self.grid { 235.0 } else { 45.0 };
        egui::ScrollArea::vertical().show_rows(
            ui,
            row_height,
            apps.len().div_ceil(columns),
            |ui, range| {
                for row in range {
                    ui.horizontal(|ui| {
                        for app in apps.iter().skip(row * columns).take(columns) {
                            if self.grid {
                                self.card(ui, app);
                            } else {
                                self.row(ui, app);
                            }
                        }
                    });
                }
            },
        );
    }
    fn card(&mut self, ui: &mut egui::Ui, view: &AppView) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.vertical(|ui| {
                ui.set_width(185.0);
                self.art(ui, &view.app.id, ArtUse::Grid, egui::vec2(165.0, 130.0));
                ui.label(RichText::new(&view.app.name).strong())
                    .on_hover_text(&view.app.name);
                ui.label(
                    RichText::new(format!(
                        "{} releases · {}",
                        view.releases.len(),
                        if view.stale { "stale" } else { "cached" }
                    ))
                    .small()
                    .weak(),
                );
                if ui.button("Details →").clicked() {
                    self.selected_app = Some(view.app.id.clone());
                }
            });
        });
    }
    fn row(&mut self, ui: &mut egui::Ui, view: &AppView) {
        if ui
            .add_sized(
                [ui.available_width() * 0.6, 30.0],
                egui::Button::new(&view.app.name),
            )
            .clicked()
        {
            self.selected_app = Some(view.app.id.clone());
        }
        ui.label(format!("{} releases", view.releases.len()));
        ui.label(if view.stale { "Stale cache" } else { "Cached" });
    }
    fn details(&mut self, ui: &mut egui::Ui, id: &str) {
        let Some(view) = self.snapshot.apps.iter().find(|v| v.app.id == id).cloned() else {
            self.selected_app = None;
            return;
        };
        if ui.button("← Catalog").clicked() {
            self.selected_app = None;
            return;
        }
        ui.add_space(8.0);
        egui::ScrollArea::vertical().show(ui,|ui|{
            ui.horizontal(|ui|{self.art(ui,id,ArtUse::Detail,egui::vec2(230.0,135.0));ui.vertical(|ui|{
                ui.heading(&view.app.name);ui.label(RichText::new(&view.app.id).monospace().weak());
                ui.label(format!("{} · {}",view.app.visibility,view.app.state));if view.stale{ui.colored_label(Color32::YELLOW,"Metadata is stale");}
                if let Some(doc)=view.releases.first().and_then(|r|r.declaration.as_ref()).map(|s|&s.app)
                    && let Ok(Some(info))=doc.convention::<AppConvention>() {
                        if let Some(summary)=info.summary{ui.label(summary);}
                        if let Some(description)=info.description{ui.label(description);}
                        if let Some(publisher)=info.publisher{ui.label(format!("Publisher: {publisher}"));}
                }
            });});ui.add_space(15.0);
            if view.releases.is_empty(){empty(ui,"No cached Releases for this App.");}
            for release in &view.releases{
                egui::CollapsingHeader::new(format!("Release {} · {}",release.release.version.as_deref().unwrap_or("unversioned"),release.release.state)).id_salt(&release.release.id).default_open(true).show(ui,|ui|{
                    ui.label(RichText::new(&release.release.id).monospace().weak());
                    if release.associations.is_empty(){ui.label("No Artifact associations");}
                    for item in &release.associations{egui::Frame::group(ui.style()).show(ui,|ui|{ui.horizontal(|ui|{
                        ui.vertical(|ui|{ui.label(RichText::new(item.artifact.as_ref().map_or("Artifact metadata unavailable",|a|a.kind.as_str())).strong());
                            ui.label(RichText::new(&item.association.id).small().monospace().weak());
                            if let Some(a)=&item.artifact{ui.label(RichText::new(&a.id).small().monospace()).on_hover_text(&a.id);ui.label(a.size.map_or_else(||"Size unspecified".into(),|n|format!("{n} bytes")));}
                            if let Some(doc)=release.declaration.as_ref().and_then(|s|s.associations.iter().find(|a|a.association_id==item.association.id)).map(|a|&a.declaration){
                                match doc.convention::<AssociationConvention>() {
                                    Ok(Some(info))=>{
                                        if let Some(label)=info.label{ui.label(label);}
                                        if let Some(purpose)=info.purpose{ui.label(format!("Purpose: {purpose}"));}
                                        if let Some(points)=info.entry_points{for point in points{let target=match point.target{EntryPointTarget::ArtifactFile=>"Artifact file".to_owned(),EntryPointTarget::TreeFile{path}=>format!("Tree file: {path}")};ui.label(format!("Entry point {}: {target} · launch unsupported",point.label.as_deref().unwrap_or(&point.id)));}}
                                        else{ui.label("Launch information unspecified");}
                                    }
                                    Ok(None)=>{ui.label("Launch information unspecified");},
                                    Err(e)=>{ui.colored_label(Color32::YELLOW,format!("Invalid association convention: {e}"));},
                                };
                            }
                            ui.label(self.association_status(&item.association.id));});
                        let enabled=item.artifact.as_ref().is_some_and(|a|matches!(a.kind.as_str(),"File"|"Tree"))&&self.profile().is_some_and(|p|!p.archived)&&self.association_status(&item.association.id)=="Available to download";
                        if ui.add_enabled(enabled,egui::Button::new("Download")).clicked() && let (Some(profile),Some(a))=(self.active.clone(),&item.artifact){self.send(Command::Download{profile,release:release.release.id.clone(),association:item.association.id.clone(),kind:a.kind.clone()});self.screen=Screen::Downloads;}
                    });});}
                    if let Some(s)=&release.declaration{
                        match s.release.convention::<ReleaseConvention>() {
                            Ok(Some(info))=>{
                                if let Some(notes)=info.notes{ui.label(format!("Notes: {notes}"));}
                                if let Some(platforms)=info.platforms{ui.label(format!("Declared platforms (unassessed): {}",platforms.join(", ")));}
                                else{ui.label("Platform unspecified; Artifact remains downloadable");}
                                if let Some(runtime)=info.runtime{ui.label(format!("Declared runtime: {} {} · preparation unsupported",runtime.name.unwrap_or_else(||"unspecified".into()),runtime.version.unwrap_or_default()));}
                            }
                            Ok(None)=>{ui.label("Platform and runtime unspecified");},
                            Err(e)=>{ui.colored_label(Color32::YELLOW,format!("Invalid Release convention: {e}"));},
                        };
                        ui.label(RichText::new("Technical declarations · inert Server JSON, including unknown fields").strong());declaration(ui,"App",&s.app.data);declaration(ui,"Release",&s.release.data);for a in &s.associations{declaration(ui,&format!("Association {}",a.association_id),&a.declaration.data);}
                    }
                    else{ui.label("Declaration snapshot unavailable in cache");}
                    ui.label(RichText::new("Launch information may be unspecified or unsupported. Installation and launching are unavailable.").small().weak());
                });ui.add_space(8.0);
            }
        });
    }
    fn association_status(&self, id: &str) -> &'static str {
        if self.snapshot.files.iter().any(|f| f.association_id == id)
            || self.snapshot.trees.iter().any(|t| t.association_id == id)
        {
            "Downloaded and verified"
        } else if self.snapshot.jobs.iter().any(|j| {
            j.association_id == id
                && matches!(
                    j.state.as_str(),
                    "Queued" | "FetchingInventory" | "Downloading" | "Verifying" | "Committing"
                )
        }) {
            "Downloading"
        } else if self
            .snapshot
            .jobs
            .iter()
            .any(|j| j.association_id == id && j.state == "Paused")
        {
            "Download paused"
        } else if self
            .snapshot
            .jobs
            .iter()
            .any(|j| j.association_id == id && j.state == "Failed")
        {
            "Download failed"
        } else {
            "Available to download"
        }
    }
    fn library(&mut self, ui: &mut egui::Ui) {
        ui.heading("Library");
        ui.label(RichText::new("Verified downloads remain available offline. Downloaded content is not installed software.").weak());
        ui.add_space(12.0);
        if self.snapshot.files.is_empty() && self.snapshot.trees.is_empty() {
            empty(ui, "No verified downloads for this profile.");
            return;
        }
        let mut items: Vec<_> = self
            .snapshot
            .files
            .iter()
            .map(|f| {
                (
                    "File",
                    f.association_id.clone(),
                    f.artifact_id.clone(),
                    f.path.clone(),
                    format!("{} bytes · SHA-256 verified", f.size),
                )
            })
            .collect();
        items.extend(self.snapshot.trees.iter().map(|t| {
            (
                "Tree",
                t.association_id.clone(),
                t.artifact_id.clone(),
                t.path.clone(),
                format!("{} files · Tree Identity v1 verified", t.file_count),
            )
        }));
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (kind, assoc, artifact, path, detail) in items {
                let context = self
                    .snapshot
                    .apps
                    .iter()
                    .flat_map(|a| a.releases.iter().map(move |r| (a, r)))
                    .find(|(_, r)| r.associations.iter().any(|x| x.association.id == assoc));
                let title = context.map_or(kind.to_owned(), |(a, r)| {
                    format!(
                        "{} · {} · {kind}",
                        a.app.name,
                        r.release.version.as_deref().unwrap_or("unversioned")
                    )
                });
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(title).strong());
                            ui.label(RichText::new(detail).small());
                            ui.label(RichText::new(artifact).monospace().small().weak());
                        });
                        if ui.button("Open folder").clicked() {
                            self.send(Command::Reveal(path));
                        }
                    });
                });
                ui.add_space(6.0);
            }
        });
    }
    fn downloads(&mut self, ui: &mut egui::Ui) {
        ui.heading("Downloads");
        ui.label(RichText::new("Progress comes from durable core job state.").weak());
        ui.add_space(12.0);
        let jobs = self.snapshot.jobs.clone();
        if jobs.is_empty() {
            empty(ui, "No download jobs for this profile.");
            return;
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            for job in jobs.iter().rev() {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{} · {}",
                                    if job.artifact_id.starts_with("adtree:") {
                                        "Tree"
                                    } else {
                                        "File"
                                    },
                                    job.state
                                ))
                                .strong(),
                            );
                            ui.label(RichText::new(&job.artifact_id).monospace().small())
                                .on_hover_text(&job.artifact_id);
                            ui.label(format!("{} / {} bytes", job.progress, job.expected_size));
                            if job.expected_size > 0 {
                                ui.add(
                                    egui::ProgressBar::new(
                                        (job.progress as f64 / job.expected_size as f64)
                                            .clamp(0.0, 1.0)
                                            as f32,
                                    )
                                    .desired_width(270.0),
                                );
                            }
                            if let Some(e) = &job.last_error {
                                ui.colored_label(Color32::LIGHT_RED, e);
                            }
                        });
                        for (label, cmd) in job_actions(job) {
                            if ui.button(label).clicked() {
                                self.send(cmd);
                            }
                        }
                    });
                    if job.artifact_id.starts_with("adtree:") {
                        if ui
                            .button(if self.selected_job.as_deref() == Some(&job.id) {
                                "Hide members"
                            } else {
                                "Show members"
                            })
                            .clicked()
                        {
                            if self.selected_job.as_deref() == Some(&job.id) {
                                self.selected_job = None;
                            } else {
                                self.selected_job = Some(job.id.clone());
                                self.send(Command::TreeFiles(job.id.clone()));
                            }
                        }
                        if self.selected_job.as_deref() == Some(&job.id)
                            && let Some(files) = self.tree_progress.get(&job.id)
                        {
                            for f in files {
                                ui.label(format!(
                                    "{} · {} / {} bytes{}",
                                    f.path,
                                    f.progress,
                                    f.size,
                                    if f.verified { " · verified" } else { "" }
                                ));
                            }
                        }
                    }
                });
                ui.add_space(6.0);
            }
        });
    }
    fn settings(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui,|ui|{
            ui.heading("Settings");ui.add_space(10.0);ui.label(RichText::new("Add server profile").strong());
            ui.horizontal(|ui|{ui.label("Name");ui.text_edit_singleline(&mut self.profile_name);});
            ui.horizontal(|ui|{ui.label("Origin URL");ui.add(egui::TextEdit::singleline(&mut self.profile_url).desired_width(340.0));});
            ui.checkbox(&mut self.allow_http,"Allow LAN HTTP (Bearer tokens are visible in transit)");
            if ui.button("Add profile").clicked(){self.send(Command::AddProfile{name:self.profile_name.trim().into(),url:self.profile_url.trim().into(),allow_http:self.allow_http});}
            ui.separator();if let Some(p)=self.profile().cloned(){
                ui.label(RichText::new(format!("Selected: {}",p.name)).strong());ui.label(RichText::new(format!("ID: {}",p.id.0)).monospace().small());
                ui.label(format!("Origin: {}",p.origin));if p.origin.starts_with("http://"){ui.colored_label(Color32::YELLOW,"LAN HTTP is unencrypted; Bearer tokens are exposed in transit.");}
                ui.label(format!("Connection: {} · Last sync: {}",p.connection_status,p.last_sync.map_or_else(||"never".into(),|n|n.to_string())));
                if let Some(c)=&p.capabilities{ui.label(format!("API {} · File {} · Tree {} · Range {}",c.api_version,c.file_identity,c.tree_identity,c.download_ranges));
                    ui.label(format!("Limits: page {} · concurrent ranges {} · upload {} bytes",c.limits.max_page_size,c.limits.max_concurrent_ranges,c.limits.max_upload_bytes));
                    if !c.features.is_empty(){ui.label(format!("Features: {}",c.features.join(", ")));}}
                for issue in &p.compatibility_issues{ui.colored_label(Color32::YELLOW,issue);}
                if !p.archived{ui.horizontal(|ui|{if ui.button("Check connection").clicked(){self.send(Command::Check(p.id.clone()));}
                    if ui.button("Synchronize").clicked(){self.send(Command::Sync(p.id.clone()));}});
                    ui.add_space(8.0);ui.label("Edit selected profile");
                    ui.horizontal(|ui|{ui.label("New name");ui.text_edit_singleline(&mut self.edit_name);if ui.button("Rename").clicked(){self.send(Command::RenameProfile{id:p.id.clone(),name:self.edit_name.trim().into()});self.edit_name.clear();}});
                    ui.horizontal(|ui|{ui.label("New origin");ui.add(egui::TextEdit::singleline(&mut self.edit_url).desired_width(300.0));if ui.button("Change origin").clicked(){self.confirmation=Some(Confirmation::Move);}});
                    ui.add_space(8.0);ui.label(format!("Credential: {}",if p.credential_ref.is_some(){"native secure storage"}else{"anonymous or session token"}));
                    ui.add(egui::TextEdit::singleline(&mut self.token).password(true).hint_text("Bearer token"));
                    ui.checkbox(&mut self.token_persist,"Save in native secure storage");
                    if ui.button("Set token").clicked(){let token=std::mem::take(&mut self.token);self.send(Command::SetToken{id:p.id.clone(),token,persist:self.token_persist});}
                    if ui.button("Remove token…").clicked(){self.confirmation=Some(Confirmation::RemoveToken);}
                    if ui.button("Archive profile…").clicked(){self.confirmation=Some(Confirmation::Archive);}
                }else{ui.label("Archived profiles remain available for offline browsing.");}
            }ui.separator();ui.label(RichText::new("Local storage").strong());
            ui.label(format!("Metadata: {}",self.snapshot.library_data.display()));ui.label(format!("Content: {}",self.snapshot.library_content.display()));
            ui.label("Set APP_DEPOT_LIBRARY_DATA_DIR and APP_DEPOT_LIBRARY_CONTENT_DIR before launch to choose separate roots.");
            ui.separator();ui.label(format!("App Depot Desktop {} · read, download and verify only",env!("CARGO_PKG_VERSION")));
            ui.checkbox(&mut self.dark,"Dark theme");
        });
    }
    fn confirmation(&mut self, ctx: &egui::Context) {
        let Some(kind) = self.confirmation else {
            return;
        };
        let text = match kind {
            Confirmation::Archive => {
                "Archive this profile? Cached metadata and verified downloads remain. Its secure token will be removed."
            }
            Confirmation::Move => {
                "Change origin? The old profile and cache will be archived; a new profile ID will be created. Remove any stored token first."
            }
            Confirmation::RemoveToken => "Remove this profile's stored or session token?",
            Confirmation::Exit => {
                "Downloads are active. Closing stops this process; incomplete jobs can be resumed after restart."
            }
        };
        egui::Window::new("Confirm action")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_max_width(430.0);
                ui.label(text);
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        self.confirmation = None;
                    }
                    if ui.button("Confirm").clicked() {
                        if let Some(id) = self.active.clone() {
                            match kind {
                                Confirmation::Archive => self.send(Command::ArchiveProfile(id)),
                                Confirmation::Move => {
                                    self.send(Command::MoveProfile {
                                        id,
                                        url: self.edit_url.trim().into(),
                                        allow_http: self.allow_http,
                                    });
                                    self.edit_url.clear();
                                }
                                Confirmation::RemoveToken => self.send(Command::RemoveToken(id)),
                                Confirmation::Exit => {
                                    self.exit_allowed = true;
                                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                }
                            }
                        } else if kind == Confirmation::Exit {
                            self.exit_allowed = true;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        self.confirmation = None;
                    }
                });
            });
    }
}
fn empty(ui: &mut egui::Ui, text: &str) {
    ui.add_space(28.0);
    ui.label(RichText::new(text).weak());
}
fn declaration(ui: &mut egui::Ui, label: &str, value: &serde_json::Value) {
    egui::CollapsingHeader::new(label).show(ui, |ui| {
        let text =
            serde_json::to_string_pretty(value).unwrap_or_else(|_| "Invalid local JSON".into());
        ui.add(
            egui::TextEdit::multiline(&mut text.as_str())
                .desired_width(f32::INFINITY)
                .code_editor()
                .interactive(false),
        );
    });
}
fn job_actions(job: &DownloadJob) -> Vec<(&'static str, Command)> {
    match job.state.as_str() {
        "Queued" | "FetchingInventory" | "Downloading" => vec![
            ("Pause", Command::Pause(job.id.clone())),
            ("Cancel", Command::Cancel(job.id.clone())),
        ],
        "Paused" => vec![
            ("Resume", Command::Resume(job.id.clone())),
            ("Cancel", Command::Cancel(job.id.clone())),
        ],
        "Failed" => vec![
            ("Retry", Command::Retry(job.id.clone())),
            ("Cancel", Command::Cancel(job.id.clone())),
        ],
        _ => vec![],
    }
}
impl eframe::App for Desktop {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        if ctx.input(|i| i.viewport().close_requested())
            && !self.exit_allowed
            && self.snapshot.jobs.iter().any(|j| {
                matches!(
                    j.state.as_str(),
                    "Queued" | "FetchingInventory" | "Downloading" | "Verifying" | "Committing"
                )
            })
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirmation = Some(Confirmation::Exit);
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.ctx().set_visuals(if self.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        });
        let viewport_height = ui.ctx().content_rect().height();
        let layout_height = (viewport_height - 16.0).max(0.0);
        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            ui.set_min_height(layout_height);
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(185.0, layout_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| self.nav(ui),
                );
                let divider_x = ui.cursor().left() + 4.0;
                let divider_top = ui.min_rect().top();
                ui.painter().line_segment(
                    [
                        egui::pos2(divider_x, divider_top),
                        egui::pos2(divider_x, divider_top + layout_height),
                    ],
                    ui.visuals().widgets.noninteractive.bg_stroke,
                );
                ui.add_space(10.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), layout_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.add_space(8.0);
                        if let Some((message, error)) = self.message.clone() {
                            ui.horizontal_wrapped(|ui| {
                                ui.colored_label(
                                    if error {
                                        Color32::LIGHT_RED
                                    } else {
                                        ui.visuals().text_color()
                                    },
                                    message,
                                );
                                if ui.small_button("Dismiss").clicked() {
                                    self.message = None;
                                }
                            });
                            ui.separator();
                        }
                        match self.screen {
                            Screen::Catalog => self.catalog(ui),
                            Screen::Library => self.library(ui),
                            Screen::Downloads => self.downloads(ui),
                            Screen::Settings => self.settings(ui),
                        }
                    },
                );
            });
        });
        self.confirmation(ui.ctx());
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
            std::process::exit(2);
        }
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("App Depot")
            .with_inner_size([1120.0, 760.0])
            .with_min_inner_size([760.0, 520.0]),
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
        assert!(matches!(
            job_actions(&job("Downloading"))[0].1,
            Command::Pause(_)
        ));
        assert!(job_actions(&job("Verifying")).is_empty());
        assert!(job_actions(&job("Completed")).is_empty());
    }
    #[test]
    fn unknown_json_keeps_large_integer_for_display() {
        let value: serde_json::Value =
            serde_json::from_str(r#"{"appDepot":{"future":18446744073709551616},"other":null}"#)
                .unwrap();
        let rendered = serde_json::to_string_pretty(&value).unwrap();
        assert!(rendered.contains("18446744073709551616"));
        assert!(rendered.contains("\"other\": null"));
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
}
