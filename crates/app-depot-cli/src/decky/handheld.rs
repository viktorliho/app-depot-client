//! One Gamescope window for review, choices, transfer progress and recovery.
use super::*;
use app_depot_core::{
    installation::OptionGroup,
    telemetry::LiveProgress,
    transfer::{TransferObservation, TransferPhase},
};
use std::{
    cell::{Cell, RefCell},
    sync::Mutex,
};

pub(super) fn bytes(n: u64) -> String {
    if n >= 1 << 30 {
        format!("{:.2} GiB", n as f64 / (1u64 << 30) as f64)
    } else if n >= 1 << 20 {
        format!("{:.1} MiB", n as f64 / 1048576.)
    } else if n >= 1024 {
        format!("{:.1} KiB", n as f64 / 1024.)
    } else {
        format!("{n} B")
    }
}

#[derive(Default)]
pub(super) struct Activity {
    phase: String,
    transfer: Option<(String, LiveProgress)>,
    pub release: Option<String>,
}

impl Activity {
    pub fn observe(&mut self, id: &str, event: TransferObservation) {
        let now = Instant::now();
        match &mut self.transfer {
            Some((previous, live)) if previous == id => live.update(event, now),
            _ => self.transfer = Some((id.into(), LiveProgress::new(event, now))),
        }
    }
}

struct Prompt {
    title: String,
    detail: String,
    actions: Vec<String>,
    back: bool,
    initial: usize,
    reply: mpsc::Sender<Option<usize>>,
}

#[derive(PartialEq)]
struct Question {
    title: String,
    detail: String,
    actions: Vec<String>,
}
pub(super) struct Session {
    history: RefCell<Vec<(Question, usize)>>,
    cursor: Cell<usize>,
    prompts: mpsc::Sender<Prompt>,
    pub activity: Arc<Mutex<Activity>>,
    cancel: Arc<AtomicBool>,
}

impl Session {
    pub fn check(&self) -> Result<(), CliError> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(LibraryError::Invalid("Installation interrupted".into()).into())
        } else {
            Ok(())
        }
    }
    pub fn phase(&self, message: &str) {
        let mut a = self.activity.lock().unwrap();
        a.phase = message.into();
        a.transfer = None;
    }
    pub fn choose(
        &self,
        title: &str,
        detail: &str,
        actions: Vec<String>,
    ) -> Result<usize, CliError> {
        self.choose_from(title, detail, actions, 0)
    }
    fn choose_from(
        &self,
        title: &str,
        detail: &str,
        actions: Vec<String>,
        initial: usize,
    ) -> Result<usize, CliError> {
        self.check()?;
        let index = self.cursor.get();
        let question = Question {
            title: title.into(),
            detail: detail.into(),
            actions: actions.clone(),
        };
        if let Some((known, answer)) = self.history.borrow().get(index)
            && known == &question
        {
            self.cursor.set(index + 1);
            return Ok(*answer);
        }
        self.history.borrow_mut().truncate(index);
        let (reply, rx) = mpsc::channel();
        self.prompts
            .send(Prompt {
                title: title.into(),
                detail: detail.into(),
                actions,
                back: index > 0,
                initial,
                reply,
            })
            .map_err(|_| CliError::Decky("closed"))?;
        loop {
            self.check()?;
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Some(i)) => {
                    self.history.borrow_mut().push((question, i));
                    self.cursor.set(index + 1);
                    return Ok(i);
                }
                Ok(None) => {
                    self.history.borrow_mut().truncate(index.saturating_sub(1));
                    return Err(CliError::Decky("back"));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => (),
                Err(_) => return Err(CliError::Decky("closed")),
            }
        }
    }
    pub fn options(&self, group: &OptionGroup) -> Result<Vec<String>, CliError> {
        if !group.multiple {
            let mut labels: Vec<_> = group.choices.iter().map(|c| c.name.clone()).collect();
            if !group.required {
                labels.push("Skip".into());
            }
            let initial = group
                .choices
                .iter()
                .position(|c| group.defaults.contains(&c.id))
                .unwrap_or(if group.required { 0 } else { labels.len() - 1 });
            let i = self.choose_from(&group.name, "Select an option", labels, initial)?;
            return Ok(group
                .choices
                .get(i)
                .map(|c| c.id.clone())
                .into_iter()
                .collect());
        }
        let mut ids = group.defaults.clone();
        loop {
            let mut labels: Vec<_> = group
                .choices
                .iter()
                .map(|c| {
                    format!(
                        "{}  {}",
                        if ids.contains(&c.id) { "✓" } else { "○" },
                        c.name
                    )
                })
                .collect();
            if !group.required || !ids.is_empty() {
                labels.push("Continue".into());
            }
            let i = self.choose(&group.name, "Select components, then continue", labels)?;
            let Some(c) = group.choices.get(i) else {
                return Ok(ids);
            };
            if ids.contains(&c.id) {
                ids.retain(|id| id != &c.id)
            } else {
                ids.push(c.id.clone())
            }
        }
    }
}

