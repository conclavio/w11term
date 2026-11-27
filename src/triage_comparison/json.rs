use super::{Change, Comparison, Kind, check_complete, complete_checks};
use crate::ssh::{TriageDelta, TriageReport, TriageSampleStatus};
use serde::Serialize;
use std::{
    borrow::Cow,
    io::{self, BufWriter, Write},
};

impl Comparison {
    pub fn write_json(&self, output: impl Write) -> io::Result<()> {
        let changes = self
            .changes
            .iter()
            .map(|change| {
                let [before, after] = [0, 1].map(|side| self.change_side(change, side));
                ChangeReport {
                    probe_id: &self.reports[0].target.probes[change.probe].id,
                    kind: match change.kind {
                        Kind::Check => "check",
                        Kind::Finding(_) => "observation",
                    },
                    title: self.title(change),
                    before,
                    after,
                    delta: change.delta,
                }
            })
            .collect();
        let changed_checks = (0..self.reports[0].target.probes.len())
            .filter(|&probe| self.changes.iter().any(|change| change.probe == probe))
            .map(|probe| CheckReport {
                id: &self.reports[0].target.probes[probe].id,
                before: check_side(&self.reports[0], probe),
                after: check_side(&self.reports[1], probe),
            })
            .collect();
        let [before, after] = [0, 1].map(|side| {
            let report = &self.reports[side];
            Source {
                file_name: self.file_name(side),
                started_unix_ms: report.collection.started_unix_ms,
                finished_unix_ms: report.collection.finished_unix_ms,
                complete_checks: complete_checks(report),
                total_checks: report.collection.samples.len(),
                stopped_by_user: report.collection.stopped_by_user,
                supervisor_status: report.collection.supervisor_status,
            }
        });
        let report = Report {
            schema_version: 1,
            before,
            after,
            same_endpoint: self.same_endpoint(),
            changes,
            changed_checks,
        };
        let mut output = BufWriter::new(output);
        serde_json::to_writer(&mut output, &report).map_err(io::Error::other)?;
        output.write_all(b"\n")?;
        output.flush()
    }

    fn change_side(&self, change: &Change, side: usize) -> ChangeSide<'_> {
        let value = match change.kind {
            Kind::Check => Some(Cow::Owned(self.value(change, side))),
            Kind::Finding(indices) => indices[side]
                .map(|index| Cow::Borrowed(self.reports[side].findings[index].value.as_str())),
        };
        let evidence = self
            .observation_line(change, side)
            .map(|(stdout_line, text)| Observation { stdout_line, text });
        ChangeSide { value, evidence }
    }
}

#[derive(Serialize)]
struct Report<'a> {
    schema_version: u8,
    before: Source<'a>,
    after: Source<'a>,
    same_endpoint: bool,
    changes: Vec<ChangeReport<'a>>,
    changed_checks: Vec<CheckReport<'a>>,
}

#[derive(Serialize)]
struct Source<'a> {
    file_name: Option<Cow<'a, str>>,
    started_unix_ms: u64,
    finished_unix_ms: u64,
    complete_checks: usize,
    total_checks: usize,
    stopped_by_user: bool,
    supervisor_status: u32,
}

#[derive(Serialize)]
struct ChangeReport<'a> {
    probe_id: &'a str,
    kind: &'static str,
    title: String,
    before: ChangeSide<'a>,
    after: ChangeSide<'a>,
    delta: Option<TriageDelta>,
}

#[derive(Serialize)]
struct ChangeSide<'a> {
    value: Option<Cow<'a, str>>,
    evidence: Option<Observation<'a>>,
}

#[derive(Serialize)]
struct Observation<'a> {
    stdout_line: usize,
    text: &'a str,
}

#[derive(Serialize)]
struct CheckReport<'a> {
    id: &'a str,
    before: CheckSide<'a>,
    after: CheckSide<'a>,
}

#[derive(Serialize)]
struct CheckSide<'a> {
    status: TriageSampleStatus,
    exit_status: Option<u32>,
    complete: bool,
    command: &'a str,
    sampling: &'a str,
    stdout: Stream<'a>,
    stderr: Stream<'a>,
}

#[derive(Serialize)]
struct Stream<'a> {
    text: &'a str,
    at_byte_limit: bool,
}

fn check_side(report: &TriageReport, index: usize) -> CheckSide<'_> {
    let probe = &report.target.probes[index];
    let sample = &report.collection.samples[index];
    let [stdout, stderr] = [&sample.stdout, &sample.stderr].map(|output| Stream {
        text: &output.text,
        at_byte_limit: output.at_byte_limit,
    });
    CheckSide {
        status: sample.status,
        exit_status: sample.exit_status,
        complete: check_complete(report, index),
        command: &probe.command,
        sampling: &probe.sampling,
        stdout,
        stderr,
    }
}
