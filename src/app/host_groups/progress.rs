use super::{Delivery, DeliveryStatus, GroupExecution, HostGroup, Submission};
use std::{cmp::Reverse, time::Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Waiting,
    Running,
    Succeeded,
    Attention,
    Skipped,
}

impl Outcome {
    pub(super) fn of(delivery: &Delivery) -> Self {
        Self::with_execution(&delivery.status, delivery.execution().as_ref())
    }

    pub(super) fn with_execution(
        status: &DeliveryStatus,
        execution: Option<&GroupExecution>,
    ) -> Self {
        match status {
            DeliveryStatus::Queued(_) | DeliveryStatus::Pending(..) => Self::Waiting,
            DeliveryStatus::Rejected(_) | DeliveryStatus::Unconfirmed => Self::Attention,
            DeliveryStatus::Skipped => Self::Skipped,
            DeliveryStatus::Sent => match execution {
                Some(GroupExecution::AwaitingMarkers | GroupExecution::AwaitingPrompt) => {
                    Self::Waiting
                }
                Some(GroupExecution::Running { .. }) => Self::Running,
                Some(GroupExecution::Finished(result)) if result.exit_code == Some(0) => {
                    Self::Succeeded
                }
                _ => Self::Attention,
            },
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum ResultOrder {
    #[default]
    Submission,
    Longest,
    Shortest,
}

pub(super) const RESULT_ORDERS: [(ResultOrder, &str); 3] = [
    (ResultOrder::Submission, "Submission order"),
    (ResultOrder::Longest, "Longest shell time"),
    (ResultOrder::Shortest, "Shortest shell time"),
];

impl ResultOrder {
    pub(super) fn apply(self, rows: &mut [(&Delivery, Option<GroupExecution>)], now: Instant) {
        let time = |(_, execution): &(&Delivery, Option<GroupExecution>)| {
            execution
                .as_ref()
                .and_then(|execution| execution.shell_time(now))
        };
        match self {
            Self::Submission => {}
            Self::Longest => rows.sort_by_cached_key(|row| Reverse(time(row))),
            Self::Shortest => rows.sort_by_cached_key(|row| {
                let duration = time(row);
                (duration.is_none(), duration)
            }),
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Only(Outcome),
}

pub(super) const FILTERS: [(Filter, &str); 6] = [
    (Filter::All, "All results"),
    (Filter::Only(Outcome::Waiting), "Waiting"),
    (Filter::Only(Outcome::Running), "Running"),
    (Filter::Only(Outcome::Succeeded), "Succeeded"),
    (Filter::Only(Outcome::Attention), "Needs attention"),
    (Filter::Only(Outcome::Skipped), "Skipped"),
];

impl Filter {
    pub(super) fn includes(self, outcome: Outcome) -> bool {
        self == Self::All || self == Self::Only(outcome)
    }
}

impl HostGroup {
    pub(super) fn matches_result(&self, delivery: &Delivery, outcome: Outcome) -> bool {
        self.result_filter.includes(outcome)
            && self.recipient_search.matches_query(&delivery.target)
    }

    pub(super) fn result_rows<'a>(
        &self,
        submission: &'a Submission,
        now: Instant,
    ) -> (Summary, Vec<(&'a Delivery, Option<GroupExecution>)>) {
        let mut summary = Summary::default();
        let mut shown: Vec<_> = submission
            .results
            .iter()
            .filter_map(|delivery| {
                let execution = delivery.execution();
                let outcome = Outcome::with_execution(&delivery.status, execution.as_ref());
                summary.add(outcome);
                self.matches_result(delivery, outcome)
                    .then_some((delivery, execution))
            })
            .collect();
        self.result_order.apply(&mut shown, now);
        (summary, shown)
    }
}

#[derive(Default, serde::Serialize)]
pub(super) struct Summary {
    waiting: usize,
    running: usize,
    succeeded: usize,
    attention: usize,
    skipped: usize,
}

impl Summary {
    pub(super) fn capture(deliveries: &[Delivery]) -> Self {
        let mut summary = Self::default();
        for delivery in deliveries {
            summary.add(Outcome::of(delivery));
        }
        summary
    }

    pub(super) fn in_progress(&self) -> bool {
        self.waiting + self.running != 0
    }

    pub(super) fn picker_caption(&self) -> String {
        let summary = if self.running > 0 {
            "Running"
        } else if self.waiting > 0 {
            "Waiting"
        } else if self.attention > 0 {
            "Needs attention"
        } else if self.skipped > 0 {
            "Hosts skipped"
        } else {
            "Finished"
        };
        format!("{summary} · {} of {} settled", self.settled(), self.total())
    }

    pub(super) fn add(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Waiting => self.waiting += 1,
            Outcome::Running => self.running += 1,
            Outcome::Succeeded => self.succeeded += 1,
            Outcome::Attention => self.attention += 1,
            Outcome::Skipped => self.skipped += 1,
        }
    }

    pub(super) fn settled(&self) -> usize {
        self.succeeded + self.attention + self.skipped
    }

    pub(super) fn total(&self) -> usize {
        self.waiting + self.running + self.settled()
    }

    pub(super) fn caption(&self) -> String {
        let attention = if self.attention == 1 {
            "needs attention"
        } else {
            "need attention"
        };
        let mut caption = format!(
            "{} of {} settled · {} waiting · {} running · {} succeeded · {} {attention}",
            self.settled(),
            self.total(),
            self.waiting,
            self.running,
            self.succeeded,
            self.attention
        );
        if self.skipped > 0 {
            caption.push_str(&format!(" · {} skipped", self.skipped));
        }
        caption
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::host_groups::{Target, TargetState};
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };
    use tokio::sync::{oneshot, watch};
    use w11term::events::{CommandLink, GroupCommandResult, GroupInterruptTarget, GroupOutput};

    fn delivery(status: DeliveryStatus, execution: Option<GroupExecution>) -> Delivery {
        Delivery {
            target: Target {
                pane: 1,
                name: "host".into(),
                names: Default::default(),
                profile: None,
                connected: true,
                state: TargetState::InUse,
            },
            status,
            execution: execution.map(|result| watch::channel(result).1),
            interrupt: None,
        }
    }

    fn finished(exit_code: Option<i32>) -> GroupExecution {
        GroupExecution::Finished(Arc::new(GroupCommandResult {
            link: CommandLink {
                generation: 1,
                id: 2,
            },
            exit_code,
            duration: Duration::from_secs(3),
            output: GroupOutput::Unavailable("Output expired"),
        }))
    }

    #[test]
    fn sorts_running_and_finished_times_with_stable_ties_and_unknowns_last() {
        let now = Instant::now();
        let states = [
            None,
            Some(finished(Some(0))),
            Some(GroupExecution::Running {
                target: GroupInterruptTarget {
                    generation: 1,
                    serial: 4,
                },
                started_at: now - Duration::from_secs(2),
            }),
            Some(finished(Some(17))),
            Some(GroupExecution::AwaitingMarkers),
        ];
        let recipients: Vec<_> = states
            .into_iter()
            .enumerate()
            .map(|(index, execution)| {
                let mut recipient = delivery(DeliveryStatus::Sent, execution);
                recipient.target.pane = index as u64 + 1;
                recipient
            })
            .collect();
        let ordered = |order: ResultOrder, now| {
            let mut rows: Vec<_> = recipients
                .iter()
                .map(|recipient| (recipient, recipient.execution()))
                .collect();
            order.apply(&mut rows, now);
            rows.iter()
                .map(|(delivery, _)| delivery.target.pane)
                .collect::<Vec<_>>()
        };
        assert_eq!(ordered(ResultOrder::Submission, now), [1, 2, 3, 4, 5]);
        assert_eq!(ordered(ResultOrder::Longest, now), [2, 4, 3, 1, 5]);
        assert_eq!(ordered(ResultOrder::Shortest, now), [3, 2, 4, 1, 5]);
        assert_eq!(
            ordered(ResultOrder::Longest, now + Duration::from_secs(3)),
            [3, 2, 4, 1, 5],
        );
    }

    #[test]
    fn progress_distinguishes_waiting_running_verified_success_and_uncertain_results() {
        use DeliveryStatus::*;
        use GroupExecution::*;
        use Outcome::*;
        let cases = [
            (
                delivery(Pending(Instant::now(), oneshot::channel().1), None),
                Waiting,
            ),
            (delivery(Sent, Some(AwaitingMarkers)), Waiting),
            (delivery(Sent, Some(AwaitingPrompt)), Waiting),
            (
                delivery(
                    Sent,
                    Some(GroupExecution::Running {
                        target: GroupInterruptTarget {
                            generation: 1,
                            serial: 2,
                        },
                        started_at: Instant::now(),
                    }),
                ),
                Outcome::Running,
            ),
            (delivery(Sent, Some(finished(Some(0)))), Succeeded),
            (delivery(Sent, Some(finished(Some(17)))), Attention),
            (delivery(Sent, Some(finished(None))), Attention),
            (delivery(Sent, Some(PromptReturned)), Attention),
            (
                delivery(Sent, Some(Unavailable("Connection lost"))),
                Attention,
            ),
            (delivery(Rejected("Read-only".into()), None), Attention),
            (delivery(Unconfirmed, None), Attention),
        ];
        let mut summary = Summary::default();
        for (delivery, expected) in cases {
            let outcome = Outcome::of(&delivery);
            assert_eq!(outcome, expected);
            assert!(Filter::All.includes(outcome));
            assert_eq!(
                Filter::Only(Attention).includes(outcome),
                expected == Attention
            );
            summary.add(outcome);
        }
        assert_eq!(summary.total(), 11);
        assert_eq!(summary.settled(), 7);
        assert_eq!(
            summary.caption(),
            "7 of 11 settled · 3 waiting · 1 running · 1 succeeded · 6 need attention"
        );
        let skipped = Outcome::of(&delivery(DeliveryStatus::Skipped, None));
        assert_eq!(skipped, Outcome::Skipped);
        assert!(Filter::Only(Outcome::Skipped).includes(skipped));
        assert!(!Filter::Only(Attention).includes(skipped));
        summary.add(skipped);
        assert_eq!(
            summary.caption(),
            "8 of 12 settled · 3 waiting · 1 running · 1 succeeded · 6 need attention · 1 skipped"
        );
    }
}
