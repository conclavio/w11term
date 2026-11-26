use super::*;
use serde_json::{Value, json};
use w11term::control::SshGroupQueueAction;
use w11term::events::{GroupQueueEventKind, GroupTranscriptHost};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StopReason {
    Failure,
    CanaryFailure,
    Expired,
}

impl StopReason {
    fn message(self) -> &'static str {
        match self {
            Self::Failure => "Queue stopped after an unsuccessful host.",
            Self::CanaryFailure => "Canary did not report a successful exit.",
            Self::Expired => "Queue expired before sending.",
        }
    }

    fn event(self) -> GroupQueueEventKind {
        match self {
            Self::Failure => GroupQueueEventKind::StoppedOnFailure,
            Self::CanaryFailure => GroupQueueEventKind::CanaryFailed,
            Self::Expired => GroupQueueEventKind::Expired,
        }
    }
}

fn deadline_reached(deadline: Option<Instant>, now: Instant) -> bool {
    deadline.is_some_and(|deadline| now >= deadline)
}

struct DispatchJob {
    run: u64,
    index: usize,
    target: Target,
    text: Arc<str>,
    host: GroupTranscriptHost,
    transcript: GroupTranscript,
    stop_on_failure: bool,
    deadline: Option<Instant>,
}

impl DispatchJob {
    fn stop_reason(&self, now: Instant) -> Option<StopReason> {
        if deadline_reached(self.deadline, now) {
            Some(StopReason::Expired)
        } else if self.stop_on_failure && self.transcript.has_unsuccessful_result() {
            Some(StopReason::Failure)
        } else {
            None
        }
    }
}

const SEND_INTERVALS: [(&str, SshGroupCommandInterval); 6] = [
    ("No delay", SshGroupCommandInterval::None),
    ("500 milliseconds", SshGroupCommandInterval::HalfSecond),
    ("1 second", SshGroupCommandInterval::OneSecond),
    ("2 seconds", SshGroupCommandInterval::TwoSeconds),
    ("5 seconds", SshGroupCommandInterval::FiveSeconds),
    ("10 seconds", SshGroupCommandInterval::TenSeconds),
];
const QUEUE_EXPIRIES: [(&str, SshGroupQueueExpiry); 6] = [
    ("Never", SshGroupQueueExpiry::Never),
    ("5 seconds", SshGroupQueueExpiry::FiveSeconds),
    ("15 seconds", SshGroupQueueExpiry::FifteenSeconds),
    ("30 seconds", SshGroupQueueExpiry::ThirtySeconds),
    ("1 minute", SshGroupQueueExpiry::OneMinute),
    ("5 minutes", SshGroupQueueExpiry::FiveMinutes),
];

pub enum Action {
    SetPaused(bool),
    ApproveCanary(u64),
    CancelCanaryNotice,
    SkipHost(u64),
    Cancel,
}

#[derive(Clone, Copy)]
enum CanaryGate {
    Pending(u64),
    AwaitingApproval(u64),
}

pub(super) struct Queue {
    slots: Arc<Semaphore>,
    stop_on_failure: bool,
    paused: bool,
    canary: Option<CanaryGate>,
    notify_canary: bool,
    interval: Duration,
    last_dispatch: Option<Instant>,
    deadline: Option<Instant>,
}

impl Queue {
    pub(super) fn new(
        limit: usize,
        stop_on_failure: bool,
        canary: Option<u64>,
        notify_canary: bool,
        interval: Duration,
        deadline: Option<Instant>,
    ) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(limit)),
            stop_on_failure,
            paused: false,
            canary: canary.map(CanaryGate::Pending),
            notify_canary,
            interval,
            last_dispatch: None,
            deadline,
        }
    }

    pub(super) fn is_canary(&self, pane: u64) -> bool {
        matches!(
            self.canary,
            Some(CanaryGate::Pending(id) | CanaryGate::AwaitingApproval(id)) if id == pane
        )
    }

    fn waiting_for_interval(&self, now: Instant) -> bool {
        self.last_dispatch
            .is_some_and(|last| now.saturating_duration_since(last) < self.interval)
    }
}

