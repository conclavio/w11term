use super::*;
use serde_json::{Value, json};

impl TerminalApp {
    pub(in crate::app) fn control_ssh_group_retry_plan(
        &mut self,
        tab: TabId,
        run: u64,
    ) -> Result<Value, String> {
        let targets = self.host_group_targets();
        let roster = self.retry_roster(tab, &targets)?;
        let submission = self.control_group_run(tab, run)?;
        submission.refresh_deliveries();
        unsent_retry_plan(submission, &roster)
    }

    pub(in crate::app) fn control_retry_ssh_group_run(
        &mut self,
        tab: TabId,
        source: u64,
        pane_ids: &[u64],
    ) -> Result<u64, String> {
        let targets = self.host_group_targets();
        let roster = self.retry_roster(tab, &targets)?;
        let submission = self.control_group_run(tab, source)?;
        submission.refresh_deliveries();
        let command: Arc<str> = retry_command(submission)?.into();
        let unsent: BTreeMap<_, _> = submission
            .results
            .iter()
            .filter(|delivery| matches!(delivery.status, DeliveryStatus::Rejected(_)))
            .map(|delivery| (delivery.target.pane, &delivery.target))
            .collect();
        for &pane in pane_ids {
            let original = unsent.get(&pane).ok_or_else(|| {
                format!("Pane {pane} has no confirmed unsent command in this run")
            })?;
            if let Some(reason) = retry_blocker(submission, original, roster.get(&pane).copied()) {
                return Err(format!("Pane {pane} cannot be retried. {reason}"));
            }
        }

        let offer = self.control_command_offer(tab, pane_ids, command, Default::default())?;
        let run = self.dispatch_control_offer(tab, offer, None, None)?;
        if let Ok(submission) = self.control_group_run(tab, source) {
            submission
                .unsent_retries
                .extend(pane_ids.iter().map(|&pane| (pane, run)));
        }
        Ok(run)
    }

    fn retry_roster<'a>(
        &self,
        tab: TabId,
        targets: &'a [Target],
    ) -> Result<BTreeMap<u64, &'a Target>, String> {
        let Some(Page::HostGroup(page)) = self.tabs.get(tab).map(|entry| &entry.page) else {
            return Err(format!("unknown SSH group tab {tab} in this window"));
        };
        Ok(page
            .scoped_targets(targets)
            .map(|target| (target.pane, target))
            .collect())
    }
}

fn retry_command(submission: &Submission) -> Result<&str, &'static str> {
    submission
        .content
        .command()
        .ok_or("This SSH group run did not submit a shell command")
}

fn retry_blocker(
    submission: &Submission,
    original: &Target,
    current: Option<&Target>,
) -> Option<Cow<'static, str>> {
    if let Some(run) = submission.unsent_retries.get(&original.pane) {
        return Some(format!("Already retried in run {run}").into());
    }
    if submission.has_unapproved_canary() {
        return Some("The source run's canary was not approved".into());
    }
    match current {
        None => Some("The pane is outside this group's current roster".into()),
        Some(target) if target.names.address() != original.names.address() => {
            Some("The pane's SSH destination changed".into())
        }
        Some(target) if !target.state.is_ready() => Some(target.state.label()),
        Some(_) => None,
    }
}

fn unsent_retry_plan(
    submission: &Submission,
    roster: &BTreeMap<u64, &Target>,
) -> Result<Value, String> {
    let command = retry_command(submission)?;
    let mut ready_pane_ids = Vec::new();
    let mut recipients = Vec::new();
    for delivery in &submission.results {
        let DeliveryStatus::Rejected(reason) = &delivery.status else {
            continue;
        };
        let original = &delivery.target;
        let block_reason = retry_blocker(submission, original, roster.get(&original.pane).copied());
        let ready = block_reason.is_none();
        if ready {
            ready_pane_ids.push(original.pane);
        }
        recipients.push(json!({
            "pane_id": original.pane,
            "label": original.names.label,
            "address": original.names.address(),
            "delivery_reason": reason,
            "ready": ready,
            "block_reason": block_reason,
            "retry_run_id": submission.unsent_retries.get(&original.pane),
        }));
    }
    Ok(json!({
        "command": command,
        "recipients": recipients,
        "ready_pane_ids": ready_pane_ids,
    }))
}