pub(super) fn run<F>(
    name: String,
    paths: LibraryPaths,
    profile: ServerProfileId,
    stop: Arc<AtomicBool>,
    mut prepare: F,
) -> Result<Option<LaunchCandidate>, CliError>
where
    F: FnMut(&Session) -> Result<LaunchCandidate, CliError> + Send + 'static,
{
    let observer = Library::open(paths)?;
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let activity = Arc::new(Mutex::new(Activity::default()));
    let session = Session {
        history: RefCell::new(Vec::new()),
        cursor: Cell::new(0),
        prompts: tx,
        cancel: cancel.clone(),
        activity: activity.clone(),
    };
    let shutting_down = Arc::new(AtomicBool::new(false));
    let closing = shutting_down.clone();
    let result = Arc::new(Mutex::new(None));
    let output = result.clone();
    let done = Arc::new(AtomicBool::new(false));
    let complete = done.clone();
    let join = std::thread::spawn(move || {
        let choice = loop {
            if closing.load(Ordering::Relaxed) {
                break None;
            }
            session.cursor.set(0);
            session.phase("Checking installation");
            match prepare(&session) {
                Ok(choice) if !session.cancel.load(Ordering::Relaxed) => break Some(choice),
                Err(CliError::Decky("back")) => continue,
                result => {
                    session.history.borrow_mut().clear();
                    session.cursor.set(0);
                    if closing.load(Ordering::Relaxed) {
                        break None;
                    }
                    let paused = session.cancel.swap(false, Ordering::Relaxed);
                    let message = if paused {
                        "Verified downloads are retained. Resume to review and continue.".into()
                    } else {
                        result.err().map_or_else(String::new, |e| e.to_string())
                    };
                    session.phase("Installation stopped");
                    match session.choose(
                        if paused {
                            "Installation paused"
                        } else {
                            "Installation needs attention"
                        },
                        &message,
                        vec![
                            if paused {
                                "Resume".into()
                            } else {
                                "Retry".into()
                            },
                            "Exit".into(),
                        ],
                    ) {
                        Ok(0) => {
                            session.history.borrow_mut().clear();
                            continue;
                        }
                        _ => break None,
                    }
                }
            }
        };
        if choice.is_none() {
            closing.store(true, Ordering::Relaxed);
        }
        *output.lock().unwrap() = choice;
        complete.store(true, Ordering::Release);
    });
    let app = PlayUi {
        name,
        observer,
        profile,
        prompts: rx,
        prompt: None,
        focus: 0,
        activity,
        cancel: cancel.clone(),
        closing: shutting_down.clone(),
        stop,
        done,
        finish_at: None,
        gamepad: gilrs::Gilrs::new().ok(),
        axis: 0,
        next_move: Instant::now(),
        last_poll: Instant::now(),
        total: 0,
        job: String::new(),
        review_scroll: 0.,
    };
    let window = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([960., 600.]),
        ..Default::default()
    };
    let window_result = eframe::run_native(
        "App Depot · Play",
        window,
        Box::new(move |cc| {
            cc.egui_ctx.set_pixels_per_point(1.18);
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            let mut style = (*cc.egui_ctx.style_of(egui::Theme::Dark)).clone();
            style
                .text_styles
                .insert(egui::TextStyle::Body, egui::FontId::proportional(17.));
            style
                .text_styles
                .insert(egui::TextStyle::Button, egui::FontId::proportional(17.));
            style.visuals.panel_fill = egui::Color32::from_rgb(22, 29, 38);
            style.spacing.button_padding = egui::vec2(12., 7.);
            cc.egui_ctx.set_style_of(egui::Theme::Dark, style);
            Ok(Box::new(app))
        }),
    );
    shutting_down.store(true, Ordering::Relaxed);
    cancel.store(true, Ordering::Relaxed);
    join.join()
        .map_err(|_| LibraryError::Invalid("Installation worker stopped unexpectedly".into()))?;
    window_result.map_err(|e| LibraryError::Invalid(format!("Could not open Play window: {e}")))?;
    Ok(result.lock().unwrap().take())
}

struct PlayUi {
    name: String,
    observer: Library,
    profile: ServerProfileId,
    prompts: Receiver<Prompt>,
    prompt: Option<Prompt>,
    focus: usize,
    activity: Arc<Mutex<Activity>>,
    cancel: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    finish_at: Option<Instant>,
    gamepad: Option<gilrs::Gilrs>,
    axis: i8,
    next_move: Instant,
    last_poll: Instant,
    total: u64,
    job: String,
    review_scroll: f32,
}

