//! Presentation model for the shared Standard installation workflow.
use crate::{
    Desktop, design,
    worker::{Command, ReleaseView},
};
use app_depot_core::{
    ServerProfileId,
    installation::{self, Localization, Manifest, SavedPlan, Selections},
};
use eframe::egui;
use std::sync::Arc;

pub(crate) fn declared(release: &ReleaseView) -> bool {
    release.declaration.as_ref().is_some_and(|d| {
        d.release
            .app_depot()
            .is_some_and(|v| v.get("installation").is_some())
    })
}
pub(crate) fn localization(value: Option<&Localization>) -> String {
    match value {
        Some(Localization::Fixed { language }) => format!("Language: {language} (fixed)"),
        Some(Localization::Bundled { languages }) => {
            format!("Bundled languages: {}", languages.join(", "))
        }
        Some(Localization::Selectable) => "Language is selectable".into(),
        Some(Localization::Unknown) | None => "Language information unavailable".into(),
    }
}
fn attempt_label(state: &str) -> &str {
    match state {
        "Running" => "Running / preparing",
        "Succeeded" | "Reused" => "Completed",
        "Failed" => "Failed",
        "Cancelled" => "Cancelled",
        "Ambiguous" => "Result uncertain — review before retry",
        "Declined" => "Skipped by choice",
        _ => "Unknown",
    }
}
pub(crate) fn defaults(manifest: &Manifest) -> Selections {
    manifest
        .options
        .iter()
        .map(|g| (g.id.clone(), g.defaults.clone()))
        .collect()
}
pub(crate) fn complete(manifest: &Manifest, selections: &Selections) -> bool {
    manifest.options.iter().all(|g| {
        selections
            .get(&g.id)
            .is_some_and(|s| (!g.required || !s.is_empty()) && (g.multiple || s.len() <= 1))
    })
}

pub(crate) struct InstallDialog {
    pub request: u64,
    pub profile: ServerProfileId,
    pub release: String,
    pub title: String,
    pub revision: String,
    pub manifest: Option<Manifest>,
    pub selections: Selections,
    pub plan: Option<Arc<SavedPlan>>,
    pub selected: Vec<String>,
    pub generation: Option<String>,
    pub retry: bool,
    pub busy: bool,
    pub error: Option<String>,
}
impl InstallDialog {
    pub fn set_plan(&mut self, plan: Arc<SavedPlan>) {
        self.selected = plan.plan["prerequisites"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["optional"].as_bool() == Some(false))
            .filter_map(|p| p["id"].as_str().map(str::to_owned))
            .collect();
        self.plan = Some(plan);
        self.busy = false;
        self.error = None;
    }
}

