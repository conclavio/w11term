use super::*;
use std::time::{Duration, Instant};
use tokio::sync::{
    mpsc,
    oneshot::{self, error::TryRecvError},
};
use w11term::events::{CommandCompletionResult, CommandLink, RunningCommandTarget};

type Selection = (u64, RunningCommandTarget);

pub struct Review {
    selection: Selection,
    label: String,
}

pub enum Action {
    Review(Arc<()>, Selection),
    Send(Arc<Review>),
    Cancel(Arc<Review>),
    ViewCompletion(Arc<()>, CompletedTarget),
}

#[derive(Default)]
pub(super) struct State {
    review: Option<Arc<Review>>,
    status: Option<String>,
    outcome: Option<Outcome>,
}

pub(in crate::app) struct Pending {
    launch: Arc<()>,
    pane: u64,
    response: oneshot::Receiver<Result<(), String>>,
    deadline: Instant,
    completion: CompletionWatch,
}

struct CompletionWatch {
    input: mpsc::UnboundedSender<Command>,
    response: oneshot::Receiver<CommandCompletionResult>,
}

impl CompletionWatch {
    fn start(input: &mpsc::UnboundedSender<Command>, start_token: u64) -> Self {
        let (reply, response) = oneshot::channel();
        let _ = input.send(Command::WaitCommandCapture { start_token, reply });
        Self {
            input: input.clone(),
            response,
        }
    }
}

impl Drop for CompletionWatch {
    fn drop(&mut self) {
        self.response.close();
        let _ = self.input.send(Command::PruneCommandWaits);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CompletedTarget {
    pane: u64,
    link: CommandLink,
    start_token: u64,
}

enum Outcome {
    Waiting {
        pane: u64,
        watch: CompletionWatch,
    },
    Finished {
        target: CompletedTarget,
        exit_code: Option<i32>,
    },
    Unavailable(String),
}

impl Picker {
    pub(in crate::app) fn reviewing_running_interrupt(&self) -> bool {
        self.running_interrupt.review.is_some()
    }

    pub(in crate::app) fn running_controls_height(&self) -> f64 {
        if self.reviewing_running_interrupt() {
            280.0
        } else {
            let outcome = if self.running_interrupt.outcome.is_some() {
                64.0
            } else {
                0.0
            };
            let controls = if self.running_output_open() {
                404.0
            } else {
                296.0
            };
            outcome + controls
        }
    }

    pub(in crate::app) fn running_command_controls(
        &self,
        app: &TerminalApp,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let state = &self.running_interrupt;
        if let Some(review) = &state.review {
            let current = self.selected_running_command() == Some(review.selection)
                && app.interrupt_session(review.selection).is_some();
            let send = review.clone();
            let cancel = review.clone();
            return StackPanel::new().spacing(6.0).children((
                TextBlock::new()
                    .text(if current {
                        format!("Send Ctrl+C to pane {}?", review.selection.0)
                    } else {
                        "The selected command changed or can no longer be interrupted.".into()
                    })
                    .text_wrapping(TextWrapping::Wrap),
                TextBlock::new()
                    .text(&review.label)
                    .text_trimming(TextTrimming::CharacterEllipsis),
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .is_enabled(current)
                            .automation_name("Send Ctrl+C")
                            .on_click(ctx.callback(move |_| {
                                Message::RunningInterrupt(Action::Send(send.clone()))
                            }))
                            .content("Send Ctrl+C  (Ctrl+Enter)"),
                        Button::new()
                            .on_click(ctx.callback(move |_| {
                                Message::RunningInterrupt(Action::Cancel(cancel.clone()))
                            }))
                            .content("Cancel interrupt"),
                    )),
            ));
        }
        let interrupt = self
            .selected_running_command()
            .map_or_else(View::empty, |selection| {
                let launch = self.launch.clone();
                Button::new()
                    .is_enabled(
                        app.running_interrupt_pending.is_none()
                            && app.interrupt_session(selection).is_some(),
                    )
                    .automation_name("Interrupt selected command")
                    .on_click(ctx.callback(move |_| {
                        Message::RunningInterrupt(Action::Review(launch.clone(), selection))
                    }))
                    .content("Interrupt selected command…  (F10)")
            });
        let status = state.status.as_ref().map_or_else(View::empty, |text| {
            TextBlock::new()
                .text(text)
                .text_wrapping(TextWrapping::Wrap)
                .into()
        });
        StackPanel::new().spacing(4.0).children((
            self.running_command_watch_button(app, ctx),
            self.running_output_view(app, ctx),
            interrupt,
            status,
            self.interrupt_outcome_view(app, ctx),
        ))
    }

