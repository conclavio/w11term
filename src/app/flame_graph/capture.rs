use super::{Action as ViewerAction, CapturedProfile, Outcome, Profile, message, text};
use crate::app::*;
use std::num::NonZeroU32;
use std::path::PathBuf;
use w11term::{
    control::{CpuCallGraph, CpuProfileOptions},
    ssh::{CaptureStop, CpuLaunchOptions, CpuLaunchPlan, CpuProfilePlan, TriageTarget},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Connection {
    pub pane: u64,
    pub generation: u64,
}

pub enum Action {
    Open,
    Repeat,
    Back,
    Edit(Field, String),
    Mode(usize),
    TargetMode(usize),
    KeepRaw(bool),
    Review,
    Record,
}

#[derive(Clone, Copy)]
pub enum Field {
    Pid,
    Seconds,
    Frequency,
    Program,
    Arguments,
    Directory,
}

#[derive(Clone)]
pub struct Review {
    connection: Connection,
    plan: ReviewedTarget,
}

pub struct Recording {
    target: TriageTarget,
    review: Review,
    path: PathBuf,
    keep_raw: bool,
}

pub(super) struct Repeat {
    review: Review,
    keep_raw: bool,
}

#[derive(Clone)]
enum ReviewedTarget {
    Process(CpuProfilePlan),
    Command(CpuLaunchPlan),
}

enum Options {
    Process(CpuProfileOptions),
    Command(CpuLaunchOptions),
}

enum TargetInput {
    Process(String),
    Command {
        program: String,
        arguments: String,
        directory: String,
    },
}

pub(super) struct Form {
    target: TargetInput,
    seconds: String,
    frequency: String,
    mode: CpuCallGraph,
    keep_raw: bool,
    source: Option<Connection>,
    expected_process: Option<(Connection, u64)>,
    pub review: Option<Review>,
}

impl Form {
    fn new() -> Self {
        Self {
            target: TargetInput::Process(String::new()),
            seconds: CpuProfileOptions::DEFAULT_SECONDS.to_string(),
            frequency: CpuProfileOptions::DEFAULT_FREQUENCY.to_string(),
            mode: CpuCallGraph::Fp,
            keep_raw: false,
            source: None,
            expected_process: None,
            review: None,
        }
    }

    pub fn from_triage(source: Connection, pid: NonZeroU32) -> Self {
        Self {
            target: TargetInput::Process(pid.to_string()),
            source: Some(source),
            ..Self::new()
        }
    }

    fn launches(&self) -> bool {
        matches!(self.target, TargetInput::Command { .. })
    }

    fn options(&self) -> anyhow::Result<Options> {
        let seconds = self
            .seconds
            .trim()
            .parse()
            .context("Enter a duration from 1 to 30 seconds")?;
        let frequency = self
            .frequency
            .trim()
            .parse()
            .context("Enter a frequency from 1 to 199 Hz")?;
        let call_graph = self.mode;
        match &self.target {
            TargetInput::Process(pid) => Ok(Options::Process(
                CpuProfileOptions {
                    target_pid: pid.trim().parse().context("Enter a positive Linux PID")?,
                    seconds,
                    frequency,
                    call_graph,
                }
                .validate()?,
            )),
            TargetInput::Command {
                program,
                arguments,
                directory,
            } => {
                let arguments = if arguments.trim().is_empty() {
                    Vec::new()
                } else {
                    serde_json::from_str(arguments)
                        .context("Arguments must be a JSON array of strings")?
                };
                let options = CpuLaunchOptions {
                    program: program.clone(),
                    directory: directory.clone(),
                    arguments,
                    seconds,
                    frequency,
                    call_graph,
                };
                options.validate()?;
                Ok(Options::Command(options))
            }
        }
    }

    fn available(&self, active: Option<Connection>) -> bool {
        let source = self
            .source
            .or(self.expected_process.map(|(connection, _)| connection));
        active.is_some() && (source.is_none() || source == active)
    }

    pub fn fields(
        &self,
        active: Option<Connection>,
        idle: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> Vec<KeyedView> {
        let input = |key, field, label, value: &str| {
            KeyedView::new(
                key,
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new().text(label),
                    TextBox::new()
                        .automation_name(label)
                        .is_enabled(idle)
                        .text(value)
                        .on_text_changed(
                            ctx.callback(move |value| send(Action::Edit(field, value))),
                        ),
                )),
            )
        };
        let mut fields = vec![KeyedView::new(
            "capture-back",
            Button::new()
                .is_enabled(idle)
                .on_click(ctx.callback(|_| send(Action::Back)))
                .content("Back to flame graph"),
        )];
        if self.source.is_none() {
            fields.push(KeyedView::new(
                "target-mode",
                ComboBox::new()
                    .automation_name("CPU profile target")
                    .is_enabled(idle)
                    .items_source(["Attach to PID", "Launch command"])
                    .selected_index(usize::from(self.launches()))
                    .on_selection_changed(ctx.callback(|index: Option<usize>| {
                        index.map_or(Message::ControlIdle, |index| {
                            send(Action::TargetMode(index))
                        })
                    })),
            ));
        }
        let has_target = match &self.target {
            TargetInput::Process(pid) => {
                fields.push(input("pid", Field::Pid, "Linux process ID", pid));
                !pid.trim().is_empty()
            }
            TargetInput::Command {
                program,
                arguments,
                directory,
            } => {
                fields.extend([
                    input("program", Field::Program, "Linux executable", program),
                    input(
                        "args",
                        Field::Arguments,
                        "Arguments (JSON array)",
                        arguments,
                    ),
                    input(
                        "directory",
                        Field::Directory,
                        "Working directory (defaults to home)",
                        directory,
                    ),
                ]);
                !program.trim().is_empty()
            }
        };
        fields.extend([
            input(
                "seconds",
                Field::Seconds,
                if self.launches() {
                    "Time limit in seconds"
                } else {
                    "Duration in seconds"
                },
                &self.seconds,
            ),
            input(
                "frequency",
                Field::Frequency,
                "Sampling frequency in Hz",
                &self.frequency,
            ),
            KeyedView::new(
                "mode",
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new().text("Call stack unwinding"),
                    ComboBox::new()
                        .automation_name("Call stack unwinding")
                        .is_enabled(idle)
                        .items_source(["Frame pointers", "DWARF"])
                        .selected_index(usize::from(matches!(self.mode, CpuCallGraph::Dwarf)))
                        .on_selection_changed(ctx.callback(|index: Option<usize>| {
                            index.map_or(Message::ControlIdle, |index| send(Action::Mode(index)))
                        })),
                )),
            ),
            KeyedView::new(
                "profile-review",
                Button::new()
                    .is_enabled(idle && self.available(active) && has_target)
                    .on_click(ctx.callback(|_| send(Action::Review)))
                    .content("Review CPU profile"),
            ),
        ]);
        if !self.available(active) {
            fields.push(text(
                "connection-needed",
                if self.source.is_some() {
                    "Return to the SSH connection that collected this triage report."
                } else if self.expected_process.is_some() {
                    "Return to the original SSH connection or enter a PID for the selected pane."
                } else {
                    "Select a connected Linux SSH pane."
                },
            ));
        }
        if let Some(review) = &self.review {
            let ready = Some(review.connection) == active;
            review.plan.describe(&mut fields);
            fields.extend([
                KeyedView::new(
                    "keep-raw",
                    CheckBox::new()
                        .is_enabled(idle)
                        .is_checked(self.keep_raw)
                        .on_is_checked_changed(
                            ctx.callback(|checked| send(Action::KeepRaw(checked))),
                        )
                        .content("Keep raw perf recording"),
                ),
                KeyedView::new(
                    "record",
                    Button::new()
                        .is_enabled(idle && ready)
                        .on_click(ctx.callback(|_| send(Action::Record)))
                        .content("Record CPU profile"),
                ),
            ]);
            if self.keep_raw {
                fields.push(text("raw-destination", format!(
                    "Saves a .perf.data file next to the folded profile. The transfer can take up to {} additional seconds.",
                    w11term::ssh::PERF_DATA_TRANSFER_SECONDS,
                )));
            }
            if !ready {
                fields.push(text(
                    "review-stale",
                    "Return to the reviewed SSH connection or review the target again.",
                ));
            }
        }
        fields
    }
}

