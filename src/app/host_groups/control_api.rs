use super::*;
use serde_json::{Value, json};
use w11term::control::{
    SshGroupCommandOptions, SshGroupDispatchOverrides, SshGroupOutput, SshGroupReadiness,
    SshGroupReportFormat, SshGroupReportWriter, SshGroupRunWait,
};
use w11term::events::GroupCompletionState;

pub(in crate::app) type ControlReply = oneshot::Sender<Result<Value, String>>;

pub(super) struct ControlRunWait {
    until: SshGroupRunWait,
    reply: ControlReply,
}

pub struct PreparedControlRecording {
    tab: TabId,
    offer: CommandOffer,
    dispatch: SshGroupDispatchOverrides,
    alert: Option<alerted_send::Alert>,
    file: Result<output_recording::PreparedFile, String>,
    reply: ControlReply,
}

pub(super) struct ReadinessWait {
    panes: BTreeSet<u64>,
    explicit: bool,
    reply: ControlReply,
}

impl ReadinessWait {
    fn outcome(
        &self,
        checked: &BTreeSet<u64>,
        scoped: &BTreeMap<u64, bool>,
    ) -> Option<Result<(), String>> {
        if self.explicit {
            if let Some(pane) = self.panes.iter().find(|pane| !scoped.contains_key(pane)) {
                return Some(Err(format!(
                    "Requested SSH pane {pane} is outside this group's roster"
                )));
            }
        } else if self.panes != *checked {
            return Some(Err(
                "The checked, shown SSH hosts changed during the readiness wait".into(),
            ));
        }
        self.panes
            .iter()
            .all(|pane| scoped.get(pane) == Some(&true))
            .then_some(Ok(()))
    }
}

impl HostGroup {
    fn wait_for_readiness(
        &mut self,
        label: &str,
        targets: &[Target],
        scope: SshGroupReadiness,
        reply: ControlReply,
    ) {
        let (panes, explicit) = match scope {
            SshGroupReadiness::CheckedShown => (self.selected_shown(targets), false),
            SshGroupReadiness::Panes(panes) => (panes.into_iter().collect(), true),
        };
        if panes.is_empty() {
            let _ = reply.send(Err("No checked, shown SSH hosts in this group".into()));
        } else {
            self.ready_waiters.push(ReadinessWait {
                panes,
                explicit,
                reply,
            });
            self.poll_readiness_waiters(label, targets);
        }
    }

    fn poll_readiness_waiters(&mut self, label: &str, targets: &[Target]) {
        let checked = self.selected_shown(targets);
        let scoped = self
            .scoped_targets(targets)
            .map(|target| (target.pane, target.state.is_ready()))
            .collect();
        let mut snapshot = None;
        for wait in std::mem::take(&mut self.ready_waiters) {
            if wait.reply.is_closed() {
                continue;
            }
            if let Some(result) = wait.outcome(&checked, &scoped) {
                let result = result.map(|()| {
                    snapshot
                        .get_or_insert_with(|| roster_report::roster_json(label, self, targets))
                        .clone()
                });
                let _ = wait.reply.send(result);
            } else {
                self.ready_waiters.push(wait);
            }
        }
    }
}

impl Submission {
    pub(super) fn refresh_deliveries(&mut self) {
        for delivery in &mut self.results {
            delivery.poll();
        }
    }

    fn settled_run_snapshot(&mut self) -> Option<Value> {
        if self.transcript.completion_state() != GroupCompletionState::Settled
            || self
                .control_recording
                .as_ref()
                .is_some_and(|recording| !recording.finished())
        {
            return None;
        }
        self.refresh_deliveries();
        report::describe_run(self, true)
    }

