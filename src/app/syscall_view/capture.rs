use super::*;
use crate::app::flame_graph::Connection;
use std::path::PathBuf;
use w11term::{
    control::{SyscallFilter, SyscallOptions},
    ssh::{SyscallCapture, SyscallCapturePlan, SyscallCaptureStop, TriageTarget},
};

const PRESETS: [(&str, SyscallFilter); 4] = [
    ("All calls", SyscallFilter::All),
    ("Files and descriptors", SyscallFilter::Files),
    ("Processes and signals", SyscallFilter::Processes),
    ("Network and processes", SyscallFilter::Network),
];

pub(in crate::app) struct Candidate {
    source: Connection,
    label: String,
}

#[derive(Clone, Copy)]
pub enum Field {
    Program,
    Arguments,
    Directory,
    Seconds,
    Budget,
    StringSize,
}

pub enum Action {
    Open(TabId),
    Back(TabId),
    Select(TabId, Connection),
    Edit(TabId, Field, String),
    Preset(TabId, usize),
    Review(TabId),
    Choose(TabId),
    Stop(TabId),
    Ready(TabId, Arc<()>, Result<Option<Outcome>, String>),
}

pub enum Outcome {
    Reviewed(Connection, Box<SyscallCapturePlan>),
    Destination(PathBuf),
    Saved(SyscallCapture, Result<Arc<Trace>, String>),
}

struct Review {
    source: Connection,
    plan: SyscallCapturePlan,
}

struct Inputs {
    program: String,
    arguments: String,
    directory: String,
    seconds: String,
    budget: String,
    string_size: String,
    preset: usize,
}

impl Default for Inputs {
    fn default() -> Self {
        Self {
            program: String::new(),
            arguments: "[]".into(),
            directory: String::new(),
            seconds: SyscallOptions::DEFAULT_SECONDS.to_string(),
            budget: SyscallOptions::DEFAULT_MAX_MIB.to_string(),
            string_size: SyscallOptions::DEFAULT_STRING_SIZE.to_string(),
            preset: 0,
        }
    }
}

impl Inputs {
    fn options(&self) -> anyhow::Result<SyscallOptions> {
        let arguments = if self.arguments.trim().is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(&self.arguments)
                .context("Arguments must be a JSON array of strings")?
        };
        SyscallOptions {
            program: self.program.clone(),
            arguments,
            directory: self.directory.clone(),
            seconds: self
                .seconds
                .trim()
                .parse()
                .context("Enter a time limit in seconds")?,
            max_mib: self
                .budget
                .trim()
                .parse()
                .context("Enter a trace output limit in MiB")?,
            string_size: self
                .string_size
                .trim()
                .parse()
                .context("Enter a maximum string size in bytes")?,
            filter: PRESETS[self.preset].1,
        }
        .validate()
    }

    fn field(&mut self, field: Field) -> &mut String {
        match field {
            Field::Program => &mut self.program,
            Field::Arguments => &mut self.arguments,
            Field::Directory => &mut self.directory,
            Field::Seconds => &mut self.seconds,
            Field::Budget => &mut self.budget,
            Field::StringSize => &mut self.string_size,
        }
    }
}

#[derive(Default)]
pub(super) struct ViewState {
    pub(super) open: bool,
    pub(super) activity: Option<diagnostics::Activity>,
    inputs: Inputs,
    source: Option<Connection>,
    review: Option<Review>,
    stop: Option<SyscallCaptureStop>,
}

impl Drop for ViewState {
    fn drop(&mut self) {
        if let Some(stop) = &self.stop {
            stop.request();
        }
    }
}

fn message(action: Action) -> Message {
    send(super::Action::Capture(action))
}