impl Repeat {
    fn draft(&self, active: Option<Connection>) -> Form {
        let mut form = Form {
            keep_raw: self.keep_raw,
            ..Form::new()
        };
        let (seconds, frequency, mode) = match &self.review.plan {
            ReviewedTarget::Process(plan) => {
                if active == Some(self.review.connection) {
                    form.target = TargetInput::Process(plan.options.target_pid.to_string());
                    form.expected_process =
                        Some((self.review.connection, plan.process_start_ticks));
                }
                (
                    plan.options.seconds,
                    plan.options.frequency,
                    plan.options.call_graph,
                )
            }
            ReviewedTarget::Command(plan) => {
                form.target = TargetInput::Command {
                    program: plan.options.program.clone(),
                    arguments: serde_json::json!(plan.options.arguments).to_string(),
                    directory: plan.options.directory.clone(),
                };
                (
                    plan.options.seconds,
                    plan.options.frequency,
                    plan.options.call_graph,
                )
            }
        };
        form.seconds = seconds.to_string();
        form.frequency = frequency.to_string();
        form.mode = mode;
        form
    }
}

impl ReviewedTarget {
    fn describe(&self, fields: &mut Vec<KeyedView>) {
        let (host, user, port, key, seconds, frequency, graph, deadline, record_bytes, stack_bytes) =
            match self {
                Self::Process(plan) => {
                    fields.push(text(
                        "process",
                        format!("PID {} · {}", plan.options.target_pid, plan.process_name),
                    ));
                    (
                        &plan.host,
                        &plan.user,
                        plan.port,
                        &plan.host_key_fingerprint,
                        plan.options.seconds,
                        plan.options.frequency,
                        plan.options.call_graph,
                        plan.deadline_seconds,
                        plan.max_recording_bytes,
                        plan.max_stack_output_bytes,
                    )
                }
                Self::Command(plan) => {
                    fields.extend([
                        text("command", format!("Executable {:?}", plan.program)),
                        text(
                            "directory-reviewed",
                            format!("Working directory {:?}", plan.directory),
                        ),
                        text(
                            "args-reviewed",
                            format!("Arguments {}", serde_json::json!(plan.options.arguments)),
                        ),
                    ]);
                    (
                        &plan.host,
                        &plan.user,
                        plan.port,
                        &plan.host_key_fingerprint,
                        plan.options.seconds,
                        plan.options.frequency,
                        plan.options.call_graph,
                        plan.deadline_seconds,
                        plan.max_recording_bytes,
                        plan.max_stack_output_bytes,
                    )
                }
            };
        fields.push(text(
            "endpoint",
            format!("{user}@{host}:{port}\nHost key {key}"),
        ));
        let graph = match graph {
            CpuCallGraph::Fp => "Frame pointers",
            CpuCallGraph::Dwarf => "DWARF",
        };
        fields.push(text("sampling", format!(
            "{seconds} seconds · {frequency} Hz · {graph}\n{deadline}-second deadline · {} MiB recording · {} MiB stack output",
            record_bytes / (1024 * 1024),
            stack_bytes / (1024 * 1024),
        )));
    }
}

