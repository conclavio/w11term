use super::LocalCapture;
use anyhow::{Context, Result};
use serde::{Serialize, Serializer};
use std::ops::Range;

#[derive(Serialize)]
pub struct ComparedWindow {
    #[serde(serialize_with = "serialize_ns")]
    pub start_offset_ns: i128,
    #[serde(serialize_with = "serialize_ns")]
    pub end_offset_ns: i128,
    pub before: WindowSide,
    pub after: WindowSide,
}

#[derive(Serialize)]
pub struct WindowSide {
    #[serde(serialize_with = "serialize_ns")]
    pub start_unix_ns: i128,
    #[serde(serialize_with = "serialize_ns")]
    pub end_unix_ns: i128,
    pub untimestamped_packets: u32,
    pub span_covers_window: bool,
}

impl ComparedWindow {
    pub(super) fn new(sources: [&LocalCapture; 2], offsets: Range<i128>) -> Result<Self> {
        Ok(Self {
            start_offset_ns: offsets.start,
            end_offset_ns: offsets.end,
            before: WindowSide::new(sources[0], &offsets).context("Aligning the before capture")?,
            after: WindowSide::new(sources[1], &offsets).context("Aligning the after capture")?,
        })
    }
}

impl WindowSide {
    fn new(source: &LocalCapture, offsets: &Range<i128>) -> Result<Self> {
        let scan = source
            .summary
            .window
            .as_ref()
            .context("The capture was not scanned for an interval")?;
        let origin = scan
            .origin
            .context("The capture has no timestamped records")?;
        let start_unix_ns = origin
            .checked_add(offsets.start)
            .context("The elapsed start is outside the timestamp range")?;
        let end_unix_ns = origin
            .checked_add(offsets.end)
            .context("The elapsed end is outside the timestamp range")?;
        Ok(Self {
            start_unix_ns,
            end_unix_ns,
            untimestamped_packets: scan.untimestamped_packets(),
            span_covers_window: scan.span_covers(start_unix_ns, end_unix_ns),
        })
    }

    pub fn coverage_label(&self) -> &'static str {
        if self.span_covers_window {
            "Timestamp span covers the interval"
        } else {
            "Timestamp span does not cover the whole interval"
        }
    }
}

fn serialize_ns<S: Serializer>(value: &i128, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(value)
}