impl TerminalApp {
    pub(in crate::app) fn syscall_capture_targets(&self) -> Vec<Candidate> {
        self.tabs
            .entries()
            .iter()
            .filter_map(|entry| {
                let Page::Terminal(live) = &entry.value.page else {
                    return None;
                };
                let Source::Ssh(options) = &live.source else {
                    return None;
                };
                let generation = live.session.triage_target().ok()?.connection_generation();
                let pane = live.session.pane_id;
                let label = format!(
                    "{}@{}:{} · Pane {pane}",
                    options.user, options.host, options.port
                );
                Some(Candidate {
                    source: Connection { pane, generation },
                    label: safe_text(&label),
                })
            })
            .collect()
    }

    fn target_for_syscall_capture(&self, source: Connection) -> anyhow::Result<TriageTarget> {
        let live = self
            .tab_for_pane(source.pane)
            .and_then(|tab| self.tabs.get(tab))
            .and_then(|tab| match &tab.page {
                Page::Terminal(live) => Some(live),
                _ => None,
            })
            .context("The selected SSH pane is closed")?;
        let target = live.session.triage_target()?;
        anyhow::ensure!(
            target.connection_generation() == source.generation,
            "The selected SSH connection changed. Select it and review again."
        );
        Ok(target)
    }

    pub(super) fn syscall_capture_action(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        match action {
            Action::Open(tab) => {
                let targets = self.syscall_capture_targets();
                if let Some(page) = self.syscall_page(tab).filter(|page| page.pending.is_none()) {
                    let capture = &mut page.capture;
                    capture.open = true;
                    page.status = None;
                    if !targets
                        .iter()
                        .any(|candidate| Some(candidate.source) == capture.source)
                    {
                        capture.source = match targets.as_slice() {
                            [target] => Some(target.source),
                            _ => None,
                        };
                        capture.review = None;
                    }
                }
            }
            Action::Back(tab) => {
                if let Some(page) = self.syscall_capture_input(tab) {
                    page.capture.back();
                    page.status = None;
                }
            }
            Action::Select(tab, source) => {
                if let Some(page) = self.syscall_capture_input(tab)
                    && page.capture.source != Some(source)
                {
                    page.capture.source = Some(source);
                    page.capture.review = None;
                    page.status = None;
                }
            }
            Action::Edit(tab, field, value) => {
                if let Some(page) = self.syscall_capture_input(tab) {
                    let current = page.capture.inputs.field(field);
                    if *current != value {
                        *current = value;
                        page.capture.review = None;
                        page.status = None;
                    }
                }
            }
            Action::Preset(tab, preset) => {
                if let Some(page) = self.syscall_capture_input(tab)
                    && preset < PRESETS.len()
                    && page.capture.inputs.preset != preset
                {
                    page.capture.inputs.preset = preset;
                    page.capture.review = None;
                    page.status = None;
                }
            }
            Action::Review(tab) => self.review_syscall_capture(tab, ctx),
            Action::Choose(tab) => self.choose_syscall_destination(tab, ctx),
            Action::Stop(tab) => {
                if let Some(page) = self.syscall_page(tab)
                    && let Some(stop) = &page.capture.stop
                {
                    stop.request();
                    if let Some(activity) = &mut page.capture.activity {
                        activity.stopping();
                    }
                    page.status =
                        Some("Stop requested. Saving any collected trace records…".into());
                }
            }
            Action::Ready(tab, ticket, result) => {
                let Some(page) = self.syscall_page(tab) else {
                    return;
                };
                if !page.finish(&ticket) {
                    return;
                }
                page.capture.stop = None;
                if let Some(activity) = &mut page.capture.activity {
                    activity.finish(&ticket, &result, |outcome| match outcome {
                        Outcome::Saved(capture, trace) => Some(capture_summary(capture, trace)),
                        _ => None,
                    });
                }
                match result {
                    Ok(Some(Outcome::Reviewed(source, plan))) => {
                        page.capture.review = Some(Review {
                            source,
                            plan: *plan,
                        });
                    }
                    Ok(Some(Outcome::Destination(path))) => {
                        self.start_syscall_capture(tab, path, ctx)
                    }
                    Ok(Some(Outcome::Saved(capture, trace))) => {
                        page.capture.review = None;
                        let summary = capture_summary(&capture, &trace);
                        if let Ok(trace) = trace {
                            page.load(trace);
                        }
                        page.status = Some(summary);
                        self.remember_diagnostic(
                            w11term::diagnostic_history::Kind::SyscallTrace,
                            [capture.path],
                            ctx,
                        );
                    }
                    Ok(None) => (),
                    Err(error) => page.status = Some(error),
                }
            }
        }
    }