    fn poll_control_waiters(&mut self) {
        self.control_waiters.retain(|wait| !wait.reply.is_closed());
        let has_waiter = |until| self.control_waiters.iter().any(|wait| wait.until == until);
        let wait_for_settlement = has_waiter(SshGroupRunWait::Settlement);
        let wait_for_approval = has_waiter(SshGroupRunWait::CanaryApproval);
        let settled = wait_for_settlement
            .then(|| self.settled_run_snapshot())
            .flatten()
            .map(Ok);
        let approval = if wait_for_approval {
            match self.canary_approval_available() {
                Ok(false) => None,
                Ok(true) => {
                    self.refresh_deliveries();
                    report::describe_run(self, false).map(Ok)
                }
                Err(error) => Some(Err(error.into())),
            }
        } else {
            None
        };
        let result = |until| match until {
            SshGroupRunWait::Settlement => settled.as_ref(),
            SshGroupRunWait::CanaryApproval => approval.as_ref(),
        };
        for wait in self
            .control_waiters
            .extract_if(.., |wait| result(wait.until).is_some())
        {
            let _ = wait
                .reply
                .send(result(wait.until).expect("a resolved wait").clone());
        }
    }
}

impl TerminalApp {
    pub(super) fn control_group_run(
        &mut self,
        tab: TabId,
        run: u64,
    ) -> Result<&mut Submission, String> {
        self.host_group_by_id_mut(tab)
            .ok_or_else(|| format!("unknown SSH group tab {tab} in this window"))?
            .submissions
            .iter_mut()
            .find(|submission| submission.id == run)
            .ok_or_else(|| format!("SSH group run {run} is not retained in tab {tab}"))
    }

    pub(in crate::app) fn control_interrupt_ssh_group_command(
        &mut self,
        tab: TabId,
        run: u64,
        pane: u64,
    ) -> Result<Value, String> {
        let submission = self.control_group_run(tab, run)?;
        let index = submission
            .results
            .iter()
            .position(|delivery| delivery.target.pane == pane)
            .ok_or("The specified pane is not a recipient in this run")?;
        let delivery = &mut submission.results[index];
        delivery.poll();
        let target = delivery
            .interrupt_target()
            .ok_or("This pane has no verified running command available to interrupt")?;

        let status = self.dispatch_group_interrupt(pane, target);
        let submission = self.control_group_run(tab, run)?;
        submission.results[index].interrupt = Some(status);
        submission.refresh_deliveries();
        Ok(report::describe_run(submission, false).expect("an unfiltered run snapshot"))
    }

    pub(in crate::app) fn control_capture_ssh_group_report(
        &mut self,
        tab: TabId,
        run: u64,
        format: SshGroupReportFormat,
    ) -> Result<SshGroupReportWriter, String> {
        let page = self
            .host_group_by_id_mut(tab)
            .ok_or_else(|| format!("unknown SSH group tab {tab} in this window"))?;
        let index = page
            .submissions
            .iter()
            .position(|item| item.id == run)
            .ok_or_else(|| format!("SSH group run {run} is not retained in tab {tab}"))?;
        page.submissions[index].refresh_deliveries();

        let Some(Page::HostGroup(page)) = self.tabs.get(tab).map(|tab| &tab.page) else {
            unreachable!("the validated group remains open during capture");
        };
        let report = self.capture_full_host_group_report(&page.submissions[index]);
        Ok(SshGroupReportWriter::new(move |file| match format {
            SshGroupReportFormat::Text => report.write_to(file),
            SshGroupReportFormat::Json => report.write_json(file),
        }))
    }

