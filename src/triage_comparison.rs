//! Shared offline comparison of saved Linux triage reports.

use crate::ssh::{TriageDelta, TriageFinding, TriagePlan, TriageReport, TriageSampleStatus};
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    sync::Arc,
};

mod export;
mod json;

pub const SIDES: [&str; 2] = ["Before", "After"];

#[derive(Clone, Copy)]
enum Kind {
    Check,
    Finding([Option<usize>; 2]),
}

pub struct Change {
    pub probe: usize,
    kind: Kind,
    pub delta: Option<TriageDelta>,
}

pub struct Comparison {
    reports: [Arc<TriageReport>; 2],
    paths: [Option<PathBuf>; 2],
    changes: Vec<Change>,
}

impl Comparison {
    pub fn new(reports: [Arc<TriageReport>; 2], paths: [Option<PathBuf>; 2]) -> Self {
        let [before, after] = &reports;
        let same_target = same_target(&before.target, &after.target);
        let mut changes = Vec::new();
        for probe in 0..before.target.probes.len() {
            let recipe_changed = !same_recipe([before, after], probe);
            if recipe_changed || check_status(before, probe) != check_status(after, probe) {
                changes.push(Change {
                    probe,
                    kind: Kind::Check,
                    delta: None,
                });
            }
        }
        for (index, first) in before.findings.iter().enumerate() {
            let peer = after
                .findings
                .iter()
                .position(|second| same_finding(first, second));
            let Some(probe) = probe_for(before, first) else {
                continue;
            };
            let second = peer.map(|id| &after.findings[id]);
            let delta = second
                .filter(|_| {
                    same_recipe([before, after], probe)
                        && [before, after]
                            .into_iter()
                            .all(|report| check_complete(report, probe))
                })
                .and_then(|finding| finding.delta_from(first, same_target));
            if second.is_none_or(|second| first.value != second.value) || delta.is_some() {
                changes.push(Change {
                    probe,
                    kind: Kind::Finding([Some(index), peer]),
                    delta,
                });
            }
        }
        for (index, second) in after.findings.iter().enumerate() {
            if !before
                .findings
                .iter()
                .any(|first| same_finding(first, second))
                && let Some(probe) = probe_for(after, second)
            {
                changes.push(Change {
                    probe,
                    kind: Kind::Finding([None, Some(index)]),
                    delta: None,
                });
            }
        }
        Self {
            reports,
            paths,
            changes,
        }
    }

    pub fn reports(&self) -> &[Arc<TriageReport>; 2] {
        &self.reports
    }

    pub fn changes(&self) -> &[Change] {
        &self.changes
    }

    pub fn same_endpoint(&self) -> bool {
        same_target(&self.reports[0].target, &self.reports[1].target)
    }

    pub fn after_path(&self) -> Option<&Path> {
        self.paths[1].as_deref()
    }

    pub fn before_source(&self) -> (Arc<TriageReport>, Option<PathBuf>) {
        (self.reports[0].clone(), self.paths[0].clone())
    }

    pub fn value(&self, change: &Change, side: usize) -> String {
        let report = &self.reports[side];
        match change.kind {
            Kind::Check => check_status(report, change.probe),
            Kind::Finding(indices) => indices[side].map_or_else(
                || "Observation unavailable".into(),
                |index| display_output(&report.findings[index].value),
            ),
        }
    }

    pub fn title(&self, change: &Change) -> String {
        let probe = &self.reports[0].target.probes[change.probe];
        match change.kind {
            Kind::Check if !same_recipe(self.reports.each_ref().map(Arc::as_ref), change.probe) => {
                format!("{} check or sampling changed", probe.id)
            }
            Kind::Check => format!("{} check status", probe.id),
            Kind::Finding(indices) => self
                .reports
                .iter()
                .zip(indices)
                .find_map(|(report, index)| index.map(|index| &report.findings[index]))
                .map_or_else(|| probe.id.clone(), |finding| finding.label.clone()),
        }
    }

