use serde::Serialize;
use std::{collections::BTreeMap, mem};

const MIN_BUCKET_NS: i128 = 1_000;
const MAX_BUCKETS: i128 = 64;

#[derive(Clone, Copy, Default, Serialize)]
pub struct Bucket {
    pub packets: u32,
    pub captured_bytes: u64,
}

pub struct Timeline {
    width: i128,
    bins: BTreeMap<i128, Bucket>,
}

impl Default for Timeline {
    fn default() -> Self {
        Self {
            width: MIN_BUCKET_NS,
            bins: BTreeMap::new(),
        }
    }
}

impl Timeline {
    pub(super) fn record(&mut self, timestamp: i128, captured: u32) {
        let mut index = timestamp.div_euclid(self.width);
        if let Some((&first, _)) = self.bins.first_key_value() {
            let mut first = first;
            let mut last = *self.bins.last_key_value().unwrap().0;
            let mut multiplier = 1;
            // Align groups to the same origin as the previous width so existing
            // packet and byte counts can move exactly into the larger bins.
            while last.max(index) - first.min(index) >= MAX_BUCKETS {
                multiplier *= 2;
                self.width *= 2;
                first = first.div_euclid(2);
                last = last.div_euclid(2);
                index = index.div_euclid(2);
            }
            if multiplier != 1 {
                for (index, bucket) in mem::take(&mut self.bins) {
                    let aggregate = self.bins.entry(index.div_euclid(multiplier)).or_default();
                    aggregate.packets += bucket.packets;
                    aggregate.captured_bytes += bucket.captured_bytes;
                }
            }
        }
        let bucket = self.bins.entry(index).or_default();
        bucket.packets += 1;
        bucket.captured_bytes += u64::from(captured);
    }

    pub fn start_unix_ns(&self) -> Option<i128> {
        self.bins
            .first_key_value()
            .map(|(&index, _)| index * self.width)
    }

    pub fn bucket_width_ns(&self) -> u128 {
        self.width as u128
    }

    pub fn buckets(&self) -> impl Iterator<Item = Bucket> + '_ {
        let first = self.bins.first_key_value().map_or(0, |(&index, _)| index);
        let last = self.bins.last_key_value().map_or(-1, |(&index, _)| index);
        (first..=last).map(|index| self.bins.get(&index).copied().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widening_keeps_signed_bin_boundaries_empty_intervals_and_exact_totals() {
        let mut timeline = Timeline::default();
        timeline.record(4_000, 7);
        timeline.record(-1, 2);
        timeline.record(-129_000, 3);
        timeline.record(5_500, 5);
        let buckets: Vec<_> = timeline.buckets().collect();
        assert_eq!(timeline.start_unix_ns(), Some(-132_000));
        assert_eq!(timeline.bucket_width_ns(), 4_000);
        assert_eq!(buckets.len(), 35);
        assert_eq!(buckets[0].captured_bytes, 3);
        assert_eq!(buckets[1].packets, 0);
        assert_eq!(buckets[32].captured_bytes, 2);
        assert_eq!(buckets[33].packets, 0);
        assert_eq!((buckets[34].packets, buckets[34].captured_bytes), (2, 12));
        assert_eq!(buckets.iter().map(|bucket| bucket.packets).sum::<u32>(), 4);
        assert_eq!(
            buckets
                .iter()
                .map(|bucket| bucket.captured_bytes)
                .sum::<u64>(),
            17
        );
    }
}