    pub(in crate::app) fn control_submit_ssh_group_command(
        &mut self,
        tab: TabId,
        pane_ids: &[u64],
        command: String,
        options: SshGroupCommandOptions,
        reply: ControlReply,
        ctx: &ComponentContext<Self>,
    ) {
        let dispatch = options.dispatch;
        let offer = match self.control_command_offer(tab, pane_ids, command.into(), dispatch) {
            Ok(offer) => offer,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        if options.dry_run {
            let canary = |target: &&Target| Some(target.pane) == offer.canary;
            let recipients: Vec<_> = offer
                .targets
                .iter()
                .filter(canary)
                .chain(offer.targets.iter().filter(|target| !canary(target)))
                .map(|target| {
                    json!({
                        "pane_id": target.pane,
                        "label": target.name,
                        "address": target.names.address(),
                        "profile": target.profile,
                    })
                })
                .collect();
            let _ = reply.send(Ok(json!({
                "command": &*offer.text,
                "dispatch_policy": offer.dispatch_policy(),
                "recipients": recipients,
            })));
            return;
        }
        let alert = options
            .expect_output
            .map(|(rule, pattern)| alerted_send::Alert::expected_output(rule, pattern));
        let Some(path) = options.record_output else {
            let result = self.dispatch_control_offer(tab, offer, alert, None);
            let _ = reply.send(result.map(Into::into));
            self.update_host_group_timer(ctx);
            return;
        };
        ctx.spawn_background(move |cancel| {
            if cancel.is_cancelled() || reply.is_closed() {
                return Message::ControlIdle;
            }
            let file = output_recording::PreparedFile::open(path)
                .map_err(|error| format!("Could not prepare the recording · {error:#}"));
            Message::HostGroup(Action::ControlRecordingPrepared(Box::new(
                PreparedControlRecording {
                    tab,
                    offer,
                    dispatch,
                    alert,
                    file,
                    reply,
                },
            )))
        });
    }

    pub(super) fn finish_control_recording(
        &mut self,
        prepared: PreparedControlRecording,
        ctx: &ComponentContext<Self>,
    ) {
        let PreparedControlRecording {
            tab,
            offer,
            dispatch,
            alert,
            file,
            reply,
        } = prepared;
        if reply.is_closed() {
            return;
        }
        let result = file.and_then(|file| {
            let panes: Vec<_> = offer.targets.iter().map(|target| target.pane).collect();
            let latest = self.control_command_offer(tab, &panes, offer.text.clone(), dispatch)?;
            if latest.canary != offer.canary {
                return Err("The SSH canary changed while preparing the recording".into());
            }
            if offer
                .targets
                .iter()
                .zip(&latest.targets)
                .any(|(before, after)| {
                    before.state.prompt() != after.state.prompt() || before.name != after.name
                })
            {
                return Err(
                    "An SSH prompt or recipient changed while preparing the recording".into(),
                );
            }
            self.dispatch_control_offer(tab, latest, alert, Some(file))
        });
        let _ = reply.send(result.map(Into::into));
        self.update_host_group_timer(ctx);
    }

    fn control_command_offer(
        &mut self,
        tab: TabId,
        pane_ids: &[u64],
        command: Arc<str>,
        dispatch: SshGroupDispatchOverrides,
    ) -> Result<CommandOffer, String> {
        let targets = self.host_group_targets();
        let Some(Page::HostGroup(page)) = self.tabs.get_mut(tab).map(|tab| &mut tab.page) else {
            return Err(format!("unknown SSH group tab {tab} in this window"));
        };
        if let Some(run) = page.submissions.back_mut() {
            run.refresh_deliveries();
        }
        if page.sending() || page.recorded_send.pending() {
            return Err("This SSH group still has a submission in progress".into());
        }

        let scoped: BTreeMap<_, _> = page
            .scoped_targets(&targets)
            .map(|target| (target.pane, target))
            .collect();
        let mut recipients = Vec::with_capacity(pane_ids.len());
        for pane in pane_ids {
            let target = scoped
                .get(pane)
                .copied()
                .ok_or_else(|| format!("SSH pane {pane} is outside this group's roster"))?;
            if !target.state.is_ready() {
                return Err(format!(
                    "SSH pane {pane} is not ready. {}",
                    target.state.label()
                ));
            }
            recipients.push(target.clone());
        }
        let canary = dispatch.canary_pane_id.or(page.canary);
        command_queue::canary_readiness(&recipients, canary).map_err(|_| {
            "Include the canary at a fresh shell marker and at least one other ready SSH pane"
                .to_owned()
        })?;
        let mut offer = page.command_offer(command, recipients, canary);
        if let Some(limit) = dispatch.parallel_limit {
            offer.parallel_limit = Some(limit.get());
        }
        if let Some(interval_ms) = dispatch.interval_ms {
            offer.interval = Duration::from_millis(interval_ms);
        }
        if let Some(expiry_ms) = dispatch.queue_expiry_ms {
            offer.queue_expiry = (expiry_ms != 0).then_some(Duration::from_millis(expiry_ms));
        }
        if let Some(stop_on_failure) = dispatch.stop_on_failure {
            offer.stop_queue_on_failure = stop_on_failure;
            // A failure can withhold unsent work only when there is a dispatch limit or delay.
            if stop_on_failure && offer.parallel_limit.is_none() && offer.interval.is_zero() {
                offer.parallel_limit = Some(1);
            }
        }
        offer.queue_deadline(Instant::now())?;
        Ok(offer)
    }

    fn dispatch_control_offer(
        &mut self,
        tab: TabId,
        offer: CommandOffer,
        alert: Option<alerted_send::Alert>,
        file: Option<output_recording::PreparedFile>,
    ) -> Result<u64, String> {
        let transcript = GroupTranscript::default();
        let recording = match file {
            Some(file) => file.start(
                &transcript,
                dispatch_policy::report_heading(&offer.text, Some(&offer.dispatch_policy())),
                output_recording::names(offer.targets.iter()),
            )?,
            None => Default::default(),
        };
        if let Some(alert) = alert {
            self.arm_group_send_alert(tab, &offer, alert, &transcript);
        }
        self.submit_group_command(tab, offer, transcript, recording, SubmissionOrigin::Control)
    }

    pub(in crate::app) fn control_ssh_group_roster(
        &mut self,
        tab: TabId,
        readiness: Option<SshGroupReadiness>,
        reply: ControlReply,
    ) {
        let targets = self.host_group_targets();
        let Some(AppTab {
            label,
            page: Page::HostGroup(page),
        }) = self.tabs.get_mut(tab)
        else {
            let _ = reply.send(Err(format!("unknown SSH group tab {tab} in this window")));
            return;
        };
        if let Some(readiness) = readiness {
            page.wait_for_readiness(label, &targets, readiness, reply);
        } else {
            let _ = reply.send(Ok(roster_report::roster_json(label, page, &targets)));
        }
    }

    fn ssh_group_tab_ids(&self, include: impl Fn(&HostGroup) -> bool) -> Vec<TabId> {
        self.tabs
            .entries()
            .iter()
            .filter_map(|entry| {
                matches!(&entry.value.page, Page::HostGroup(page) if include(page))
                    .then_some(entry.id)
            })
            .collect()
    }

    pub(in crate::app) fn control_ssh_groups(&mut self) -> Value {
        let groups: Vec<_> = self
            .ssh_group_tab_ids(|_| true)
            .into_iter()
            .filter_map(|id| {
                let AppTab {
                    label,
                    page: Page::HostGroup(page),
                } = self.tabs.get_mut(id)?
                else {
                    return None;
                };
                for run in &mut page.submissions {
                    run.refresh_deliveries();
                }
                let runs: Vec<_> = page.submissions.iter().map(report::summarize_run).collect();
                Some(json!({
                    "tab_id": id,
                    "label": label,
                    "runs": runs,
                }))
            })
            .collect();
        json!(groups)
    }

    pub(in crate::app) fn control_ssh_group_run(
        &mut self,
        tab: TabId,
        run: u64,
        wait_for: Option<SshGroupRunWait>,
        reply: ControlReply,
    ) {
        let submission = match self.control_group_run(tab, run) {
            Ok(submission) => submission,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        if let Some(until) = wait_for {
            submission
                .control_waiters
                .push(ControlRunWait { until, reply });
            submission.poll_control_waiters();
        } else {
            submission.refresh_deliveries();
            let snapshot =
                report::describe_run(submission, false).expect("an unfiltered run snapshot");
            let _ = reply.send(Ok(snapshot));
        }
    }

    pub(super) fn poll_ssh_group_waiters(&mut self) {
        let groups = self.ssh_group_tab_ids(|page| {
            page.submissions
                .iter()
                .any(|run| !run.control_waiters.is_empty())
        });
        for group in groups {
            if let Some(Page::HostGroup(page)) = self.tabs.get_mut(group).map(|tab| &mut tab.page) {
                for run in &mut page.submissions {
                    run.poll_control_waiters();
                }
            }
        }
    }

    pub(in crate::app) fn control_ssh_group_output(
        &mut self,
        tab: TabId,
        run: u64,
        pane: Option<u64>,
        follow: bool,
    ) -> Result<SshGroupOutput, String> {
        let Some(AppTab {
            page: Page::HostGroup(page),
            ..
        }) = self.tabs.get_mut(tab)
        else {
            return Err(format!("unknown SSH group tab {tab} in this window"));
        };
        let Some(submission) = page.submissions.iter_mut().find(|item| item.id == run) else {
            return Err(format!("SSH group run {run} is not retained in tab {tab}"));
        };
        let hosts: BTreeMap<_, _> = submission
            .results
            .iter()
            .filter_map(|delivery| {
                let target = &delivery.target;
                pane.is_none_or(|pane| pane == target.pane).then(|| {
                    (
                        target.pane,
                        (
                            target.names.label.clone(),
                            target.names.address().to_owned(),
                        ),
                    )
                })
            })
            .collect();
        if let Some(pane) = pane
            && hosts.is_empty()
        {
            return Err(format!(
                "pane {pane} was not a recipient of SSH group run {run}"
            ));
        }
        let output = submission.transcript.read_output(pane, follow);
        let retained = output.incoming.is_some().then(|| {
            submission
                .output_lifetime
                .get_or_insert_with(|| watch::channel(()).0)
                .subscribe()
        });
        Ok(SshGroupOutput {
            output,
            hosts,
            retained,
        })
    }

    pub(super) fn poll_ssh_group_ready_waiters(&mut self) {
        let groups = self.ssh_group_tab_ids(|page| !page.ready_waiters.is_empty());
        if groups.is_empty() {
            return;
        }
        let targets = self.host_group_targets();
        for group in groups {
            if let Some(AppTab {
                label,
                page: Page::HostGroup(page),
            }) = self.tabs.get_mut(group)
            {
                page.poll_readiness_waiters(label, &targets);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_wait_reports_changes_to_the_checked_shown_set() {
        let target = |pane, name: &str| Target {
            pane,
            name: name.into(),
            names: filter::Names::new(name, "example.test", "ops", 22),
            profile: Some(name.into()),
            connected: true,
            state: TargetState::Unverified,
        };
        let targets = [target(1, "East"), target(2, "West")];
        let mut group = HostGroup::default();
        let (empty, mut empty_reply) = oneshot::channel();
        group.wait_for_readiness("Fleet", &targets, SshGroupReadiness::CheckedShown, empty);
        assert!(
            empty_reply
                .try_recv()
                .unwrap()
                .unwrap_err()
                .contains("No checked, shown")
        );

        group.selected = BTreeSet::from([1, 2]);
        let (reply, mut result) = oneshot::channel();
        let (cancelled, dropped_reply) = oneshot::channel();
        group.wait_for_readiness("Fleet", &targets, SshGroupReadiness::CheckedShown, reply);
        group.wait_for_readiness(
            "Fleet",
            &targets,
            SshGroupReadiness::CheckedShown,
            cancelled,
        );
        drop(dropped_reply);
        group.poll_readiness_waiters("Fleet", &targets);
        assert!(matches!(result.try_recv(), Err(TryRecvError::Empty)));
        assert_eq!(group.ready_waiters.len(), 1);

        group.filter.set_query("east".into());
        group.poll_readiness_waiters("Fleet", &targets);
        assert!(
            result
                .try_recv()
                .unwrap()
                .unwrap_err()
                .contains("hosts changed")
        );
        assert_eq!(group.selected, BTreeSet::from([1, 2]));
        assert!(group.ready_waiters.is_empty());
    }

    #[test]
    fn explicit_readiness_survives_selection_changes_until_a_pane_leaves_the_roster() {
        let west = Target {
            pane: 7,
            name: "West".into(),
            names: filter::Names::default(),
            profile: Some("West".into()),
            connected: true,
            state: TargetState::Unverified,
        };
        let mut group = HostGroup {
            roster: Some(Box::new(MemberRoster {
                panes: BTreeSet::from([7]),
                ..Default::default()
            })),
            selected: BTreeSet::from([7]),
            ..Default::default()
        };
        let (reply, mut result) = oneshot::channel();
        group.wait_for_readiness(
            "Fleet",
            std::slice::from_ref(&west),
            SshGroupReadiness::Panes(vec![7]),
            reply,
        );
        group.selected.clear();
        group.filter.set_query("East".into());
        group.poll_readiness_waiters("Fleet", std::slice::from_ref(&west));
        assert!(matches!(result.try_recv(), Err(TryRecvError::Empty)));

        group.roster.as_mut().unwrap().panes.clear();
        group.poll_readiness_waiters("Fleet", &[west]);
        assert_eq!(
            result.try_recv().unwrap().unwrap_err(),
            "Requested SSH pane 7 is outside this group's roster"
        );
    }

    #[test]
    fn settled_cli_run_waits_for_the_recording_result() {
        use output_recording::{State, WriterStatus};
        use std::path::PathBuf;

        let (writer, worker) = watch::channel(WriterStatus::Recording);
        let mut group = HostGroup::default();
        let run = group.submit(
            SubmissionContent::Command("show".into()),
            Vec::new(),
            GroupTranscript::default(),
        );
        run.control_recording = State::Writing {
            path: PathBuf::from("capture.txt"),
            worker,
            recording: None,
        }
        .control_watcher();
        assert_eq!(
            run.transcript.completion_state(),
            GroupCompletionState::Settled
        );
        let (reply, mut received) = oneshot::channel();
        run.control_waiters.push(ControlRunWait {
            until: SshGroupRunWait::Settlement,
            reply,
        });
        run.poll_control_waiters();
        assert!(matches!(received.try_recv(), Err(TryRecvError::Empty)));

        writer.send_replace(WriterStatus::Finished(Err("disk full".into())));
        run.poll_control_waiters();
        let snapshot = received.try_recv().unwrap().unwrap();
        assert_eq!(
            snapshot["recording"],
            json!({ "path": "capture.txt", "state": "failed", "error": "disk full" })
        );
    }

    #[test]
    fn waiters_receive_the_same_snapshot_after_transcript_and_delivery_settle() {
        let transcript = GroupTranscript::default();
        let host = transcript.host(3);
        let (acknowledge, received) = oneshot::channel();
        let delivery = Delivery {
            target: Target {
                pane: 3,
                name: "fixture".into(),
                names: Default::default(),
                profile: None,
                connected: true,
                state: TargetState::Unverified,
            },
            status: DeliveryStatus::Pending(Instant::now(), received),
            execution: None,
            interrupt: None,
        };
        let mut group = HostGroup::default();
        let run = group.submit(
            SubmissionContent::Command("show".into()),
            vec![delivery],
            transcript,
        );
        let (first, mut first_reply) = oneshot::channel();
        let (second, mut second_reply) = oneshot::channel();
        let (cancelled, cancelled_reply) = oneshot::channel();
        drop(cancelled_reply);
        run.control_waiters
            .extend([first, second, cancelled].map(|reply| ControlRunWait {
                until: SshGroupRunWait::Settlement,
                reply,
            }));
        let (approval, mut approval_reply) = oneshot::channel();
        run.control_waiters.push(ControlRunWait {
            until: SshGroupRunWait::CanaryApproval,
            reply: approval,
        });
        run.poll_control_waiters();
        assert_eq!(
            approval_reply.try_recv().unwrap().unwrap_err(),
            "This SSH group run has no canary gate"
        );
        assert_eq!(run.control_waiters.len(), 2);
        assert!(matches!(first_reply.try_recv(), Err(TryRecvError::Empty)));
        drop(host);
        run.poll_control_waiters();
        assert!(matches!(first_reply.try_recv(), Err(TryRecvError::Empty)));
        acknowledge.send(Err("prompt changed".into())).unwrap();
        run.poll_control_waiters();
        let snapshot = first_reply.try_recv().unwrap().unwrap();
        assert_eq!(snapshot["summary"]["settled_count"], 1);
        assert_eq!(snapshot["recipients"][0]["delivery"]["state"], "rejected");
        assert_eq!(snapshot, second_reply.try_recv().unwrap().unwrap());
        assert!(run.control_waiters.is_empty());
    }
}
