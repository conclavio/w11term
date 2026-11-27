use super::Traffic;
use serde::Serialize;
use std::num::NonZeroU128;

const NANOS_PER_SECOND: f64 = 1_000_000_000.0;

#[derive(Clone, Copy, Serialize)]
pub struct AverageRateComparison {
    pub before: Option<TrafficRate>,
    pub after: Option<TrafficRate>,
    pub delta: Option<TrafficRate>,
}

#[derive(Clone, Copy, Serialize)]
pub struct TrafficRate {
    pub packets_per_second: f64,
    pub captured_bytes_per_second: f64,
}

impl AverageRateComparison {
    pub(super) fn new(
        observations: [Option<Traffic>; 2],
        spans_ns: [Option<NonZeroU128>; 2],
    ) -> Self {
        let [before, after] = observations;
        let [before_span, after_span] = spans_ns;
        let before = before.zip(before_span);
        let after = after.zip(after_span);
        Self {
            before: before.map(|(traffic, span)| TrafficRate::new(traffic, span)),
            after: after.map(|(traffic, span)| TrafficRate::new(traffic, span)),
            delta: before.zip(after).map(TrafficRate::difference),
        }
    }

    pub(super) fn has_observation(self) -> bool {
        self.before.is_some() || self.after.is_some()
    }

    pub(super) fn changed(self) -> bool {
        self.delta.is_some_and(|delta| {
            delta.packets_per_second != 0.0 || delta.captured_bytes_per_second != 0.0
        })
    }

    pub fn report_lines(self) -> [String; 3] {
        [
            ("Before average", self.before, false),
            ("After average", self.after, false),
            ("Average change", self.delta, true),
        ]
        .map(|(label, rate, signed)| {
            let value = rate.map_or_else(
                || "Unavailable".to_owned(),
                |rate| {
                    format!(
                        "{} packets/s, {} captured bytes/s",
                        number(rate.packets_per_second, signed),
                        number(rate.captured_bytes_per_second, signed),
                    )
                },
            );
            format!("{label} · {value}")
        })
    }
}

impl TrafficRate {
    fn new(traffic: Traffic, span: NonZeroU128) -> Self {
        let span = span.get();
        Self {
            packets_per_second: per_second(u64::from(traffic.packets), span),
            captured_bytes_per_second: per_second(traffic.captured_bytes, span),
        }
    }

    fn difference(
        ((before, before_span), (after, after_span)): (
            (Traffic, NonZeroU128),
            (Traffic, NonZeroU128),
        ),
    ) -> Self {
        let before_span = before_span.get();
        let after_span = after_span.get();
        let difference = |before: u64, after: u64| {
            // Subtract before rounding so proportional counts and spans yield an exact zero.
            let a = u128::from(after).checked_mul(before_span);
            let b = u128::from(before).checked_mul(after_span);
            if let Some((a, b)) = a.zip(b) {
                let magnitude = a.abs_diff(b) as f64 * NANOS_PER_SECOND
                    / (before_span as f64 * after_span as f64);
                if a < b { -magnitude } else { magnitude }
            } else {
                // Extreme pcapng timestamps can exceed the integer cross-product range.
                per_second(after, after_span) - per_second(before, before_span)
            }
        };
        Self {
            packets_per_second: difference(u64::from(before.packets), u64::from(after.packets)),
            captured_bytes_per_second: difference(before.captured_bytes, after.captured_bytes),
        }
    }
}

fn per_second(count: u64, span_ns: u128) -> f64 {
    count as f64 * NANOS_PER_SECOND / span_ns as f64
}

fn number(value: f64, signed: bool) -> String {
    let magnitude = value.abs();
    let digits = if magnitude != 0.0 && !(0.001..1e9).contains(&magnitude) {
        format!("{magnitude:.3e}")
    } else {
        format!("{magnitude:.3}")
    };
    if value < 0.0 {
        format!("-{digits}")
    } else if signed {
        format!("+{digits}")
    } else {
        digits
    }
}