impl Desktop {
    pub(crate) fn open_manifest(&mut self, release: &ReleaseView, title: &str) {
        let Some(profile) = self.active.clone() else {
            return;
        };
        let Some(doc) = release.declaration.as_ref() else {
            return;
        };
        match installation::manifest(&doc.release) {
            Ok(Some(manifest)) if !manifest.recipe_payload => {
                self.manifest_request += 1;
                self.install_dialog = Some(InstallDialog {
                    request: self.manifest_request,
                    profile,
                    release: release.release.id.clone(),
                    title: title.into(),
                    revision: doc.release.revision_id.clone(),
                    selections: defaults(&manifest),
                    manifest: Some(manifest),
                    plan: None,
                    selected: vec![],
                    generation: None,
                    retry: false,
                    busy: false,
                    error: None,
                });
            }
            _ => {
                self.message = Some((
                    "This installation manifest is not supported".into(),
                    true,
                    std::time::Instant::now(),
                ))
            }
        }
    }
    pub(crate) fn open_saved_install(
        &mut self,
        release: &str,
        title: &str,
        plan: Arc<SavedPlan>,
        generation: Option<String>,
        retry: bool,
    ) {
        self.manifest_request += 1;
        let mut dialog = InstallDialog {
            request: self.manifest_request,
            profile: plan.profile_id.clone(),
            release: release.into(),
            title: title.into(),
            revision: String::new(),
            manifest: None,
            selections: Selections::new(),
            plan: None,
            selected: vec![],
            generation,
            retry,
            busy: false,
            error: None,
        };
        dialog.set_plan(plan);
        if let Some(g) = &dialog.generation
            && let Some(selected) = self
                .snapshot
                .prerequisites
                .get(g)
                .and_then(|p| p["consents"].as_array())
                .and_then(|r| r.last())
                .and_then(|r| r["selected"].as_array())
        {
            dialog.selected = selected
                .iter()
                .filter_map(|id| id.as_str().map(str::to_owned))
                .collect();
        }
        self.install_dialog = Some(dialog);
    }
    pub(crate) fn manifest_action(&mut self, ui: &mut egui::Ui, release: &ReleaseView, name: &str) {
        if !declared(release) {
            return;
        }
        if let Some(state) = self.snapshot.standard.get(&release.release.id) {
            let pending = state.generations.iter().any(|g| {
                matches!(
                    g.state.as_str(),
                    "Preparing" | "PrerequisitesPending" | "Removing"
                )
            });
            if ui
                .button(if pending {
                    "Resume / manage installation…"
                } else {
                    "Manage installation"
                })
                .clicked()
            {
                self.screen = crate::Screen::Library;
                self.selected_app = None;
            }
            return;
        }
        let valid = release
            .declaration
            .as_ref()
            .and_then(|d| installation::manifest(&d.release).ok().flatten())
            .is_some_and(|m| !m.recipe_payload);
        let host = crate::worker::manifest_platform();
        let compatible = host.is_some_and(|host| {
            release
                .declaration
                .as_ref()
                .and_then(|d| {
                    d.release
                        .convention::<app_depot_core::models::ReleaseConvention>()
                        .ok()
                        .flatten()
                })
                .and_then(|c| c.platforms)
                .is_none_or(|platforms| platforms.is_empty() || platforms.iter().any(|p| p == host))
        });
        let enabled = valid
            && compatible
            && self.profile().is_some_and(|p| !p.archived)
            && crate::worker::manifest_platform().is_some()
            && !self.active.as_ref().is_some_and(|profile| {
                self.local_operations
                    .contains_key(&crate::worker::standard_operation_key(
                        profile,
                        &release.release.id,
                    ))
            });
        if ui
            .add_enabled(enabled, design::primary("Install…", design::palette(ui)))
            .clicked()
        {
            self.open_manifest(release, name);
        }
        if !valid {
            ui.label("Installation configuration unavailable or unsupported");
        } else if !compatible {
            ui.label("Installation unavailable on this device");
        }
    }
    pub(crate) fn manifest_browse_action(
        &mut self,
        ui: &mut egui::Ui,
        app: &crate::worker::AppView,
    ) {
        let releases: Vec<_> = app.releases.iter().filter(|r| declared(r)).collect();
        if releases.len() == 1 {
            self.manifest_action(ui, releases[0], &app.app.name);
        } else if !releases.is_empty() {
            ui.menu_button("Install…", |ui| {
                for (i, r) in releases.iter().enumerate() {
                    ui.label(
                        r.release
                            .version
                            .clone()
                            .unwrap_or_else(|| format!("Release {}", i + 1)),
                    );
                    self.manifest_action(ui, r, &app.app.name);
                }
            });
        }
    }