    fn interrupt_outcome_view(&self, app: &TerminalApp, ctx: &ViewContext<TerminalApp>) -> View {
        let status = match &self.running_interrupt.outcome {
            None => return View::empty(),
            Some(Outcome::Waiting { pane, .. }) => {
                format!("Waiting for completion in pane {pane}…")
            }
            Some(Outcome::Unavailable(error)) => error.clone(),
            Some(Outcome::Finished { target, exit_code }) => {
                let target = *target;
                let status = match exit_code {
                    Some(code) => format!(
                        "Command in pane {} finished with exit code {code}.",
                        target.pane
                    ),
                    None => format!(
                        "Command in pane {} finished. Exit status is unavailable.",
                        target.pane
                    ),
                };
                let launch = self.launch.clone();
                return StackPanel::new().spacing(4.0).children((
                    TextBlock::new()
                        .text(status)
                        .text_wrapping(TextWrapping::Wrap),
                    Button::new()
                        .is_enabled(app.interrupt_history_retained(target))
                        .automation_name("View interrupted command output")
                        .on_click(ctx.callback(move |_| {
                            Message::RunningInterrupt(Action::ViewCompletion(
                                launch.clone(),
                                target,
                            ))
                        }))
                        .content("View command output"),
                ));
            }
        };
        TextBlock::new()
            .text(status)
            .text_wrapping(TextWrapping::Wrap)
            .into()
    }
}

impl TerminalApp {
    fn interrupt_session(&self, (pane, target): Selection) -> Option<&Session> {
        let (id, session) = self.running_command_session(pane, target)?;
        let Page::Terminal(live) = &self.tabs.get(id)?.page else {
            return None;
        };
        (!live.read_only.get() && session.prompts.current().is_none()).then_some(session)
    }

    fn interrupt_history_retained(&self, target: CompletedTarget) -> bool {
        self.history_frame(target.pane).is_some_and(|frame| {
            frame
                .command_for_link(target.link)
                .is_some_and(|command| command.start_token == Some(target.start_token))
        })
    }

    pub(super) fn poll_interrupt_completion(&mut self) {
        let Some(Outcome::Waiting { pane, watch }) = self
            .picker
            .as_mut()
            .and_then(|picker| picker.running_interrupt.outcome.as_mut())
        else {
            return;
        };
        let outcome = match watch.response.try_recv() {
            Ok(Ok(capture)) => match capture.command.start_token {
                Some(start_token) => Outcome::Finished {
                    target: CompletedTarget {
                        pane: *pane,
                        link: CommandLink {
                            generation: capture.connection_generation,
                            id: capture.command.id,
                        },
                        start_token,
                    },
                    exit_code: capture.command.exit_code,
                },
                None => Outcome::Unavailable("The command completion is unavailable.".into()),
            },
            Ok(Err(error)) => {
                Outcome::Unavailable(format!("Could not confirm command completion. {error}"))
            }
            Err(TryRecvError::Closed) => {
                Outcome::Unavailable("The pane closed before confirming command completion.".into())
            }
            Err(TryRecvError::Empty) => return,
        };
        self.picker.as_mut().unwrap().running_interrupt.outcome = Some(outcome);
    }

