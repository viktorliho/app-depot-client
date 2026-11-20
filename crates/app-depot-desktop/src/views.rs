use super::*;
use app_depot_core::transfer::TransferPhase;

impl Desktop {
    fn header(&self, ui: &mut egui::Ui, title: &str, subtitle: &str) {
        ui.label(RichText::new(title).size(26.0).strong());
        ui.label(RichText::new(subtitle).size(13.0).weak());
        ui.add_space(14.0);
    }
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            ui.add_space(10.0);
            ui.label(RichText::new("APP DEPOT").size(18.0).strong());
            ui.label(RichText::new("Software library").small().weak());
            ui.add_space(28.0);
            for (screen, label) in [
                (Screen::Catalog, "Browse"),
                (Screen::Library, "My library"),
                (Screen::Downloads, "Downloads"),
                (Screen::Settings, "Settings"),
            ] {
                let selected = self.screen == screen;
                if ui
                    .add_sized(
                        [ui.available_width(), 38.0],
                        egui::Button::selectable(selected, label),
                    )
                    .clicked()
                {
                    self.screen = screen;
                }
            }
            ui.add_space(24.0);
            ui.separator();
            ui.add_space(8.0);
            ui.label(RichText::new("SERVER").small().weak());
            let options: Vec<_> = self
                .snapshot
                .profiles
                .iter()
                .map(|p| (p.id.clone(), p.name.clone(), p.archived))
                .collect();
            let name = self
                .profile()
                .map_or("Choose server", |p| p.name.as_str())
                .to_owned();
            egui::ComboBox::from_id_salt("sidebar-profile")
                .selected_text(name)
                .width(ui.available_width())
                .show_ui(ui, |ui| {
                    for (id, name, archived) in options {
                        if ui
                            .selectable_label(
                                self.active.as_ref() == Some(&id),
                                if archived {
                                    format!("{name} · saved")
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
                status_line(ui, p, self.syncing.contains(&p.id.0));
                if p.origin.starts_with("http://") {
                    ui.label(
                        RichText::new("Unencrypted LAN connection")
                            .small()
                            .color(Color32::from_rgb(205, 145, 65)),
                    );
                }
            }
            if ui.small_button("+ Add server").clicked() {
                self.begin_setup();
                self.setup_error = None;
            }
        });
    }
    fn top_navigation(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("APP DEPOT").strong());
            for (screen, label) in [
                (Screen::Catalog, "Browse"),
                (Screen::Library, "Library"),
                (Screen::Downloads, "Downloads"),
                (Screen::Settings, "Settings"),
            ] {
                if ui.selectable_label(self.screen == screen, label).clicked() {
                    self.screen = screen;
                }
            }
            if ui.small_button("+ Server").clicked() {
                self.begin_setup();
            }
        });
        ui.separator();
    }
    fn catalog(&mut self, ui: &mut egui::Ui) {
        if let Some(id) = self.selected_app.clone() {
            self.details(ui, &id);
            return;
        }
        self.header(
            ui,
            "Browse",
            "Explore software and download the artifacts you choose.",
        );
        if self.active.is_none() {
            self.first_run(ui);
            return;
        }
        let profile = self.profile().cloned();
        ui.horizontal_wrapped(|ui| {
            if let Some(p) = &profile {
                status_line(ui, p, self.syncing.contains(&p.id.0));
            }
            if ui
                .add_enabled(
                    profile.as_ref().is_some_and(|p| !p.archived)
                        && !self.syncing.contains(&self.active.as_ref().unwrap().0),
                    egui::Button::new("Refresh"),
                )
                .clicked()
            {
                self.send(Command::Sync(self.active.clone().unwrap()));
            }
            if let Some(p) = &profile {
                ui.label(
                    RichText::new(format!("Last updated {}", format_time(p.last_sync)))
                        .small()
                        .weak(),
                );
            }
        });
        if let Some(p) = &profile
            && let Some(error) = self.sync_errors.get(&p.id.0)
        {
            egui::CollapsingHeader::new("Connection details")
                .id_salt(&p.id.0)
                .show(ui, |ui| {
                    ui.label(RichText::new(error).small().weak());
                });
        }
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("Search software")
                    .desired_width(260.0),
            );
            if ui.selectable_label(self.prefs.grid, "Grid").clicked() {
                self.prefs.grid = true;
                self.remember();
            }
            if ui.selectable_label(!self.prefs.grid, "List").clicked() {
                self.prefs.grid = false;
                self.remember();
            }
            if ui
                .button(if self.sort_desc {
                    "Name Z–A"
                } else {
                    "Name A–Z"
                })
                .clicked()
            {
                self.sort_desc = !self.sort_desc;
            }
        });
        ui.add_space(10.0);
        let current_catalog = profile
            .as_ref()
            .is_some_and(|p| p.connection_status == "connected")
            && self
                .active
                .as_ref()
                .is_some_and(|id| !self.sync_errors.contains_key(&id.0));
        let query = self.search.to_lowercase();
        let mut apps: Vec<_> = self
            .snapshot
            .apps
            .iter()
            .filter(|a| {
                catalog_item_visible(a.stale, current_catalog)
                    && (query.is_empty()
                        || a.app.name.to_lowercase().contains(&query)
                        || a.app.slug.to_lowercase().contains(&query))
            })
            .cloned()
            .collect();
        apps.sort_by_key(|a| a.app.name.to_lowercase());
        if self.sort_desc {
            apps.reverse();
        }
        if apps.is_empty() {
            if self.snapshot.apps.is_empty() {
                self.empty_panel(
                    ui,
                    "Nothing in this catalog yet",
                    if profile
                        .as_ref()
                        .is_some_and(|p| p.connection_status == "authentication-required")
                    {
                        "Sign in to view this server's catalog."
                    } else if self.syncing.contains(&self.active.as_ref().unwrap().0) {
                        "Loading the catalog in the background…"
                    } else {
                        "Saved items will appear here after the first successful connection."
                    },
                    "Manage connection",
                    None,
                );
            } else {
                self.empty_panel(
                    ui,
                    if query.is_empty() && current_catalog {
                        "No apps currently available"
                    } else {
                        "No matches"
                    },
                    if query.is_empty() && current_catalog {
                        "Saved downloads remain in My library."
                    } else {
                        "Try a different search term."
                    },
                    if query.is_empty() && current_catalog {
                        "My library"
                    } else {
                        "Clear search"
                    },
                    None,
                );
            }
            return;
        }
        let width = ui.available_width();
        let columns = if self.prefs.grid {
            ((width + 10.0) / 228.0).floor().max(1.0) as usize
        } else {
            1
        };
        let card_width = ((width - 10.0 * (columns.saturating_sub(1)) as f32) / columns as f32)
            .clamp(160.0, 280.0);
        let row_height = if self.prefs.grid { 236.0 } else { 68.0 };
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, row_height, apps.len().div_ceil(columns), |ui, range| {
                for row in range {
                    ui.horizontal(|ui| {
                        for view in apps.iter().skip(row * columns).take(columns) {
                            if self.prefs.grid {
                                self.app_card(ui, view, card_width);
                            } else {
                                self.app_row(ui, view);
                            }
                        }
                    });
                    ui.add_space(4.0);
                }
            });
    }
    fn first_run(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width().min(700.0);
        let top = ((ui.available_height() - 390.0) / 3.0).max(16.0);
        ui.add_space(top);
        ui.horizontal(|ui| {
            ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
            egui::Frame::new()
                .fill(ui.visuals().faint_bg_color)
                .corner_radius(16)
                .inner_margin(egui::Margin::same(28))
                .show(ui, |ui| {
                    ui.set_width((width - 56.0).max(240.0));
                    ui.set_min_height(310.0);
                    ui.vertical_centered(|ui| {
                        ui.add_space(15.0);
                        ui.label(RichText::new("Your software library starts here").size(23.0).strong());
                        ui.add_space(6.0);
                        ui.label(RichText::new("Connect to your self-hosted App Depot Server. Browse and verify software here, then use your local library offline.").size(14.0));
                        ui.add_space(22.0);
                        if ui.add_sized([190.0, 40.0], egui::Button::new(RichText::new("Add Server").color(Color32::WHITE).strong()).fill(Color32::from_rgb(0, 108, 157))).clicked() {
                            self.begin_setup();
                        }
                        ui.add_space(28.0);
                        ui.separator();
                        ui.add_space(12.0);
                        ui.label(RichText::new("Connect a server   ·   Browse its catalog   ·   Download verified artifacts").size(12.0).weak());
                    });
                });
        });
    }
    fn empty_panel(
        &mut self,
        ui: &mut egui::Ui,
        title: &str,
        body: &str,
        action: &str,
        setup: Option<Setup>,
    ) {
        let width = ui.available_width().min(620.0);
        let height = ui.available_height().clamp(220.0, 330.0);
        ui.allocate_ui_with_layout(
            egui::vec2(width, height),
            egui::Layout::top_down(egui::Align::Center),
            |ui| {
                egui::Frame::new()
                    .fill(ui.visuals().faint_bg_color)
                    .corner_radius(14)
                    .inner_margin(egui::Margin::same(26))
                    .show(ui, |ui| {
                        ui.set_width((width - 52.0).max(150.0));
                        ui.add_space(15.0);
                        ui.label(RichText::new(title).size(21.0).strong());
                        ui.label(RichText::new(body).size(14.0).weak());
                        ui.add_space(18.0);
                        if ui.button(action).clicked() {
                            if let Some(step) = setup {
                                if step == Setup::Url {
                                    self.begin_setup();
                                } else {
                                    self.setup = step;
                                }
                            } else if action == "Clear search" {
                                self.search.clear();
                            } else if action == "My library" {
                                self.screen = Screen::Library;
                            } else if action == "Browse catalog" {
                                self.screen = Screen::Catalog;
                            } else {
                                self.screen = Screen::Settings;
                            }
                        }
                    });
            },
        );
    }
    fn app_summary(view: &AppView) -> Option<String> {
        view.releases.iter().find_map(|r| {
            r.declaration
                .as_ref()
                .and_then(|s| s.app.convention::<AppConvention>().ok().flatten())
                .and_then(|c| c.summary.or(c.description))
        })
    }
    fn app_card(&mut self, ui: &mut egui::Ui, view: &AppView, width: f32) {
        let response = egui::Frame::new()
            .fill(ui.visuals().faint_bg_color)
            .corner_radius(12)
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.set_width(width - 24.0);
                    ui.set_min_height(206.0);
                    self.request_art(
                        ui,
                        &view.app.id,
                        ArtUse::Grid,
                        egui::vec2(width - 24.0, 118.0),
                        &view.app.name,
                    );
                    ui.add_space(7.0);
                    ui.label(
                        RichText::new(truncate(&view.app.name, 42))
                            .size(16.0)
                            .strong(),
                    )
                    .on_hover_text(&view.app.name);
                    ui.label(
                        RichText::new(truncate(
                            &Self::app_summary(view).unwrap_or_else(|| {
                                format!(
                                    "{} release{} available",
                                    view.releases.len(),
                                    if view.releases.len() == 1 { "" } else { "s" }
                                )
                            }),
                            90,
                        ))
                        .small()
                        .weak(),
                    );
                    let verified = view
                        .releases
                        .iter()
                        .flat_map(|r| &r.associations)
                        .any(|a| self.is_verified(&a.association.id));
                    ui.add_space(7.0);
                    ui.horizontal(|ui| {
                        if verified {
                            ui.label(
                                RichText::new("Downloaded and verified")
                                    .small()
                                    .color(Color32::from_rgb(80, 170, 125)),
                            );
                        } else if view.stale {
                            ui.label(RichText::new("Saved metadata").small().weak());
                        } else {
                            ui.label(RichText::new("Available").small().weak());
                        }
                    });
                });
            })
            .response;
        if ui
            .interact(
                response.rect,
                ui.scope_id().with(&view.app.id),
                egui::Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            self.selected_app = Some(view.app.id.clone());
        }
    }
    fn app_row(&mut self, ui: &mut egui::Ui, view: &AppView) {
        let response = egui::Frame::new()
            .fill(ui.visuals().faint_bg_color)
            .corner_radius(9)
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    self.request_art(
                        ui,
                        &view.app.id,
                        ArtUse::Grid,
                        egui::vec2(55.0, 46.0),
                        &view.app.name,
                    );
                    ui.vertical(|ui| {
                        ui.label(RichText::new(truncate(&view.app.name, 68)).strong())
                            .on_hover_text(&view.app.name);
                        ui.label(
                            RichText::new(
                                Self::app_summary(view)
                                    .unwrap_or_else(|| format!("{} releases", view.releases.len())),
                            )
                            .small()
                            .weak(),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(
                                if view
                                    .releases
                                    .iter()
                                    .flat_map(|r| &r.associations)
                                    .any(|a| self.is_verified(&a.association.id))
                                {
                                    "Verified locally"
                                } else {
                                    "View details →"
                                },
                            )
                            .small(),
                        );
                    });
                });
            })
            .response;
        if ui
            .interact(
                response.rect,
                ui.scope_id().with(("row", &view.app.id)),
                egui::Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            self.selected_app = Some(view.app.id.clone());
        }
        ui.add_space(4.0);
    }
    fn is_verified(&self, id: &str) -> bool {
        self.snapshot.files.iter().any(|f| f.association_id == id)
            || self.snapshot.trees.iter().any(|t| t.association_id == id)
    }
    fn association_state(&self, id: &str) -> &'static str {
        if self.is_verified(id) {
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
            "Paused"
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
    fn play_controls(&mut self, ui: &mut egui::Ui, association: &str) {
        let choices: Vec<_> = self
            .snapshot
            .launches
            .iter()
            .filter(|c| c.association_id == association)
            .cloned()
            .collect();
        for candidate in choices {
            let key = format!("{}:{}", association, candidate.entry_id);
            let state = self.launch_states.get(&key).cloned();
            if candidate.status == "Ready" || candidate.status == "PreparationRequired" {
                let label = candidate
                    .label
                    .clone()
                    .unwrap_or_else(|| candidate.entry_id.clone());
                let multiple = self
                    .snapshot
                    .launches
                    .iter()
                    .filter(|c| {
                        c.release_id == candidate.release_id
                            && matches!(c.status.as_str(), "Ready" | "PreparationRequired")
                    })
                    .count()
                    > 1;
                let busy = state
                    .as_ref()
                    .is_some_and(|(s, _)| s == "Starting" || s == "Preparing" || s == "Running");
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            !busy,
                            egui::Button::new(if multiple {
                                format!("Play: {label}")
                            } else {
                                "Play".into()
                            }),
                        )
                        .clicked()
                    {
                        self.send(Command::Play {
                            profile: candidate.profile_id.clone(),
                            release: candidate.release_id.clone(),
                            association: candidate.association_id.clone(),
                            entry: candidate.entry_id.clone(),
                            declaration_revision: candidate.declaration_revision.clone(),
                        });
                        self.launch_states.insert(
                            key.clone(),
                            (
                                "Starting".into(),
                                "Checking local content before launch".into(),
                            ),
                        );
                    }
                    ui.label(
                        RichText::new(format!(
                            "{label} · {}",
                            state.as_ref().map_or(
                                if candidate.status == "Ready" {
                                    "Ready to play"
                                } else {
                                    "Will prepare on Play"
                                },
                                |(s, _)| s.as_str()
                            )
                        ))
                        .small(),
                    );
                });
            } else if candidate.status != "NoLaunch" {
                ui.label(
                    RichText::new(format!(
                        "{}: {}",
                        candidate.label.as_deref().unwrap_or(&candidate.entry_id),
                        candidate.reason
                    ))
                    .small()
                    .weak(),
                );
            }
            if let Some((state, message)) = &state
                && state != "Starting"
            {
                ui.label(RichText::new(message).small());
            }
        }
    }
    fn details(&mut self, ui: &mut egui::Ui, id: &str) {
        let Some(view) = self.snapshot.apps.iter().find(|v| v.app.id == id).cloned() else {
            self.selected_app = None;
            return;
        };
        if ui.button("← Back to catalog").clicked() {
            self.selected_app = None;
            return;
        }
        ui.add_space(8.0);
        egui::ScrollArea::vertical().auto_shrink([false,false]).show(ui,|ui| {
            ui.horizontal_wrapped(|ui| { self.request_art(ui,id,ArtUse::Detail,egui::vec2(210.0,130.0),&view.app.name);
                ui.vertical(|ui| { ui.label(RichText::new(&view.app.name).size(27.0).strong());
                    if let Some(summary)=Self::app_summary(&view) { ui.label(RichText::new(summary).size(14.0)); }
                    if let Some(info)=view.releases.iter().find_map(|r|r.declaration.as_ref().and_then(|s|s.app.convention::<AppConvention>().ok().flatten())) {
                        if let Some(description)=info.description.filter(|text|Some(text.as_str())!=info.summary.as_deref()) { ui.label(RichText::new(description).small().weak()); }
                        if let Some(developer)=info.developer { ui.label(RichText::new(format!("By {developer}")).small().weak()); }
                    }
                    let count=view.releases.iter().flat_map(|r|&r.associations).filter(|a|self.is_verified(&a.association.id)).count();
                    ui.label(RichText::new(if count>0 { format!("{count} artifact{} verified locally",if count==1{""}else{"s"}) }else if view.stale { "Saved metadata".into() } else { "Available to download".into() }).small().weak());
                    if count>0 && let Some(profile)=self.active.clone() {
                        let prepared=self.snapshot.installations.iter().any(|item|item.app_id==view.app.id && item.state=="Ready");
                        let busy=self.local_operations.contains_key(&view.app.id);
                        if let Some(phase)=self.local_operations.get(&view.app.id){ui.horizontal(|ui|{ui.spinner();ui.label(phase);});}
                        if prepared && ui.add_enabled(!busy,egui::Button::new("Remove local application…")).clicked(){self.remove_app_target=Some((profile.clone(),view.app.id.clone(),view.app.name.clone(),false));self.confirmation=Some(Confirmation::RemoveApp);}
                        if ui.add_enabled(!busy,egui::Button::new("Remove all local content and managed data…")).clicked(){self.remove_app_target=Some((profile,view.app.id.clone(),view.app.name.clone(),true));self.confirmation=Some(Confirmation::RemoveApp);}
                    }
                    if view.stale { ui.label(RichText::new("Saved metadata; this app was absent from the latest catalog refresh.").small().weak()); }
                });
            }); ui.add_space(18.0);
            ui.label(RichText::new("Releases & downloads").size(19.0).strong());
            ui.label(RichText::new("Choose an artifact from any release. Downloading does not install or launch software.").small().weak()); ui.add_space(8.0);
            if view.releases.is_empty() { ui.label("No releases are available in the saved catalog."); }
            for (r_index,release) in view.releases.iter().enumerate() {
                egui::Frame::new().fill(ui.visuals().faint_bg_color).corner_radius(12).inner_margin(egui::Margin::same(14)).show(ui,|ui| {
                    ui.set_width((ui.available_width()-28.0).max(300.0));
                    let version=release.release.version.as_deref().filter(|s|!s.is_empty()).map_or_else(||format!("Release {}",r_index+1),|v|format!("Version {v}"));
                    egui::CollapsingHeader::new(RichText::new(version).size(17.0).strong()).id_salt(&release.release.id).default_open(r_index==0).show(ui,|ui| {
                        ui.label(RichText::new(format!("{} · {}",release.release.state,release.associations.len())).small().weak());
                        if let Some(snapshot)=&release.declaration
                            && let Ok(Some(info))=snapshot.release.convention::<ReleaseConvention>() {
                                if let Some(notes)=info.notes { ui.label(notes); }
                                if let Some(platforms)=info.platforms { ui.label(format!("Declared platforms: {}",platforms.join(", "))); }
                                if let Some(runtime)=info.runtime { ui.label(format!("Declared runtime: {} {} · preparation unavailable",runtime.name.unwrap_or_else(||"Unspecified".into()),runtime.version.unwrap_or_default())); }
                        }
                        if release.associations.is_empty() { ui.label("No artifacts in this release."); }
                        for (a_index,item) in release.associations.iter().enumerate() {
                            let convention=release.declaration.as_ref().and_then(|s|s.associations.iter().find(|a|a.association_id==item.association.id)).and_then(|a|a.declaration.convention::<AssociationConvention>().ok().flatten());
                            let kind=item.artifact.as_ref().map_or("Artifact",|a|a.kind.as_str());
                            let title=convention.as_ref().and_then(|c|c.label.clone()).unwrap_or_else(||format!("{} artifact{}",if kind=="Tree"{"Directory"}else{"File"},if release.associations.len()>1 {format!(" {}",a_index+1)}else{String::new()}));
                            egui::Frame::new().fill(ui.visuals().panel_fill).corner_radius(9).inner_margin(egui::Margin::same(12)).show(ui,|ui| {
                                ui.set_width((ui.available_width()-24.0).max(250.0));
                                ui.horizontal_wrapped(|ui| {
                                    ui.vertical(|ui| { ui.label(RichText::new(&title).strong());
                                        let size=item.artifact.as_ref().and_then(|a|a.size).map(format_bytes).unwrap_or_else(||"Size not specified".into());
                                        ui.label(RichText::new(format!("{kind} · {size}")).small().weak());
                                        if let Some(purpose)=convention.as_ref().and_then(|c|c.purpose.as_deref()) { ui.label(RichText::new(purpose).small().weak()); }
                                        if let Some(description)=convention.as_ref().and_then(|c|c.description.as_deref()) { ui.label(RichText::new(description).small().weak()); }
                                        ui.label(RichText::new(self.association_state(&item.association.id)).small().color(if self.is_verified(&item.association.id){Color32::from_rgb(80,170,125)}else{ui.visuals().text_color()}));
                                    });
                                    let enabled=!view.stale && item.artifact.as_ref().is_some_and(|a|matches!(a.kind.as_str(),"File"|"Tree")) && self.profile().is_some_and(|p|!p.archived) && self.association_state(&item.association.id)=="Available to download";
                                    if ui.add_enabled(enabled,egui::Button::new("Download")).clicked() && let (Some(profile),Some(artifact))=(self.active.clone(),&item.artifact) {
                                        self.send(Command::Download {profile,release:release.release.id.clone(),association:item.association.id.clone(),kind:artifact.kind.clone()}); self.screen=Screen::Downloads;
                                    }
                                });
                                self.play_controls(ui,&item.association.id);
                                egui::CollapsingHeader::new("Technical details").id_salt(&item.association.id).show(ui,|ui| {
                                    copy_line(ui,"Association ID",&item.association.id);
                                    if let Some(artifact)=&item.artifact { copy_line(ui,"Artifact ID",&artifact.id); }
                                    if let Some(s)=&release.declaration && let Some(a)=s.associations.iter().find(|a|a.association_id==item.association.id) { declaration(ui,"Association declaration",&a.declaration.data); }
                                });
                            }); ui.add_space(7.0);
                        }
                        egui::CollapsingHeader::new("Advanced declarations and identity").id_salt(("advanced",&release.release.id)).show(ui,|ui| {
                            copy_line(ui,"App ID",&view.app.id); copy_line(ui,"Release ID",&release.release.id);
                            if let Some(s)=&release.declaration { declaration(ui,"App declaration",&s.app.data); declaration(ui,"Release declaration",&s.release.data); }
                            else { ui.label("Declaration snapshot not available in the local cache."); }
                        });
                    });
                }); ui.add_space(9.0);
            }
        });
    }
    fn library(&mut self, ui: &mut egui::Ui) {
        self.header(
            ui,
            "My library",
            "Your verified downloads remain here when a server is offline.",
        );
        if self.active.is_none() {
            self.empty_panel(
                ui,
                "No server selected",
                "Choose a server profile to view its saved downloads.",
                "Manage servers",
                None,
            );
            return;
        }
        if self.snapshot.files.is_empty()
            && self.snapshot.trees.is_empty()
            && self.snapshot.local_issues.is_empty()
            && self.snapshot.installations.is_empty()
        {
            self.empty_panel(
                ui,
                "No downloads yet",
                "Choose an artifact in Browse. Verified files and directories appear here.",
                "Browse catalog",
                None,
            );
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
                    Some(f.size),
                    None,
                )
            })
            .collect();
        items.extend(self.snapshot.trees.iter().map(|t| {
            (
                "Directory",
                t.association_id.clone(),
                t.artifact_id.clone(),
                t.path.clone(),
                None,
                Some(t.file_count),
            )
        }));
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for issue in &self.snapshot.local_issues {
                    ui.group(|ui| {
                        ui.label(
                            egui::RichText::new(format!("{} download needs repair", issue.kind))
                                .strong(),
                        );
                        ui.label(&issue.reason);
                        if issue.kind=="Tree" && let Some(association)=&issue.association_id {
                            if ui.button("Repair from verified local files…").clicked(){
                                self.repair_target=Some((issue.profile_id.clone(),association.clone()));
                                self.confirmation=Some(Confirmation::RepairSource);
                            }
                            ui.label(RichText::new("The existing directory will be kept in staging so its extra files can be inspected.").small().weak());
                        }
                        egui::CollapsingHeader::new("Technical identity").show(ui, |ui| {
                            ui.label(&issue.artifact_id);
                        });
                    });
                    ui.add_space(8.0);
                }
                let isolated:Vec<_>=self.snapshot.installations.iter().filter(|installation|installation.state=="Ready" && !self.snapshot.files.iter().any(|file|file.association_id==installation.association_id) && !self.snapshot.trees.iter().any(|tree|tree.association_id==installation.association_id)).cloned().collect();
                for installation in isolated {
                    let name=self.snapshot.apps.iter().find(|app|app.app.id==installation.app_id).map_or("Saved application",|app|app.app.name.as_str()).to_owned();
                    ui.group(|ui|{
                        ui.label(RichText::new(name.clone()).strong());
                        ui.label("Writable application copy remains available. Its original verified download needs inspection or repair.");
                        if ui.button("Open working copy").clicked(){self.send(Command::Reveal(installation.path.clone()));}
                        self.play_controls(ui,&installation.association_id);
                        if ui.button("Remove working copy…").clicked(){self.remove_target=Some((installation.id.clone(),false,name.clone()));self.confirmation=Some(Confirmation::RemoveInstallation);}
                        if ui.button("Remove completely…").clicked(){self.remove_target=Some((installation.id.clone(),true,name.clone()));self.confirmation=Some(Confirmation::RemoveInstallation);}
                    });
                    ui.add_space(8.0);
                }
                for (kind, association, artifact, path, size, file_count) in items {
                    let context = self
                        .snapshot
                        .apps
                        .iter()
                        .flat_map(|a| a.releases.iter().map(move |r| (a, r)))
                        .find(|(_, r)| {
                            r.associations
                                .iter()
                                .any(|x| x.association.id == association)
                        })
                        .map(|(a, r)| {
                            (
                                a.app.id.clone(),
                                a.app.name.clone(),
                                r.release.version.clone(),
                            )
                        });
                    let title = context.as_ref().map_or_else(
                        || format!("Saved {kind} artifact"),
                        |(_, name, _)| name.clone(),
                    );
                    let subtitle = context.as_ref().map_or_else(
                        || kind.to_owned(),
                        |(_, _, version)| {
                            version.as_deref().map_or_else(
                                || format!("Release · {kind}"),
                                |v| format!("Version {v} · {kind}"),
                            )
                        },
                    );
                    egui::Frame::new()
                        .fill(ui.visuals().faint_bg_color)
                        .corner_radius(12)
                        .inner_margin(egui::Margin::same(13))
                        .show(ui, |ui| {
                            ui.set_width((ui.available_width() - 26.0).clamp(300.0, 850.0));
                            ui.horizontal_wrapped(|ui| {
                                if let Some((id, name, _)) = &context {
                                    self.request_art(
                                        ui,
                                        id,
                                        ArtUse::Grid,
                                        egui::vec2(72.0, 64.0),
                                        name,
                                    );
                                } else {
                                    let (rect, _) = ui.allocate_exact_size(
                                        egui::vec2(72.0, 64.0),
                                        egui::Sense::hover(),
                                    );
                                    ui.painter().rect_filled(rect, 9.0, ui.visuals().panel_fill);
                                    ui.painter().text(
                                        rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        "A",
                                        egui::FontId::proportional(28.0),
                                        ui.visuals().weak_text_color(),
                                    );
                                }
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(&title).size(16.0).strong());
                                    ui.label(RichText::new(subtitle).small().weak());
                                    ui.label(
                                        RichText::new(format!(
                                            "Original download verified{}",
                                            size.map(|n| format!(" · {}", format_bytes(n)))
                                                .or_else(
                                                    || file_count.map(|n| format!(" · {n} files"))
                                                )
                                                .unwrap_or_default()
                                        ))
                                        .small()
                                        .color(Color32::from_rgb(80, 170, 125)),
                                    );
                                });
                                if ui.button("Open folder").clicked() {
                                    self.send(Command::Reveal(path.clone()));
                                }
                                self.play_controls(ui, &association);
                            });
                            if let Some(installation) = self
                                .snapshot
                                .installations
                                .iter()
                                .find(|i| {
                                    i.association_id == association && i.artifact_id == artifact
                                })
                                .cloned()
                            {
                                if let Some(phase)=self.local_operations.get(&installation.id){
                                    ui.horizontal(|ui|{ui.spinner();ui.label(phase);});
                                }
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(format!("Writable installation: {}", installation.state));
                                    if ui.add_enabled(!self.local_operations.contains_key(&installation.id),egui::Button::new("Verify integrity")).clicked(){
                                        self.local_operations.insert(installation.id.clone(),"Comparing installed files with the published Artifact".into());
                                        self.send(Command::VerifyInstallation(installation.id.clone()));
                                    }
                                    if ui.add_enabled(!self.local_operations.contains_key(&installation.id),egui::Button::new("Repair…")).clicked(){
                                        self.repair_installation_target=Some((installation.id.clone(),title.clone()));
                                        self.confirmation=Some(Confirmation::RepairInstallation);
                                    }
                                    if ui.add_enabled(!self.local_operations.contains_key(&installation.id),egui::Button::new("Remove application…")).clicked() {
                                        self.remove_target =
                                            Some((installation.id.clone(), false, title.clone()));
                                        self.confirmation = Some(Confirmation::RemoveInstallation);
                                    }
                                    if ui.add_enabled(!self.local_operations.contains_key(&installation.id),egui::Button::new("Remove completely…")).clicked() {
                                        self.remove_target =
                                            Some((installation.id.clone(), true, title.clone()));
                                        self.confirmation = Some(Confirmation::RemoveInstallation);
                                    }
                                });
                                if let Some(report)=self.integrity_reports.get(&installation.id){
                                    egui::CollapsingHeader::new(if report.matches_original(){"Integrity check: matches original"}else{"Integrity check: local changes found"}).show(ui,|ui|{
                                        for path in &report.missing {ui.label(format!("Missing: {path}"));}
                                        for path in &report.modified {ui.label(format!("Modified: {path}"));}
                                        for path in &report.unexpected {ui.label(format!("Unexpected: {path}"));}
                                    });
                                }
                            } else if let Some(profile)=self.active.clone() {
                                if let Some(phase)=self.local_operations.get(&association){ui.horizontal(|ui|{ui.spinner();ui.label(phase);});}
                                if ui.add_enabled(!self.local_operations.contains_key(&association),egui::Button::new("Remove local download…")).clicked() {
                                    self.remove_artifact_target=Some((profile,association.clone(),title.clone()));
                                    self.confirmation=Some(Confirmation::RemoveArtifact);
                                }
                            }
                            egui::CollapsingHeader::new("Technical details")
                                .id_salt(&association)
                                .show(ui, |ui| {
                                    copy_line(ui, "Association ID", &association);
                                    copy_line(ui, "Artifact ID", &artifact);
                                    copy_line(ui, "Local path", &path.display().to_string());
                                });
                        });
                    ui.add_space(8.0);
                }
            });
    }
    fn downloads(&mut self, ui: &mut egui::Ui) {
        self.header(
            ui,
            "Downloads",
            "Track transfers, verification and saved content.",
        );
        let jobs = self.snapshot.jobs.clone();
        if jobs.is_empty() {
            self.empty_panel(
                ui,
                "No downloads yet",
                "Select a file or directory artifact in Browse to start.",
                "Browse catalog",
                None,
            );
            return;
        }
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for job in &jobs {
                    let live = self.live_progress.get(&job.id).cloned().filter(|_| matches!(job.state.as_str(), "Downloading" | "FetchingInventory" | "Verifying" | "Committing"));
                    let now = Instant::now();
                    let received = live.as_ref().map_or(job.progress, |p| p.observation.received_bytes.max(job.progress)).min(job.expected_size);
                    let context=self.snapshot.job_context.get(&job.id).cloned();
                    let title=context.as_ref().map_or("Artifact download",|c|c.0.as_str()).to_owned();
                    let release=context.as_ref().map_or("Release".to_owned(),|c|if c.1=="Release"{"Release".to_owned()}else{format!("Version {}",c.1)});
                    let server=context.as_ref().map_or("Saved server",|c|c.2.as_str()).to_owned();
                    let kind = if job.artifact_id.starts_with("adtree:") {
                        "Directory"
                    } else {
                        "File"
                    };
                    egui::Frame::new()
                        .fill(ui.visuals().faint_bg_color)
                        .corner_radius(12)
                        .inner_margin(egui::Margin::same(14))
                        .show(ui, |ui| {
                            ui.set_width((ui.available_width() - 28.0).clamp(300.0, 850.0));
                            ui.horizontal_wrapped(|ui| {
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(&title).size(16.0).strong());
                                    ui.label(
                                        RichText::new(format!(
                                            "{server} · {release} · {kind} · {}",
                                            live.as_ref().map_or(if job.auto_resume {"Waiting for priority"} else {friendly_job_state(&job.state)}, |p| phase_label(p.observation.phase, &job.state))
                                        ))
                                        .small()
                                        .weak(),
                                    );
                                });
                                for (label, cmd) in job_actions(job) {
                                    if ui.button(label).clicked() {
                                        self.send(cmd);
                                    }
                                }
                            });
                            ui.label(
                                RichText::new(format!(
                                    "{} of {}",
                                    format_bytes(received),
                                    format_bytes(job.expected_size)
                                ))
                                .small(),
                            );
                            if job.expected_size > 0 {
                                let ratio = (received as f64 / job.expected_size as f64)
                                    .clamp(0.0, 1.0)
                                    as f32;
                                ui.add(
                                    egui::ProgressBar::new(if job.state == "Completed" {
                                        1.0
                                    } else {
                                        ratio.min(0.99)
                                    })
                                    .desired_width(ui.available_width().min(460.0)),
                                );
                            }
                            if let Some(live) = live.as_ref() {
                                ui.horizontal_wrapped(|ui| {
                                    if let Some(speed) = live.speed(now) {
                                        ui.label(format!("{}/s", format_bytes(speed as u64)));
                                        ui.label(live.eta(job.expected_size, now).map_or("Time remaining: estimating…".to_owned(), |eta| format!("About {} remaining", format_duration(eta))));
                                    } else if live.stalled(now) {
                                        ui.label("Waiting for activity — the server or local storage may be busy");
                                    } else if live.observation.phase == TransferPhase::Downloading {
                                        ui.label("Measuring transfer speed…");
                                    }
                                    if live.observation.files_total > 1 {
                                        ui.label(format!("{} of {} files", live.observation.files_done, live.observation.files_total));
                                    }
                                });
                                if let Some(path) = &live.observation.current_file {
                                    ui.label(RichText::new(format!("Current file: {}", truncate(path, 100))).small().weak());
                                }
                            }
                            if let Some(e) = &job.last_error {
                                if job.state == "Paused" && e.contains("interrupted") {
                                    ui.label(RichText::new("Transfer was interrupted. Resume to validate saved bytes and continue.").small().weak());
                                } else if job.state == "Failed" {
                                    ui.colored_label(Color32::from_rgb(210, 95, 95), friendly_download_error(e));
                                }
                            }
                            if kind == "Directory" {
                                if ui
                                    .small_button(
                                        if self.selected_job.as_deref() == Some(&job.id) {
                                            "Hide files"
                                        } else {
                                            "Show files"
                                        },
                                    )
                                    .clicked()
                                {
                                    if self.selected_job.as_deref() == Some(&job.id) {
                                        self.selected_job = None;
                                    } else {
                                        self.selected_job = Some(job.id.clone());
                                        self.last_tree_details_refresh = Instant::now();
                                        self.send(Command::TreeFiles(job.id.clone()));
                                    }
                                }
                                if self.selected_job.as_deref() == Some(&job.id)
                                    && let Some(files) = self.tree_progress.get(&job.id)
                                {
                                    egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| for f in files.iter().filter(|f| !f.verified).take(20) {
                                        ui.label(
                                            RichText::new(format!(
                                                "{} · {} of {}{}",
                                                f.path,
                                                format_bytes(f.progress),
                                                format_bytes(f.size),
                                                if f.verified { " · verified" } else { "" }
                                            ))
                                            .small(),
                                        );
                                    });
                                    let complete = files.iter().filter(|f| f.verified).count();
                                    ui.label(RichText::new(format!("{complete} of {} files verified; showing up to 20 unfinished members", files.len())).small().weak());
                                }
                            }
                            egui::CollapsingHeader::new("Technical details")
                                .id_salt(&job.id)
                                .show(ui, |ui| {
                                    copy_line(ui, "Job ID", &job.id);
                                    copy_line(ui, "Artifact ID", &job.artifact_id);
                                    copy_line(ui, "Association ID", &job.association_id);
                                    copy_line(ui, "Durable checkpoint", &format_bytes(job.progress));
                                    if let Some(error) = &job.last_error { ui.label(RichText::new(error).small().monospace()); }
                                });
                        });
                    ui.add_space(8.0);
                }
            });
    }
    fn settings(&mut self, ui: &mut egui::Ui) {
        self.header(ui, "Settings", "Servers, storage and appearance.");
        ui.horizontal_wrapped(|ui| {
            for (tab, label) in [
                (SettingsTab::Servers, "Servers & accounts"),
                (SettingsTab::Storage, "Storage"),
                (SettingsTab::Appearance, "Appearance"),
                (SettingsTab::Diagnostics, "Diagnostics"),
            ] {
                if ui
                    .selectable_label(self.settings_tab == tab, label)
                    .clicked()
                {
                    self.settings_tab = tab;
                    if tab == SettingsTab::Storage {
                        self.send(Command::StorageUsage);
                    }
                }
            }
        });
        ui.add_space(12.0);
        egui::ScrollArea::vertical().auto_shrink([false,false]).show(ui,|ui| {
            if self.settings_tab==SettingsTab::Servers {
            section(ui,"Servers & accounts",|ui| {
                let profiles:Vec<_>=self.snapshot.profiles.iter().map(|p|(p.id.clone(),p.name.clone(),p.archived)).collect();
                for (id,name,archived) in profiles { if ui.add_sized([ui.available_width().min(430.0),34.0],egui::Button::selectable(self.active.as_ref()==Some(&id),if archived {format!("{name} · archived")}else{name})).clicked() { self.select(id); } }
                if ui.button("+ Add Server").clicked() { self.begin_setup(); }
                if let Some(p)=self.profile().cloned() {
                    ui.add_space(10.0); ui.label(RichText::new(&p.name).size(17.0).strong()); ui.label(&p.origin);
                    status_line(ui,&p,self.syncing.contains(&p.id.0)); ui.label(RichText::new(format!("Last catalog refresh: {}",format_time(p.last_sync))).small().weak());
                    if p.origin.starts_with("http://") { ui.label(RichText::new("HTTP on the local network is unencrypted, including Bearer tokens.").color(Color32::from_rgb(205,145,65))); }
                    if !p.archived {
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("Refresh now").clicked() { self.send(Command::Sync(p.id.clone())); }
                            if ui.button("Open Server Account page").clicked() { open_account(ui.ctx(), &p.origin); }
                        });
                        ui.add_space(8.0); ui.label(RichText::new("Personal API token").strong());
                        ui.label(RichText::new(if p.credential_ref.is_some(){"Saved in native secure storage."}else{"Use a personal API token for private catalogs. The Server shows a new token only once on its Account page."}).small().weak());
                        ui.horizontal(|ui| { ui.add(egui::TextEdit::singleline(&mut self.token).password(!self.token_visible).hint_text("Paste token here").desired_width(300.0)); ui.checkbox(&mut self.token_visible,"Show"); });
                        ui.checkbox(&mut self.token_persist,"Save securely on this device (otherwise session only)");
                        ui.horizontal(|ui| { if ui.add_enabled(!self.token.is_empty(),egui::Button::new("Verify & save token")).clicked() { let token=std::mem::take(&mut self.token); self.send(Command::SetToken{id:p.id.clone(),token,persist:self.token_persist}); }
                            if ui.button("Remove token…").clicked(){self.confirmation=Some(Confirmation::RemoveToken);} });
                        ui.add_space(10.0); ui.label(RichText::new("Profile details").strong());
                        ui.horizontal(|ui| { ui.add(egui::TextEdit::singleline(&mut self.edit_name).hint_text("New display name")); if ui.button("Rename").clicked(){self.send(Command::RenameProfile{id:p.id.clone(),name:self.edit_name.trim().into()});self.edit_name.clear();} });
                        ui.horizontal(|ui| { ui.add(egui::TextEdit::singleline(&mut self.edit_url).hint_text("New server origin").desired_width(290.0)); if ui.button("Change origin…").clicked(){self.confirmation=Some(Confirmation::Move);} });
                        ui.checkbox(&mut self.edit_allow_http,"Allow unencrypted HTTP for the new origin");
                        if ui.button("Archive server…").clicked(){self.confirmation=Some(Confirmation::Archive);}
                    } else { ui.label("Archived server: saved catalog and verified downloads are available offline."); }
                }
            }); }
            if self.settings_tab==SettingsTab::Storage {
            section(ui,"Storage",|ui| {
                ui.label("New Linux libraries use ~/.appdepot. An existing marked XDG library remains active until you migrate it explicitly.");
                ui.label("Metadata, profiles and preferences"); ui.label(RichText::new(self.snapshot.library_data.display().to_string()).small().weak());
                ui.add_space(5.0); ui.label("Verified downloads and staging"); ui.label(RichText::new(self.snapshot.library_content.display().to_string()).small().weak());
                ui.add_space(10.0);
                if ui.button("Refresh storage usage").clicked(){self.send(Command::StorageUsage);}
                if let Some(usage)=&self.storage_usage {
                    for (label,bytes) in [
                        ("Immutable verified objects",usage.immutable_objects_logical_bytes),
                        ("Verified materializations",usage.verified_materializations_logical_bytes),
                        ("Writable application copies",usage.writable_installations_logical_bytes),
                        ("Managed application data",usage.managed_userdata_logical_bytes),
                        ("Staging and recovery",usage.staging_logical_bytes),
                        ("Artwork cache",usage.artwork_logical_bytes),
                    ] {ui.label(format!("{label}: {}",format_bytes(bytes)));}
                    ui.label(RichText::new("These are logical sizes. Reflinks, compression and shared objects make physical reclaimable space different.").small().weak());
                }
                egui::CollapsingHeader::new("Migration and advanced locations").show(ui,|ui| {
                    ui.label("Close App Depot and pause active downloads, then use `app-depot library migrate \"$HOME/.appdepot\" --confirm` from the existing library. The old root stays intact for review.");
                    ui.label("APP_DEPOT_LIBRARY_DATA_DIR and APP_DEPOT_LIBRARY_CONTENT_DIR can select explicitly managed roots before starting the app. A location change is never made by merely editing this screen.");
                });
            }); }
            if self.settings_tab==SettingsTab::Appearance {
            section(ui,"Appearance",|ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Theme");
                    for (theme,name) in [(Theme::System,"System"),(Theme::Light,"Light"),(Theme::Dark,"Dark")] { if ui.selectable_label(self.prefs.theme==theme,name).clicked() { self.prefs.theme=theme;self.apply_theme(ui.ctx());self.remember(); } }
                });
                ui.label(RichText::new("Follows the desktop theme until you choose Light or Dark.").small().weak());
            }); }
            if self.settings_tab==SettingsTab::Diagnostics {
            section(ui,"Diagnostics & about",|ui| {
                ui.label(format!("App Depot Desktop {}",env!("CARGO_PKG_VERSION")));
                ui.label("Verified downloads and native portable launch on Linux. Installation remains unavailable.");
                if let Some(p)=self.profile() {
                    egui::CollapsingHeader::new("Server technical details").show(ui,|ui| {
                        copy_line(ui,"Profile ID",&p.id.0);
                        if let Some(c)=&p.capabilities { ui.label(format!("API {} · File {} · Tree {} · Range {}",c.api_version,c.file_identity,c.tree_identity,c.download_ranges));
                            ui.label(format!("Page limit {} · concurrent ranges {}",c.limits.max_page_size,c.limits.max_concurrent_ranges));
                            for feature in &c.features { ui.label(feature); }
                        }
                        for issue in &p.compatibility_issues {ui.colored_label(Color32::from_rgb(205,145,65),issue);}
                    });
                }
            }); }
        });
    }
    fn setup_dialog(&mut self, ctx: &egui::Context) {
        if self.setup == Setup::Closed {
            return;
        }
        egui::Modal::new(egui::Id::unique("connect-to-app-depot")).show(ctx,|ui| {
            ui.set_width(420.0); ui.label(RichText::new(if self.setup==Setup::Token{"Sign in to your server"}else{"Add a server"}).size(21.0).strong());
            ui.label(RichText::new("Enter the address of your self-hosted App Depot Server. Public catalogs need no token.").weak()); ui.add_space(11.0);
            ui.label("Server URL"); ui.add_enabled(self.setup!=Setup::Checking,egui::TextEdit::singleline(&mut self.profile_url).hint_text("https://depot.example.org").desired_width(390.0));
            ui.label("Display name"); ui.add_enabled(self.setup!=Setup::Checking,egui::TextEdit::singleline(&mut self.profile_name).hint_text("My server").desired_width(390.0));
            ui.add_enabled(self.setup!=Setup::Checking,egui::Checkbox::new(&mut self.allow_http,"Allow unencrypted LAN HTTP"));
            if self.allow_http {ui.label(RichText::new("HTTP exposes Bearer tokens to anyone able to observe this network.").small().color(Color32::from_rgb(205,145,65)));}
            if let Some(error)=&self.setup_error {ui.add_space(8.0);ui.colored_label(Color32::from_rgb(210,95,95),error);}
            if self.setup==Setup::Token {
                ui.add_space(8.0); ui.label("Sign in to the Server in your browser, open Account and create a personal API token. It is displayed once.");
                if ui.button("Open Server Account page").clicked() && let Err(e)=open_account_checked(ctx,&self.profile_url,self.allow_http) {self.setup_error=Some(e);}
                ui.horizontal(|ui| {ui.add(egui::TextEdit::singleline(&mut self.token).password(!self.token_visible).hint_text("Paste personal API token").desired_width(300.0));ui.checkbox(&mut self.token_visible,"Show");});
                ui.checkbox(&mut self.token_persist,"Save securely on this device");
                if !self.token_persist {ui.label(RichText::new("Session-only token is forgotten when App Depot closes.").small().weak());}
            }
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {self.send(Command::CancelConnect(self.connect_attempt));self.setup=Setup::Closed;self.token.clear();self.setup_error=None;}
                let allowed=self.setup!=Setup::Checking && !self.profile_url.trim().is_empty() && (self.setup!=Setup::Token||!self.token.is_empty());
                if ui.add_enabled(allowed,egui::Button::new(if self.setup==Setup::Token{"Verify & connect"}else if self.setup==Setup::Checking{"Checking…"}else{"Connect"})).clicked() {
                    let url=self.profile_url.trim().to_owned();
                    match ApiClient::new(&url,None,self.allow_http) {
                        Err(e)=>self.setup_error=Some(e.to_string()),
                        Ok(api)=>{ self.setup_error=None; let name=if self.profile_name.trim().is_empty(){api.origin().trim_end_matches('/').split('/').next_back().unwrap_or("Server").to_owned()}else{self.profile_name.trim().to_owned()};
                            let token=if self.token.is_empty(){None}else{Some(self.token.clone())};
                            self.connect_attempt=self.connect_attempt.wrapping_add(1);
                            self.send(Command::Connect{name,url,allow_http:self.allow_http,token,persist:self.token_persist,attempt:self.connect_attempt});self.setup=Setup::Checking; }
                    }
                }
            });
        });
    }
    fn confirmation(&mut self, ctx: &egui::Context) {
        let Some(kind) = self.confirmation else {
            return;
        };
        let text = match kind {
            Confirmation::Archive => {
                "Archive this server? Its token will be removed; cached catalog and verified downloads remain."
            }
            Confirmation::Move => {
                "Changing the origin creates a separate profile. Existing cache and downloads remain with the archived origin."
            }
            Confirmation::RemoveToken => "Remove this server's saved or session token?",
            Confirmation::RemoveInstallation => {
                if self
                    .remove_target
                    .as_ref()
                    .is_some_and(|(_, complete, _)| *complete)
                {
                    "Remove this local working copy, its managed user data and its selected verified download? Shared content used elsewhere remains. This does not remove anything from the Server or external application data."
                } else {
                    "Remove this writable working copy? The verified download and managed user data remain for later preparation. Files written inside the working copy will be removed."
                }
            }
            Confirmation::RemoveArtifact => {
                "Remove this selected local download? Its Server entry and other local owners remain. The local bytes are discarded when no longer shared."
            }
            Confirmation::RemoveApp => {
                if self
                    .remove_app_target
                    .as_ref()
                    .is_some_and(|(_, _, _, complete)| *complete)
                {
                    "Remove all local downloads and writable copies for this App, including App Depot-managed user data? Other Apps, Server data and external application files remain."
                } else {
                    "Remove this App's writable working copies? Verified downloads and managed user data remain for later preparation."
                }
            }
            Confirmation::RepairSource => {
                "Rebuild this downloaded directory from verified local objects? The current directory, including any files added by an application, will be preserved in staging."
            }
            Confirmation::RepairInstallation => {
                "Restore original installation files from the verified download? Modified files, mods and other additions in the current working copy will be moved to a managed backup in staging. Managed user data stays. This may require space for another working copy."
            }
            Confirmation::Exit => {
                "Downloads are active. Closing interrupts them; incomplete jobs can be resumed after restart."
            }
        };
        egui::Modal::new(egui::Id::unique("confirm-action")).show(ctx, |ui| {
            ui.set_width(400.0);
            let selected = match kind {
                Confirmation::RemoveInstallation => self
                    .remove_target
                    .as_ref()
                    .map(|(_, _, name)| name.as_str()),
                Confirmation::RemoveArtifact => self
                    .remove_artifact_target
                    .as_ref()
                    .map(|(_, _, name)| name.as_str()),
                Confirmation::RemoveApp => self
                    .remove_app_target
                    .as_ref()
                    .map(|(_, _, name, _)| name.as_str()),
                Confirmation::RepairInstallation => self.repair_installation_target.as_ref().map(|(_,name)|name.as_str()),
                _ => None,
            };
            if let Some(name) = selected {
                ui.label(RichText::new(name).strong());
                ui.add_space(6.0);
            }
            ui.label(text);
            if kind == Confirmation::RemoveInstallation
                && let Some((id, complete, _)) = &self.remove_target
                && let Some(item) = self.snapshot.installations.iter().find(|item| &item.id == id)
            {
                ui.add_space(6.0);
                ui.label(format!("Working copy: {}", item.path.display()));
                if *complete {
                    ui.label(format!("Managed data: {}", item.userdata.display()));
                    ui.label("This Association's verified download and jobs will also be removed.");
                }
            }
            if kind == Confirmation::RemoveApp
                && let Some((profile, app, _, complete)) = &self.remove_app_target
            {
                let copies = self.snapshot.installations.iter().filter(|item| {
                    &item.profile_id == profile && &item.app_id == app && item.state != "Removed"
                }).count();
                ui.add_space(6.0);
                ui.label(format!("Writable copies in scope: {copies}"));
                if *complete {
                    ui.label("Every locally saved Association of this App is in scope, including managed data. Shared immutable objects stay while another local item uses them.");
                }
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Keep").clicked() {
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
                                    allow_http: self.edit_allow_http,
                                });
                                self.edit_url.clear();
                                self.edit_allow_http = false;
                            }
                            Confirmation::RemoveToken => self.send(Command::RemoveToken(id)),
                            Confirmation::RemoveInstallation => {
                                if let Some((installation, complete, _)) = self.remove_target.take()
                                {
                                    self.local_operations.insert(installation.clone(),"Removing working data".into());
                                    self.send(Command::RemoveInstallation {
                                        id: installation,
                                        complete,
                                    });
                                }
                            }
                            Confirmation::RemoveArtifact => {
                                if let Some((profile, association, _)) =
                                    self.remove_artifact_target.take()
                                {
                                    self.local_operations.insert(association.clone(),"Removing local download".into());
                                    self.send(Command::RemoveArtifact {
                                        profile,
                                        association,
                                    });
                                }
                            }
                            Confirmation::RemoveApp => {
                                if let Some((profile, app, _, complete)) =
                                    self.remove_app_target.take()
                                {
                                    self.local_operations.insert(app.clone(),"Removing application items".into());
                                    self.send(Command::RemoveApp {
                                        profile,
                                        app,
                                        complete,
                                    });
                                }
                            }
                            Confirmation::RepairSource => {
                                if let Some((profile, association)) = self.repair_target.take() {
                                    self.send(Command::RepairSource {
                                        profile,
                                        association,
                                    });
                                }
                            }
                            Confirmation::RepairInstallation => {
                                if let Some((installation,_))=self.repair_installation_target.take(){
                                    self.local_operations.insert(installation.clone(),"Preserving modified installation and restoring original files".into());
                                    self.send(Command::RepairInstallation(installation));
                                }
                            }
                            Confirmation::Exit => {
                                self.exit_allowed = true;
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        }
                    } else if kind == Confirmation::Exit {
                        self.exit_allowed = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if kind == Confirmation::RemoveInstallation {
                        self.remove_target = None;
                    }
                    if kind == Confirmation::RemoveArtifact {
                        self.remove_artifact_target = None;
                    }
                    if kind == Confirmation::RemoveApp {
                        self.remove_app_target = None;
                    }
                    if kind == Confirmation::RepairSource {
                        self.repair_target = None;
                    }
                    if kind == Confirmation::RepairInstallation {self.repair_installation_target=None;}
                    self.confirmation = None;
                }
            });
        });
    }
}
fn section(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    let width = ui.available_width().min(900.0);
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .corner_radius(12)
        .inner_margin(egui::Margin::same(17))
        .show(ui, |ui| {
            ui.set_width((width - 34.0).max(0.0));
            ui.label(RichText::new(title).size(18.0).strong());
            ui.add_space(9.0);
            body(ui);
        });
}
fn status_line(ui: &mut egui::Ui, p: &app_depot_core::library::Profile, syncing: bool) {
    let (label, color) = if p.archived {
        ("Saved offline", Color32::GRAY)
    } else if syncing {
        ("Checking for updates…", Color32::from_rgb(100, 150, 220))
    } else {
        match p.connection_status.as_str() {
            "connected" => ("Up to date", Color32::from_rgb(80, 170, 125)),
            "authentication-required" => ("Sign-in required", Color32::from_rgb(210, 115, 95)),
            "forbidden" => ("Access forbidden", Color32::from_rgb(210, 115, 95)),
            "incompatible" => ("Incompatible server", Color32::from_rgb(210, 115, 95)),
            "protocol-error" => ("Invalid server response", Color32::from_rgb(210, 115, 95)),
            "unreachable" => ("Offline · saved catalog", Color32::from_rgb(205, 145, 65)),
            "never" => ("Not connected yet", Color32::GRAY),
            _ => ("Unable to refresh", Color32::from_rgb(205, 145, 65)),
        }
    };
    ui.label(RichText::new(label).small().color(color));
}
pub(super) fn format_time(value: Option<i64>) -> String {
    let Some(at) = value else {
        return "never".into();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    let age = now.saturating_sub(at) / 1000;
    if age < 60 {
        "just now".into()
    } else if age < 3600 {
        format!("{} min ago", age / 60)
    } else if age < 86400 {
        format!("{} h ago", age / 3600)
    } else {
        format!("{} d ago", age / 86400)
    }
}
fn format_bytes(bytes: u64) -> String {
    let mut amount = bytes as f64;
    let mut unit = "B";
    for next in ["KiB", "MiB", "GiB", "TiB"] {
        if amount < 1024.0 {
            break;
        }
        amount /= 1024.0;
        unit = next;
    }
    if unit == "B" {
        format!("{bytes} B")
    } else {
        format!("{amount:.1} {unit}")
    }
}
fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 60 {
        format!("{} s", seconds.max(1))
    } else if seconds < 3600 {
        format!("{} min", (seconds + 30) / 60)
    } else {
        format!("{} h {} min", seconds / 3600, (seconds % 3600) / 60)
    }
}
fn phase_label(phase: TransferPhase, state: &str) -> &'static str {
    if state == "Completed" {
        return "Downloaded and verified";
    }
    match phase {
        TransferPhase::PreparingInventory => "Preparing file list",
        TransferPhase::WaitingForServer => "Waiting for server",
        TransferPhase::Downloading => "Downloading",
        TransferPhase::Verifying => "Verifying content",
        TransferPhase::PreparingLocalFiles => "Preparing local files",
        TransferPhase::SavingVerifiedContent => "Saving verified content",
    }
}
fn friendly_download_error(error: &str) -> &'static str {
    let lower = error.to_ascii_lowercase();
    if lower.contains("sha-256") || lower.contains("identity mismatch") || lower.contains("digest")
    {
        "Integrity check failed. Retry the download; the saved bytes were not accepted."
    } else if lower.contains("insufficient") && lower.contains("space") {
        "Not enough free space in the content library."
    } else if lower.contains("401") || lower.contains("unauthorized") {
        "Sign in again with a valid server token."
    } else if lower.contains("403") || lower.contains("forbidden") {
        "This account cannot access the artifact."
    } else if lower.contains("429")
        || lower.contains("503")
        || lower.contains("502")
        || lower.contains("504")
    {
        "The server is temporarily unavailable. Try again later."
    } else if lower.contains("transport")
        || lower.contains("connect")
        || lower.contains("timed out")
    {
        "Connection interrupted. Retry when the server is reachable."
    } else if lower.contains("unsafe")
        || lower.contains("unsupported")
        || lower.contains("invalid tree")
    {
        "This artifact cannot be saved safely on this filesystem."
    } else {
        "Download failed. Open Technical details for the diagnostic message."
    }
}
fn truncate(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}
pub(super) fn friendly_job_state(state: &str) -> &str {
    match state {
        "Queued" => "Waiting",
        "FetchingInventory" => "Reading file list",
        "Downloading" => "Downloading",
        "Verifying" => "Verifying",
        "Committing" => "Saving verified content",
        "Completed" => "Downloaded and verified",
        "Paused" => "Paused",
        "Failed" => "Needs attention",
        "Cancelled" => "Cancelled",
        _ => "Status unavailable",
    }
}