    fn syscall_capture_input(&mut self, tab: TabId) -> Option<&mut PageState> {
        self.syscall_page(tab)
            .filter(|page| page.capture.open && page.pending.is_none())
    }

    fn review_syscall_capture(&mut self, tab: TabId, ctx: &ComponentContext<Self>) {
        let draft = self.syscall_capture_input(tab).and_then(|page| {
            page.capture.review = None;
            page.capture
                .source
                .map(|source| (source, page.capture.inputs.options()))
        });
        let Some((source, options)) = draft else {
            return;
        };
        let result =
            options.and_then(|options| Ok((self.target_for_syscall_capture(source)?, options)));
        let Some(page) = self.syscall_capture_input(tab) else {
            return;
        };
        let (target, options) = match result {
            Ok(value) => value,
            Err(error) => {
                page.status = Some(format!("{error:#}"));
                return;
            }
        };
        let ticket = page.begin("Reviewing the command and Linux tracing permissions…");
        run(ctx, tab, ticket, move || {
            let plan = target.preview_syscall_capture_blocking(options)?;
            Ok(Some(Outcome::Reviewed(source, Box::new(plan))))
        });
    }

    fn choose_syscall_destination(&mut self, tab: TabId, ctx: &ComponentContext<Self>) {
        if self.tabs.active_id() != Some(tab) {
            return;
        }
        let Some(owner) = self.keyboard.hwnd().map(|window| window.0 as usize) else {
            return;
        };
        let profile_path = self.store.path().to_owned();
        let Some(page) = self.syscall_capture_input(tab) else {
            return;
        };
        let Some(review) = &page.capture.review else {
            return;
        };
        let source = review.source;
        let initial = page
            .loaded
            .as_ref()
            .map_or(profile_path, |trace| trace.first_path().to_owned());
        if let Err(error) = self.target_for_syscall_capture(source) {
            if let Some(page) = self.syscall_capture_input(tab) {
                page.status = Some(format!("{error:#}"));
            }
            return;
        }
        let Some(page) = self.syscall_capture_input(tab) else {
            return;
        };
        let ticket = page.begin("Choosing a Windows destination…");
        run(ctx, tab, ticket, move || {
            Ok(
                file_picker::pick(owner, file_picker::Purpose::SaveSyscallTrace(initial))?
                    .map(|file| Outcome::Destination(file.path)),
            )
        });
    }

    fn start_syscall_capture(&mut self, tab: TabId, path: PathBuf, ctx: &ComponentContext<Self>) {
        let Some((source, plan)) = self
            .syscall_capture_input(tab)
            .and_then(|page| page.capture.review.as_ref())
            .map(|review| (review.source, review.plan.clone()))
        else {
            return;
        };
        let target = self.target_for_syscall_capture(source);
        let Some(page) = self.syscall_capture_input(tab) else {
            return;
        };
        let target = match target {
            Ok(target) => target,
            Err(error) => {
                page.status = Some(format!("{error:#}"));
                return;
            }
        };
        let stop = SyscallCaptureStop::default();
        page.capture.stop = Some(stop.clone());
        let ticket = page
            .begin("Tracing the remote command. The completed file will be saved and opened here.");
        page.capture.activity = Some(diagnostics::Activity::start(
            &ticket, &plan.user, &plan.host, plan.port,
        ));
        run(ctx, tab, ticket, move || {
            let capture = target.save_syscall_capture_blocking(source.pane, plan, path, stop)?;
            let trace = Trace::open(vec![capture.path.clone()])
                .map(Arc::new)
                .map_err(|error| format!("{error:#}"));
            Ok(Some(Outcome::Saved(capture, trace)))
        });
    }
}