    pub(in crate::app) fn poll_running_interrupt(&mut self) {
        let Some(pending) = &mut self.running_interrupt_pending else {
            return;
        };
        let result = match pending.response.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Closed) => {
                Err("The pane closed before confirming the Ctrl+C write.".into())
            }
            Err(TryRecvError::Empty) if Instant::now() >= pending.deadline => {
                Err("The Ctrl+C write could not be confirmed.".into())
            }
            Err(TryRecvError::Empty) => return,
        };
        let pending = self.running_interrupt_pending.take().unwrap();
        if let Some(picker) = &mut self.picker
            && Arc::ptr_eq(&picker.launch, &pending.launch)
        {
            picker.running_interrupt.status = Some(match result {
                Ok(()) => {
                    picker.running_interrupt.outcome = Some(Outcome::Waiting {
                        pane: pending.pane,
                        watch: pending.completion,
                    });
                    format!("Ctrl+C sent to pane {}.", pending.pane)
                }
                Err(error) => error,
            });
        } else if let Err(error) = result {
            self.window_error = Some(error);
        }
        self.poll_interrupt_completion();
    }

    pub(in crate::app) fn review_selected_running_interrupt(
        &mut self,
        ctx: &ComponentContext<Self>,
    ) {
        if let Some(picker) = &self.picker
            && let Some(selection) = picker.selected_running_command()
        {
            self.running_interrupt(Action::Review(picker.launch.clone(), selection), ctx);
        }
    }

    pub(in crate::app) fn confirm_selected_running_interrupt(
        &mut self,
        ctx: &ComponentContext<Self>,
    ) {
        if let Some(review) = self
            .picker
            .as_ref()
            .and_then(|picker| picker.running_interrupt.review.clone())
        {
            self.running_interrupt(Action::Send(review), ctx);
        }
    }

    pub(in crate::app) fn cancel_running_interrupt_review(&mut self) -> bool {
        self.picker
            .as_mut()
            .is_some_and(|picker| picker.running_interrupt.review.take().is_some())
    }

    pub(in crate::app) fn running_interrupt(
        &mut self,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        match action {
            Action::Review(launch, selection) => {
                let Some(picker) = &self.picker else { return };
                if !Arc::ptr_eq(&picker.launch, &launch)
                    || picker.selected_running_command() != Some(selection)
                    || picker.running_interrupt.review.is_some()
                    || self.running_interrupt_pending.is_some()
                    || self.interrupt_session(selection).is_none()
                {
                    return;
                }
                let review = Arc::new(Review {
                    selection,
                    label: picker.commands[picker.selected].label.clone(),
                });
                let state = &mut self.picker.as_mut().unwrap().running_interrupt;
                state.review = Some(review);
            }
            Action::Cancel(review) => {
                if let Some(picker) = &mut self.picker {
                    picker
                        .running_interrupt
                        .review
                        .take_if(|current| Arc::ptr_eq(current, &review));
                }
            }
            Action::Send(review) => {
                let Some(picker) = &mut self.picker else {
                    return;
                };
                if picker
                    .running_interrupt
                    .review
                    .take_if(|current| Arc::ptr_eq(current, &review))
                    .is_none()
                {
                    return;
                }
                let selection = picker.selected_running_command();
                let launch = picker.launch.clone();
                let input = selection
                    .filter(|selection| *selection == review.selection)
                    .and_then(|selection| self.interrupt_session(selection))
                    .map(|session| session.input.clone());
                let state = &mut self.picker.as_mut().unwrap().running_interrupt;
                state.outcome = None;
                let Some(input) = input else {
                    state.status =
                        Some("The reviewed command can no longer be interrupted.".into());
                    return;
                };
                let completion = CompletionWatch::start(&input, review.selection.1.start_token);
                let (reply, response) = oneshot::channel();
                if input
                    .send(Command::InterruptCommand {
                        start_token: review.selection.1.start_token,
                        reply,
                    })
                    .is_err()
                {
                    state.status = Some("The pane closed before sending Ctrl+C.".into());
                    return;
                }
                self.running_interrupt_pending = Some(Pending {
                    launch,
                    pane: review.selection.0,
                    response,
                    deadline: Instant::now() + Duration::from_secs(10),
                    completion,
                });
                state.status = Some(format!("Sending Ctrl+C to pane {}…", review.selection.0));
                self.arm_window_tick(ctx);
            }
            Action::ViewCompletion(launch, target) => {
                if self.picker.as_ref().is_some_and(|picker| {
                    Arc::ptr_eq(&picker.launch, &launch)
                        && matches!(picker.running_interrupt.outcome, Some(Outcome::Finished { target: current, .. }) if current == target)
                }) && !(self.interrupt_history_retained(target)
                    && self.open_command_history(target.pane, Some(target.link), ctx))
                {
                    self.picker.as_mut().unwrap().running_interrupt.status =
                        Some("The interrupted command output is no longer retained.".into());
                }
            }
        }
    }
}