impl PlayUi {
    fn inputs(&mut self, ctx: &egui::Context) -> (bool, bool, i8, f32) {
        use gilrs::{Axis, Button, EventType};
        let focused = ctx.input(|i| i.focused);
        let (mut ok, mut back, mut direction, mut scroll) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
                i8::from(i.key_pressed(egui::Key::ArrowDown))
                    - i8::from(i.key_pressed(egui::Key::ArrowUp)),
                if i.key_pressed(egui::Key::PageDown) {
                    180.
                } else if i.key_pressed(egui::Key::PageUp) {
                    -180.
                } else {
                    0.
                },
            )
        });
        if let Some(pad) = &mut self.gamepad {
            while let Some(e) = pad.next_event() {
                if !focused {
                    self.axis = 0;
                    continue;
                }
                match e.event {
                    EventType::ButtonPressed(Button::South, _) => ok = true,
                    EventType::ButtonPressed(Button::East, _) => back = true,
                    EventType::ButtonPressed(Button::DPadUp, _) => direction = -1,
                    EventType::ButtonPressed(Button::DPadDown, _) => direction = 1,
                    EventType::ButtonPressed(Button::LeftTrigger, _) => scroll = -180.,
                    EventType::ButtonPressed(Button::RightTrigger, _) => scroll = 180.,
                    EventType::AxisChanged(Axis::LeftStickY, v, _) => {
                        let next = if v > 0.55 {
                            -1
                        } else if v < -0.55 {
                            1
                        } else {
                            0
                        };
                        if next != self.axis {
                            direction = next;
                            self.next_move = Instant::now() + Duration::from_millis(360);
                        }
                        self.axis = next;
                    }
                    _ => (),
                }
            }
        }
        if !focused {
            self.axis = 0;
            return (false, false, 0, 0.);
        }
        if self.axis != 0 && Instant::now() >= self.next_move {
            direction = self.axis;
            self.next_move = Instant::now() + Duration::from_millis(160);
        }
        (ok, back, direction, scroll)
    }

    fn back(&mut self) {
        if self.prompt.as_ref().is_some_and(|p| p.back) {
            if let Some(p) = self.prompt.take() {
                let _ = p.reply.send(None);
            }
        } else {
            self.halt(self.prompt.is_some());
        }
    }
    fn halt(&mut self, exit: bool) {
        if exit {
            self.closing.store(true, Ordering::Relaxed);
        }
        self.cancel.store(true, Ordering::Relaxed);
        self.prompt = None;
        self.last_poll = Instant::now() - Duration::from_secs(1);
    }
}