impl Options {
    fn preview(self, target: TriageTarget) -> anyhow::Result<ReviewedTarget> {
        match self {
            Self::Process(options) => target
                .preview_cpu_profile_blocking(options)
                .map(ReviewedTarget::Process),
            Self::Command(options) => target
                .preview_cpu_launch_blocking(options)
                .map(ReviewedTarget::Command),
        }
    }
}

fn send(action: Action) -> Message {
    message(ViewerAction::Capture(action))
}

impl TerminalApp {
    pub(in crate::app) fn profile_connection(&self) -> Option<Connection> {
        let session = self.triage_session()?;
        Some(Connection {
            pane: session.pane_id,
            generation: session.ssh_connection_generation()?,
        })
    }

    pub(in crate::app) fn show_triage_process_profile(
        &mut self,
        connection: Connection,
        pid: NonZeroU32,
    ) {
        if self.profile_connection() != Some(connection) || self.flame_graph.pending.is_some() {
            return;
        }
        self.command_reference.close();
        self.set_diagnostics_drawer(Some(diagnostics::Drawer::FlameGraph));
        self.flame_graph.status = None;
        self.flame_graph.capture = Some(Form::from_triage(connection, pid));
    }

    pub(super) fn cpu_capture_action(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        if self.flame_graph.pending.is_some() {
            return;
        }
        match action {
            Action::Open => {
                self.flame_graph.status = None;
                self.flame_graph.capture = Some(Form::new());
            }
            Action::Repeat => {
                let active = self.profile_connection();
                let panel = &mut self.flame_graph;
                if let Some(capture) = panel
                    .visible_graph()
                    .and_then(|graph| graph.capture.as_ref())
                {
                    panel.capture = Some(capture.repeat.draft(active));
                    panel.status = None;
                }
            }
            Action::Back => {
                self.flame_graph.status = None;
                self.flame_graph.capture = None;
            }
            Action::Edit(field, value) => {
                if let Some(form) = &mut self.flame_graph.capture {
                    let current = match (field, &mut form.target) {
                        (Field::Seconds, _) => &mut form.seconds,
                        (Field::Frequency, _) => &mut form.frequency,
                        (Field::Pid, TargetInput::Process(pid)) => pid,
                        (Field::Program, TargetInput::Command { program, .. }) => program,
                        (Field::Arguments, TargetInput::Command { arguments, .. }) => arguments,
                        (Field::Directory, TargetInput::Command { directory, .. }) => directory,
                        _ => return,
                    };
                    if *current != value {
                        *current = value;
                        if matches!(field, Field::Pid) {
                            form.expected_process = None;
                        }
                        form.review = None;
                        self.flame_graph.status = None;
                    }
                }
            }
            Action::TargetMode(index) => {
                if let Some(form) = &mut self.flame_graph.capture
                    && form.source.is_none()
                    && index != usize::from(form.launches())
                {
                    form.target = if index == 1 {
                        TargetInput::Command {
                            program: String::new(),
                            arguments: "[]".into(),
                            directory: String::new(),
                        }
                    } else {
                        TargetInput::Process(String::new())
                    };
                    form.expected_process = None;
                    form.review = None;
                    self.flame_graph.status = None;
                }
            }
            Action::KeepRaw(checked) => {
                if let Some(form) = &mut self.flame_graph.capture {
                    form.keep_raw = checked;
                }
            }
            Action::Mode(index) => {
                if let Some(form) = &mut self.flame_graph.capture
                    && index != usize::from(matches!(form.mode, CpuCallGraph::Dwarf))
                {
                    form.mode = if index == 1 {
                        CpuCallGraph::Dwarf
                    } else {
                        CpuCallGraph::Fp
                    };
                    form.review = None;
                    self.flame_graph.status = None;
                }
            }
            Action::Review => self.review_cpu_capture(ctx),
            Action::Record => self.record_cpu_capture(ctx),
        }
    }

