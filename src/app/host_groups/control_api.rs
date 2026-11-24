use super::*;
use serde_json::{Value, json};
use w11term::events::GroupCompletionState;

pub(in crate::app) type RunReply = oneshot::Sender<Result<Value, String>>;

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
    pub(in crate::app) fn control_ssh_group_roster(&self, tab: TabId) -> Result<Value, String> {
        let Some(AppTab {
            label,
            page: Page::HostGroup(page),
        }) = self.tabs.get(tab)
        else {
            return Err(format!("unknown SSH group tab {tab} in this window"));
        };
        Ok(roster_report::roster_json(
            label,
            page,
            &self.host_group_targets(),
        ))
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
        reply: RunReply,
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
