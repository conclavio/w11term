use super::*;
use w11term::events::GroupTranscriptHost;

const FAILED_RESULT: &str = "Queue stopped after an unsuccessful host.";
const CANARY_FAILED: &str = "Canary did not report a successful exit.";

pub enum Action {
    SetPaused(bool),
    ApproveCanary,
    Cancel,
}

enum CanaryGate {
    Pending(u64),
    AwaitingApproval,
    Failed,
}

pub(super) struct Queue {
    slots: Arc<Semaphore>,
    stop_on_failure: bool,
    paused: bool,
    canary: Option<u64>,
}

impl Queue {
    pub(super) fn new(limit: usize, stop_on_failure: bool, canary: Option<u64>) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(limit)),
            stop_on_failure,
            paused: false,
            canary,
        }
    }

    fn canary_gate(&self, transcript: &GroupTranscript) -> Option<CanaryGate> {
        let pane = self.canary?;
        Some(match transcript.settled_host_succeeded(pane) {
            None => CanaryGate::Pending(pane),
            Some(true) => CanaryGate::AwaitingApproval,
            Some(false) => CanaryGate::Failed,
        })
    }
}

pub(super) fn canary_readiness(
    targets: &[Target],
    canary: Option<u64>,
) -> Result<(), &'static str> {
    let Some(pane) = canary else {
        return Ok(());
    };
    if !targets
        .iter()
        .any(|target| target.pane == pane && matches!(target.state, TargetState::Ready(_)))
    {
        return Err("The canary must be checked, visible, and at a fresh shell marker.");
    }
    if !targets
        .iter()
        .any(|target| target.pane != pane && target.state.is_ready())
    {
        return Err("Check another ready host to run after the canary.");
    }
    Ok(())
}

pub(super) fn canary_picker(
    id: TabId,
    page: &HostGroup,
    targets: &[Target],
    ctx: &ViewContext<TerminalApp>,
) -> View {
    let mut hosts = vec![(None, "No canary".to_string())];
    hosts.extend(
        targets
            .iter()
            .map(|target| (Some(target.pane), target.name.clone())),
    );
    if let Some(pane) = page
        .canary
        .filter(|pane| !targets.iter().any(|target| target.pane == *pane))
    {
        hosts.push((Some(pane), format!("Canary unavailable · Pane {pane}")));
    }
    StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(8.0)
        .children((
            TextBlock::new()
                .vertical_alignment(VerticalAlignment::Center)
                .text("Canary host"),
            ComboBox::new()
                .automation_name("Canary host")
                .max_width(248.0)
                .is_enabled(
                    !page.recorded_send.pending() && (page.canary.is_some() || targets.len() > 1),
                )
                .items_source(hosts.iter().map(|(_, label)| label.as_str()))
                .selected_index(hosts.iter().position(|(pane, _)| *pane == page.canary))
                .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                    index
                        .and_then(|index| hosts.get(index))
                        .map_or(Message::ControlIdle, |&(pane, _)| {
                            Message::HostGroup(super::Action::CommandCanary(id, pane))
                        })
                })),
        ))
}

pub(super) struct Waiting(Option<GroupTranscriptHost>);

impl Waiting {
    pub(super) fn new(host: GroupTranscriptHost) -> Self {
        Self(Some(host))
    }

    fn reject(&mut self, reason: &str) {
        if let Some(host) = self.0.take() {
            host.delivery_receipt().reject(reason);
        }
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        self.reject("The SSH group closed before sending.");
    }
}

impl Submission {
    pub(super) fn has_queue(&self) -> bool {
        self.results
            .iter()
            .any(|result| matches!(result.status, DeliveryStatus::Queued(_)))
    }

    fn reject_queued_commands(&mut self, reason: &str) {
        for delivery in &mut self.results {
            if let DeliveryStatus::Queued(waiting) = &mut delivery.status {
                waiting.reject(reason);
                delivery.status = DeliveryStatus::Rejected(reason.into());
            }
        }
    }
}

