use super::*;
use crate::design;
use app_depot_core::launch::LaunchCandidate;
use app_depot_core::transfer::TransferPhase;

type LibraryItem = (
    &'static str,
    String,
    String,
    PathBuf,
    Option<u64>,
    Option<usize>,
);

impl Desktop {
    #[cfg(any(target_os = "linux", windows))]
    fn updates_settings(&mut self, ui: &mut egui::Ui) {
        section(ui, "App Depot updates", |ui| {
            ui.label(
                RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION")))
                    .size(18.0)
                    .strong(),
            );
            ui.label(format!(
                "{} build · revision {}",
                env!("APP_DEPOT_BUILD_KIND"),
                env!("APP_DEPOT_SOURCE_REVISION")
            ));
            ui.add_space(8.0);
            match &self.updater.mode {
                #[cfg(target_os = "linux")]
                updater::InstallMode::OfficialAppImage { .. } => {
                    ui.label("Official AppImage · stable GitHub releases");
                }
                #[cfg(target_os = "linux")]
                updater::InstallMode::Development => {
                    ui.label("Development build. Automatic installation is disabled.");
                }
                #[cfg(target_os = "linux")]
                updater::InstallMode::Unsupported(reason) => {
                    ui.label(reason);
                }
                #[cfg(windows)]
                updater::InstallMode::WindowsStable => {
                    ui.label("Windows x64 · stable GitHub releases");
                }
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        self.updater.can_check(),
                        egui::Button::new("Check for updates"),
                    )
                    .clicked()
                {
                    self.updater.check(ui.ctx());
                }
                if ui.button("Open official releases").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(
                        "https://github.com/viktorliho/app-depot-client/releases",
                    ));
                }
            });
            ui.label(&self.updater.status);
            if let Some(error) = &self.updater.last_error
                && error != &self.updater.status
            {
                ui.colored_label(
                    design::palette(ui).error,
                    format!("Last update error: {error}"),
                );
            }
            if self.updater.last_error.is_some()
                && ui.small_button("Dismiss previous error").clicked()
            {
                self.updater.last_error = None;
            }
            if let Some((done, total)) = self.updater.progress
                && total > 0
            {
                ui.add(
                    egui::ProgressBar::new(done as f32 / total as f32).text(format!(
                        "{} of {}",
                        format_bytes(done),
                        format_bytes(total)
                    )),
                );
            }
            #[cfg(windows)]
            if let Some(cancel) = &self.updater.download_cancel
                && ui.button("Cancel download").clicked()
            {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            if let Some(release) = &self.updater.release {
                let tag = release.tag.clone();
                let notes = release.notes.clone();
                ui.add_space(14.0);
                ui.label(RichText::new(format!("Available: {tag}")).strong());
                #[cfg(windows)]
                ui.label(format!(
                    "Installer size: {}",
                    format_bytes(release.image_size)
                ));
                #[cfg(windows)]
                ui.collapsing("Installer integrity", |ui| {
                    copy_line(ui, "Published SHA-256", &release.sha256);
                });
                if !notes.is_empty() {
                    egui::CollapsingHeader::new("Release notes").show(ui, |ui| {
                        ui.label(notes);
                    });
                }
                #[cfg(target_os = "linux")]
                if self.updater.staged.is_none()
                    && matches!(
                        self.updater.mode,
                        updater::InstallMode::OfficialAppImage { .. }
                    )
                    && ui
                        .add_enabled(!self.updater.busy, egui::Button::new("Download update"))
                        .clicked()
                {
                    self.updater.download(ui.ctx());
                }
                #[cfg(windows)]
                if self.updater.staged.is_none()
                    && ui
                        .add_enabled(
                            !self.updater.busy,
                            egui::Button::new("Download verified installer"),
                        )
                        .clicked()
                {
                    self.updater.download(ui.ctx());
                }
                if !self.updater.dismissed && ui.button("Later").clicked() {
                    self.updater.dismissed = true;
                }
            }
            #[cfg(target_os = "linux")]
            if self.updater.staged.is_some() {
                let active_transfer = self.snapshot.jobs.iter().any(|job| {
                    matches!(
                        job.state.as_str(),
                        "Queued" | "FetchingInventory" | "Downloading" | "Verifying" | "Committing"
                    )
                });
                let active_launch = self.launch_states.values().any(|(state, _)| {
                    matches!(state.as_str(), "Starting" | "Preparing" | "Running")
                });
                let unsafe_operation =
                    active_transfer || active_launch || !self.local_operations.is_empty();
                if unsafe_operation {
                    ui.label("Finish or pause queued downloads, and finish local operations and running programs before restarting App Depot.");
                }
                if ui
                    .add_enabled(!unsafe_operation, egui::Button::new("Update and restart"))
                    .clicked()
                {
                    match self.updater.handoff() {
                        Ok(()) => {
                            self.exit_allowed = true;
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        Err(error) => self.updater.status = error,
                    }
                }
            }
            ui.add_space(10.0);
            #[cfg(target_os = "linux")]
            ui.label(RichText::new("Updates use the official GitHub release, verify its SHA-256 and keep the previous AppImage for recovery. Your library and credentials remain outside the AppImage.").color(design::palette(ui).muted));
            #[cfg(windows)]
            {
                if self.updater.staged.is_some() {
                    let busy = self.snapshot.update_occupancy_clear != Some(true)
                        || self.launch_states.values().any(|(state, _)| {
                            matches!(
                                state.as_str(),
                                "Starting" | "Preparing" | "Running" | "ProcessUnknown"
                            )
                        })
                        || self.snapshot.jobs.iter().any(|job| {
                            matches!(
                                job.state.as_str(),
                                "Queued"
                                    | "FetchingInventory"
                                    | "Downloading"
                                    | "Verifying"
                                    | "Committing"
                            )
                        })
                        || !self.local_operations.is_empty();
                    if busy {
                        ui.label("Wait for managed applications and local operations to finish before installing.");
                    }
                    if ui.button("Discard downloaded installer").clicked() {
                        self.updater.staged = None;
                        self.updater.confirm_windows_update = false;
                        self.updater.status = "Installer discarded".into();
                    } else if self.updater.confirm_windows_update {
                        ui.label("App Depot will close. Complete the Setup wizard to reopen it. Your library stays in place.");
                        ui.horizontal(|ui| {
                            if ui.button("Keep App Depot open").clicked() {
                                self.updater.confirm_windows_update = false;
                            } else if ui
                                .add_enabled(!busy, egui::Button::new("Close and start Setup"))
                                .clicked()
                            {
                                match self.updater.handoff() {
                                    Ok(()) => {
                                        self.exit_allowed = true;
                                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                                    }
                                    Err(error) => {
                                        self.updater.last_error = Some(error.clone());
                                        self.updater.status = error;
                                    }
                                }
                            }
                        });
                    } else if ui
                        .add_enabled(!busy, egui::Button::new("Install update…"))
                        .clicked()
                    {
                        self.updater.confirm_windows_update = true;
                    }
                }
            }
        });
    }
    fn titlebar(&mut self, ui: &mut egui::Ui) {
        let p = design::palette(ui);
        let maximized = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
        egui::Frame::new()
            .fill(p.sidebar)
            .inner_margin(egui::Margin::symmetric(16, 6))
            .show(ui, |ui| {
                ui.set_min_height(42.0);
                ui.horizontal_centered(|ui| {
                    let controls = 3.0 * 42.0 + 28.0;
                    let drag_width = (ui.available_width() - controls).max(80.0);
                    let (rect, response) = ui.allocate_exact_size(
                        egui::vec2(drag_width, 38.0),
                        egui::Sense::click_and_drag(),
                    );
                    ui.painter().image(
                        self.app_icon.id(),
                        egui::Rect::from_center_size(
                            egui::pos2(rect.left() + 15.0, rect.center().y),
                            egui::vec2(28.0, 28.0),
                        ),
                        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                    ui.painter().text(
                        egui::pos2(rect.left() + 40.0, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        "App Depot",
                        egui::FontId::proportional(16.0),
                        p.text,
                    );
                    if let Some(profile) = self.profile() {
                        ui.painter().text(
                            egui::pos2(rect.left() + 158.0, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            format!("/  {}", truncate(&profile.name, 32)),
                            egui::FontId::proportional(14.0),
                            p.muted,
                        );
                    }
                    if response.double_clicked() {
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                    } else if response.drag_started()
                        || (response.hovered() && ui.input(|i| i.pointer.primary_pressed()))
                    {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    response.on_hover_cursor(egui::CursorIcon::Grab);
                    for index in 0..3 {
                        let hint = match index {
                            0 => "Minimize",
                            1 if maximized => "Restore",
                            1 => "Maximize",
                            _ => "Close",
                        };
                        let response = ui
                            .add_sized(
                                [42.0, 36.0],
                                egui::Button::new("")
                                    .fill(p.sidebar)
                                    .stroke(egui::Stroke::NONE),
                            )
                            .on_hover_text(hint);
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::Role::Button, true, hint)
                        });
                        let rect = response.rect;
                        let ink = if index == 2 && response.hovered() {
                            p.error
                        } else {
                            p.text
                        };
                        if response.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                8.0,
                                if index == 2 {
                                    p.error.gamma_multiply(0.15)
                                } else {
                                    p.elevated
                                },
                            );
                        }
                        let center = rect.center();
                        let stroke = egui::Stroke::new(1.7, ink);
                        match index {
                            0 => {
                                ui.painter().hline(
                                    (center.x - 7.0)..=(center.x + 7.0),
                                    center.y + 5.0,
                                    stroke,
                                );
                            }
                            1 => {
                                let box_rect =
                                    egui::Rect::from_center_size(center, egui::vec2(13.0, 11.0));
                                ui.painter().rect_stroke(
                                    box_rect,
                                    1.0,
                                    stroke,
                                    egui::StrokeKind::Inside,
                                );
                            }
                            _ => {
                                ui.painter().line_segment(
                                    [
                                        center + egui::vec2(-6.0, -6.0),
                                        center + egui::vec2(6.0, 6.0),
                                    ],
                                    stroke,
                                );
                                ui.painter().line_segment(
                                    [
                                        center + egui::vec2(6.0, -6.0),
                                        center + egui::vec2(-6.0, 6.0),
                                    ],
                                    stroke,
                                );
                            }
                        }
                        if response.clicked() {
                            ui.ctx()
                                .send_viewport_cmd(titlebar_command(index, maximized));
                        }
                    }
                });
            });
        ui.painter().hline(
            ui.min_rect().x_range(),
            ui.cursor().top(),
            egui::Stroke::new(1.0, p.border),
        );
    }

    fn resize_edges(&self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
            return;
        }
        let rect = ctx.content_rect();
        let (pointer, pressed) =
            ctx.input(|i| (i.pointer.hover_pos(), i.pointer.primary_pressed()));
        if let Some(direction) = pointer.and_then(|pos| resize_direction(rect, pos)) {
            ctx.set_cursor_icon(resize_cursor(direction));
            if pressed {
                ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(direction));
            }
        }
    }
    fn header(&self, ui: &mut egui::Ui, title: &str, subtitle: &str) {
        let p = design::palette(ui);
        ui.label(RichText::new(title).size(30.0).strong().color(p.text));
        ui.label(RichText::new(subtitle).size(15.0).color(p.muted));
        ui.add_space(22.0);
    }
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let p = design::palette(ui);
        egui::Frame::new()
            .fill(p.sidebar)
            .inner_margin(egui::Margin::symmetric(15, 20))
            .show(ui, |ui| {
                ui.set_width(206.0);
                ui.set_min_height((ui.available_height() - 40.0).max(0.0));
                ui.label(design::eyebrow("YOUR SPACE", p));
                ui.add_space(14.0);
                let active_jobs = self
                    .snapshot
                    .jobs
                    .iter()
                    .filter(|j| {
                        matches!(
                            j.state.as_str(),
                            "Queued"
                                | "FetchingInventory"
                                | "Downloading"
                                | "Verifying"
                                | "Committing"
                        )
                    })
                    .count();
                for (screen, label) in [
                    (Screen::Catalog, "Browse"),
                    (Screen::Library, "My library"),
                    (Screen::Downloads, "Downloads"),
                    (Screen::Settings, "Settings"),
                ] {
                    let selected = self.screen == screen;
                    let count = if screen == Screen::Downloads {
                        active_jobs
                    } else {
                        0
                    };
                    if navigation_item(ui, screen, label, selected, count) {
                        self.screen = screen;
                        if screen == Screen::Catalog {
                            self.selected_app = None;
                        }
                    }
                    ui.add_space(5.0);
                }
                #[cfg(any(target_os = "linux", windows))]
                {
                    let update_label = if self.updater.last_error.is_some() {
                        Some("Update needs attention".to_owned())
                    } else if !self.updater.dismissed {
                        self.updater
                            .release
                            .as_ref()
                            .map(|release| format!("Update available · {}", release.tag))
                    } else {
                        None
                    };
                    if let Some(label) = update_label {
                        ui.add_space(6.0);
                        if ui.small_button(label).clicked() {
                            self.screen = Screen::Settings;
                            self.settings_tab = SettingsTab::Updates;
                        }
                    }
                }
                ui.add_space((ui.available_height() - 230.0).max(18.0));
                ui.separator();
                ui.add_space(12.0);
                ui.label(design::eyebrow("SERVER", p));
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
                            RichText::new("HTTP · unencrypted").color(design::palette(ui).warning),
                        )
                        .on_hover_text(
                            "This connection is unencrypted. Bearer tokens are sent without TLS.",
                        );
                    }
                }
                ui.add_space(5.0);
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
        ui.add_space(8.0);
        ui.separator();
    }
    fn catalog(&mut self, ui: &mut egui::Ui) {
        if let Some(id) = self.selected_app.clone() {
            self.details(ui, &id);
            return;
        }
        let compact = ui.available_width() < 800.0;
        if compact {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| self.catalog_contents(ui, true));
        } else {
            self.catalog_contents(ui, false);
        }
    }
    fn catalog_contents(&mut self, ui: &mut egui::Ui, compact: bool) {
        if compact {
            ui.label(RichText::new("Browse").size(27.0).strong());
            ui.add_space(10.0);
        } else {
            self.header(
                ui,
                "Browse",
                "Software available from your selected server.",
            );
        }
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
        ui.add_space(if compact { 9.0 } else { 18.0 });
        ui.horizontal_wrapped(|ui| {
            let search = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("Search your catalog…")
                    .desired_width(ui.available_width().min(350.0)),
            );
            if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::F)) {
                search.request_focus();
            }
            if search.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.search.clear();
                search.surrender_focus();
            }
            ui.add_space(6.0);
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
        ui.add_space(if compact { 9.0 } else { 18.0 });
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
        let width = ui.max_rect().width().min(1400.0);
        let capacity = if self.prefs.grid {
            ((width + 14.0) / 300.0).floor().max(1.0) as usize
        } else {
            1
        };
        let columns = capacity.min(apps.len().max(1));
        let card_width = ((width - 14.0 * (columns.saturating_sub(1)) as f32) / columns as f32)
            .clamp(220.0, 430.0);
        let row_height = if self.prefs.grid { 310.0 } else { 84.0 };
        let render_rows = |ui: &mut egui::Ui, range: std::ops::Range<usize>, this: &mut Self| {
            for row in range {
                ui.horizontal(|ui| {
                    for view in apps.iter().skip(row * columns).take(columns) {
                        if this.prefs.grid {
                            this.app_card(ui, view, card_width);
                        } else {
                            this.app_row(ui, view, width);
                        }
                    }
                });
                ui.add_space(5.0);
            }
        };
        if compact {
            render_rows(ui, 0..apps.len().div_ceil(columns), self);
        } else {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show_rows(ui, row_height, apps.len().div_ceil(columns), |ui, range| {
                    render_rows(ui, range, self);
                });
        }
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
                        if ui.add_sized([190.0, 44.0], design::primary("Add Server",design::palette(ui))).clicked() {
                            self.begin_setup();
                        }
                        ui.add_space(28.0);
                        ui.separator();
                        ui.add_space(12.0);
                        let summary = if worker::WINDOWS_INSTALL_ONLY {
                            "Connect a server   ·   Browse its catalog"
                        } else {
                            "Connect a server   ·   Browse its catalog   ·   Download verified artifacts"
                        };
                        ui.label(RichText::new(summary).size(12.0).weak());
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
                .and_then(|c| {
                    c.summary
                        .or_else(|| c.description.map(|d| truncate(&d, 145)))
                })
        })
    }
    fn app_card(&mut self, ui: &mut egui::Ui, view: &AppView, width: f32) {
        let p = design::palette(ui);
        let response = egui::Frame::new()
            .fill(p.surface)
            .corner_radius(13)
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.set_width(width - 24.0);
                    ui.set_min_height(286.0);
                    self.request_art(
                        ui,
                        &view.app.id,
                        ArtUse::Grid,
                        egui::vec2(width - 24.0, 170.0),
                        &view.app.name,
                    );
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new(truncate(&view.app.name, 42))
                            .size(19.0)
                            .strong(),
                    )
                    .on_hover_text(&view.app.name);
                    ui.label(
                        RichText::new(truncate(
                            &Self::app_summary(view).unwrap_or_else(|| {
                                if view.releases.len() == 1 {
                                    "1 release".into()
                                } else {
                                    format!("{} releases", view.releases.len())
                                }
                            }),
                            68,
                        ))
                        .size(14.0)
                        .color(p.muted),
                    );
                    let verified = view
                        .releases
                        .iter()
                        .flat_map(|r| &r.associations)
                        .any(|a| self.is_verified(&a.association.id));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if verified {
                            ui.label(RichText::new("In your library").size(14.0).color(p.good));
                        } else if view.stale {
                            ui.label(RichText::new("Saved catalog").size(14.0).color(p.muted));
                        } else {
                            ui.label(RichText::new("View releases").size(14.0).color(p.accent));
                        }
                    });
                });
            })
            .response;
        let interaction = ui
            .interact(
                response.rect,
                ui.scope_id().with(&view.app.id),
                egui::Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        interaction
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &view.app.name));
        interaction.context_menu(|ui| {
            for release in view
                .releases
                .iter()
                .filter(|r| crate::manifest_ui::declared(r))
            {
                self.manifest_action(ui, release, &view.app.name);
            }
            if ui.button("View details").clicked() {
                self.selected_app = Some(view.app.id.clone());
                ui.close();
            }
        });
        if interaction.hovered() {
            ui.painter().rect_stroke(
                response.rect,
                12.0,
                egui::Stroke::new(2.0, p.accent),
                egui::StrokeKind::Inside,
            );
        }
        if interaction.clicked()
            || (interaction.has_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Space)))
        {
            self.selected_app = Some(view.app.id.clone());
        }
        self.manifest_browse_action(ui, view);
    }
    fn app_row(&mut self, ui: &mut egui::Ui, view: &AppView, row_width: f32) {
        let p = design::palette(ui);
        let verified = view
            .releases
            .iter()
            .flat_map(|release| &release.associations)
            .any(|item| self.is_verified(&item.association.id));
        let status = if view.releases.iter().any(crate::manifest_ui::declared) {
            "Installation available"
        } else if verified {
            "In your library"
        } else if view.stale {
            "Saved catalog"
        } else {
            "View releases"
        };
        let summary =
            Self::app_summary(view).unwrap_or_else(|| format!("{} releases", view.releases.len()));
        let response = egui::Frame::new()
            .fill(p.surface)
            .corner_radius(11)
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width((row_width - 24.0).max(0.0));
                let inner = (row_width - 24.0).max(0.0);
                let compact = inner < 500.0;
                let art = if compact {
                    egui::vec2(58.0, 58.0)
                } else {
                    egui::vec2(72.0, 64.0)
                };
                let trailing = if compact { 0.0 } else { 166.0 };
                let text_width = (inner - art.x - trailing - 20.0).max(100.0);
                ui.horizontal(|ui| {
                    self.request_art(ui, &view.app.id, ArtUse::Grid, art, &view.app.name);
                    ui.add_space(8.0);
                    ui.allocate_ui_with_layout(
                        egui::vec2(text_width, art.y),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_min_width(text_width);
                            ui.label(
                                RichText::new(truncate(
                                    &view.app.name,
                                    if compact { 34 } else { 64 },
                                ))
                                .size(17.0)
                                .strong(),
                            )
                            .on_hover_text(&view.app.name);
                            ui.label(
                                RichText::new(truncate(&summary, if compact { 46 } else { 95 }))
                                    .size(14.0)
                                    .color(p.muted),
                            )
                            .on_hover_text(&summary);
                            if compact {
                                ui.label(RichText::new(status).size(13.0).color(if verified {
                                    p.good
                                } else {
                                    p.accent
                                }));
                            }
                        },
                    );
                    if !compact {
                        ui.allocate_ui_with_layout(
                            egui::vec2(trailing, art.y),
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.label(RichText::new(status).size(14.0).color(if verified {
                                    p.good
                                } else {
                                    p.accent
                                }));
                            },
                        );
                    }
                });
            })
            .response;
        let interaction = ui
            .interact(
                response.rect,
                ui.scope_id().with(("row", &view.app.id)),
                egui::Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        interaction
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &view.app.name));
        if interaction.hovered() {
            ui.painter().rect_stroke(
                response.rect,
                11.0,
                egui::Stroke::new(2.0, p.accent),
                egui::StrokeKind::Inside,
            );
        }
        interaction.context_menu(|ui| {
            for release in view
                .releases
                .iter()
                .filter(|r| crate::manifest_ui::declared(r))
            {
                self.manifest_action(ui, release, &view.app.name);
            }

            if ui.button("View details").clicked() {
                self.selected_app = Some(view.app.id.clone());
                ui.close();
            }
        });
        if interaction.clicked()
            || (interaction.has_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Space)))
        {
            self.selected_app = Some(view.app.id.clone());
        }
        self.manifest_browse_action(ui, view);
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
        if let Some(process) = choices
            .iter()
            .find(|candidate| matches!(candidate.status.as_str(), "Running" | "ProcessUnknown"))
        {
            ui.label(&process.reason);
            return;
        }
        let has_playable = choices.iter().any(|c| launch_is_playable(&c.status));
        let unavailable: Vec<_> = choices
            .iter()
            .filter(|c| c.status != "NoLaunch" && !launch_is_playable(&c.status))
            .collect();
        for candidate in choices.iter().filter(|c| launch_is_playable(&c.status)) {
            let key = format!("{}:{}", association, candidate.entry_id);
            let state = self.launch_states.get(&key).cloned();
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
                        design::primary(
                            if multiple {
                                format!("Play: {label}")
                            } else {
                                "Play".into()
                            },
                            design::palette(ui),
                        ),
                    )
                    .clicked()
                {
                    self.start_candidate(candidate);
                }
                if let Some(label) = state.as_ref().map(|(s, _)| s.as_str()).or_else(|| {
                    (candidate.status == "PreparationRequired").then_some("Prepares on Play")
                }) {
                    ui.label(
                        RichText::new(label)
                            .size(14.0)
                            .color(design::palette(ui).muted),
                    );
                }
            });
            if let Some((state, message)) = &state
                && state != "Starting"
            {
                ui.label(RichText::new(message).small());
            }
        }
        if !unavailable.is_empty() {
            let show = |ui: &mut egui::Ui| {
                for candidate in &unavailable {
                    ui.label(format!(
                        "{}: {}",
                        candidate.label.as_deref().unwrap_or(&candidate.entry_id),
                        candidate.reason
                    ));
                }
            };
            if has_playable {
                egui::CollapsingHeader::new("Other launch options")
                    .id_salt(("other-launch", association))
                    .show(ui, show);
            } else {
                ui.label(
                    RichText::new("No compatible Play option on this device")
                        .color(design::palette(ui).muted),
                );
                egui::CollapsingHeader::new("Why?")
                    .id_salt(("unavailable-launch", association))
                    .show(ui, show);
            }
        }
    }
    fn details(&mut self, ui: &mut egui::Ui, id: &str) {
        let Some(view) = self.snapshot.apps.iter().find(|v| v.app.id == id).cloned() else {
            self.selected_app = None;
            return;
        };
        if ui.button("Back to catalog").clicked()
            || (self.setup == Setup::Closed
                && self.confirmation.is_none()
                && ui.input(|i| i.key_pressed(egui::Key::Escape)))
        {
            self.selected_app = None;
            return;
        }
        ui.add_space(8.0);
        egui::ScrollArea::vertical().auto_shrink([false,false]).show(ui,|ui| {
            let palette = design::palette(ui);
            let info = view.releases.iter().find_map(|r| r.declaration.as_ref().and_then(|s| s.app.convention::<AppConvention>().ok().flatten()));
            let hero_width=ui.available_width().min(1050.0);
            let narrow=hero_width < 650.0;
            let art_width=if narrow {(hero_width-44.0).min(300.0)} else {260.0};
            let art_height=if narrow {174.0} else {190.0};
            egui::Frame::new().fill(palette.surface).corner_radius(14).inner_margin(egui::Margin::same(22)).show(ui, |ui| {
                ui.set_width((hero_width-44.0).max(0.0));
                ui.with_layout(if narrow {egui::Layout::top_down(egui::Align::Min)} else {egui::Layout::left_to_right(egui::Align::Min)},|ui| {
                    self.request_art(ui,id,ArtUse::Detail,egui::vec2(art_width,art_height),&view.app.name);
                    ui.add_space(12.0);
                    ui.vertical(|ui| {
                        ui.set_max_width(if narrow {hero_width-44.0} else {(hero_width-44.0-art_width-12.0).max(220.0)});
                        ui.label(design::eyebrow("APPLICATION",palette));
                        ui.label(RichText::new(&view.app.name).size(if hero_width < 720.0 { 26.0 } else { 32.0 }).strong());
                        if let Some(developer)=info.as_ref().and_then(|c|c.developer.as_deref()) {
                            ui.label(RichText::new(format!("By {developer}")).size(15.0).color(palette.muted));
                        }
                        ui.add_space(8.0);
                        if let Some(summary)=Self::app_summary(&view) {
                            ui.label(RichText::new(summary).size(17.0).color(palette.text));
                        }
                        ui.add_space(14.0);
                        let mut candidates=self.snapshot.launches.iter().filter(|c|view.releases.iter().any(|r|r.associations.iter().any(|item|item.association.id==c.association_id)));
                        let ready:Vec<_>=candidates.clone().filter(|c|launch_is_playable(&c.status)).cloned().collect();
                        let action = primary_action(
                            ready.len(),
                            view.releases.len(),
                            view.releases.iter().map(|r| r.associations.len()).sum(),
                            view.stale,
                        );
                        let busy=ready.first().is_some_and(|c|self.launch_states.get(&format!("{}:{}",c.association_id,c.entry_id)).is_some_and(|(s,_)|matches!(s.as_str(),"Starting"|"Preparing"|"Running")));
                        if action==PrimaryAction::Play {
                            let candidate=&ready[0];
                            if ui.add_enabled(!busy,design::primary("Play",palette)).clicked() {
                                self.start_candidate(candidate);
                            }
                            let version=view.releases.iter().find(|r|r.release.id==candidate.release_id).and_then(|r|r.release.version.as_deref()).filter(|v|!v.is_empty()).map_or("Saved release",|v|v);
                            ui.label(RichText::new(format!("{version} · {}",candidate.label.as_deref().unwrap_or(&candidate.entry_id))).size(13.0).color(palette.muted));
                        } else if action==PrimaryAction::ChooseLaunch {
                            ui.menu_button("Choose Play", |ui| self.play_options_menu(ui, &ready));
                        } else if let Some(process) = candidates.find(|c| matches!(c.status.as_str(), "Running" | "ProcessUnknown")) {
                            ui.label(&process.reason);
                        } else if let Some(release)=view.releases.first().filter(|r|view.releases.len()==1 && crate::manifest_ui::declared(r)) {
                            self.manifest_action(ui,release,&view.app.name);
                        } else if let Some(release)=view.releases.first().filter(|_|view.releases.len()==1)
                            && let Some(item)=release.associations.first().filter(|_|release.associations.len()==1)
                            && let Some(artifact)=&item.artifact

                            && !view.stale
                            && self.association_state(&item.association.id)=="Available to download"
                            && self.profile().is_some_and(|p|!p.archived)
                        {
                            if ui.add(design::primary("Download",palette)).clicked()
                                && let Some(profile)=self.active.clone() {
                                    self.send(Command::Download {profile,release:release.release.id.clone(),association:item.association.id.clone(),kind:artifact.kind.clone()});
                                    self.screen=Screen::Downloads;
                                }
                        } else {
                            let message=if worker::WINDOWS_INSTALL_ONLY {worker::WINDOWS_INSTALL_NOTICE}
                                else if view.stale {"Saved for offline browsing"}
                                else if view.releases.iter().flat_map(|r|&r.associations).any(|a|self.is_verified(&a.association.id)) {"Available in My library"}
                                else {"Choose a release and artifact below"};
                            ui.label(RichText::new(message).color(palette.muted));
                        }
                        if !worker::WINDOWS_INSTALL_ONLY && view.releases.iter().flat_map(|r|&r.associations).any(|a|self.is_verified(&a.association.id)) {
                            ui.add_space(8.0);
                            ui.menu_button("Manage local content", |ui| {
                                if ui.button("Open My library").clicked() {
                                    self.screen=Screen::Library;
                                    ui.close();
                                }
                                if let Some(profile)=self.active.clone() {
                                    let busy=self.local_operations.contains_key(&view.app.id);
                                    let prepared=self.snapshot.installations.iter().any(|item|item.app_id==view.app.id && item.state=="Ready");
                                    if prepared && ui.add_enabled(!busy,egui::Button::new("Remove local application…")).clicked() {
                                        self.remove_app_target=Some((profile.clone(),view.app.id.clone(),view.app.name.clone(),false));
                                        self.confirmation=Some(Confirmation::RemoveApp);
                                        ui.close();
                                    }
                                    if ui.add_enabled(!busy,egui::Button::new("Remove all local content and managed data…")).clicked() {
                                        self.remove_app_target=Some((profile,view.app.id.clone(),view.app.name.clone(),true));
                                        self.confirmation=Some(Confirmation::RemoveApp);
                                        ui.close();
                                    }
                                }
                            });
                        }
                    });
                });
            });
            if let Some(description)=info.as_ref().and_then(|c|c.description.as_deref()) {
                ui.add_space(24.0);
                ui.label(RichText::new("About").size(21.0).strong());
                ui.add_space(6.0);
                ui.allocate_ui_with_layout(egui::vec2(ui.available_width().min(740.0),0.0),egui::Layout::top_down(egui::Align::Min),|ui| {
                    for paragraph in description.split("\n\n") {
                        ui.label(RichText::new(paragraph).size(16.0).color(palette.text));
                        ui.add_space(8.0);
                    }
                });
            }
            if let Some(phase)=self.local_operations.get(&view.app.id){ui.horizontal(|ui|{ui.spinner();ui.label(phase);});}
            ui.add_space(24.0);
            ui.label(RichText::new("Releases").size(23.0).strong());
            ui.label(RichText::new("Choose the exact release and download you want.").size(15.0).color(palette.muted)); ui.add_space(12.0);
            if view.releases.is_empty() { ui.label("No releases are available in the saved catalog."); }
            for (r_index,release) in view.releases.iter().enumerate() {
                design::surface(ui).show(ui,|ui| {
                    ui.set_width((ui.available_width()-36.0).max(300.0));
                    let version=release.release.version.as_deref().filter(|s|!s.is_empty()).map_or_else(||format!("Release {}",r_index+1),|v|format!("Version {v}"));
                    egui::CollapsingHeader::new(RichText::new(version).size(19.0).strong()).id_salt(&release.release.id).default_open(r_index==0).show(ui,|ui| {
                        ui.label(RichText::new(format!("{} · {} download choice{}",release.release.state,release.associations.len(),if release.associations.len()==1{""}else{"s"})).size(14.0).color(palette.muted));
                        if let Some(snapshot)=&release.declaration
                            && let Ok(Some(info))=snapshot.release.convention::<ReleaseConvention>() {
                                if let Some(notes)=info.notes { ui.label(notes); }
                                if let Some(platforms)=info.platforms { ui.label(format!("Declared platforms: {}",platforms.join(", "))); }
                                if let Some(runtime)=info.runtime { ui.label(format!("Declared runtime: {} {} · preparation unavailable",runtime.name.unwrap_or_else(||"Unspecified".into()),runtime.version.unwrap_or_default())); }
                        }
                        if crate::manifest_ui::declared(release) {
                            self.manifest_action(ui,release,&view.app.name);
                            if let Some(d)=&release.declaration && let Ok(Some(m))=app_depot_core::installation::manifest(&d.release) {ui.label(crate::manifest_ui::localization(m.localization.as_ref()));}
                        }
                        if release.associations.is_empty() { ui.label("No artifacts in this release."); }
                        for (a_index,item) in release.associations.iter().enumerate() {
                            let convention=release.declaration.as_ref().and_then(|s|s.associations.iter().find(|a|a.association_id==item.association.id)).and_then(|a|a.declaration.convention::<AssociationConvention>().ok().flatten());
                            let kind=item.artifact.as_ref().map_or("Artifact",|a|a.kind.as_str());
                            let title=convention.as_ref().and_then(|c|c.label.clone()).unwrap_or_else(||format!("{} artifact{}",if kind=="Tree"{"Directory"}else{"File"},if release.associations.len()>1 {format!(" {}",a_index+1)}else{String::new()}));
                            egui::Frame::new().fill(palette.elevated).corner_radius(9).inner_margin(egui::Margin::same(16)).show(ui,|ui| {
                                ui.set_width((ui.available_width()-32.0).max(250.0));
                                ui.horizontal_wrapped(|ui| {
                                    ui.vertical(|ui| { ui.label(RichText::new(&title).size(17.0).strong());
                                        let size=item.artifact.as_ref().and_then(|a|a.size).map(format_bytes).unwrap_or_else(||"Size not specified".into());
                                        ui.label(RichText::new(format!("{kind} · {size}")).size(14.0).color(palette.muted));
                                        if let Some(purpose)=convention.as_ref().and_then(|c|c.purpose.as_deref()) { ui.label(RichText::new(purpose).small().weak()); }
                                        if let Some(description)=convention.as_ref().and_then(|c|c.description.as_deref()) { ui.label(RichText::new(description).small().weak()); }
                                        let state=self.association_state(&item.association.id);
                                        ui.label(RichText::new(state).size(14.0).color(if self.is_verified(&item.association.id){palette.good}else{palette.muted}));
                                    });
                                    let enabled=!view.stale && item.artifact.as_ref().is_some_and(|a|matches!(a.kind.as_str(),"File"|"Tree")) && self.profile().is_some_and(|p|!p.archived) && self.association_state(&item.association.id)=="Available to download";
                                    let action=if enabled {design::primary("Download",palette)} else {egui::Button::new(if self.is_verified(&item.association.id){"Saved locally"}else{"Unavailable"})};
                                    if ui.add_enabled(enabled,action).clicked() && let (Some(profile),Some(artifact))=(self.active.clone(),&item.artifact) {
                                        self.send(Command::Download {profile,release:release.release.id.clone(),association:item.association.id.clone(),kind:artifact.kind.clone()}); self.screen=Screen::Downloads;
                                    }
                                });
                                #[cfg(windows)]
                                if !crate::manifest_ui::declared(release) && matches!(kind, "File" | "Tree") {
                                    self.windows_install_actions(ui, &item.association.id, &item.association.artifact_id);
                                }
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
    #[cfg(windows)]
    fn windows_install_actions(&mut self, ui: &mut egui::Ui, association: &str, artifact: &str) {
        if let Some(item) = self
            .snapshot
            .installations
            .iter()
            .find(|item| {
                self.active.as_ref() == Some(&item.profile_id)
                    && item.association_id == association
                    && item.artifact_id == artifact
                    && matches!(item.state.as_str(), "Ready" | "Removing")
            })
            .cloned()
        {
            ui.horizontal(|ui| {
                let ready = item.state == "Ready";
                ui.label(if ready {
                    "Installed"
                } else {
                    "Removal pending"
                });
                if let Some(phase) = self.local_operations.get(&item.id) {
                    ui.label(phase);
                    return;
                }
                if ready && ui.button("Verify").clicked() {
                    self.send(Command::VerifyInstallation(item.id.clone()));
                }
                if ui
                    .button(if ready {
                        "Uninstall…"
                    } else {
                        "Retry removal…"
                    })
                    .clicked()
                {
                    self.remove_target =
                        Some((item.id.clone(), false, "Windows working copy".into()));
                    self.confirmation = Some(Confirmation::RemoveInstallation);
                }
            });
        } else if let Some(job) = self.snapshot.jobs.iter().find(|job| {
            self.active.as_ref() == Some(&job.profile_id)
                && job.association_id == association
                && job.artifact_id == artifact
                && job.state == "Completed"
        }) {
            if let Some(phase) = self.local_operations.get(&job.id) {
                ui.label(phase);
            } else if ui.button("Install").clicked() {
                self.send(Command::InstallArtifact(job.id.clone()));
            }
        }
    }

    fn library(&mut self, ui: &mut egui::Ui) {
        self.header(
            ui,
            "My library",
            "Your applications and saved downloads. Available offline.",
        );
        egui::ScrollArea::vertical()
            .id_salt("standard-library")
            .max_height((ui.available_height() * 0.55).max(140.0))
            .show(ui, |ui| self.manifest_library(ui));
        if worker::WINDOWS_INSTALL_ONLY {
            ui.label(worker::WINDOWS_INSTALL_NOTICE);
            ui.label("Install or verify a downloaded Artifact from its release details in Browse.");
            return;
        }
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
            && self.snapshot.standard.is_empty()
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
        items.retain(|item| {
            !self
                .snapshot
                .apps
                .iter()
                .flat_map(|a| &a.releases)
                .any(|r| {
                    self.snapshot.standard.contains_key(&r.release.id)
                        && r.associations.iter().any(|a| {
                            a.association.id == item.1 && a.association.artifact_id == item.2
                        })
                })
        });
        ui.horizontal(|ui| {
            ui.label(design::eyebrow("YOUR COLLECTION", design::palette(ui)));
            ui.add_space(12.0);
            if ui.selectable_label(self.prefs.grid, "Grid").clicked() {
                self.prefs.grid = true;
                self.remember();
            }
            if ui.selectable_label(!self.prefs.grid, "List").clicked() {
                self.prefs.grid = false;
                self.remember();
            }
        });
        ui.add_space(12.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for issue in &self.snapshot.local_issues {
                    egui::Frame::new().fill(design::palette(ui).surface).corner_radius(11).inner_margin(egui::Margin::same(16)).show(ui, |ui| {
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
                    egui::Frame::new().fill(design::palette(ui).surface).corner_radius(11).inner_margin(egui::Margin::same(16)).show(ui, |ui|{
                        ui.label(RichText::new(name.clone()).size(19.0).strong());
                        ui.label(RichText::new("Your writable copy is still available. The original download needs inspection or repair.").color(design::palette(ui).muted));
                        if ui.button("Open working copy").clicked(){self.send(Command::Reveal(installation.path.clone()));}
                        self.play_controls(ui,&installation.association_id);
                        egui::CollapsingHeader::new("Manage application").id_salt(&installation.id).show(ui, |ui| {
                            if ui.button("Remove working copy…").clicked(){self.remove_target=Some((installation.id.clone(),false,name.clone()));self.confirmation=Some(Confirmation::RemoveInstallation);}
                            if ui.button("Remove completely…").clicked(){self.remove_target=Some((installation.id.clone(),true,name.clone()));self.confirmation=Some(Confirmation::RemoveInstallation);}
                        });
                    });
                    ui.add_space(8.0);
                }
                let groups = group_library_items(
                    items,
                    |association| self.snapshot.apps.iter().find(|app| app.releases.iter().any(|release| release.associations.iter().any(|entry| entry.association.id == association))).map(|app| app.app.id.clone()),
                    |association| self.snapshot.launches.iter().any(|candidate| candidate.association_id == association && launch_is_playable(&candidate.status)),
                );
                let width = ui.available_width();
                let columns = if self.prefs.grid { (((width + 14.0) / 380.0).floor().max(1.0) as usize).min(groups.len().max(1)) } else { 1 };
                let card_width = if self.prefs.grid { ((width - 14.0 * columns.saturating_sub(1) as f32) / columns as f32).clamp(260.0, 560.0) } else { width.min(960.0) };
                for row in groups.chunks(columns) {
                    ui.horizontal_top(|ui| {
                        for (app_id, members) in row {
                            ui.allocate_ui_with_layout(egui::vec2(card_width, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                                if self.prefs.grid {
                                    self.library_group_card(ui, app_id, members, card_width);
                                } else {
                                    self.library_list_row(ui, app_id, members, card_width);
                                }
                            });
                            ui.add_space(14.0);
                        }
                    });
                    ui.add_space(14.0);
                }
            });
    }
    fn local_play_candidates(&self, members: &[LibraryItem]) -> Vec<LaunchCandidate> {
        self.snapshot
            .launches
            .iter()
            .filter(|candidate| {
                members
                    .iter()
                    .any(|item| item.1 == candidate.association_id)
                    && launch_is_playable(&candidate.status)
            })
            .cloned()
            .collect()
    }

    pub(crate) fn start_candidate(&mut self, candidate: &LaunchCandidate) {
        let key = format!("{}:{}", candidate.association_id, candidate.entry_id);
        if self.launch_states.get(&key).is_some_and(|(state, _)| {
            matches!(state.as_str(), "Starting" | "Preparing" | "Running")
        }) {
            return;
        }
        if let Err(error) = self.worker.play(candidate) {
            self.launch_states
                .insert(key, ("Launch failed".into(), error));
            return;
        }
        self.launch_states
            .insert(key, ("Starting".into(), "Starting application".into()));
    }

    fn launch_busy(&self, candidate: &LaunchCandidate) -> bool {
        self.launch_states
            .get(&format!(
                "{}:{}",
                candidate.association_id, candidate.entry_id
            ))
            .is_some_and(|(state, _)| {
                matches!(state.as_str(), "Starting" | "Preparing" | "Running")
            })
    }

    fn play_action(&mut self, ui: &mut egui::Ui, ready: &[LaunchCandidate]) {
        let p = design::palette(ui);
        if ready.len() == 1 {
            let candidate = &ready[0];
            let key = format!("{}:{}", candidate.association_id, candidate.entry_id);
            let state = self
                .launch_states
                .get(&key)
                .map(|(state, _)| state.as_str());
            let busy =
                state.is_some_and(|state| matches!(state, "Starting" | "Preparing" | "Running"));
            let label = match state {
                Some("Running") => "Running",
                Some("Starting" | "Preparing") => "Starting…",
                _ => "Play",
            };
            if ui.add_enabled(!busy, design::primary(label, p)).clicked() {
                self.start_candidate(candidate);
            }
        } else if ready.len() > 1 {
            ui.menu_button("Choose Play", |ui| self.play_options_menu(ui, ready));
        }
    }

    fn play_options_menu(&mut self, ui: &mut egui::Ui, ready: &[LaunchCandidate]) {
        for candidate in ready {
            let label = candidate.label.as_deref().unwrap_or(&candidate.entry_id);
            let context = self
                .snapshot
                .apps
                .iter()
                .flat_map(|app| &app.releases)
                .find(|release| release.release.id == candidate.release_id)
                .and_then(|release| release.release.version.as_deref())
                .map_or_else(
                    || "Release".to_owned(),
                    |version| format!("Version {version}"),
                );
            if ui
                .add_enabled(
                    !self.launch_busy(candidate),
                    egui::Button::new(format!("{context} · {label}")),
                )
                .clicked()
            {
                self.start_candidate(candidate);
                ui.close();
            }
        }
    }

    fn library_item_path(&self, item: &LibraryItem) -> PathBuf {
        let (_, association, artifact, path, _, _) = item;
        self.snapshot
            .installations
            .iter()
            .find(|entry| {
                entry.association_id == *association
                    && entry.artifact_id == *artifact
                    && entry.state == "Ready"
            })
            .map_or_else(|| path.clone(), |entry| entry.path.clone())
    }

    fn library_member_menu(&mut self, ui: &mut egui::Ui, title: &str, item: &LibraryItem) {
        let (_, association, artifact, path, _, _) = item;
        let installation = self
            .snapshot
            .installations
            .iter()
            .find(|entry| entry.association_id == *association && entry.artifact_id == *artifact)
            .cloned();
        let operation_key = installation
            .as_ref()
            .map_or(association.as_str(), |entry| entry.id.as_str());
        let available = !self.local_operations.contains_key(operation_key)
            && !self.local_operations.contains_key(association)
            && !self.snapshot.launches.iter().any(|candidate| {
                candidate.association_id == *association && self.launch_busy(candidate)
            });
        let local_path = self.library_item_path(item);
        if ui.button("Open folder").clicked() {
            self.send(Command::Reveal(local_path));
            ui.close();
        }
        if let Some(installation) = &installation {
            if ui
                .add_enabled(available, egui::Button::new("Verify integrity"))
                .clicked()
            {
                self.local_operations.insert(
                    installation.id.clone(),
                    "Comparing installed files with original download".into(),
                );
                self.send(Command::VerifyInstallation(installation.id.clone()));
                ui.close();
            }
            if ui
                .add_enabled(available, egui::Button::new("Repair…"))
                .clicked()
            {
                self.repair_installation_target = Some((installation.id.clone(), title.to_owned()));
                self.confirmation = Some(Confirmation::RepairInstallation);
                ui.close();
            }
            if let Some(report) = self.integrity_reports.get(&installation.id) {
                ui.menu_button(
                    if report.matches_original() {
                        "Integrity: original files"
                    } else {
                        "Integrity: local changes"
                    },
                    |ui| {
                        if report.matches_original() {
                            ui.label("No differences found at the last check.");
                        }
                        for path in &report.missing {
                            ui.label(format!("Missing: {path}"));
                        }
                        for path in &report.modified {
                            ui.label(format!("Modified: {path}"));
                        }
                        for path in &report.unexpected {
                            ui.label(format!("Added: {path}"));
                        }
                    },
                );
            }
        }
        ui.separator();
        if let Some(installation) = &installation {
            if ui
                .add_enabled(available, egui::Button::new("Remove application…"))
                .clicked()
            {
                self.remove_target = Some((installation.id.clone(), false, title.to_owned()));
                self.confirmation = Some(Confirmation::RemoveInstallation);
                ui.close();
            }
            if ui
                .add_enabled(available, egui::Button::new("Remove completely…"))
                .clicked()
            {
                self.remove_target = Some((installation.id.clone(), true, title.to_owned()));
                self.confirmation = Some(Confirmation::RemoveInstallation);
                ui.close();
            }
        } else if let Some(profile) = self.active.clone()
            && ui
                .add_enabled(available, egui::Button::new("Remove local download…"))
                .clicked()
        {
            self.remove_artifact_target = Some((profile, association.clone(), title.to_owned()));
            self.confirmation = Some(Confirmation::RemoveArtifact);
            ui.close();
        }
        ui.menu_button("Properties", |ui| {
            copy_line(ui, "Association ID", association);
            copy_line(ui, "Artifact ID", artifact);
            copy_line(ui, "Local path", &path.display().to_string());
        });
    }

    fn library_actions_menu(
        &mut self,
        ui: &mut egui::Ui,
        app_id: &str,
        title: &str,
        members: &[LibraryItem],
        ready: &[LaunchCandidate],
    ) {
        ui.set_min_width(190.0);
        if !ready.is_empty() {
            if ready.len() == 1 {
                if ui
                    .add_enabled(!self.launch_busy(&ready[0]), egui::Button::new("Play"))
                    .clicked()
                {
                    self.start_candidate(&ready[0]);
                    ui.close();
                }
            } else {
                ui.menu_button("Choose Play", |ui| self.play_options_menu(ui, ready));
            }
        }
        if self.snapshot.apps.iter().any(|app| app.app.id == app_id)
            && ui.button("View details").clicked()
        {
            self.selected_app = Some(app_id.to_owned());
            self.screen = Screen::Catalog;
            ui.close();
        }
        ui.separator();
        if members.len() == 1 {
            self.library_member_menu(ui, title, &members[0]);
        } else {
            for (index, item) in members.iter().enumerate() {
                let context = self
                    .snapshot
                    .apps
                    .iter()
                    .flat_map(|app| &app.releases)
                    .find(|release| {
                        release
                            .associations
                            .iter()
                            .any(|entry| entry.association.id == item.1)
                    })
                    .and_then(|release| release.release.version.as_deref())
                    .map_or_else(
                        || "Saved release".to_owned(),
                        |version| format!("Version {version}"),
                    );
                ui.menu_button(format!("{context} · {} {}", item.0, index + 1), |ui| {
                    self.library_member_menu(ui, title, item);
                });
            }
        }
    }

    fn library_list_row(
        &mut self,
        ui: &mut egui::Ui,
        app_id: &str,
        members: &[LibraryItem],
        width: f32,
    ) {
        let p = design::palette(ui);
        let title = self
            .snapshot
            .apps
            .iter()
            .find(|app| app.app.id == app_id)
            .map_or("Saved download", |app| app.app.name.as_str())
            .to_owned();
        let has_app = self.snapshot.apps.iter().any(|app| app.app.id == app_id);
        let ready = self.local_play_candidates(members);
        let response = egui::Frame::new()
            .fill(p.surface)
            .corner_radius(11)
            .inner_margin(egui::Margin::symmetric(14, 11))
            .show(ui, |ui| {
                ui.set_width((width - 28.0).max(220.0));
                let inner = (width - 28.0).max(220.0);
                let narrow = inner < 480.0;
                ui.horizontal_top(|ui| {
                    if has_app {
                        self.request_art(ui, app_id, ArtUse::Grid, egui::vec2(86.0, 72.0), &title);
                    } else {
                        library_placeholder(ui, egui::vec2(86.0, 72.0));
                    }
                    ui.add_space(8.0);
                    let right = if narrow { 70.0 } else { 172.0 };
                    ui.allocate_ui_with_layout(
                        egui::vec2((inner - 104.0 - right).max(85.0), 72.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_min_width((inner - 104.0 - right).max(85.0));
                            ui.label(
                                RichText::new(truncate(&title, if narrow { 28 } else { 56 }))
                                    .size(18.0)
                                    .strong(),
                            )
                            .on_hover_text(&title);
                            ui.label(
                                RichText::new(if members.len() == 1 {
                                    "Saved locally".to_owned()
                                } else {
                                    format!("{} local downloads", members.len())
                                })
                                .color(p.muted),
                            );
                            if let Some(phase) = members
                                .iter()
                                .find_map(|item| self.local_operations.get(&item.1))
                            {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label(phase);
                                });
                            }
                        },
                    );
                    if narrow {
                        ui.menu_button("More", |ui| {
                            self.library_actions_menu(ui, app_id, &title, members, &ready)
                        })
                        .response
                        .on_hover_text("More actions");
                    } else {
                        ui.allocate_ui_with_layout(
                            egui::vec2(right, 72.0),
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.menu_button("More", |ui| {
                                    self.library_actions_menu(ui, app_id, &title, members, &ready)
                                })
                                .response
                                .on_hover_text("More actions");
                                self.play_action(ui, &ready);
                                if ready.is_empty() && ui.button("Open folder").clicked() {
                                    self.send(Command::Reveal(self.library_item_path(&members[0])));
                                }
                            },
                        );
                    }
                });
            })
            .response;
        response.container_context_menu(|ui| {
            self.library_actions_menu(ui, app_id, &title, members, &ready)
        });
    }

    fn library_group_card(
        &mut self,
        ui: &mut egui::Ui,
        app_id: &str,
        members: &[LibraryItem],
        width: f32,
    ) {
        let p = design::palette(ui);
        let title = self
            .snapshot
            .apps
            .iter()
            .find(|app| app.app.id == app_id)
            .map_or("Saved download", |app| app.app.name.as_str())
            .to_owned();
        let has_app = self.snapshot.apps.iter().any(|app| app.app.id == app_id);
        let ready = self.local_play_candidates(members);
        let response = egui::Frame::new()
            .fill(p.surface)
            .corner_radius(14)
            .inner_margin(egui::Margin::same(14))
            .show(ui, |ui| {
                ui.set_width((width - 28.0).max(220.0));
                ui.set_min_height(302.0);
                let art_size = egui::vec2((width - 28.0).max(220.0), 172.0);
                if has_app {
                    self.request_art(ui, app_id, ArtUse::Grid, art_size, &title);
                } else {
                    library_placeholder(ui, art_size);
                }
                ui.add_space(12.0);
                ui.add(egui::Label::new(RichText::new(&title).size(20.0).strong()).truncate())
                    .on_hover_text(&title);
                ui.label(
                    RichText::new(if members.len() == 1 {
                        "Saved locally".to_owned()
                    } else {
                        format!("{} local downloads", members.len())
                    })
                    .size(14.0)
                    .color(p.muted),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ready.is_empty() {
                        if ui.button("Open folder").clicked() {
                            self.send(Command::Reveal(self.library_item_path(&members[0])));
                        }
                    } else {
                        self.play_action(ui, &ready);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.menu_button("More", |ui| {
                            self.library_actions_menu(ui, app_id, &title, members, &ready)
                        })
                        .response
                        .on_hover_text("More actions");
                    });
                });
                if let Some(phase) = members.iter().find_map(|item| {
                    self.snapshot
                        .installations
                        .iter()
                        .find(|entry| entry.association_id == item.1)
                        .and_then(|entry| self.local_operations.get(&entry.id))
                        .or_else(|| self.local_operations.get(&item.1))
                }) {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(phase);
                    });
                }
            })
            .response;
        response.container_context_menu(|ui| {
            self.library_actions_menu(ui, app_id, &title, members, &ready)
        });
    }

    fn download_job_menu(&mut self, ui: &mut egui::Ui, job: &DownloadJob) {
        for (label, command) in job_actions(job) {
            if ui.button(label).clicked() {
                self.send(command);
                ui.close();
            }
        }
        if job.artifact_id.starts_with("adtree:") && ui.button("Show files").clicked() {
            self.selected_job = Some(job.id.clone());
            self.last_tree_details_refresh = Instant::now();
            self.send(Command::TreeFiles(job.id.clone()));
            ui.close();
        }
        ui.menu_button("Technical details", |ui| {
            copy_line(ui, "Job ID", &job.id);
            copy_line(ui, "Artifact ID", &job.artifact_id);
            copy_line(ui, "Association ID", &job.association_id);
        });
    }
    fn downloads(&mut self, ui: &mut egui::Ui) {
        self.header(
            ui,
            "Downloads",
            if worker::WINDOWS_INSTALL_ONLY {
                worker::WINDOWS_INSTALL_NOTICE
            } else {
                "One active transfer. New requests move to the front of the queue."
            },
        );
        let mut jobs = self.snapshot.jobs.clone();
        jobs.sort_by_key(|job| (download_group(&job.state), job.queue_rank, job.created_at));
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
        let historical = jobs
            .iter()
            .filter(|j| matches!(j.state.as_str(), "Completed" | "Cancelled"))
            .count();
        if historical > 0 {
            if ui
                .button(if self.show_history {
                    format!("Hide history · {historical}")
                } else {
                    format!("Show history · {historical}")
                })
                .clicked()
            {
                self.show_history = !self.show_history;
            }
            ui.add_space(12.0);
        }
        if !self.show_history {
            jobs.retain(|j| !matches!(j.state.as_str(), "Completed" | "Cancelled"));
        }
        if jobs.is_empty() {
            ui.label(
                RichText::new("No active downloads. Your completed transfers are in history.")
                    .color(design::palette(ui).muted),
            );
            return;
        }
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let list_width = ui.available_width().min(940.0);
                let mut current_group=None;
                let mut queue_pos=0;
                for job in &jobs {
                    let group=download_group(&job.state);
                    if current_group!=Some(group) {
                        current_group=Some(group);
                        ui.add_space(8.0);
                        ui.label(RichText::new(match group {0=>"ACTIVE",1=>"UP NEXT",2=>"PAUSED",3=>"NEEDS ATTENTION",4=>"COMPLETED",_=>"CANCELLED"}).size(13.0).strong().color(design::palette(ui).muted));
                        ui.add_space(7.0);
                    }
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
                    if group==1 {queue_pos+=1;}
                    if group >= 4 {
                        let local_path=self.snapshot.installations.iter()
                            .find(|entry|entry.association_id==job.association_id && entry.artifact_id==job.artifact_id && entry.state=="Ready")
                            .map(|entry|entry.path.clone())
                            .or_else(||self.snapshot.files.iter().find(|entry|entry.association_id==job.association_id && entry.artifact_id==job.artifact_id).map(|entry|entry.path.clone()))
                            .or_else(||self.snapshot.trees.iter().find(|entry|entry.association_id==job.association_id && entry.artifact_id==job.artifact_id).map(|entry|entry.path.clone()));
                        let row=egui::Frame::new()
                            .fill(design::palette(ui).surface)
                            .corner_radius(9)
                            .inner_margin(egui::Margin::symmetric(14, 10))
                            .show(ui, |ui| {
                                ui.set_width((list_width - 28.0).max(220.0));
                                let stored = if job.state == "Completed" { job.expected_size } else { job.progress };
                                let when = if job.updated_at > 0 {
                                    format!(" · {}", format_time(Some(job.updated_at)))
                                } else {
                                    String::new()
                                };
                                egui::CollapsingHeader::new(format!("{} · {release} · {kind} · {}{when}", truncate(&title, 48), format_bytes(stored)))
                                    .id_salt(("history", &job.id))
                                    .show(ui, |ui| {
                                        if !worker::WINDOWS_INSTALL_ONLY && let Some(path)=&local_path && ui.button("Open folder").clicked(){self.send(Command::Reveal(path.clone()));}
                                        #[cfg(windows)]
                                        if job.state == "Cancelled" && ui.button("Remove cancelled download").clicked() {
                                            self.send(Command::RemoveFailedDownload(job.id.clone()));
                                        }
                                        copy_line(ui, "Job ID", &job.id);
                                        copy_line(ui, "Artifact ID", &job.artifact_id);
                                        copy_line(ui, "Association ID", &job.association_id);
                                    });
                            });
                        row.response.container_context_menu(|ui| {
                            if !worker::WINDOWS_INSTALL_ONLY && let Some(path)=&local_path && ui.button("Open folder").clicked(){self.send(Command::Reveal(path.clone()));ui.close();}
                            ui.menu_button("Technical details",|ui|{
                                copy_line(ui,"Job ID",&job.id);
                                copy_line(ui,"Artifact ID",&job.artifact_id);
                                copy_line(ui,"Association ID",&job.association_id);
                            });
                        });
                        ui.add_space(5.0);
                        continue;
                    }
                    if group == 1 {
                        let row = egui::Frame::new()
                            .fill(design::palette(ui).surface)
                            .corner_radius(10)
                            .inner_margin(egui::Margin::symmetric(16, 12))
                            .show(ui, |ui| {
                                ui.set_width((list_width - 32.0).max(220.0));
                                let narrow = list_width < 550.0;
                                let action_width = if narrow { 70.0 } else { 190.0 };
                                let title_width = (list_width - 32.0 - action_width - 12.0).max(110.0);
                                ui.horizontal_top(|ui| {
                                    ui.allocate_ui_with_layout(egui::vec2(title_width, 0.0),
                                        egui::Layout::top_down(egui::Align::Min), |ui| {
                                            ui.set_min_width(title_width);
                                            ui.label(RichText::new(format!("{queue_pos}. {}", truncate(&title, if narrow { 36 } else { 70 })))
                                                .size(17.0).strong()).on_hover_text(&title);
                                            ui.label(RichText::new(format!("{release} · {kind} · {} saved", format_bytes(job.progress)))
                                                .size(14.0).color(design::palette(ui).muted));
                                        });
                                    ui.allocate_ui_with_layout(egui::vec2(action_width, 0.0),
                                        egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            ui.menu_button("More", |ui| self.download_job_menu(ui, job))
                                                .response.on_hover_text("Download actions");
                                            if !narrow && ui.button("Download first").clicked() {
                                                self.send(Command::MoveToTop(job.id.clone()));
                                            }
                                        });
                                });
                                if narrow && ui.small_button("Download first").clicked() {
                                    self.send(Command::MoveToTop(job.id.clone()));
                                }
                            });
                        row.response.container_context_menu(|ui| self.download_job_menu(ui, job));
                        ui.add_space(7.0);
                        continue;
                    }
                    design::surface(ui)
                        .show(ui, |ui| {
                            ui.set_width((list_width - 36.0).max(220.0));
                            ui.label(RichText::new(&title).size(19.0).strong());
                            ui.label(
                                RichText::new(format!(
                                    "{server} · {release} · {kind} · {}",
                                    live.as_ref().map_or(if job.auto_resume {"Waiting for priority"} else {friendly_job_state(&job.state)}, |p| phase_label(p.observation.phase, &job.state))
                                ))
                                .size(14.0)
                                .color(design::palette(ui).muted),
                            );
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                for (label, cmd) in job_actions(job) {
                                    if ui.button(label).clicked() {
                                        self.send(cmd);
                                    }
                                }
                            });
                            ui.add_space(10.0);
                            ui.label(
                                RichText::new(format!(
                                    "{} of {}",
                                    format_bytes(received),
                                    format_bytes(job.expected_size)
                                ))
                                .size(14.0),
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
                                    .desired_width(ui.available_width()),
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
                (SettingsTab::Updates, "Updates & about"),
                (SettingsTab::Diagnostics, "Diagnostics"),
            ] {
                if ui
                    .selectable_label(self.settings_tab == tab, label)
                    .clicked()
                {
                    self.settings_tab = tab;
                    if tab == SettingsTab::Storage && !worker::WINDOWS_INSTALL_ONLY {
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
                if profiles.len()>1 {
                    ui.label(RichText::new("Choose a server").size(14.0).color(design::palette(ui).muted));
                    for (id,name,archived) in profiles { if ui.add_sized([ui.available_width().min(430.0),34.0],egui::Button::selectable(self.active.as_ref()==Some(&id),if archived {format!("{name} · archived")}else{name})).clicked() { self.select(id); } }
                    ui.add_space(10.0);
                }
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
                        ui.add_space(12.0);
                        ui.label(RichText::new(if p.credential_ref.is_some(){"Credential saved securely"}else{"No saved credential"}).color(if p.credential_ref.is_some(){design::palette(ui).good}else{design::palette(ui).muted}));
                        egui::CollapsingHeader::new("Manage account token").show(ui,|ui| {
                            ui.label(RichText::new("The Server shows a new personal API token once on its Account page.").size(14.0).color(design::palette(ui).muted));
                            ui.horizontal(|ui| { ui.add(egui::TextEdit::singleline(&mut self.token).password(!self.token_visible).hint_text("Paste token here").desired_width(300.0)); ui.checkbox(&mut self.token_visible,"Show"); });
                            ui.checkbox(&mut self.token_persist,"Save securely on this device (otherwise session only)");
                            ui.horizontal(|ui| { if ui.add_enabled(!self.token.is_empty(),egui::Button::new("Verify & save token")).clicked() { let token=std::mem::take(&mut self.token); self.send(Command::SetToken{id:p.id.clone(),token,persist:self.token_persist}); }
                                if ui.button("Remove token…").clicked(){self.confirmation=Some(Confirmation::RemoveToken);} });
                        });
                        ui.add_space(14.0);
                        egui::CollapsingHeader::new("Manage server")
                            .id_salt(("manage-server", &p.id.0))
                            .show(ui, |ui| {
                                ui.label(RichText::new("Name and address").strong());
                                ui.horizontal(|ui| { ui.add(egui::TextEdit::singleline(&mut self.edit_name).hint_text("New display name")); if ui.button("Rename").clicked(){self.send(Command::RenameProfile{id:p.id.clone(),name:self.edit_name.trim().into()});self.edit_name.clear();} });
                                ui.horizontal(|ui| { ui.add(egui::TextEdit::singleline(&mut self.edit_url).hint_text("New server origin").desired_width(290.0)); if ui.button("Change origin…").clicked(){self.confirmation=Some(Confirmation::Move);} });
                                ui.checkbox(&mut self.edit_allow_http,"Allow unencrypted HTTP for the new origin");
                                ui.add_space(10.0);
                                if ui.button("Archive server…").clicked(){self.confirmation=Some(Confirmation::Archive);}
                            });
                    } else { ui.label("Archived server: saved catalog and verified downloads are available offline."); }
                }
            }); }
            if self.settings_tab==SettingsTab::Storage {
                #[cfg(target_os = "linux")]
                section(ui, "Windows compatibility runtime", |ui| {
                    egui::ComboBox::from_id_salt("windows-runtime")
                        .selected_text(self.prefs.windows_runtime.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "Select runtime".into()))
                        .show_ui(ui, |ui| {
                            for runtime in app_depot_core::compatibility::discover() {
                                let path = runtime.executable();
                                if ui.selectable_value(&mut self.prefs.windows_runtime, Some(path.to_owned()), path.display().to_string()).changed() {
                                    self.remember();
                                }
                            }
                        });
                });
            section(ui,"Storage",|ui| {
                ui.label(RichText::new("Your library").size(18.0).strong());
                if worker::WINDOWS_INSTALL_ONLY {
                    ui.label("Windows profiles and catalog data default to %LOCALAPPDATA%\\app-depot-client.");
                    ui.label(format!("Current location: {}", self.snapshot.library_data.display()));
                    ui.label("Storage management is unavailable in this Windows build.");
                    return;
                }
                ui.label("New Linux libraries are stored in ~/.appdepot. Existing libraries remain where they are until you explicitly migrate them.");
                ui.add_space(12.0);
                ui.label(RichText::new(format!("Current location: {}",self.snapshot.library_data.display())).size(14.0).color(design::palette(ui).muted));
                ui.add_space(10.0);
                if ui.button("Refresh storage usage").clicked(){self.send(Command::StorageUsage);}
                if let Some(usage)=&self.storage_usage {
                    ui.label(RichText::new("Logical storage by purpose").strong());
                    ui.label(RichText::new("Physical disk usage and space reclaimable from one app are unknown here. Shared objects, reflinks and compression may overlap these amounts.").size(14.0).color(design::palette(ui).muted));
                    ui.add_space(8.0);
                    let categories = [
                        ("Immutable verified objects",usage.immutable_objects_logical_bytes),
                        ("Verified materializations",usage.verified_materializations_logical_bytes),
                        ("Writable application copies",usage.writable_installations_logical_bytes),
                        ("Managed application data",usage.managed_userdata_logical_bytes),
                        ("Staging and recovery",usage.staging_logical_bytes),
                        ("Artwork cache",usage.artwork_logical_bytes),
                    ];
                    let largest = categories.iter().map(|(_, bytes)| *bytes).max().unwrap_or(0);
                    for (label, bytes) in categories {
                        ui.horizontal_wrapped(|ui| {
                            ui.add_sized([240.0, 22.0], egui::Label::new(RichText::new(label).size(14.0))).on_hover_text(label);
                            if largest > 0 {
                                let (rect, _) = ui.allocate_exact_size(egui::vec2(150.0, 7.0), egui::Sense::hover());
                                ui.painter().rect_filled(rect, 4.0, design::palette(ui).elevated);
                                let fill = rect.width() * (bytes as f64 / largest as f64) as f32;
                                if fill > 0.0 {
                                    ui.painter().rect_filled(egui::Rect::from_min_size(rect.min, egui::vec2(fill, rect.height())), 4.0, design::palette(ui).accent);
                                }
                            }
                            ui.label(RichText::new(format_bytes(bytes)).color(design::palette(ui).muted));
                        });
                        ui.add_space(4.0);
                    }
                }
                egui::CollapsingHeader::new("Locations and migration details").show(ui,|ui| {
                    copy_line(ui,"Metadata",&self.snapshot.library_data.display().to_string());
                    copy_line(ui,"Content",&self.snapshot.library_content.display().to_string());
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
                ui.add_space(16.0);
                ui.label(RichText::new("Interface size").strong());
                ui.label(RichText::new("Adjust text and controls together. System display scaling is still respected.").size(14.0).color(design::palette(ui).muted));
                if ui.add(egui::Slider::new(&mut self.prefs.ui_scale,0.85..=1.6).step_by(0.05).text("UI scale")).changed() {
                    self.apply_theme(ui.ctx()); self.remember();
                }
                if ui.button("Reset appearance").clicked() {
                    self.prefs.theme=Theme::System; self.prefs.ui_scale=1.15; self.apply_theme(ui.ctx()); self.remember();
                }
            }); }
            if self.settings_tab==SettingsTab::Updates {
                #[cfg(any(target_os = "linux", windows))]
                self.updates_settings(ui);
                #[cfg(not(any(target_os = "linux", windows)))]
                ui.label("Automatic updates are currently supported only for official Linux AppImages.");
            }
            if self.settings_tab==SettingsTab::Diagnostics {
            section(ui,"Diagnostics & about",|ui| {
                ui.label(format!("App Depot Desktop {} · {} · {}",env!("CARGO_PKG_VERSION"),env!("APP_DEPOT_BUILD_KIND"),env!("APP_DEPOT_SOURCE_REVISION")));
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
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.send(Command::CancelConnect(self.connect_attempt));
            self.setup = Setup::Closed;
            self.token.clear();
            self.setup_error = None;
            return;
        }
        egui::Modal::new(egui::Id::unique("connect-to-app-depot")).show(ctx,|ui| {
            ui.set_width(500.0); ui.label(RichText::new(if self.setup==Setup::Token{"Sign in to your server"}else{"Add a server"}).size(25.0).strong());
            ui.label(RichText::new("Enter the address of your self-hosted App Depot Server. Public catalogs need no token.").weak()); ui.add_space(11.0);
            ui.label("Server URL"); ui.add_enabled(self.setup!=Setup::Checking,egui::TextEdit::singleline(&mut self.profile_url).hint_text("https://depot.example.org").desired_width(470.0));
            ui.label("Display name"); ui.add_enabled(self.setup!=Setup::Checking,egui::TextEdit::singleline(&mut self.profile_name).hint_text("My server").desired_width(470.0));
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
                if ui.add_enabled(allowed,design::primary(if self.setup==Setup::Token{"Verify & connect"}else if self.setup==Setup::Checking{"Checking…"}else{"Connect"},design::palette(ui))).clicked() {
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
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.confirmation = None;
            return;
        }
        let text = match kind {
            Confirmation::Archive => {
                "Archive this server? Its token will be removed; cached catalog and verified downloads remain."
            }
            Confirmation::Move => {
                "Changing the origin creates a separate profile. Existing cache and downloads remain with the archived origin."
            }
            Confirmation::RemoveToken => "Remove this server's saved or session token?",
            Confirmation::RemoveInstallation => {
                if cfg!(windows) {
                    "Uninstall this working copy? Verified downloads and managed saves are kept. Running, modified or unverified installations are refused."
                } else if self
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
            ui.set_width(500.0);
            let title=match kind {Confirmation::Exit=>"Close App Depot?",Confirmation::RepairSource|Confirmation::RepairInstallation=>"Repair local content?",Confirmation::Archive=>"Archive server?",Confirmation::Move=>"Change server origin?",Confirmation::RemoveToken=>"Remove credential?",_=>"Remove local content?"};
            ui.label(RichText::new(title).size(24.0).strong());
            ui.add_space(8.0);
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
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                if ui.button("Keep current state").clicked() {
                    self.confirmation = None;
                }
                let action=match kind {Confirmation::Exit=>"Close App Depot",Confirmation::RepairSource|Confirmation::RepairInstallation=>"Repair",Confirmation::Archive=>"Archive",Confirmation::Move=>"Change origin",Confirmation::RemoveToken=>"Remove credential",_=>"Remove locally"};
                let p=design::palette(ui);
                let button=if matches!(kind,Confirmation::RemoveInstallation|Confirmation::RemoveArtifact|Confirmation::RemoveApp|Confirmation::RemoveToken|Confirmation::Archive){egui::Button::new(RichText::new(action).color(Color32::WHITE).strong()).fill(p.error)}else{design::primary(action,p)};
                if ui.add(button).clicked() {
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
fn navigation_item(
    ui: &mut egui::Ui,
    screen: Screen,
    label: &str,
    selected: bool,
    count: usize,
) -> bool {
    let p = design::palette(ui);
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 44.0), egui::Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, label));
    ui.painter().rect_filled(
        rect,
        9.0,
        if selected {
            p.accent
        } else if response.hovered() {
            p.elevated
        } else {
            p.sidebar
        },
    );
    let ink = if selected { p.accent_text } else { p.text };
    let center = egui::pos2(rect.left() + 22.0, rect.center().y);
    let stroke = egui::Stroke::new(1.8, ink);
    match screen {
        Screen::Catalog => {
            ui.painter()
                .circle_stroke(center - egui::vec2(2.0, 2.0), 6.0, stroke);
            ui.painter().line_segment(
                [center + egui::vec2(3.0, 3.0), center + egui::vec2(9.0, 9.0)],
                stroke,
            );
        }
        Screen::Library => {
            ui.painter().rect_stroke(
                egui::Rect::from_center_size(center, egui::vec2(15.0, 15.0)),
                2.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            ui.painter()
                .hline((center.x - 5.0)..=(center.x + 5.0), center.y - 2.0, stroke);
            ui.painter()
                .hline((center.x - 5.0)..=(center.x + 3.0), center.y + 2.0, stroke);
        }
        Screen::Downloads => {
            ui.painter().line_segment(
                [center - egui::vec2(0.0, 7.0), center + egui::vec2(0.0, 4.0)],
                stroke,
            );
            ui.painter().line_segment(
                [
                    center + egui::vec2(-5.0, 0.0),
                    center + egui::vec2(0.0, 5.0),
                ],
                stroke,
            );
            ui.painter().line_segment(
                [center + egui::vec2(5.0, 0.0), center + egui::vec2(0.0, 5.0)],
                stroke,
            );
            ui.painter()
                .hline((center.x - 7.0)..=(center.x + 7.0), center.y + 8.0, stroke);
        }
        Screen::Settings => {
            ui.painter().circle_stroke(center, 7.0, stroke);
            ui.painter().circle_filled(center, 2.5, ink);
        }
    }
    ui.painter().text(
        egui::pos2(rect.left() + 47.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(16.0),
        ink,
    );
    if count > 0 {
        ui.painter().text(
            egui::pos2(rect.right() - 15.0, rect.center().y),
            egui::Align2::CENTER_CENTER,
            count.to_string(),
            egui::FontId::proportional(14.0),
            ink,
        );
    }
    response.clicked()
        || (response.has_focus()
            && ui.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Space)))
}
fn section(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    let width = ui.available_width().min(1040.0);
    design::surface(ui)
        .inner_margin(egui::Margin::same(22))
        .show(ui, |ui| {
            ui.set_width((width - 44.0).max(0.0));
            ui.label(RichText::new(title).size(21.0).strong());
            ui.add_space(15.0);
            body(ui);
        });
}
fn library_placeholder(ui: &mut egui::Ui, size: egui::Vec2) {
    let p = design::palette(ui);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(rect, 10.0, p.elevated);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "A",
        egui::FontId::proportional(32.0),
        p.muted,
    );
}
pub(super) fn group_library_items(
    items: Vec<LibraryItem>,
    app_for_association: impl Fn(&str) -> Option<String>,
    playable: impl Fn(&str) -> bool,
) -> Vec<(String, Vec<LibraryItem>)> {
    let mut groups: Vec<(String, Vec<LibraryItem>)> = Vec::new();
    let mut indices: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for item in items {
        let app_id = app_for_association(&item.1).unwrap_or_else(|| item.1.clone());
        if let Some(index) = indices.get(&app_id) {
            groups[*index].1.push(item);
        } else {
            indices.insert(app_id.clone(), groups.len());
            groups.push((app_id, vec![item]));
        }
    }
    for (_, members) in &mut groups {
        members.sort_by_key(|item| !playable(&item.1));
    }
    groups
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
pub(crate) fn format_bytes(bytes: u64) -> String {
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
pub(super) fn download_group(state: &str) -> u8 {
    match state {
        "FetchingInventory" | "Downloading" | "Verifying" | "Committing" => 0,
        "Queued" => 1,
        "Paused" => 2,
        "Failed" => 3,
        "Completed" => 4,
        _ => 5,
    }
}
pub(super) fn launch_is_playable(status: &str) -> bool {
    matches!(status, "Ready" | "PreparationRequired")
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PrimaryAction {
    Play,
    ChooseLaunch,
    ChooseArtifact,
    Offline,
}
pub(super) fn primary_action(
    ready_count: usize,
    _release_count: usize,
    _association_count: usize,
    stale: bool,
) -> PrimaryAction {
    if ready_count == 1 {
        PrimaryAction::Play
    } else if ready_count > 0 {
        PrimaryAction::ChooseLaunch
    } else if stale {
        PrimaryAction::Offline
    } else {
        PrimaryAction::ChooseArtifact
    }
}
pub(super) fn resize_direction(rect: egui::Rect, pos: egui::Pos2) -> Option<egui::ResizeDirection> {
    use egui::ResizeDirection as D;
    if !rect.contains(pos) {
        return None;
    }
    let left = pos.x - rect.left();
    let right = rect.right() - pos.x;
    let top = pos.y - rect.top();
    let bottom = rect.bottom() - pos.y;
    let corner = 18.0;
    let edge = 10.0;
    if left <= corner && top <= corner {
        Some(D::NorthWest)
    } else if right <= corner && top <= corner {
        Some(D::NorthEast)
    } else if left <= corner && bottom <= corner {
        Some(D::SouthWest)
    } else if right <= corner && bottom <= corner {
        Some(D::SouthEast)
    } else if top <= edge {
        Some(D::North)
    } else if bottom <= edge {
        Some(D::South)
    } else if left <= edge {
        Some(D::West)
    } else if right <= edge {
        Some(D::East)
    } else {
        None
    }
}

fn resize_cursor(direction: egui::ResizeDirection) -> egui::CursorIcon {
    use egui::{CursorIcon as C, ResizeDirection as D};
    match direction {
        D::North | D::South => C::ResizeVertical,
        D::East | D::West => C::ResizeHorizontal,
        D::NorthEast | D::SouthWest => C::ResizeNeSw,
        D::NorthWest | D::SouthEast => C::ResizeNwSe,
    }
}

pub(super) fn titlebar_command(index: usize, maximized: bool) -> egui::ViewportCommand {
    match index {
        0 => egui::ViewportCommand::Minimized(true),
        1 => egui::ViewportCommand::Maximized(!maximized),
        _ => egui::ViewportCommand::Close,
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
            #[cfg(windows)]
            (
                "Remove failed download",
                Command::RemoveFailedDownload(job.id.clone()),
            ),
        ],
        #[cfg(windows)]
        "Cancelled" => vec![(
            "Remove cancelled download",
            Command::RemoveFailedDownload(job.id.clone()),
        )],
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
        #[cfg(any(target_os = "linux", windows))]
        {
            self.updater.poll();
            self.updater.periodic(ctx);
        }
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
        let p = design::palette(ui);
        let wide = ui.available_width() >= 920.0;
        if cfg!(target_os = "linux") {
            egui::Panel::top("app-titlebar")
                .exact_size(52.0)
                .frame(egui::Frame::new().fill(p.sidebar))
                .show(ui, |ui| self.titlebar(ui));
        }
        if wide {
            egui::Panel::left("app-sidebar")
                .exact_size(236.0)
                .frame(egui::Frame::new().fill(p.sidebar))
                .show(ui, |ui| self.sidebar(ui));
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(p.canvas)
                    .inner_margin(egui::Margin {
                        left: 26,
                        right: 26,
                        top: 24,
                        bottom: 18,
                    }),
            )
            .show(ui, |ui| {
                if !wide {
                    self.top_navigation(ui);
                    ui.add_space(14.0);
                }
                self.content(ui);
            });
        if cfg!(target_os = "linux") {
            self.resize_edges(ui.ctx());
        }
        self.setup_dialog(ui.ctx());
        self.confirmation(ui.ctx());
        self.installation_dialog(ui.ctx());
        #[cfg(target_os = "linux")]
        if self.update_ack.is_some() && !self.update_ack_armed {
            self.update_ack_armed = true;
            ui.ctx().request_repaint();
        } else if let Some(marker) = self.update_ack.take() {
            let result = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker)
                .and_then(|file| file.sync_all());
            if let Err(error) = result {
                eprintln!("Update startup acknowledgement failed: {error}");
            }
        }
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