    pub fn source_name(&self, side: usize) -> Cow<'_, str> {
        self.file_name(side)
            .unwrap_or(Cow::Borrowed("Current collection"))
    }

    fn file_name(&self, side: usize) -> Option<Cow<'_, str>> {
        self.paths[side]
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy())
    }

    pub fn observation_line(&self, change: &Change, side: usize) -> Option<(usize, &str)> {
        let report = &self.reports[side];
        let Kind::Finding(indices) = change.kind else {
            return None;
        };
        let line = report.findings[indices[side]?].stdout_line?;
        let sample = &report.collection.samples[change.probe];
        let text = sample.stdout.text.lines().nth(line.checked_sub(1)?)?;
        Some((line, text.trim()))
    }
}

fn probe_for(report: &TriageReport, finding: &TriageFinding) -> Option<usize> {
    report
        .target
        .probes
        .iter()
        .position(|probe| probe.id == finding.sample_id)
}

fn same_finding(left: &TriageFinding, right: &TriageFinding) -> bool {
    left.resource == right.resource
        && left.sample_id == right.sample_id
        && left.label == right.label
}

fn same_target(before: &TriagePlan, after: &TriagePlan) -> bool {
    before.host.eq_ignore_ascii_case(&after.host)
        && before.user == after.user
        && before.port == after.port
        && before.host_key_fingerprint == after.host_key_fingerprint
}

fn same_recipe(reports: [&TriageReport; 2], probe: usize) -> bool {
    let [before, after] = reports.map(|report| &report.target.probes[probe]);
    before.command == after.command && before.sampling == after.sampling
}

pub fn check_complete(report: &TriageReport, probe: usize) -> bool {
    let sample = &report.collection.samples[probe];
    sample.status == TriageSampleStatus::Ok
        && !sample.stdout.at_byte_limit
        && !sample.stderr.at_byte_limit
}

fn complete_checks(report: &TriageReport) -> usize {
    (0..report.collection.samples.len())
        .filter(|&index| check_complete(report, index))
        .count()
}

pub fn coverage_summary(report: &TriageReport) -> String {
    let count = report.collection.samples.len();
    let complete = complete_checks(report);
    let mut summary = format!("{complete} of {count} checks complete");
    if report.collection.stopped_by_user {
        summary.push_str(" · Stopped by request");
    } else if report.collection.supervisor_status != 0 {
        summary.push_str(&format!(
            " · Collector exit {}",
            report.collection.supervisor_status
        ));
    }
    summary
}

pub fn check_status(report: &TriageReport, probe: usize) -> String {
    let sample = &report.collection.samples[probe];
    let mut status = status_label(sample.status).to_owned();
    if let Some(exit) = sample.exit_status {
        status.push_str(&format!(" · Exit {exit}"));
    }
    for (label, limited) in [
        ("output", sample.stdout.at_byte_limit),
        ("standard error", sample.stderr.at_byte_limit),
    ] {
        if limited {
            status.push_str(&format!(" · {label} byte limit"));
        }
    }
    status
}

pub fn status_label(status: TriageSampleStatus) -> &'static str {
    match status {
        TriageSampleStatus::Ok => "OK",
        TriageSampleStatus::Missing => "Utility missing",
        TriageSampleStatus::TimedOut => "Timed out",
        TriageSampleStatus::Incomplete => "Incomplete",
        TriageSampleStatus::OutputLimit => "Output limit",
        TriageSampleStatus::PermissionDenied => "Permission denied",
        TriageSampleStatus::Failed => "Failed",
    }
}

pub fn display_output(text: &str) -> String {
    let mut display = String::with_capacity(text.len());
    for c in text.chars() {
        if (c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
            || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            display.extend(c.escape_default());
        } else {
            display.push(c);
        }
    }
    display
}
