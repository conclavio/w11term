use super::*;
use w11term::events::GroupTranscriptHost;

const FAILED_RESULT: &str = "Queue stopped after an unsuccessful host.";

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
                let (Some(slots), SubmissionContent::Command(text)) =
                    (&run.command_slots, &run.content)
                else {
                    continue;
                };
                if run.stop_queue_on_failure && run.transcript.has_unsuccessful_result() {
                    run.reject_queued_commands(FAILED_RESULT);
                    continue;
                }
                let stop_after_failure = run.stop_queue_on_failure.then(|| run.transcript.clone());
                for (index, delivery) in run.results.iter_mut().enumerate() {
                    let DeliveryStatus::Queued(waiting) = &mut delivery.status else {
                        continue;
                    };
                    let Ok(permit) = slots.clone().try_acquire_owned() else {
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

    pub(super) fn cancel_queued_commands(&mut self, id: TabId, run: u64) {
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
        submission.reject_queued_commands("Cancelled before sending.");
    }
}

pub(super) fn cancel_button(
    id: TabId,
    submission: &Submission,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    if !submission.has_queue() {
        return View::empty();
    }
    let run = submission.id;
    Button::new()
        .on_click(ctx.callback(move |_| Message::HostGroup(Action::CancelQueuedCommands(id, run))))
        .content("Cancel queued commands")
}