fn run(
    ctx: &ComponentContext<TerminalApp>,
    tab: TabId,
    ticket: Arc<()>,
    job: impl FnOnce() -> anyhow::Result<Option<Outcome>> + Send + 'static,
) {
    let rejected = message(Action::Ready(
        tab,
        ticket.clone(),
        Err("Syscall capture worker unavailable".into()),
    ));
    ctx.spawn_background_with_rejection(
        move |scope| {
            let result = if scope.is_cancelled() {
                Ok(None)
            } else {
                job()
            }
            .map_err(|error| format!("{error:#}"));
            message(Action::Ready(tab, ticket, result))
        },
        rejected,
    );
}

fn capture_summary(capture: &SyscallCapture, trace: &Result<Arc<Trace>, String>) -> String {
    let reason = match capture.stop_reason {
        "stopped" => "Stopped by request".into(),
        "time-limit" => "Time limit reached".into(),
        "byte-budget" => "Byte budget reached. The final record may be incomplete.".into(),
        _ => capture.command_exit_status.map_or_else(
            || "Command completed".into(),
            |code| format!("Command exit status {code}"),
        ),
    };
    let mut summary = format!(
        "Saved syscall trace\n{reason}\n{} bytes\n{}",
        capture.bytes,
        safe_text(&capture.path.display().to_string())
    );
    if let Err(error) = trace {
        summary.push_str(&format!("\nOpening the saved trace: {error}"));
    }
    summary
}

impl ViewState {
    pub(super) fn offer(&mut self, source: Option<Connection>) {
        if !self.open && self.review.is_none() && source.is_some() {
            self.source = source;
        }
    }

    pub(super) fn back(&mut self) {
        self.open = false;
        self.review = None;
        self.stop = None;
    }