    pub(crate) fn installation_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.install_dialog.take() else {
            return;
        };
        let mut open = true;
        let mut close = false;
        egui::Window::new(format!("Install {}",d.title)).collapsible(false).resizable(true).default_width(530.0).open(&mut open).show(ctx,|ui|{
            egui::ScrollArea::vertical().max_height(480.0).show(ui,|ui|{
                if let Some(error)=&d.error {ui.label(egui::RichText::new(error).color(design::palette(ui).error));}
                if let Some(plan)=&d.plan {
                    let parsed: Result<installation::Plan,_>=serde_json::from_value(plan.plan.clone());
                    if let Ok(p)=parsed {
                        ui.label(localization(p.localization.as_ref().or_else(||d.manifest.as_ref().and_then(|m|m.localization.as_ref()))));
                        let size=p.files.as_ref().map(|f|f.values().fold(0u64,|n,v|n.saturating_add(v.size))).unwrap_or(0);
                        ui.heading("Installation summary");
                        ui.label(format!("Installed payload: {} · {} files",crate::views::format_bytes(size),p.files.as_ref().map_or(0,|f|f.len())));
                        if let Some(m)=&d.manifest { for g in &m.options { let names:Vec<_>=g.choices.iter().filter(|c|p.selections.get(&g.id).is_some_and(|s|s.contains(&c.id))).map(|c|c.name.as_str()).collect();if !names.is_empty(){ui.label(format!("{}: {}",g.name,names.join(", ")));} } }
                        let mut sources=std::collections::BTreeSet::new();
                        for f in p.files.iter().flat_map(|f|f.values()).chain(p.prerequisites.iter().flatten().filter(|pre|d.selected.contains(&pre.id)).map(|pre|&pre.file)){sources.insert((&f.association_id,&f.artifact_id));}
                        let release=self.snapshot.apps.iter().flat_map(|a|&a.releases).find(|r|r.release.id==d.release);
                        let total=sources.iter().try_fold(0u64,|n,(assoc,artifact)|release.and_then(|r|r.associations.iter().find(|a|&a.association.id==*assoc && &a.association.artifact_id==*artifact)).and_then(|a|a.artifact.as_ref()).and_then(|a|a.size).and_then(|v|n.checked_add(v)));
                        ui.label(format!("{} source components · {}",sources.len(),total.map(|n|format!("{} total source size",crate::views::format_bytes(n))).unwrap_or_else(||"source size unavailable".into())));
                        ui.label("Existing verified content is reused; additional space is needed for working copies.");
                        if let Some(prereqs)=&p.prerequisites {
                            if !prereqs.is_empty(){ui.heading("Prerequisites");ui.label("Only selected declared installers may run. Review before authorizing.");}
                            for pre in prereqs {
                                let mut selected=d.selected.contains(&pre.id);
                                if ui.add_enabled(pre.optional && !d.busy && (crate::worker::native_prerequisites_available() || selected),egui::Checkbox::new(&mut selected,format!("{}{}",pre.name,if pre.optional{" (optional)"}else{" (required)"}))).changed(){if selected {d.selected.push(pre.id.clone());}else{d.selected.retain(|id|id!=&pre.id);}}
                                egui::CollapsingHeader::new("Installer information").id_salt(&pre.id).show(ui,|ui|{ui.label(&pre.evidence);ui.label(format!("Arguments: {:?}",pre.arguments));ui.label(format!("Expected exit codes: {:?}",pre.success_exit_codes));});
                            }
                        }
                        if !crate::worker::native_prerequisites_available() && p.prerequisites.as_ref().is_some_and(|p|!p.is_empty()){ui.label(if d.selected.is_empty(){"Native prerequisite execution is unavailable. Optional installers will be skipped."}else{"Native prerequisite execution is unavailable. Files can be prepared, but required prerequisites keep the installation pending."});}
                        if !p.unresolved.is_empty(){ui.collapsing("Unresolved installation notes",|ui|{for note in &p.unresolved{ui.label(note);}});}
                        ui.collapsing("Technical details",|ui|{ui.label(&plan.fingerprint);ui.monospace(plan.plan.to_string());});
                        if d.manifest.is_some() && ui.button("Back to options").clicked(){d.plan=None;return;}
                        if p.recipe.is_some(){ui.label("Derived installation is unavailable.");}
                        else if ui.add_enabled(!d.busy && !self.local_operations.contains_key(&crate::worker::standard_operation_key(&d.profile,&d.release)) && (d.generation.is_none() || d.selected.is_empty() || crate::worker::native_prerequisites_available()),design::primary(if d.retry{"Authorize retry"}else if d.generation.is_some(){"Confirm prerequisites"}else if !d.selected.is_empty() && !crate::worker::native_prerequisites_available(){"Confirm and prepare files"}else{"Confirm installation"},design::palette(ui))).clicked(){
                            self.send(Command::InstallManifest {profile:d.profile.clone(),release:d.release.clone(),plan:plan.id.clone(),fingerprint:plan.fingerprint.clone(),selected:d.selected.clone(),generation:d.generation.clone(),retry:d.retry});
                            self.local_operations.insert(crate::worker::standard_operation_key(&d.profile,&d.release),"Preparing installation".into());
                            self.screen=crate::Screen::Library;close=true;
                        }
                    } else {ui.label("Saved plan could not be read.");}
                } else if let Some(m)=&d.manifest {
                    ui.label(localization(m.localization.as_ref()));
                    for g in &m.options {
                        ui.heading(&g.name);
                        if g.required {ui.label("Required");}
                        let values=d.selections.entry(g.id.clone()).or_default();
                        for c in &g.choices {
                            let mut checked=values.contains(&c.id);
                            if ui.add_enabled(!d.busy,egui::Checkbox::new(&mut checked,&c.name)).changed(){
                                if checked {if !g.multiple{values.clear();}values.push(c.id.clone());}else{values.retain(|id|id!=&c.id);}
                            }
                        }
                    }
                    if ui.add_enabled(!d.busy && complete(m,&d.selections),design::primary("Review installation",design::palette(ui))).clicked(){
                        d.busy=true;d.error=None;
                        self.send(Command::ResolveManifest{request:d.request,profile:d.profile.clone(),release:d.release.clone(),revision:d.revision.clone(),selections:d.selections.clone()});
                    }
                }
                if d.busy {ui.spinner();}
            });
            if ui.button("Cancel").clicked(){close=true;}
        });
        if open && !close {
            self.install_dialog = Some(d);
        }
    }
    pub(crate) fn manifest_library(&mut self, ui: &mut egui::Ui) {
        for app in &self.snapshot.apps {
            for release in &app.releases {
                if !self.snapshot.standard.contains_key(&release.release.id)
                    && let Some(phase) = self.active.as_ref().and_then(|profile| {
                        self.local_operations
                            .get(&crate::worker::standard_operation_key(
                                profile,
                                &release.release.id,
                            ))
                    })
                {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!("{} — {phase}", app.app.name));
                    });
                }
            }
        }
        let mut entries: Vec<_> = self.snapshot.standard.clone().into_iter().collect();
        entries.sort_by(|a, b| a.1.app_id.cmp(&b.1.app_id).then(a.0.cmp(&b.0)));
        for (release, installation) in entries {
            let title = self
                .snapshot
                .apps
                .iter()
                .find(|a| a.app.id == installation.app_id)
                .map(|a| a.app.name.clone())
                .unwrap_or_else(|| "Installed application".into());
            let active = installation
                .generations
                .iter()
                .find(|g| Some(&g.id) == installation.active_generation.as_ref());
            let pending = installation.generations.iter().rev().find(|g| {
                matches!(
                    g.state.as_str(),
                    "Preparing" | "PrerequisitesPending" | "Removing"
                )
            });
            design::surface(ui).show(ui, |ui| {
                ui.heading(&title);
                if let Some(version) = self
                    .snapshot
                    .apps
                    .iter()
                    .flat_map(|a| &a.releases)
                    .find(|r| r.release.id == release)
                    .and_then(|r| r.release.version.as_deref())
                {
                    ui.label(format!("Version {version}"));
                }
                let busy = self
                    .local_operations
                    .get(&crate::worker::standard_operation_key(
                        &installation.profile_id,
                        &release,
                    ))
                    .cloned();
                if let Some(phase) = &busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(phase);
                    });
                    let plan = pending
                        .or(active)
                        .and_then(|g| self.snapshot.plans.get(&g.plan_id))
                        .and_then(|p| {
                            serde_json::from_value::<installation::Plan>(p.plan.clone()).ok()
                        });
                    for job in self.snapshot.jobs.iter().filter(|j| {
                        j.profile_id == installation.profile_id
                            && plan.as_ref().is_some_and(|p| {
                                p.files
                                    .iter()
                                    .flat_map(|f| f.values())
                                    .chain(p.prerequisites.iter().flatten().map(|p| &p.file))
                                    .any(|f| {
                                        f.association_id == j.association_id
                                            && f.artifact_id == j.artifact_id
                                    })
                            })
                            && matches!(
                                j.state.as_str(),
                                "Queued"
                                    | "FetchingInventory"
                                    | "Downloading"
                                    | "Verifying"
                                    | "Committing"
                            )
                    }) {
                        ui.add(
                            egui::ProgressBar::new(if job.expected_size == 0 {
                                0.0
                            } else {
                                job.progress as f32 / job.expected_size as f32
                            })
                            .text(format!(
                                "{} / {}",
                                crate::views::format_bytes(job.progress),
                                crate::views::format_bytes(job.expected_size)
                            )),
                        );
                    }
                }
                if active.is_some_and(|g| g.state == "Ready") {
                    ui.label(if pending.is_some() {
                        "Previous installation remains ready"
                    } else {
                        "Ready"
                    });
                    let candidates: Vec<_> = self
                        .snapshot
                        .launches
                        .iter()
                        .filter(|c| c.release_id == release && c.generation_id.is_some())
                        .cloned()
                        .collect();
                    if candidates.is_empty() {
                        ui.label("No declared Play target available");
                    }
                    for c in candidates {
                        let key = format!("{}:{}", c.association_id, c.entry_id);
                        let running = self.launch_states.get(&key).is_some_and(|(state, _)| {
                            matches!(state.as_str(), "Starting" | "Preparing" | "Running")
                        });
                        if c.status == "Ready" {
                            if ui
                                .add_enabled(
                                    !running,
                                    egui::Button::new(if running {
                                        "Running / starting"
                                    } else {
                                        "Play"
                                    }),
                                )
                                .clicked()
                            {
                                self.start_candidate(&c);
                            }
                        } else {
                            ui.label(&c.reason);
                        }
                    }
                    if let Some(g) = active
                        && let Some(progress) = self.snapshot.prerequisites.get(&g.id)
                        && progress["consents"]
                            .as_array()
                            .is_some_and(|r| !r.is_empty())
                    {
                        ui.collapsing("Prerequisite results", |ui| {
                            if let Some(round) =
                                progress["consents"].as_array().and_then(|r| r.last())
                                && let Some(attempts) = round["attempts"].as_array()
                            {
                                for a in attempts {
                                    let name = progress["savedPlan"]["plan"]["prerequisites"]
                                        .as_array()
                                        .and_then(|p| {
                                            p.iter().find(|p| p["id"] == a["prerequisiteId"])
                                        })
                                        .and_then(|p| p["name"].as_str())
                                        .unwrap_or("Prerequisite");
                                    ui.label(format!(
                                        "{name}: {}",
                                        attempt_label(a["state"].as_str().unwrap_or("Unknown"))
                                    ));
                                }
                            }
                        });
                    }
                    if ui
                        .add_enabled(busy.is_none(), egui::Button::new("Verify"))
                        .clicked()
                    {
                        self.send(Command::StandardVerify {
                            profile: installation.profile_id.clone(),
                            release: release.clone(),
                        });
                    }
                    if pending.is_none()
                        && let Some(g) = active
                        && let Some(plan) = self.snapshot.plans.get(&g.plan_id).cloned()
                        && ui
                            .add_enabled(busy.is_none(), egui::Button::new("Reinstall…"))
                            .clicked()
                    {
                        self.open_saved_install(&release, &title, plan, None, false);
                    }
                }
                if let Some(g) = pending {
                    ui.label(match g.state.as_str() {
                        "Preparing" => "Preparing files",
                        "PrerequisitesPending" => "Prerequisites pending",
                        _ => "Removal pending",
                    });
                    if let Some(error) = &g.last_error {
                        ui.label("Installation needs attention.");
                        ui.collapsing("Failure details", |ui| {
                            ui.label(format!("Last recorded failure for generation {}", g.id));
                            ui.label(error);
                        });
                    }
                    if g.state == "PrerequisitesPending"
                        && !crate::worker::native_prerequisites_available()
                    {
                        ui.label("Native prerequisite execution is unavailable on this device.");
                    }
                    let progress = self.snapshot.prerequisites.get(&g.id);
                    if g.state == "PrerequisitesPending"
                        && progress
                            .is_none_or(|p| p["consents"].as_array().is_none_or(|r| r.is_empty()))
                        && let Some(plan) = self.snapshot.plans.get(&g.plan_id)
                        && let Ok(p) =
                            serde_json::from_value::<installation::Plan>(plan.plan.clone())
                    {
                        for pre in p.prerequisites.iter().flatten() {
                            ui.label(format!(
                                "{} — {}",
                                pre.name,
                                if pre.optional {
                                    "Optional, awaiting review"
                                } else {
                                    "Required, awaiting authorization"
                                }
                            ));
                        }
                    }
                    if let Some(progress) = progress
                        && let Some(rounds) = progress["consents"].as_array()
                    {
                        for round in rounds.iter().rev().take(1) {
                            if let Some(attempts) = round["attempts"].as_array() {
                                for attempt in attempts {
                                    let id = attempt["prerequisiteId"].as_str().unwrap_or("");
                                    let name = progress["savedPlan"]["plan"]["prerequisites"]
                                        .as_array()
                                        .and_then(|p| p.iter().find(|p| p["id"] == id))
                                        .and_then(|p| p["name"].as_str())
                                        .unwrap_or("Prerequisite");
                                    ui.label(format!(
                                        "{name}: {}",
                                        attempt_label(
                                            attempt["state"].as_str().unwrap_or("Unknown")
                                        )
                                    ));
                                }
                            }
                        }
                    }
                    if let Some(plan) = self.snapshot.plans.get(&g.plan_id).cloned()
                        && g.state != "Removing"
                        && ui
                            .add_enabled(busy.is_none(), egui::Button::new("Review / resume…"))
                            .clicked()
                    {
                        let retry = progress.is_some_and(|p| {
                            p["consents"].as_array().is_some_and(|a| !a.is_empty())
                        });
                        self.open_saved_install(
                            &release,
                            &title,
                            plan,
                            (g.state == "PrerequisitesPending").then(|| g.id.clone()),
                            retry,
                        );
                    }
                }
                if ui
                    .add_enabled(busy.is_none(), egui::Button::new("Uninstall…"))
                    .clicked()
                {
                    self.standard_remove = Some((
                        installation.profile_id.clone(),
                        release.clone(),
                        title.clone(),
                    ));
                }
            });
            ui.add_space(8.0);
        }
        if let Some((profile, release, title)) = self.standard_remove.clone() {
            egui::Window::new("Uninstall application?")
                .collapsible(false)
                .show(ui.ctx(), |ui| {
                    ui.label(format!(
                        "Remove {title} working copies? Verified downloads are retained."
                    ));
                    if ui.button("Uninstall").clicked() {
                        self.send(Command::StandardUninstall { profile, release });
                        self.standard_remove = None;
                    }
                    if ui.button("Cancel").clicked() {
                        self.standard_remove = None;
                    }
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn synthetic_install_dialog_renders_options_and_review() {
        let temp = tempfile::tempdir().unwrap();
        let ctx = egui::Context::default();
        let mut desktop = Desktop::new(
            &ctx,
            app_depot_core::library::LibraryPaths {
                metadata: temp.path().join("meta"),
                content: temp.path().join("content"),
            },
        );
        let profile = ServerProfileId("fixture".into());
        let manifest:Manifest=serde_json::from_value(json!({"version":1,"options":[{"id":"lang","name":"Language","required":true,"language":true,"choices":[{"id":"en","name":"English","payload":[]}]}],"localization":{"mode":"selectable"}})).unwrap();
        desktop.install_dialog = Some(InstallDialog {
            request: 1,
            profile: profile.clone(),
            release: "release".into(),
            title: "Synthetic application".into(),
            revision: "revision".into(),
            selections: defaults(&manifest),
            manifest: Some(manifest),
            plan: None,
            selected: vec![],
            generation: None,
            retry: false,
            busy: false,
            error: None,
        });
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            desktop.installation_dialog(ui.ctx())
        });
        output.textures_delta.clear();
        assert!(!output.shapes.is_empty());
        let d = desktop.install_dialog.as_mut().unwrap();
        d.selections.insert("lang".into(), vec!["en".into()]);
        d.set_plan(Arc::new(SavedPlan{id:"saved".into(),profile_id:profile,fingerprint:"fingerprint".into(),assessment:"Standard".into(),plan:json!({"version":1,"appId":"app","releaseId":"release","revisionId":"revision","platform":crate::worker::manifest_platform().unwrap(),"selections":{"lang":["en"]},"unresolved":[],"localization":{"mode":"selectable"},"files":{},"directories":[],"prerequisites":[],"launch":null})}));
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            desktop.installation_dialog(ui.ctx())
        });
        output.textures_delta.clear();
        assert!(!output.shapes.is_empty());
        assert!(desktop.install_dialog.as_ref().unwrap().plan.is_some());
    }
    #[test]
    fn localization_and_required_choices_are_distinct() {
        let manifest:Manifest=serde_json::from_value(json!({"version":1,"payload":[],"options":[{"id":"language","name":"Language","language":true,"required":true,"multiple":false,"choices":[{"id":"english","name":"English","payload":[]},{"id":"french","name":"French","payload":[]}]}],"localization":{"mode":"selectable"}})).unwrap();
        let mut choices = defaults(&manifest);
        assert!(!complete(&manifest, &choices));
        choices.insert("language".into(), vec!["french".into()]);
        assert!(complete(&manifest, &choices));
        assert_eq!(
            localization(manifest.localization.as_ref()),
            "Language is selectable"
        );
        assert_eq!(
            localization(Some(&Localization::Fixed {
                language: "ru".into()
            })),
            "Language: ru (fixed)"
        );
        assert_eq!(
            localization(Some(&Localization::Bundled {
                languages: vec!["en".into(), "fr".into()]
            })),
            "Bundled languages: en, fr"
        );
        assert_eq!(
            localization(Some(&Localization::Unknown)),
            "Language information unavailable"
        );
    }
}