pub(super) fn catalog_item_visible(stale: bool, current_catalog: bool) -> bool {
    !stale || !current_catalog
}
fn copy_line(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(label).small().weak());
        ui.label(RichText::new(value).small().monospace());
        if ui.small_button("Copy").clicked() {
            ui.ctx().copy_text(value.to_owned());
        }
    });
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
        if ui.small_button("Copy JSON").clicked() {
            ui.ctx().copy_text(text);
        }
    });
}
pub(super) fn job_actions(job: &DownloadJob) -> Vec<(&'static str, Command)> {
    match job.state.as_str() {
        "Queued" => vec![
            ("Download first", Command::MoveToTop(job.id.clone())),
            ("Pause", Command::Pause(job.id.clone())),
            ("Cancel", Command::Cancel(job.id.clone())),
        ],
        "FetchingInventory" | "Downloading" => vec![
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
fn open_account_checked(ctx: &egui::Context, url: &str, allow_http: bool) -> Result<(), String> {
    let origin = ApiClient::new(url, None, allow_http)
        .map_err(|e| e.to_string())?
        .origin()
        .to_owned();
    ctx.open_url(egui::OpenUrl::new_tab(format!("{origin}account")));
    Ok(())
}
fn open_account(ctx: &egui::Context, url: &str) {
    let _ = open_account_checked(ctx, url, url.starts_with("http://"));
}
impl eframe::App for Desktop {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        if let Some(id) = self.selected_job.clone()
            && self.last_tree_details_refresh.elapsed() >= Duration::from_secs(1)
            && self
                .snapshot
                .jobs
                .iter()
                .any(|job| job.id == id && job.state == "Downloading")
        {
            self.send(Command::TreeFiles(id));
            self.last_tree_details_refresh = Instant::now();
        }
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(false));
        if focused && !self.last_focus && self.last_activation.elapsed() > Duration::from_secs(600)
        {
            if let Some(id) = self.active.clone() {
                self.send(Command::Refresh(Some(id)));
            }
            self.last_activation = Instant::now();
        }
        self.last_focus = focused;
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
        ctx.request_repaint_after(Duration::from_secs(1));
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let available = ui.available_size();
        egui::Frame::new()
            .fill(ui.visuals().panel_fill)
            .inner_margin(egui::Margin::same(16))
            .show(ui, |ui| {
                ui.set_min_size(available - egui::vec2(32.0, 32.0));
                if ui.available_width() < 700.0 {
                    self.top_navigation(ui);
                    self.content(ui);
                } else {
                    let height = ui.available_height();
                    ui.horizontal_top(|ui| {
                        ui.allocate_ui_with_layout(
                            egui::vec2(205.0, height),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| self.sidebar(ui),
                        );
                        ui.separator();
                        ui.add_space(8.0);
                        let available = ui.available_width();
                        let width = available.min(1560.0);
                        ui.add_space(((available - width) / 2.0).max(0.0));
                        ui.allocate_ui_with_layout(
                            egui::vec2(width, height),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| self.content(ui),
                        );
                    });
                }
            });
        self.setup_dialog(ui.ctx());
        self.confirmation(ui.ctx());
    }
}
impl Desktop {
    fn content(&mut self, ui: &mut egui::Ui) {
        if let Some((message, error, at)) = self.message.clone() {
            if !error && at.elapsed() > Duration::from_secs(8) {
                self.message = None;
            } else {
                egui::Frame::new()
                    .fill(if error {
                        Color32::from_rgb(80, 36, 36)
                    } else {
                        ui.visuals().faint_bg_color
                    })
                    .corner_radius(8)
                    .inner_margin(egui::Margin::same(9))
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(message);
                            if ui.small_button("Dismiss").clicked() {
                                self.message = None;
                            }
                        });
                    });
                ui.add_space(9.0);
            }
        }
        match self.screen {
            Screen::Catalog => self.catalog(ui),
            Screen::Library => self.library(ui),
            Screen::Downloads => self.downloads(ui),
            Screen::Settings => self.settings(ui),
        }
    }
}
