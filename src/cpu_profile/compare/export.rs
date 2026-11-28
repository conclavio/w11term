use super::{super::sample_share, FilteredComparison};
use crate::ssh::{CpuProfileCaptureReport, CpuProfileSamplingField};
use serde::{Serialize, Serializer, ser::SerializeSeq};
use std::{
    borrow::Cow,
    io::{self, BufWriter, Write},
};

impl FilteredComparison {
    pub fn write_json(&self, output: impl Write) -> io::Result<()> {
        self.write_json_with_capture_reports(output, [None, None])
    }

    pub fn write_json_with_capture_reports(
        &self,
        output: impl Write,
        capture_reports: [Option<&CpuProfileCaptureReport>; 2],
    ) -> io::Result<()> {
        let comparison = &self.comparison;
        for (side, label) in ["Before", "After"].into_iter().enumerate() {
            if capture_reports[side]
                .is_some_and(|report| !report.matches_profile(&comparison.profiles[side]))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{label} capture report does not match the loaded profile"),
                ));
            }
        }
        let source = |side: usize| Source {
            file_name: comparison.paths[side]
                .file_name()
                .map(|name| name.to_string_lossy()),
            total_samples: comparison.totals[side],
            capture_report: capture_reports[side],
        };
        let report = Report {
            schema_version: 1,
            before: source(0),
            after: source(1),
            sampling_differences: capture_reports[0]
                .zip(capture_reports[1])
                .map(|(before, after)| before.sampling().differences(&after.sampling())),
            filter: &self.filter,
            total_changed_stacks: comparison.changes.len(),
            changes: Changes(self),
        };
        let mut output = BufWriter::new(output);
        serde_json::to_writer(&mut output, &report).map_err(io::Error::other)?;
        output.write_all(b"\n")?;
        output.flush()
    }
}

#[derive(Serialize)]
struct Source<'a> {
    file_name: Option<Cow<'a, str>>,
    total_samples: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    capture_report: Option<&'a CpuProfileCaptureReport>,
}

#[derive(Serialize)]
struct Report<'a> {
    schema_version: u8,
    before: Source<'a>,
    after: Source<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sampling_differences: Option<Vec<CpuProfileSamplingField>>,
    filter: &'a str,
    total_changed_stacks: usize,
    changes: Changes<'a>,
}

struct Changes<'a>(&'a FilteredComparison);

impl Serialize for Changes<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let snapshot = self.0;
        let comparison = &snapshot.comparison;
        let mut rows = serializer.serialize_seq(Some(snapshot.indices.len()))?;
        for &index in snapshot.indices.iter() {
            let change = &comparison.changes[index];
            let (profile, frames) = change.stack_frames(comparison);
            let [before_samples, after_samples] = change.counts;
            rows.serialize_element(&StackChange {
                stack: frames.into_iter().map(|id| profile.symbol(id)).collect(),
                before_samples,
                after_samples,
                before_percent: sample_share(before_samples, comparison.totals[0]),
                after_percent: sample_share(after_samples, comparison.totals[1]),
                delta_percentage_points: comparison.delta_points(change),
            })?;
        }
        rows.end()
    }
}

#[derive(Serialize)]
struct StackChange<'a> {
    stack: Vec<&'a str>,
    before_samples: u64,
    after_samples: u64,
    before_percent: f64,
    after_percent: f64,
    delta_percentage_points: f64,
}
