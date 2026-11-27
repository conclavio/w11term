use super::super::summary::{Bucket, MAX_BUCKETS, RelativeTimeline};
use serde::{Serialize, Serializer};
use std::{cmp::Reverse, io, ops::Range};

#[derive(Serialize)]
pub struct TimelineComparison {
    #[serde(serialize_with = "serialize_ns")]
    pub start_offset_ns: i128,
    #[serde(serialize_with = "serialize_ns")]
    pub bucket_width_ns: i128,
    #[serde(serialize_with = "serialize_optional_ns")]
    pub before_origin_unix_ns: Option<i128>,
    #[serde(serialize_with = "serialize_optional_ns")]
    pub after_origin_unix_ns: Option<i128>,
    pub buckets: Vec<TimelineBucket>,
}

#[derive(Clone, Copy, Serialize)]
pub struct TimelineBucket {
    pub before: Option<Bucket>,
    pub after: Option<Bucket>,
}

impl TimelineBucket {
    pub fn observation(self, side: usize) -> Option<Bucket> {
        [self.before, self.after].get(side).copied().flatten()
    }
}

impl TimelineComparison {
    pub(super) fn new(sources: [Option<&RelativeTimeline>; 2]) -> Option<Self> {
        let mut width = sources
            .into_iter()
            .flatten()
            .map(|source| source.timeline.bucket_width_ns() as i128)
            .max()?;
        let (mut first, mut last) = (0_i128, 0_i128);
        for source in sources.into_iter().flatten() {
            for (start, _) in source.timeline.occupied() {
                first = first.min(start);
                last = last.max(start);
            }
        }
        // Both grids share the relative zero and a power-of-two width ratio,
        // so further widening moves every original bucket without splitting it.
        while last.div_euclid(width) - first.div_euclid(width) >= MAX_BUCKETS {
            width *= 2;
        }
        let first_index = first.div_euclid(width);
        let len = (last.div_euclid(width) - first_index + 1) as usize;
        let [before, after] = sources.map(|source| {
            let mut bins = vec![None::<Bucket>; len];
            if let Some(source) = source {
                for (start, observed) in source.timeline.occupied() {
                    let index = (start.div_euclid(width) - first_index) as usize;
                    let bucket = bins[index].get_or_insert_default();
                    bucket.packets += observed.packets;
                    bucket.captured_bytes += observed.captured_bytes;
                }
                if let Some(first) = bins.iter().position(Option::is_some)
                    && let Some(last) = bins.iter().rposition(Option::is_some)
                {
                    for bucket in &mut bins[first..=last] {
                        bucket.get_or_insert_default();
                    }
                }
            }
            bins
        });
        let [before_origin_unix_ns, after_origin_unix_ns] =
            sources.map(|source| source.map(|source| source.origin_unix_ns));
        Some(Self {
            start_offset_ns: first_index * width,
            bucket_width_ns: width,
            before_origin_unix_ns,
            after_origin_unix_ns,
            buckets: before
                .into_iter()
                .zip(after)
                .map(|(before, after)| TimelineBucket { before, after })
                .collect(),
        })
    }

    pub fn peak_index(&self) -> Option<usize> {
        self.buckets
            .iter()
            .enumerate()
            .max_by_key(|&(index, bucket)| {
                (
                    bucket.before.map_or(0, |counts| counts.packets)
                        + bucket.after.map_or(0, |counts| counts.packets),
                    Reverse(index),
                )
            })
            .map(|(index, _)| index)
    }

    pub fn offset_interval(&self, index: usize) -> Option<Range<i128>> {
        self.buckets.get(index)?;
        let start = self.start_offset_ns + index as i128 * self.bucket_width_ns;
        Some(start..start + self.bucket_width_ns)
    }

    pub fn occupied_interval(&self, side: usize, index: usize) -> Option<Range<i128>> {
        let origin = [self.before_origin_unix_ns, self.after_origin_unix_ns]
            .get(side)
            .copied()
            .flatten()?;
        if self.buckets.get(index)?.observation(side)?.packets == 0 {
            return None;
        }
        let interval = self.offset_interval(index)?;
        Some(origin + interval.start..origin + interval.end)
    }

    pub(super) fn write_report(&self, mut output: impl io::Write) -> io::Result<()> {
        writeln!(
            output,
            "\nElapsed packet timeline · {} ns per interval",
            self.bucket_width_ns
        )?;
        writeln!(
            output,
            "Offsets from the first timestamped record in each file"
        )?;
        for (index, bucket) in self.buckets.iter().enumerate() {
            let offset = self.start_offset_ns + index as i128 * self.bucket_width_ns;
            let before = observation_text(bucket.before);
            let after = observation_text(bucket.after);
            writeln!(output, "{offset:+} ns · Before {before} · After {after}")?;
        }
        Ok(())
    }
}

fn observation_text(bucket: Option<Bucket>) -> String {
    bucket.map_or_else(
        || "outside timestamped coverage".to_owned(),
        |bucket| {
            format!(
                "{} packets, {} captured bytes",
                bucket.packets, bucket.captured_bytes
            )
        },
    )
}

fn serialize_ns<S: Serializer>(ns: &i128, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(ns)
}

fn serialize_optional_ns<S: Serializer>(
    ns: &Option<i128>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    ns.map(|ns| ns.to_string()).serialize(serializer)
}
