use super::{Comparison, Kind, Row, RowMatch};
use crate::syscall_trace::{MAX_QUERY_CHARS, MergeOrder, Outcome, Trace, duration_delta_ms};
use serde::{Serialize, Serializer, ser::SerializeSeq};
use std::{
    borrow::Cow,
    io::{self, BufWriter, Write},
    path::Path,
};

impl Comparison {
    /// Write a versioned report of the visible changes with the same matching evidence as the viewer.
    pub fn write_json(
        &self,
        query: &str,
        include_timing: bool,
        output: impl Write,
    ) -> io::Result<()> {
        let query: String = query.chars().take(MAX_QUERY_CHARS).collect();
        let filter = query.trim();
        let mut matches = if filter.is_empty() {
            (0..self.rows.len())
                .map(|index| RowMatch {
                    index,
                    sample: None,
                })
                .collect()
        } else {
            self.search(filter)
        };
        matches.retain(|matched| self.row_visible(matched.index, include_timing));
        let report = Report {
            schema_version: 1,
            working: source(&self.traces[0]),
            failing: source(&self.traces[1]),
            filter,
            include_timing,
            limited: self.limited,
            total_changes: self.rows.len(),
            changes: Changes {
                comparison: self,
                matches: &matches,
            },
        };
        let mut output = BufWriter::new(output);
        serde_json::to_writer(&mut output, &report).map_err(io::Error::other)?;
        output.write_all(b"\n")?;
        output.flush()
    }
}

#[derive(Serialize)]
struct Report<'a> {
    schema_version: u8,
    working: Source<'a>,
    failing: Source<'a>,
    filter: &'a str,
    include_timing: bool,
    limited: bool,
    total_changes: usize,
    changes: Changes<'a>,
}

#[derive(Serialize)]
struct Source<'a> {
    file_names: Vec<Cow<'a, str>>,
    events: usize,
    merge_order: &'static str,
}

fn source(trace: &Trace) -> Source<'_> {
    Source {
        file_names: trace
            .sources
            .iter()
            .map(|file| file_name(&file.path))
            .collect(),
        events: trace.events.len(),
        merge_order: match trace.merge_order {
            MergeOrder::File => "file",
            MergeOrder::Epoch => "epoch",
            MergeOrder::Clock { .. } => "clock",
        },
    }
}

struct Changes<'a> {
    comparison: &'a Comparison,
    matches: &'a [RowMatch],
}

impl Serialize for Changes<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let comparison = self.comparison;
        let first = comparison.first_divergence();
        let mut changes = serializer.serialize_seq(Some(self.matches.len()))?;
        for matched in self.matches {
            let index = matched.index;
            let row = &comparison.rows[index];
            let sample = matched.sample.unwrap_or(row.sample);
            let [working_count, failing_count] = row.counts();
            let [working_result, failing_result] =
                row.result.each_ref().map(|value| value.as_deref());
            let [working_duration_ns, failing_duration_ns] = row.durations;
            changes.serialize_element(&Change {
                kind: row.kind,
                syscall: &row.call,
                resource: &row.resource,
                executable: &row.process,
                first_divergence: Some(index) == first,
                uncertain: row.uncertain,
                working_count,
                failing_count,
                count_delta: failing_count as i64 - working_count as i64,
                representative_working_result: working_result,
                representative_failing_result: failing_result,
                result_changes: row.result_changes,
                working_duration_ns,
                failing_duration_ns,
                duration_delta_ms: working_duration_ns
                    .zip(failing_duration_ns)
                    .map(|(working, failing)| duration_delta_ms(working, failing)),
                evidence_pair: sample + 1,
                evidence_pairs: row.evidence_count(),
                working_evidence: evidence(comparison, row, 0, sample),
                failing_evidence: evidence(comparison, row, 1, sample),
            })?;
        }
        changes.end()
    }
}

#[derive(Serialize)]
struct Change<'a> {
    kind: Kind,
    syscall: &'a str,
    resource: &'a str,
    executable: &'a str,
    first_divergence: bool,
    uncertain: bool,
    working_count: usize,
    failing_count: usize,
    count_delta: i64,
    representative_working_result: Option<&'a str>,
    representative_failing_result: Option<&'a str>,
    result_changes: usize,
    working_duration_ns: Option<u64>,
    failing_duration_ns: Option<u64>,
    duration_delta_ms: Option<f64>,
    evidence_pair: usize,
    evidence_pairs: usize,
    working_evidence: Option<Evidence<'a>>,
    failing_evidence: Option<Evidence<'a>>,
}

#[derive(Serialize)]
struct Evidence<'a> {
    pid: Option<u32>,
    outcome: Outcome,
    errno: Option<&'a str>,
    elapsed_ns: Option<u64>,
    duration_ns: Option<u64>,
    sources: Vec<Line<'a>>,
}

#[derive(Serialize)]
struct Line<'a> {
    file_name: Cow<'a, str>,
    line: u32,
}

fn evidence<'a>(
    comparison: &'a Comparison,
    row: &Row,
    side: usize,
    sample: usize,
) -> Option<Evidence<'a>> {
    let trace = &comparison.traces[side];
    let index = comparison.event(row, side, sample)?;
    let event = &trace.events[index];
    Some(Evidence {
        pid: event.pid,
        outcome: event.outcome,
        errno: event.error.as_deref(),
        elapsed_ns: trace.relative_start(index),
        duration_ns: event.duration,
        sources: event
            .originals()
            .map(|raw| Line {
                file_name: file_name(trace.original(raw).0),
                line: raw.line,
            })
            .collect(),
    })
}

fn file_name(path: &Path) -> Cow<'_, str> {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
}