impl TerminalApp {
    pub(super) fn poll_command_queues(&mut self) {
        let groups = self
            .tabs
            .entries()
            .iter()
            .filter_map(|entry| match &entry.value.page {
                Page::HostGroup(page) if page.submissions.iter().any(Submission::has_queue) => {
                    Some(entry.id)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for id in groups {
            let Some(Page::HostGroup(page)) = self.tabs.get_mut(id).map(|tab| &mut tab.page) else {
                continue;
            };
            let mut jobs = Vec::new();
            for run in &mut page.submissions {
                let (Some(queue), SubmissionContent::Command(text)) =
                    (&run.command_queue, &run.content)
                else {
                    continue;
                };
                let only_canary = match queue.canary_gate(&run.transcript) {
                    Some(CanaryGate::Pending(pane)) => Some(pane),
                    Some(CanaryGate::AwaitingApproval) => continue,
                    Some(CanaryGate::Failed) => {
                        run.reject_queued_commands(CANARY_FAILED);
                        continue;
                    }
                    None => None,
                };
                if queue.stop_on_failure && run.transcript.has_unsuccessful_result() {
                    run.reject_queued_commands(FAILED_RESULT);
                    continue;
                }
                if queue.paused {
                    continue;
                }
                let stop_after_failure = queue.stop_on_failure.then(|| run.transcript.clone());
                for (index, delivery) in run.results.iter_mut().enumerate() {
                    if only_canary.is_some_and(|pane| delivery.target.pane != pane) {
                        continue;
                    }
                    let DeliveryStatus::Queued(waiting) = &mut delivery.status else {
                        continue;
                    };
                    let Ok(permit) = queue.slots.clone().try_acquire_owned() else {
                        break;
                    };
                    let mut host = waiting
                        .0
                        .take()
                        .expect("queued command owns its delivery receipt");
                    host.hold_execution_slot(permit);
                    jobs.push((
                        run.id,
                        index,
                        delivery.target.clone(),
                        text.clone(),
                        host,
                        stop_after_failure.clone(),
                    ));
                }
            }
            for (run, index, target, text, host, stop_after_failure) in jobs {
                let delivery = if stop_after_failure
                    .is_some_and(|transcript| transcript.has_unsuccessful_result())
                {
                    host.delivery_receipt().reject(FAILED_RESULT);
                    Delivery {
                        target,
                        status: DeliveryStatus::Rejected(FAILED_RESULT.into()),
                        execution: None,
                        interrupt: None,
                    }
                } else {
                    self.dispatch_to_host(target, text, host)
                };
                if let Some(Page::HostGroup(page)) = self.tabs.get_mut(id).map(|tab| &mut tab.page)
                    && let Some(submission) = page
                        .submissions
                        .iter_mut()
                        .find(|submission| submission.id == run)
                {
                    submission.results[index] = delivery;
                }
            }
        }
    }

    pub(super) fn handle_command_queue(&mut self, id: TabId, run: u64, action: Action) {
        if self.host_group_command_target() != Some(id) {
            return;
        }
        let Some(submission) = self
            .host_group_mut(id)
            .filter(|page| page.viewing == run)
            .and_then(|page| {
                page.submissions
                    .iter_mut()
                    .find(|submission| submission.id == run && submission.has_queue())
            })
        else {
            return;
        };
        let Some(queue) = &mut submission.command_queue else {
            return;
        };
        match action {
            Action::Cancel => submission.reject_queued_commands("Cancelled before sending."),
            Action::SetPaused(paused) if queue.canary.is_none() => {
                queue.paused = paused;
                if !paused {
                    self.poll_command_queues();
                }
            }
            Action::ApproveCanary
                if matches!(
                    queue.canary_gate(&submission.transcript),
                    Some(CanaryGate::AwaitingApproval)
                ) =>
            {
                queue.canary = None;
                self.poll_command_queues();
            }
            _ => {}
        }
    }
}

pub(super) fn controls(id: TabId, submission: &Submission, ctx: &ViewContext<TerminalApp>) -> View {
    let Some(queue) = submission
        .command_queue
        .as_ref()
        .filter(|_| submission.has_queue())
    else {
        return View::empty();
    };
    let run = submission.id;
    let paused = queue.paused;
    let event = move |action| Message::HostGroup(super::Action::CommandQueue(id, run, action));
    let canary_name = || {
        submission
            .results
            .iter()
            .find(|delivery| queue.canary == Some(delivery.target.pane))
            .map_or("Canary host", |delivery| delivery.target.name.as_str())
    };
    let (advance, status) = match queue.canary_gate(&submission.transcript) {
        Some(CanaryGate::Pending(_)) => (
            View::empty(),
            Some(format!("{} · Waiting for the canary result", canary_name())),
        ),
        Some(CanaryGate::AwaitingApproval) => (
            Button::new()
                .on_click(ctx.callback(move |_| event(Action::ApproveCanary)))
                .content("Approve remaining hosts"),
            Some(format!(
                "{} · Canary succeeded. Review its result before continuing.",
                canary_name()
            )),
        ),
        Some(CanaryGate::Failed) => (View::empty(), None),
        None => (
            Button::new()
                .on_click(ctx.callback(move |_| event(Action::SetPaused(!paused))))
                .content(if paused {
                    "Resume queue"
                } else {
                    "Pause queue"
                }),
            paused.then(|| "Command queue paused".to_owned()),
        ),
    };
    let buttons = StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(8.0)
        .children((
            advance,
            Button::new()
                .on_click(ctx.callback(move |_| event(Action::Cancel)))
                .content("Cancel queued commands"),
        ));
    StackPanel::new().spacing(4.0).children((
        status.map_or_else(View::empty, |status| {
            TextBlock::new()
                .text_wrapping(TextWrapping::Wrap)
                .text(status)
                .into()
        }),
        buttons,
    ))
}