impl eframe::App for PlayUi {
    fn ui(&mut self, root: &mut egui::Ui, _: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        ctx.request_repaint_after(Duration::from_millis(50));
        let finished = self.done.load(Ordering::Acquire);
        if let Ok(prompt) = self.prompts.try_recv() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
                "{} · App Depot",
                prompt.title
            )));
            self.focus = prompt.initial;
            self.prompt = Some(prompt);
            self.review_scroll = 0.;
        }
        let (confirm, back, step, page) = self.inputs(&ctx);
        if !finished
            && (self.stop.load(Ordering::Relaxed) || ctx.input(|i| i.viewport().close_requested()))
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.halt(true);
        }
        if back && !finished {
            self.back();
        }
        let stopping = self.cancel.load(Ordering::Relaxed);
        let a = self.activity.lock().unwrap();
        let phase = a.phase.clone();
        let transfer = a.transfer.clone();
        let release = a.release.clone();
        drop(a);
        if self.last_poll.elapsed() >= Duration::from_millis(250) {
            self.last_poll = Instant::now();
            if let Some((id, _)) = &transfer
                && let Ok(job) = self.observer.job(id)
            {
                self.total = job.expected_size;
                self.job = id.clone();
                if stopping {
                    let _ = self.observer.pause_job(id);
                }
            }
            if stopping
                && phase.starts_with("Running ")
                && let Some(release) = release
            {
                let _ = self
                    .observer
                    .cancel_compatibility_prerequisite(&self.profile, &release);
            }
        }
        let accent = egui::Color32::from_rgb(48, 123, 176);
        root.spacing_mut().item_spacing = egui::vec2(10., 9.);
        egui::Frame::new().inner_margin(20.).show(root, |ui| {
            ui.label(
                egui::RichText::new("APP DEPOT")
                    .size(14.)
                    .color(egui::Color32::LIGHT_BLUE),
            );
            ui.heading(egui::RichText::new(&self.name).size(32.));
            ui.separator();
            if finished {
                ui.heading(if self.closing.load(Ordering::Relaxed) {
                    "Closing"
                } else {
                    "Starting game session"
                });
                if self.finish_at.get_or_insert_with(Instant::now).elapsed()
                    > Duration::from_millis(250)
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                return;
            }
            if let Some(p) = &self.prompt {
                ui.heading(&p.title);
                if !p.detail.is_empty() {
                    let height = (ui.available_height() * 0.40).clamp(62., 240.);
                    let mut area = egui::ScrollArea::vertical()
                        .id_salt("review")
                        .max_height(height);
                    if page != 0. {
                        area = area.vertical_scroll_offset((self.review_scroll + page).max(0.));
                    }
                    self.review_scroll = area
                        .show(ui, |ui| ui.label(egui::RichText::new(&p.detail).size(18.)))
                        .state
                        .clamped_offset()
                        .y;
                }
                self.focus = (self.focus as isize + step as isize)
                    .rem_euclid(p.actions.len() as isize) as usize;
                let mut picked = None;
                egui::ScrollArea::vertical()
                    .id_salt("actions")
                    .max_height((ui.available_height() - 42.).max(52.))
                    .show(ui, |ui| {
                        for (i, label) in p.actions.iter().enumerate() {
                            let focused = i == self.focus;
                            let mut button =
                                egui::Button::new(egui::RichText::new(label).size(20.));
                            if focused {
                                button = button.fill(accent);
                            }
                            let response = ui.add_sized([ui.available_width(), 45.], button);
                            if focused && step != 0 {
                                response.scroll_to_me(None);
                            }
                            if response.clicked() || (focused && confirm) {
                                picked = Some(i);
                            }
                        }
                    });
                let back_label = if p.back { "B  Back" } else { "B  Exit" };
                if let Some(i) = picked
                    && let Some(p) = self.prompt.take()
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title("App Depot · Play".into()));
                    let _ = p.reply.send(Some(i));
                }
                ui.horizontal(|ui| {
                    ui.label("↑ ↓  Choose     A  Confirm     LB / RB  Scroll");
                    if ui.button(back_label).clicked() {
                        self.back();
                    }
                });
            } else {
                if stopping {
                    ui.heading("Stopping at a safe checkpoint");
                }
                if let Some((id, live)) = transfer {
                    let event = &live.observation;
                    ui.heading(match event.phase {
                        TransferPhase::PreparingInventory => "Reading file inventory",
                        TransferPhase::WaitingForServer => "Waiting for download",
                        TransferPhase::Downloading => "Downloading",
                        TransferPhase::Verifying => "Verifying downloaded content",
                        TransferPhase::PreparingLocalFiles => "Preparing local files",
                        TransferPhase::SavingVerifiedContent => "Saving verified content",
                    });
                    let total = if self.job == id { self.total } else { 0 };
                    if event.phase == TransferPhase::PreparingLocalFiles && event.files_total > 0 {
                        ui.add(
                            egui::ProgressBar::new(
                                event.files_done as f32 / event.files_total as f32,
                            )
                            .show_percentage(),
                        );
                    } else if event.phase != TransferPhase::Downloading
                        && event.phase != TransferPhase::WaitingForServer
                    {
                        ui.spinner();
                    } else if total > 0 {
                        ui.add(
                            egui::ProgressBar::new(
                                (event.received_bytes as f64 / total as f64).clamp(0., 1.) as f32,
                            )
                            .show_percentage(),
                        );
                        ui.label(format!(
                            "{} of {}",
                            bytes(event.received_bytes),
                            bytes(total)
                        ));
                    } else {
                        ui.spinner();
                        ui.label(format!("{} received", bytes(event.received_bytes)));
                    }
                    ui.horizontal(|ui| {
                        if let Some(rate) = live.speed(Instant::now()) {
                            ui.label(format!("{}/s", bytes(rate as u64)));
                        }
                        if let Some(eta) = live.eta(total, Instant::now()) {
                            ui.label(format!("About {} s remaining", eta.as_secs()));
                        }
                    });
                    if event.files_total > 0 {
                        ui.label(format!(
                            "{} of {} files complete",
                            event.files_done, event.files_total
                        ));
                    }
                    if let Some(file) = &event.current_file {
                        ui.label(file);
                    }
                } else {
                    ui.heading(if phase.is_empty() {
                        "Checking installation"
                    } else {
                        &phase
                    });
                    ui.spinner();
                }
                if !stopping
                    && (confirm
                        || ui
                            .add_sized(
                                [220., 44.],
                                egui::Button::new("B  Cancel installation").fill(accent),
                            )
                            .clicked())
                {
                    self.halt(false);
                }
            }
        });
    }
}
