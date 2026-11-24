use super::{Delivery, DeliveryStatus, GroupExecution};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Waiting,
    Running,
    Succeeded,
    Attention,
}

impl Outcome {
    pub(super) fn of(delivery: &Delivery) -> Self {
        match &delivery.status {
            DeliveryStatus::Pending(..) => Self::Waiting,
            DeliveryStatus::Rejected(_) | DeliveryStatus::Unconfirmed => Self::Attention,
            DeliveryStatus::Sent => match delivery.execution() {
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
pub enum Filter {
    #[default]
    All,
    Only(Outcome),
}

pub(super) const FILTERS: [(Filter, &str); 5] = [
    (Filter::All, "All results"),
    (Filter::Only(Outcome::Waiting), "Waiting"),
    (Filter::Only(Outcome::Running), "Running"),
    (Filter::Only(Outcome::Succeeded), "Succeeded"),
    (Filter::Only(Outcome::Attention), "Needs attention"),
];

impl Filter {
    pub(super) fn includes(self, outcome: Outcome) -> bool {
        self == Self::All || self == Self::Only(outcome)
    }
}

#[derive(Default)]
pub(super) struct Summary {
    waiting: usize,
    running: usize,
    succeeded: usize,
    attention: usize,
}

impl Summary {
    pub(super) fn add(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Waiting => self.waiting += 1,
            Outcome::Running => self.running += 1,
            Outcome::Succeeded => self.succeeded += 1,
            Outcome::Attention => self.attention += 1,
        }
    }

    pub(super) fn settled(&self) -> usize {
        self.succeeded + self.attention
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
        format!(
            "{} of {} settled · {} waiting · {} running · {} succeeded · {} {attention}",
            self.settled(),
            self.total(),
            self.waiting,
            self.running,
            self.succeeded,
            self.attention
        )
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
            duration: Duration::ZERO,
            output: GroupOutput::Unavailable("Output expired"),
        }))
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
    }
}
