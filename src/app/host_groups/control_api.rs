use super::*;
use serde_json::{Value, json};
use w11term::control::SshGroupOutput;
use w11term::events::GroupCompletionState;

pub(in crate::app) type ControlReply = oneshot::Sender<Result<Value, String>>;

pub(super) struct ReadinessWait {
    checked: BTreeSet<u64>,
    reply: ControlReply,
}

fn all_checked_ready(checked: &BTreeSet<u64>, targets: &[Target]) -> bool {
    !checked.is_empty()
        && targets
            .iter()
            .filter(|target| target.state.is_ready() && checked.contains(&target.pane))
            .count()
            == checked.len()
}

impl HostGroup {
    fn wait_for_readiness(&mut self, label: &str, targets: &[Target], reply: ControlReply) {
        let checked = self.selected_shown(targets);
        if checked.is_empty() {
            let _ = reply.send(Err("No checked, shown SSH hosts in this group".into()));
        } else if all_checked_ready(&checked, targets) {
            let _ = reply.send(Ok(roster_report::roster_json(label, self, targets)));
        } else {
            self.ready_waiters.push(ReadinessWait { checked, reply });
        }
    }

    fn poll_readiness_waiters(&mut self, label: &str, targets: &[Target]) {
        let checked = self.selected_shown(targets);
        let ready = all_checked_ready(&checked, targets);
        let finished: Vec<_> = self
            .ready_waiters
            .extract_if(.., |wait| {
                wait.reply.is_closed() || wait.checked != checked || ready
            })
            .collect();
        let mut snapshot = None;
        for wait in finished {
            if wait.reply.is_closed() {
                continue;
            }
            let result = if wait.checked != checked {
                Err("The checked, shown SSH hosts changed during the readiness wait".into())
            } else {
                Ok(snapshot
                    .get_or_insert_with(|| roster_report::roster_json(label, self, targets))
                    .clone())
            };
            let _ = wait.reply.send(result);
        }
    }
}

impl Submission {
    pub(super) fn refresh_deliveries(&mut self) {
        for delivery in &mut self.results {
            delivery.poll();
        }
    }

    fn poll_control_waiters(&mut self) {
        self.control_waiters.retain(|reply| !reply.is_closed());
        if self.control_waiters.is_empty()
            || self.transcript.completion_state() != GroupCompletionState::Settled
        {
            return;
        }
        self.refresh_deliveries();
        if let Some(snapshot) = report::describe_run(self, true) {
            for reply in std::mem::take(&mut self.control_waiters) {
                let _ = reply.send(Ok(snapshot.clone()));
            }
        }
    }
}

impl TerminalApp {
    pub(in crate::app) fn control_ssh_group_roster(
        &mut self,
        tab: TabId,
        wait_for_readiness: bool,
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
        if wait_for_readiness {
            page.wait_for_readiness(label, &targets, reply);
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
        wait_for_settlement: bool,
        reply: ControlReply,
    ) {
        let Some(AppTab {
            page: Page::HostGroup(page),
            ..
        }) = self.tabs.get_mut(tab)
        else {
            let _ = reply.send(Err(format!("unknown SSH group tab {tab} in this window")));
            return;
        };
        let Some(submission) = page.submissions.iter_mut().find(|item| item.id == run) else {
            let _ = reply.send(Err(format!(
                "SSH group run {run} is not retained in tab {tab}"
            )));
            return;
        };
        if wait_for_settlement {
            submission.control_waiters.push(reply);
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
        group.wait_for_readiness("Fleet", &targets, empty);
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
        group.wait_for_readiness("Fleet", &targets, reply);
        group.wait_for_readiness("Fleet", &targets, cancelled);
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
        run.control_waiters.extend([first, second, cancelled]);
        run.poll_control_waiters();
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