pub(super) fn interval_picker(id: TabId, page: &HostGroup, ctx: &ViewContext<TerminalApp>) -> View {
    time_picker(
        "Time between sends",
        page.roster_preferences.command_interval,
        &SEND_INTERVALS,
        !page.recorded_send.pending(),
        move |interval| Message::HostGroup(super::Action::CommandInterval(id, interval)),
        ctx,
    )
}

pub(super) fn expiry_picker(id: TabId, page: &HostGroup, ctx: &ViewContext<TerminalApp>) -> View {
    time_picker(
        "Expire unsent commands",
        page.roster_preferences.command_queue_expiry,
        &QUEUE_EXPIRIES,
        !page.recorded_send.pending(),
        move |expiry| Message::HostGroup(super::Action::CommandQueueExpiry(id, expiry)),
        ctx,
    )
}

fn time_picker<T: Copy + PartialEq + 'static>(
    label: &'static str,
    current: T,
    choices: &'static [(&'static str, T)],
    enabled: bool,
    select: impl Fn(T) -> Message + 'static,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(8.0)
        .children((
            TextBlock::new()
                .vertical_alignment(VerticalAlignment::Center)
                .text(label),
            ComboBox::new()
                .automation_name(label)
                .is_enabled(enabled)
                .items_source(choices.iter().map(|&(name, _)| name))
                .selected_index(choices.iter().position(|&(_, value)| value == current))
                .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                    index
                        .and_then(|index| choices.get(index))
                        .map_or(Message::ControlIdle, |&(_, value)| select(value))
                })),
        ))
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
    StackPanel::new().spacing(4.0).children((
        Grid::new()
            .columns([GridLength::Auto, GridLength::STAR])
            .column_spacing(8.0)
            .children((
                TextBlock::new()
                    .vertical_alignment(VerticalAlignment::Center)
                    .text("Canary host"),
                ComboBox::new()
                    .grid_column(1)
                    .min_width(0.0)
                    .max_width(248.0)
                    .horizontal_alignment(HorizontalAlignment::Left)
                    .automation_name("Canary host")
                    .is_enabled(
                        !page.recorded_send.pending()
                            && (page.canary.is_some() || targets.len() > 1),
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
            )),
        CheckBox::new()
            .automation_name("Notify when canary finishes")
            .is_checked(page.canary.is_some() && page.notify_canary)
            .is_enabled(page.canary.is_some() && !page.recorded_send.pending())
            .on_is_checked_changed(ctx.callback(move |enabled| {
                Message::HostGroup(super::Action::NotifyCanary(id, enabled))
            }))
            .content("Notify when canary finishes"),
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
    pub(super) fn has_unapproved_canary(&self) -> bool {
        self.command_queue
            .as_ref()
            .is_some_and(|queue| queue.canary.is_some())
    }

    pub(super) fn canary_approval_available(&self) -> Result<bool, &'static str> {
        let Some(queue) = &self.command_queue else {
            return Err("This SSH group run has no canary gate");
        };
        let Some(gate) = queue.canary else {
            return Err("This SSH group run has no canary awaiting approval");
        };
        if deadline_reached(queue.deadline, Instant::now()) {
            return Err("The SSH group command queue expired before approval");
        }
        if !self.has_queue() {
            return Err("The canary gate closed before approval");
        }
        Ok(matches!(gate, CanaryGate::AwaitingApproval(_)))
    }

    pub(super) fn queue_snapshot(&self) -> Option<Value> {
        let queue = self.command_queue.as_ref()?;
        let unsent = self
            .results
            .iter()
            .filter(|result| matches!(result.status, DeliveryStatus::Queued(_)))
            .count();
        Some(match (unsent, queue.canary, queue.paused) {
            (0, _, _) => json!({ "state": "drained", "unsent": unsent }),
            (_, Some(CanaryGate::Pending(pane)), _) => {
                json!({ "state": "canary_running", "unsent": unsent, "canary_pane_id": pane })
            }
            (_, Some(CanaryGate::AwaitingApproval(pane)), _) => {
                json!({ "state": "canary_approval", "unsent": unsent, "canary_pane_id": pane })
            }
            (_, None, true) => json!({ "state": "paused", "unsent": unsent }),
            (_, None, false) => json!({ "state": "dispatching", "unsent": unsent }),
        })
    }

    pub(super) fn has_queue(&self) -> bool {
        self.results
            .iter()
            .any(|result| matches!(result.status, DeliveryStatus::Queued(_)))
    }

    fn canary_name(&self, pane: u64) -> &str {
        self.results
            .iter()
            .find(|delivery| delivery.target.pane == pane)
            .map_or("Canary host", |delivery| delivery.target.name.as_str())
    }

    fn stop_queue(&mut self, reason: StopReason) {
        self.transcript.note_queue_activity(reason.event());
        self.reject_queued_commands(reason.message());
    }

    fn reject_queued_commands(&mut self, reason: &str) {
        for delivery in &mut self.results {
            if let DeliveryStatus::Queued(waiting) = &mut delivery.status {
                waiting.reject(reason);
                delivery.status = DeliveryStatus::Rejected(reason.into());
            }
        }
    }

    fn change_queue(&mut self, action: Action, now: Instant) -> Result<bool, &'static str> {
        if !self.has_queue() {
            return Err("This SSH group run has no unsent commands");
        }
        let Some(queue) = &mut self.command_queue else {
            return Err("This SSH group run has no command queue");
        };
        if deadline_reached(queue.deadline, now) {
            queue.notify_canary = false;
            self.stop_queue(StopReason::Expired);
            return Err("The SSH group command queue expired");
        }
        match action {
            Action::Cancel => {
                queue.notify_canary = false;
                self.transcript
                    .note_queue_activity(GroupQueueEventKind::Cancelled);
                self.reject_queued_commands("Cancelled before sending.");
            }
            Action::CancelCanaryNotice => queue.notify_canary = false,
            Action::SkipHost(pane) => {
                if queue.is_canary(pane) {
                    return Err("The active canary cannot be skipped");
                }
                let delivery = self
                    .results
                    .iter_mut()
                    .find(|delivery| delivery.target.pane == pane)
                    .ok_or("The specified pane is not a recipient in this run")?;
                if let DeliveryStatus::Queued(waiting) = &mut delivery.status
                    && let Some(host) = waiting.0.take()
                {
                    host.skip_queued();
                    delivery.status = DeliveryStatus::Skipped;
                    return Ok(true);
                }
                return Err("The specified pane has no queued command to skip");
            }
            Action::SetPaused(_) if queue.canary.is_some() => {
                return Err("Pause and resume are unavailable while the canary is pending");
            }
            Action::SetPaused(paused) if queue.paused != paused => {
                queue.paused = paused;
                let event = if paused {
                    GroupQueueEventKind::Paused
                } else {
                    GroupQueueEventKind::Resumed
                };
                self.transcript.note_queue_activity(event);
                return Ok(!paused);
            }
            Action::ApproveCanary(pane) => {
                match queue.canary {
                    Some(CanaryGate::AwaitingApproval(expected)) if expected == pane => {}
                    Some(CanaryGate::Pending(expected)) if expected == pane => {
                        return Err("The canary has not reported a successful exit yet");
                    }
                    Some(_) => return Err("The specified pane is not the canary for this run"),
                    None => return Err("This SSH group run is not awaiting canary approval"),
                }
                queue.canary = None;
                self.transcript
                    .note_queue_activity(GroupQueueEventKind::CanaryApproved);
                return Ok(true);
            }
            _ => {}
        }
        Ok(false)
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
            let Some(AppTab {
                label,
                page: Page::HostGroup(page),
            }) = self.tabs.get_mut(id)
            else {
                continue;
            };
            let mut jobs = Vec::new();
            for run in page.submissions.iter_mut().filter(|run| run.has_queue()) {
                let (Some(queue), SubmissionContent::Command(text)) =
                    (&mut run.command_queue, &run.content)
                else {
                    continue;
                };
                let now = Instant::now();
                if deadline_reached(queue.deadline, now) {
                    queue.notify_canary = false;
                    run.stop_queue(StopReason::Expired);
                    continue;
                }
                let stopped = queue.stop_on_failure && run.transcript.has_unsuccessful_result();
                let only_canary = match queue.canary {
                    Some(CanaryGate::Pending(pane)) => {
                        let result = run.transcript.settled_host_succeeded(pane);
                        if let Some(succeeded) = result.or(stopped.then_some(false)) {
                            let notify = std::mem::take(&mut queue.notify_canary);
                            if succeeded {
                                queue.canary = Some(CanaryGate::AwaitingApproval(pane));
                                run.transcript
                                    .note_queue_activity(GroupQueueEventKind::CanaryReady);
                            } else {
                                run.stop_queue(if result.is_some() {
                                    StopReason::CanaryFailure
                                } else {
                                    StopReason::Failure
                                });
                            }
                            if notify {
                                self.inbox.group_canary_finished(
                                    id,
                                    run.id,
                                    label,
                                    run.canary_name(pane),
                                    succeeded,
                                );
                            }
                            continue;
                        }
                        Some(pane)
                    }
                    Some(CanaryGate::AwaitingApproval(_)) => continue,
                    None => None,
                };
                if stopped {
                    run.stop_queue(StopReason::Failure);
                    continue;
                }
                if queue.paused || queue.waiting_for_interval(now) {
                    continue;
                }
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
                    jobs.push(DispatchJob {
                        run: run.id,
                        index,
                        target: delivery.target.clone(),
                        text: text.clone(),
                        host,
                        transcript: run.transcript.clone(),
                        stop_on_failure: queue.stop_on_failure,
                        deadline: queue.deadline,
                    });
                    if !queue.interval.is_zero() {
                        break;
                    }
                }
            }
            let mut stopped_runs = BTreeMap::new();
            for job in jobs {
                let reason = stopped_runs
                    .get(&job.run)
                    .copied()
                    .or_else(|| job.stop_reason(Instant::now()));
                let DispatchJob {
                    run,
                    index,
                    target,
                    text,
                    host,
                    transcript,
                    ..
                } = job;
                let delivery = if let Some(reason) = reason {
                    if stopped_runs.insert(run, reason).is_none() {
                        transcript.note_queue_activity(reason.event());
                    }
                    host.delivery_receipt().reject(reason.message());
                    Delivery {
                        target,
                        status: DeliveryStatus::Rejected(reason.message().into()),
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
                    if let DeliveryStatus::Pending(sent_at, _) = &delivery.status
                        && let Some(queue) = &mut submission.command_queue
                        && !queue.interval.is_zero()
                    {
                        queue.last_dispatch = Some(*sent_at);
                    }
                    submission.results[index] = delivery;
                }
            }
            if !stopped_runs.is_empty()
                && let Some(Page::HostGroup(page)) = self.tabs.get_mut(id).map(|tab| &mut tab.page)
            {
                for run in &mut page.submissions {
                    if let Some(reason) = stopped_runs.get(&run.id) {
                        run.reject_queued_commands(reason.message());
                    }
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
                    .find(|submission| submission.id == run)
            })
        else {
            return;
        };
        if submission.change_queue(action, Instant::now()) == Ok(true) {
            self.poll_command_queues();
        }
    }

    pub(in crate::app) fn control_ssh_group_queue(
        &mut self,
        id: TabId,
        run: u64,
        action: SshGroupQueueAction,
    ) -> Result<Value, String> {
        let action = match action {
            SshGroupQueueAction::Pause => Action::SetPaused(true),
            SshGroupQueueAction::Resume => Action::SetPaused(false),
            SshGroupQueueAction::Stop => Action::Cancel,
            SshGroupQueueAction::ApproveCanary { pane_id } => Action::ApproveCanary(pane_id),
            SshGroupQueueAction::Skip { pane_id } => Action::SkipHost(pane_id),
        };
        if self
            .control_group_run(id, run)?
            .change_queue(action, Instant::now())?
        {
            self.poll_command_queues();
        }
        let submission = self.control_group_run(id, run)?;
        submission.refresh_deliveries();
        Ok(report::describe_run(submission, false).expect("an unfiltered run snapshot"))
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
    let (advance, status) = match queue.canary {
        Some(CanaryGate::Pending(pane)) => (
            View::empty(),
            Some(format!(
                "{} · Waiting for the canary result",
                submission.canary_name(pane)
            )),
        ),
        Some(CanaryGate::AwaitingApproval(pane)) => (
            Button::new()
                .on_click(ctx.callback(move |_| event(Action::ApproveCanary(pane))))
                .content("Approve remaining hosts"),
            Some(format!(
                "{} · Canary succeeded. Review its result before continuing.",
                submission.canary_name(pane)
            )),
        ),

        None => (
            Button::new()
                .on_click(ctx.callback(move |_| event(Action::SetPaused(!paused))))
                .content(if paused {
                    "Resume queue"
                } else {
                    "Pause queue"
                }),
            if paused {
                Some("Command queue paused".to_owned())
            } else {
                queue
                    .waiting_for_interval(Instant::now())
                    .then(|| "Waiting for the next send interval".to_owned())
            },
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
            if queue.notify_canary {
                Button::new()
                    .on_click(ctx.callback(move |_| event(Action::CancelCanaryNotice)))
                    .content("Cancel canary notification")
            } else {
                View::empty()
            },
        ));
    StackPanel::new().spacing(4.0).children((
        status.map_or_else(View::empty, |status| {
            TextBlock::new()
                .text_wrapping(TextWrapping::Wrap)
                .text(status)
                .into()
        }),
        queue.deadline.map_or_else(View::empty, |deadline| {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let seconds = remaining.as_secs() + u64::from(remaining.subsec_nanos() != 0);
            TextBlock::new()
                .text(if remaining.is_zero() {
                    "Unsent command deadline reached".into()
                } else {
                    format!("Unsent commands expire in {seconds} s")
                })
                .into()
        }),
        buttons,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_staged_host_is_withheld_at_the_deadline_even_without_stop_on_failure() {
        let start = Instant::now();
        let deadline = start + Duration::from_secs(5);
        let transcript = GroupTranscript::default();
        let job = DispatchJob {
            run: 1,
            index: 0,
            target: Target {
                pane: 1,
                name: "host".into(),
                names: Default::default(),
                profile: None,
                connected: true,
                state: TargetState::Unverified,
            },
            text: "show".into(),
            host: transcript.host(1),
            transcript,
            stop_on_failure: false,
            deadline: Some(deadline),
        };
        assert_eq!(job.stop_reason(deadline - Duration::from_millis(1)), None);
        assert_eq!(job.stop_reason(deadline), Some(StopReason::Expired));
        assert!(!deadline_reached(None, deadline));
    }

    #[test]
    fn a_dispatch_interval_holds_until_its_exact_deadline() {
        let start = Instant::now();
        let interval = Duration::from_secs(2);
        let mut queue = Queue::new(4, false, None, false, interval, None);
        assert!(!queue.waiting_for_interval(start));

        queue.last_dispatch = Some(start);
        assert!(queue.waiting_for_interval(start + interval - Duration::from_millis(1)));
        assert!(!queue.waiting_for_interval(start + interval));

        queue.interval = Duration::ZERO;
        assert!(!queue.waiting_for_interval(start));
    }
}
