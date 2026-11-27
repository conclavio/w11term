use super::{Action as ViewerAction, Outcome, Profile, message, text};
use crate::app::*;
use std::num::NonZeroU32;
use w11term::{
    control::{CpuCallGraph, CpuProfileOptions},
    ssh::CpuProfilePlan,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Connection {
    pub pane: u64,
    pub generation: u64,
}

pub enum Action {
    Open,
    Back,
    Edit(Field, String),
    Mode(usize),
    Review,
    Record,
}

#[derive(Clone, Copy)]
pub enum Field {
    Pid,
    Seconds,
    Frequency,
}

#[derive(Clone)]
pub struct Review {
    connection: Connection,
    plan: CpuProfilePlan,
}

pub(super) struct Form {
    pid: String,
    seconds: String,
    frequency: String,
    mode: CpuCallGraph,
    source: Option<Connection>,
    pub review: Option<Review>,
}

impl Form {
    fn new() -> Self {
        Self {
            pid: String::new(),
            seconds: CpuProfileOptions::DEFAULT_SECONDS.to_string(),
            frequency: CpuProfileOptions::DEFAULT_FREQUENCY.to_string(),
            mode: CpuCallGraph::Fp,
            source: None,
            review: None,
        }
    }

    pub fn from_triage(source: Connection, pid: NonZeroU32) -> Self {
        Self {
            pid: pid.to_string(),
            source: Some(source),
            ..Self::new()
        }
    }

    fn options(&self) -> anyhow::Result<CpuProfileOptions> {
        CpuProfileOptions {
            target_pid: self
                .pid
                .trim()
                .parse()
                .context("Enter a positive Linux PID")?,
            seconds: self
                .seconds
                .trim()
                .parse()
                .context("Enter a duration from 1 to 30 seconds")?,
            frequency: self
                .frequency
                .trim()
                .parse()
                .context("Enter a frequency from 1 to 199 Hz")?,
            call_graph: self.mode,
        }
        .validate()
    }

    fn available(&self, active: Option<Connection>) -> bool {
        active.is_some() && (self.source.is_none() || self.source == active)
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
        let mut fields = vec![
            KeyedView::new(
                "capture-back",
                Button::new()
                    .is_enabled(idle)
                    .on_click(ctx.callback(|_| send(Action::Back)))
                    .content("Back to flame graph"),
            ),
            input("pid", Field::Pid, "Linux process ID", &self.pid),
            input(
                "seconds",
                Field::Seconds,
                "Duration in seconds",
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
                    .is_enabled(idle && self.available(active) && !self.pid.trim().is_empty())
                    .on_click(ctx.callback(|_| send(Action::Review)))
                    .content("Review CPU profile"),
            ),
        ];
        if !self.available(active) {
            fields.push(text(
                "connection-needed",
                if self.source.is_some() {
                    "Return to the SSH connection that collected this triage report."
                } else {
                    "Select a connected Linux SSH pane."
                },
            ));
        }
        if let Some(review) = &self.review {
            let plan = &review.plan;
            let ready = Some(review.connection) == active;
            fields.extend([
                text(
                    "process",
                    format!("PID {} · {}", plan.options.target_pid, plan.process_name),
                ),
                text(
                    "endpoint",
                    format!(
                        "{}@{}:{}\nHost key {}",
                        plan.user, plan.host, plan.port, plan.host_key_fingerprint
                    ),
                ),
                text(
                    "sampling",
                    format!(
                        "{} seconds · {} Hz · {}\n{}-second deadline · {} MiB recording · {} MiB stack output",
                        plan.options.seconds,
                        plan.options.frequency,
                        match plan.options.call_graph {
                            CpuCallGraph::Fp => "Frame pointers",
                            CpuCallGraph::Dwarf => "DWARF",
                        },
                        plan.deadline_seconds,
                        plan.max_recording_bytes / (1024 * 1024),
                        plan.max_stack_output_bytes / (1024 * 1024),
                    ),
                ),
                KeyedView::new(
                    "record",
                    Button::new()
                        .is_enabled(idle && ready)
                        .on_click(ctx.callback(|_| send(Action::Record)))
                        .content("Record CPU profile"),
                ),
            ]);
            if !ready {
                fields.push(text(
                    "review-stale",
                    "Return to the reviewed SSH connection or review the process again.",
                ));
            }
        }
        fields
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
        self.triage.open = false;
        self.flame_graph.open = true;
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
            Action::Back => {
                self.flame_graph.status = None;
                self.flame_graph.capture = None;
            }
            Action::Edit(field, value) => {
                if let Some(form) = &mut self.flame_graph.capture {
                    let current = match field {
                        Field::Pid => &mut form.pid,
                        Field::Seconds => &mut form.seconds,
                        Field::Frequency => &mut form.frequency,
                    };
                    if *current != value {
                        *current = value;
                        form.review = None;
                        self.flame_graph.status = None;
                    }
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
        let ticket = self.flame_graph.begin("Reviewing the Linux process…");
        let rejected = message(ViewerAction::Finished(
            ticket.clone(),
            Err("CPU profile worker unavailable".into()),
        ));
        ctx.spawn_background_with_rejection(
            move |_| {
                let result = target
                    .preview_cpu_profile_blocking(options)
                    .map(|plan| Some(Outcome::Reviewed(Box::new(Review { connection, plan }))))
                    .map_err(|error| format!("{error:#}"));
                message(ViewerAction::Finished(ticket, result))
            },
            rejected,
        );
    }

    fn record_cpu_capture(&mut self, ctx: &ComponentContext<Self>) {
        let Some(review) = self
            .flame_graph
            .capture
            .as_ref()
            .and_then(|form| form.review.clone())
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
        let ticket = self.flame_graph.begin("Recording the CPU profile…");
        let rejected = message(ViewerAction::Finished(
            ticket.clone(),
            Err("CPU profile worker unavailable".into()),
        ));
        ctx.spawn_background_with_rejection(
            move |scope| {
                let result = (|| -> anyhow::Result<_> {
                    let Some(file) =
                        file_picker::pick(owner, file_picker::Purpose::SaveCpuProfile(path))?
                    else {
                        return Ok(None);
                    };
                    if scope.is_cancelled() {
                        return Ok(None);
                    }
                    let summary = target.save_cpu_profile_blocking(&review.plan, &file.path)?;
                    let profile = Profile::open(&file.path).with_context(|| {
                        format!(
                            "The profile was saved to {} but could not be opened",
                            file.path.display()
                        )
                    })?;
                    Ok(Some(Outcome::Captured(
                        Arc::new(profile),
                        file.path,
                        summary,
                    )))
                })()
                .map_err(|error| format!("{error:#}"));
                message(ViewerAction::Finished(ticket, result))
            },
            rejected,
        );
    }
}
