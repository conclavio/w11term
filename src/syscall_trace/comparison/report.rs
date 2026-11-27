use super::{Comparison, Kind, Row, RowMatch, TimingAnchor};
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
        anchor: Option<TimingAnchor>,
        output: impl Write,
    ) -> io::Result<()> {
        let query: String = query.chars().take(MAX_QUERY_CHARS).collect();
        let filter = query.trim();
        let matches = if filter.is_empty() {
            (0..self.rows.len())
                .map(|index| RowMatch {
                    index,
                    sample: None,
                })
                .collect()
        } else {
            self.search(filter)
        };
        let rows: Vec<_> = matches
            .into_iter()
            .filter(|matched| self.row_visible(matched.index, include_timing))
            .map(|matched| {
                let row = &self.rows[matched.index];
                (matched.index, matched.sample.unwrap_or(row.sample))
            })
            .collect();
        self.write_visible_json(filter, include_timing, &rows, anchor, output)
    }

    /// Write a report from the viewer's ordered (row index, evidence index) selection.
    pub fn write_visible_json(
        &self,
        filter: &str,
        include_timing: bool,
        rows: &[(usize, usize)],
        anchor: Option<TimingAnchor>,
        output: impl Write,
    ) -> io::Result<()> {
        if rows.iter().any(|&(index, sample)| {
            !self.row_visible(index, include_timing) || sample >= self.rows[index].evidence_count()
        }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid syscall comparison row or evidence index",
            ));
        }
        let report = Report {
            schema_version: 1,
            working: source(&self.traces[0]),
            failing: source(&self.traces[1]),
            filter,
            include_timing,
            limited: self.limited,
            total_changes: self.rows.len(),
            timing_anchor: anchor.map(|anchor| AnchorReport::from_comparison(self, anchor)),
            changes: Changes {
                comparison: self,
                rows,
                anchor,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    timing_anchor: Option<AnchorReport<'a>>,
    changes: Changes<'a>,
}

#[derive(Serialize)]
struct AnchorReport<'a> {
    syscall: &'a str,
    working: AnchorSource<'a>,
    failing: AnchorSource<'a>,
}

#[derive(Serialize)]
struct AnchorSource<'a> {
    file_number: usize,
    #[serde(flatten)]
    source: Line<'a>,
}

impl<'a> AnchorReport<'a> {
    fn from_comparison(comparison: &'a Comparison, anchor: TimingAnchor) -> Self {
        let [working, failing] = [0, 1].map(|side| {
            let trace = &comparison.traces[side];
            let raw = trace.events[anchor.event(side)].first;
            AnchorSource {
                file_number: raw.source_number(),
                source: Line {
                    file_name: file_name(trace.original(raw).0),
                    line: raw.line,
                },
            }
        });
        Self {
            syscall: &comparison.traces[0].events[anchor.event(0)].name,
            working,
            failing,
        }
    }
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
    rows: &'a [(usize, usize)],
    anchor: Option<TimingAnchor>,
}

impl Serialize for Changes<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let comparison = self.comparison;
        let first = comparison.first_divergence();
        let mut changes = serializer.serialize_seq(Some(self.rows.len()))?;
        for &(index, sample) in self.rows {
            let row = &comparison.rows[index];
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
                working_evidence: evidence(comparison, row, 0, sample, self.anchor),
                failing_evidence: evidence(comparison, row, 1, sample, self.anchor),
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
    #[serde(skip_serializing_if = "Option::is_none")]
    offset_from_anchor_ns: Option<i128>,
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
    anchor: Option<TimingAnchor>,
) -> Option<Evidence<'a>> {
    let trace = &comparison.traces[side];
    let index = comparison.event(row, side, sample)?;
    let event = &trace.events[index];
    Some(Evidence {
        pid: event.pid,
        outcome: event.outcome,
        errno: event.error.as_deref(),
        elapsed_ns: trace.relative_start(index),
        offset_from_anchor_ns: anchor
            .and_then(|anchor| comparison.offset_from_anchor_ns(anchor, side, index)),
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