    fn review_cpu_capture(&mut self, ctx: &ComponentContext<Self>) {
        let Some(form) = &self.flame_graph.capture else {
            return;
        };
        let connection = self.profile_connection();
        let Some(connection) = connection.filter(|_| form.available(connection)) else {
            return;
        };
        let expected_start = form.expected_process.map(|(_, ticks)| ticks);
        let result = form.options().and_then(|options| {
            let target = self
                .triage_session()
                .context("Select the SSH pane")?
                .triage_target()?;
            anyhow::ensure!(
                target.connection_generation() == connection.generation,
                "The SSH connection changed. Select the connection and review again."
            );
            Ok((options, target))
        });
        let (options, target) = match result {
            Ok(preview) => preview,
            Err(error) => {
                self.flame_graph.status = Some(format!("{error:#}"));
                return;
            }
        };
        if let Some(form) = &mut self.flame_graph.capture {
            form.review = None;
        }
        let ticket = self.flame_graph.begin("Reviewing the CPU profile target…");
        let rejected = message(ViewerAction::Finished(
            ticket.clone(),
            Err("CPU profile worker unavailable".into()),
        ));
        ctx.spawn_background_with_rejection(
            move |_| {
                let result = (|| -> anyhow::Result<_> {
                    let plan = options.preview(target)?;
                    if let Some(expected) = expected_start {
                        anyhow::ensure!(
                            matches!(&plan, ReviewedTarget::Process(plan) if plan.process_start_ticks == expected),
                            "The original PID now belongs to a different process. Enter the PID you want to profile."
                        );
                    }
                    Ok(Some(Outcome::Reviewed(Box::new(Review { connection, plan }))))
                })().map_err(|error| format!("{error:#}"));
                message(ViewerAction::Finished(ticket, result))
            },
            rejected,
        );
    }