    pub(super) fn switch(
        &self,
        tab: TabId,
        idle: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> KeyedView {
        KeyedView::new(
            "syscall-capture",
            Button::new()
                .is_enabled(idle)
                .on_click(ctx.callback({
                    let open = self.open;
                    move |_| {
                        message(if open {
                            Action::Back(tab)
                        } else {
                            Action::Open(tab)
                        })
                    }
                }))
                .content(if self.open {
                    "Back to saved traces"
                } else {
                    "Capture over SSH…"
                }),
        )
    }

    pub(super) fn view(
        &self,
        tab: TabId,
        targets: Vec<Candidate>,
        mut header: Vec<KeyedView>,
        idle: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let selected = targets
            .iter()
            .position(|target| Some(target.source) == self.source);
        if let Some(stop) = &self.stop {
            header.push(KeyedView::new(
                "stop-capture",
                Button::new()
                    .is_enabled(!stop.is_requested())
                    .on_click(ctx.callback(move |_| message(Action::Stop(tab))))
                    .content("Stop syscall capture"),
            ));
        }
        let mut fields = Vec::new();
        if targets.is_empty() {
            fields.push(text(
                "connect",
                "Connect a Linux SSH pane to review a command.",
            ));
        } else {
            let labels = targets
                .iter()
                .map(|candidate| candidate.label.clone())
                .collect::<Vec<_>>();
            let identities = targets
                .into_iter()
                .map(|candidate| candidate.source)
                .collect::<Vec<_>>();
            fields.push(KeyedView::new(
                "ssh-target",
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new().text("SSH pane"),
                    ComboBox::new()
                        .automation_name("Syscall capture SSH pane")
                        .items_source(labels)
                        .selected_index(selected)
                        .is_enabled(idle)
                        .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                            index
                                .and_then(|index| identities.get(index).copied())
                                .map_or(Message::ControlIdle, |source| {
                                    message(Action::Select(tab, source))
                                })
                        })),
                )),
            ));
        }
        let input = |key, label, field, value: &str| {
            KeyedView::new(
                key,
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new()
                        .text(label)
                        .text_wrapping(TextWrapping::Wrap),
                    TextBox::new()
                        .automation_name(label)
                        .is_enabled(idle)
                        .text(value)
                        .on_text_changed(
                            ctx.callback(move |value| message(Action::Edit(tab, field, value))),
                        ),
                )),
            )
        };
        fields.extend([
            input(
                "program",
                "Linux executable",
                Field::Program,
                &self.inputs.program,
            ),
            input(
                "arguments",
                "Arguments (JSON array)",
                Field::Arguments,
                &self.inputs.arguments,
            ),
            input(
                "directory",
                "Working directory (optional)",
                Field::Directory,
                &self.inputs.directory,
            ),
            input(
                "seconds",
                "Time limit (seconds)",
                Field::Seconds,
                &self.inputs.seconds,
            ),
            input(
                "budget",
                "Trace output limit (MiB)",
                Field::Budget,
                &self.inputs.budget,
            ),
            input(
                "string-size",
                "Maximum string bytes",
                Field::StringSize,
                &self.inputs.string_size,
            ),
            KeyedView::new(
                "preset",
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new().text("Syscall preset"),
                    ComboBox::new()
                        .automation_name("Syscall preset")
                        .items_source(PRESETS.map(|(label, _)| label))
                        .selected_index(self.inputs.preset)
                        .is_enabled(idle)
                        .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                            index.map_or(Message::ControlIdle, |index| {
                                message(Action::Preset(tab, index))
                            })
                        })),
                )),
            ),
            KeyedView::new(
                "review",
                Button::new()
                    .is_enabled(
                        idle && selected.is_some() && !self.inputs.program.trim().is_empty(),
                    )
                    .on_click(ctx.callback(move |_| message(Action::Review(tab))))
                    .content("Review syscall capture"),
            ),
        ]);
        if let Some(review) = &self.review {
            let plan = &review.plan;
            let valid = selected.is_some() && Some(review.source) == self.source;
            fields.extend([
                text(
                    "endpoint",
                    safe_text(&format!(
                        "{}@{}:{} · Pane {}",
                        plan.user, plan.host, plan.port, review.source.pane
                    )),
                ),
                text(
                    "host-key",
                    safe_text(&format!("Host key {}", plan.host_key_fingerprint)),
                ),
                text(
                    "program-reviewed",
                    safe_text(&format!("Executable {}", plan.program)),
                ),
                text(
                    "directory-reviewed",
                    safe_text(&format!("Working directory {}", plan.directory)),
                ),
                text(
                    "bounds",
                    format!(
                        "{} seconds · {} MiB · {} bytes per string",
                        plan.options.seconds, plan.options.max_mib, plan.options.string_size
                    ),
                ),
                text(
                    "arguments-reviewed",
                    safe_text(&format!(
                        "strace {}",
                        serde_json::json!(&plan.strace_arguments)
                    )),
                ),
                KeyedView::new(
                    "save",
                    Button::new()
                        .is_enabled(idle && valid)
                        .on_click(ctx.callback(move |_| message(Action::Choose(tab))))
                        .content("Trace and save…"),
                ),
            ]);
            if !valid {
                fields.push(text(
                    "stale",
                    "Select an active SSH connection and review again.",
                ));
            }
        }
        Grid::new()
            .rows([GridLength::Auto, GridLength::Star(1.0)])
            .row_spacing(12.0)
            .margin(12.0)
            .children((
                StackPanel::new().spacing(8.0).keyed_children(header),
                ScrollViewer::new().grid_row(1).content(
                    StackPanel::new()
                        .spacing(8.0)
                        .max_width(760.0)
                        .keyed_children(fields),
                ),
            ))
    }
}

fn text(key: &str, value: impl Into<String>) -> KeyedView {
    KeyedView::new(
        key,
        TextBlock::new()
            .text(value.into())
            .text_wrapping(TextWrapping::Wrap),
    )
}