fn retryable(delivery: &Delivery) -> bool {
    match delivery.status {
        DeliveryStatus::Rejected(_) => true,
        DeliveryStatus::Sent => matches!(
            delivery.execution(),
            Some(GroupExecution::Finished(result)) if result.exit_code.is_some_and(|code| code != 0)
        ),
        DeliveryStatus::Queued(_)
        | DeliveryStatus::Pending(..)
        | DeliveryStatus::Unconfirmed
        | DeliveryStatus::Skipped => false,
    }
}

impl HostGroup {
    fn retry_candidates(
        &self,
        submission: &Submission,
        visible: impl Iterator<Item = u64>,
    ) -> BTreeSet<u64> {
        if submission.content.command().is_none() {
            return BTreeSet::new();
        }
        let candidates: BTreeSet<_> = submission
            .results
            .iter()
            .filter(|delivery| {
                retryable(delivery) && self.matches_result(delivery, progress::Outcome::Attention)
            })
            .map(|delivery| delivery.target.pane)
            .collect();
        visible.filter(|pane| candidates.contains(pane)).collect()
    }

    pub(super) fn prepare_retry(
        &mut self,
        id: u64,
        roster: &[Target],
        offered: BTreeSet<u64>,
    ) -> bool {
        if offered.is_empty() {
            return false;
        }
        let Some(submission) = self.viewed_submission(id) else {
            return false;
        };
        let Some(command) = submission.content.command() else {
            return false;
        };
        let eligible =
            self.retry_candidates(submission, self.shown(roster).map(|target| target.pane));
        if eligible != offered {
            self.retry_feedback =
                "The retry targets changed. Review the results and prepare the retry again.".into();
            return false;
        }
        self.load_scoped_draft(command.to_owned(), offered);
        true
    }
}

pub(super) fn button(
    id: TabId,
    page: &HostGroup,
    submission: &Submission,
    visible: impl Iterator<Item = u64>,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    if submission.content.command().is_none() {
        return View::empty();
    }
    let offered = page.retry_candidates(submission, visible);
    let submission = submission.id;
    let caption = if offered.is_empty() {
        "Prepare retry".into()
    } else {
        format!("Prepare retry ({})", offered.len())
    };
    Button::new()
        .automation_name("Prepare retry")
        .is_enabled(!offered.is_empty())
        .on_click(ctx.callback(move |_| {
            Message::HostGroup(Action::PrepareRetry(id, submission, offered.clone()))
        }))
        .content(caption)
}

