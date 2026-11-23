use super::*;

pub(super) struct Scope {
    pub selected: BTreeSet<u64>,
    pub filter: String,
}

fn retryable(delivery: &Delivery) -> bool {
    match delivery.status {
        DeliveryStatus::Rejected(_) => true,
        DeliveryStatus::Sent => matches!(
            delivery.execution(),
            Some(GroupExecution::Finished(result)) if result.exit_code.is_some_and(|code| code != 0)
        ),
        DeliveryStatus::Pending(..) | DeliveryStatus::Unconfirmed => false,
    }
}

impl HostGroup {
    fn retry_candidates(
        &self,
        submission: &Submission,
        visible: impl Iterator<Item = u64>,
    ) -> BTreeSet<u64> {
        if !self.result_filter.includes(progress::Outcome::Attention) {
            return BTreeSet::new();
        }
        let candidates: BTreeSet<_> = submission
            .results
            .iter()
            .filter(|delivery| retryable(delivery))
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
        let eligible =
            self.retry_candidates(submission, self.shown(roster).map(|target| target.pane));
        if eligible != offered {
            self.retry_feedback =
                "The retry targets changed. Review the results and prepare the retry again.".into();
            return false;
        }
        let command = submission.text.to_string();
        let previous = std::mem::replace(&mut self.draft, command);
        let selected = std::mem::replace(&mut self.selected, offered);
        let backup = self.previous_draft.get_or_insert(DraftBackup {
            text: previous,
            scope: None,
        });
        backup.scope.get_or_insert_with(|| Scope {
            selected,
            filter: self.filter.clone(),
        });
        self.retry_feedback.clear();
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
        "Retry draft loaded. Review the command and checked hosts before sending."
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
            output: GroupOutput::Unavailable("fixture"),
        }))
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
            filter: "west".into(),
            selected: BTreeSet::from([3, 6]),
            ..Default::default()
        };
        group.submit(
            "show  status".into(),
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
        assert!(group.prepare_retry(1, &targets, offered.clone()));
        assert_eq!(group.selected, offered);
        assert_eq!(group.draft, "show  status");
        assert_eq!(group.submissions.len(), 1);
        group.filter.clear();
        group.selected.clear();
        assert!(group.restore_draft());
        assert_eq!(group.selected, BTreeSet::from([3, 6]));
        assert_eq!(group.filter, "west");
        assert_eq!(group.draft, "working draft");
    }
}