    fn record_cpu_capture(&mut self, ctx: &ComponentContext<Self>) {
        let Some((review, keep_raw)) = self
            .flame_graph
            .capture
            .as_ref()
            .and_then(|form| form.review.clone().map(|review| (review, form.keep_raw)))
        else {
            return;
        };
        if self.profile_connection() != Some(review.connection) {
            return;
        }
        let target = match self
            .triage_session()
            .context("Select the reviewed SSH pane")
            .and_then(Session::triage_target)
        {
            Ok(target) => target,
            Err(error) => {
                self.flame_graph.status = Some(format!("{error:#}"));
                return;
            }
        };
        let Some(owner) = self.keyboard.hwnd().map(|window| window.0 as usize) else {
            return;
        };
        let path = self
            .flame_graph
            .loaded
            .as_ref()
            .map_or_else(|| self.store.path().into(), |loaded| loaded.path.clone());
        let ticket = self.flame_graph.begin("Choosing a Windows file…");
        let rejected = message(ViewerAction::Finished(
            ticket.clone(),
            Err("CPU profile destination picker unavailable".into()),
        ));
        ctx.spawn_background_with_rejection(
            move |scope| {
                let result = (|| {
                    if scope.is_cancelled() {
                        return Ok(None);
                    }
                    Ok(
                        file_picker::pick(owner, file_picker::Purpose::SaveCpuProfile(path))?.map(
                            |file| {
                                Outcome::Destination(Box::new(Recording {
                                    target,
                                    review,
                                    path: file.path,
                                    keep_raw,
                                }))
                            },
                        ),
                    )
                })()
                .map_err(|error: anyhow::Error| format!("{error:#}"));
                message(ViewerAction::Finished(ticket, result))
            },
            rejected,
        );
    }

    pub(super) fn start_cpu_capture(&mut self, recording: Recording, ctx: &ComponentContext<Self>) {
        let Recording {
            target,
            review,
            path,
            keep_raw,
        } = recording;
        let (user, host, port) = match &review.plan {
            ReviewedTarget::Process(plan) => (&plan.user, &plan.host, plan.port),
            ReviewedTarget::Command(plan) => (&plan.user, &plan.host, plan.port),
        };
        let ticket = self.flame_graph.begin("Recording the CPU profile…");
        self.flame_graph.activity = Some(diagnostics::Activity::start(&ticket, user, host, port));
        let stop = CaptureStop::default();
        self.flame_graph.stop = Some(stop.clone());
        let rejected = message(ViewerAction::Finished(
            ticket.clone(),
            Err("CPU profile worker unavailable".into()),
        ));
        ctx.spawn_background_with_rejection(
            move |scope| {
                let result = (|| -> anyhow::Result<_> {
                    if scope.is_cancelled() {
                        return Ok(None);
                    }
                    let raw_path = keep_raw.then(|| path.with_extension("perf.data"));
                    let summary = match &review.plan {
                        ReviewedTarget::Process(plan) => target
                            .save_cpu_profile_artifacts_with_stop_blocking(
                                plan,
                                &path,
                                raw_path.as_deref(),
                                Some(&stop),
                            )?,
                        ReviewedTarget::Command(plan) => target
                            .save_cpu_launch_artifacts_with_stop_blocking(
                                plan,
                                &path,
                                raw_path.as_deref(),
                                Some(&stop),
                            )?,
                    };
                    let profile = Profile::open(&path).with_context(|| {
                        format!(
                            "The profile was saved to {} but could not be opened",
                            path.display()
                        )
                    })?;
                    let capture = Box::new(CapturedProfile {
                        summary,
                        repeat: Repeat { review, keep_raw },
                    });
                    Ok(Some(Outcome::Captured(Arc::new(profile), path, capture)))
                })()
                .map_err(|error| format!("{error:#}"));
                message(ViewerAction::Finished(ticket, result))
            },
            rejected,
        );
    }
}