pub(super) fn feedback(page: &HostGroup) -> View {
    let label = if !page.retry_feedback.is_empty() {
        &page.retry_feedback
    } else if page
        .previous_draft
        .as_ref()
        .is_some_and(|backup| backup.scope.is_some())
    {
        "Draft loaded. Review the command and checked hosts before sending."
    } else {
        return View::empty();
    };
    TextBlock::new()
        .text(label)
        .text_wrapping(TextWrapping::Wrap)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use w11term::events::GroupCommandResult;

    fn target(pane: u64, name: &str) -> Target {
        Target {
            pane,
            name: name.into(),
            names: filter::Names::new(name, "example.test", "ops", 22),
            profile: None,
            connected: false,
            state: TargetState::Offline,
        }
    }

    fn delivery(
        target: Target,
        status: DeliveryStatus,
        execution: Option<GroupExecution>,
    ) -> Delivery {
        Delivery {
            target,
            status,
            execution: execution.map(|value| watch::channel(value).1),
            interrupt: None,
        }
    }

    fn finished(code: Option<i32>) -> GroupExecution {
        GroupExecution::Finished(Arc::new(GroupCommandResult {
            link: CommandLink {
                generation: 1,
                id: 1,
            },
            exit_code: code,
            duration: Duration::ZERO,
            output: GroupOutput::Unavailable("fixture"),
        }))
    }

    #[test]
    fn unsent_retry_plan_excludes_uncertain_or_sent_commands_and_checks_live_destinations() {
        let mut group = HostGroup::default();
        let run = group.submit(
            SubmissionContent::Command("show  status".into()),
            vec![
                delivery(
                    target(1, "withheld"),
                    DeliveryStatus::Rejected("stopped".into()),
                    None,
                ),
                delivery(target(2, "unconfirmed"), DeliveryStatus::Unconfirmed, None),
                delivery(
                    target(3, "sent"),
                    DeliveryStatus::Sent,
                    Some(finished(Some(17))),
                ),
                delivery(target(4, "skipped"), DeliveryStatus::Skipped, None),
                delivery(
                    target(5, "closed"),
                    DeliveryStatus::Rejected("closed".into()),
                    None,
                ),
            ],
            GroupTranscript::default(),
        );
        let mut changed = target(1, "withheld");
        changed.names = filter::Names::new("withheld", "other.test", "ops", 22);
        let plan = unsent_retry_plan(run, &BTreeMap::from([(1, &changed)])).unwrap();
        assert_eq!(plan["command"], "show  status");
        assert_eq!(plan["ready_pane_ids"], json!([]));
        assert_eq!(plan["recipients"].as_array().unwrap().len(), 2);
        assert_eq!(plan["recipients"][0]["pane_id"], 1);
        assert_eq!(plan["recipients"][0]["delivery_reason"], "stopped");
        assert_eq!(
            plan["recipients"][0]["block_reason"],
            "The pane's SSH destination changed"
        );
        assert_eq!(plan["recipients"][1]["pane_id"], 5);
        assert_eq!(
            plan["recipients"][1]["block_reason"],
            "The pane is outside this group's current roster"
        );
    }

    #[test]
    fn retry_selects_only_visible_confirmed_failures_or_refusals_and_restores_the_working_scope() {
        let targets = [
            target(1, "west returned failure"),
            target(2, "west withheld"),
            target(3, "west completed"),
            target(4, "west unconfirmed delivery"),
            target(5, "west unconfirmed exit"),
            target(6, "east returned failure"),
        ];
        let mut group = HostGroup {
            draft: "working draft".into(),
            selected: BTreeSet::from([3, 6]),
            ..Default::default()
        };
        group.filter.set_wildcards(true);
        group.filter.set_query("west*".into());
        group.submit(
            SubmissionContent::Command("show  status".into()),
            vec![
                delivery(
                    targets[0].clone(),
                    DeliveryStatus::Sent,
                    Some(finished(Some(7))),
                ),
                delivery(
                    targets[1].clone(),
                    DeliveryStatus::Rejected("prompt".into()),
                    None,
                ),
                delivery(
                    targets[2].clone(),
                    DeliveryStatus::Sent,
                    Some(finished(Some(0))),
                ),
                delivery(targets[3].clone(), DeliveryStatus::Unconfirmed, None),
                delivery(
                    targets[4].clone(),
                    DeliveryStatus::Sent,
                    Some(finished(None)),
                ),
                delivery(
                    targets[5].clone(),
                    DeliveryStatus::Sent,
                    Some(finished(Some(9))),
                ),
                delivery(
                    target(7, "west closed"),
                    DeliveryStatus::Rejected("closed".into()),
                    None,
                ),
            ],
            GroupTranscript::default(),
        );
        let offered = BTreeSet::from([1, 2]);
        group.result_filter = progress::Filter::Only(progress::Outcome::Succeeded);
        assert!(!group.prepare_retry(1, &targets, offered.clone()));
        group.result_filter = progress::Filter::Only(progress::Outcome::Attention);
        assert!(!group.prepare_retry(1, &targets, BTreeSet::from([1, 6])));
        group.recipient_search.set_query("WITHHELD WEST".into());
        assert!(!group.prepare_retry(1, &targets, offered.clone()));
        assert_eq!(
            group.retry_candidates(
                group.viewed_submission(1).unwrap(),
                [1, 2, 3, 4, 5, 6].into_iter()
            ),
            BTreeSet::from([2])
        );
        group.recipient_search.clear();
        assert!(group.prepare_retry(1, &targets, offered.clone()));
        assert_eq!(group.selected, offered);
        assert_eq!(group.draft, "show  status");
        assert_eq!(group.submissions.len(), 1);
        group.filter.clear();
        group.filter.set_wildcards(false);
        group.selected.clear();
        assert!(group.restore_draft());
        assert_eq!(group.selected, BTreeSet::from([3, 6]));
        assert!(group.filter.wildcards());
        assert_eq!(group.filter.query(), "west*");
        assert_eq!(group.draft, "working draft");
    }
}
