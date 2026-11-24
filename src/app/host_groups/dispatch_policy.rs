use super::{CommandOffer, Duration};
use serde::Serialize;

#[derive(Clone, Serialize)]
pub(super) struct Policy {
    pub(super) parallel_limit: Option<usize>,
    pub(super) interval_ms: u64,
    pub(super) stop_on_failure: bool,
    canary: Option<Canary>,
}

#[derive(Clone, Serialize)]
struct Canary {
    pane_id: u64,
    label: String,
}

impl CommandOffer {
    pub(super) fn dispatch_policy(&self) -> Policy {
        let parallel_limit = self.limit.concurrency();
        let interval_ms = u64::try_from(self.interval.duration().as_millis()).unwrap_or(u64::MAX);
        let canary = self.canary.and_then(|pane| {
            self.targets
                .iter()
                .find(|target| target.pane == pane)
                .map(|target| Canary {
                    pane_id: pane,
                    label: target.name.replace(['\r', '\n'], " "),
                })
        });
        Policy {
            parallel_limit,
            interval_ms,
            stop_on_failure: (parallel_limit.is_some() || interval_ms != 0)
                && self.stop_queue_on_failure,
            canary,
        }
    }
}

impl Policy {
    pub(super) fn caption(&self) -> String {
        let parallel = self.parallel_limit.map_or_else(
            || "unlimited parallel".into(),
            |limit| format!("parallel limit {limit}"),
        );
        let interval = if self.interval_ms == 0 {
            "no delay".into()
        } else {
            format!(
                "{:?} between sends",
                Duration::from_millis(self.interval_ms)
            )
        };
        let mut text = format!("Dispatch · {parallel} · {interval}");
        if self.stop_on_failure {
            text.push_str(" · stop on failure");
        }
        if let Some(canary) = &self.canary {
            text.push_str(" · canary ");
            text.push_str(&canary.label);
        }
        text
    }
}

pub(super) fn report_heading(label: &str, policy: Option<&Policy>) -> String {
    let mut heading = format!("Submitted · {label}\r\n");
    if let Some(policy) = policy {
        heading.push_str(&policy.caption());
        heading.push_str("\r\n");
    }
    heading
}
