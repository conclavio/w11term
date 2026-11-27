use super::*;
use crate::app::openrouter_assistance::clip;
use std::{borrow::Cow, fmt::Write as _};
use w11term::ssh::TriageFinding;

mod export;

const SIDES: [&str; 2] = ["Before", "After"];

#[derive(Clone, Copy)]
enum Kind {
    Check,
    Finding([Option<usize>; 2]),
}

#[derive(Clone)]
struct Change {
    probe: usize,
    kind: Kind,
    delta: Option<String>,
}

#[derive(Clone)]
pub struct Comparison {
    reports: [Arc<TriageReport>; 2],
    paths: [Option<PathBuf>; 2],
    changes: Vec<Change>,
    selected: Option<usize>,
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
                .and_then(|finding| finding.change_from(first, same_target));
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
        let selected = (!changes.is_empty()).then_some(0);
        Self {
            reports,
            paths,
            changes,
            selected,
        }
    }

    pub fn select(&mut self, marker: &Arc<TriageReport>, index: usize) {
        if Arc::ptr_eq(&self.reports[1], marker) && index < self.changes.len() {
            self.selected = Some(index);
        }
    }

    pub(super) fn after_path(&self) -> Option<&std::path::Path> {
        self.paths[1].as_deref()
    }

    pub(super) fn before_source(&self) -> (Arc<TriageReport>, Option<PathBuf>) {
        (self.reports[0].clone(), self.paths[0].clone())
    }

    fn value(&self, change: &Change, side: usize) -> String {
        let report = &self.reports[side];
        match change.kind {
            Kind::Check => check_status(report, change.probe),
            Kind::Finding(indices) => indices[side].map_or_else(
                || "Observation unavailable".into(),
                |index| display_output(&report.findings[index].value),
            ),
        }
    }

    fn title(&self, change: &Change) -> String {
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

    fn source_name(&self, side: usize) -> Cow<'_, str> {
        self.paths[side]
            .as_ref()
            .and_then(|path| path.file_name())
            .map_or_else(
                || "Current collection".into(),
                |name| name.to_string_lossy(),
            )
    }

    fn source(&self, side: usize) -> View {
        let report = &self.reports[side];
        let name = self.source_name(side);
        StackPanel::new().spacing(4.0).keyed_children([
            detail("file", display_output(&format!("{} · {name}", SIDES[side]))),
            detail("target", display_output(&endpoint(&report.target))),
            detail("coverage", coverage_summary(report)),
        ])
    }

    fn observation_line(&self, change: &Change, side: usize) -> Option<(usize, &str)> {
        let report = &self.reports[side];
        let Kind::Finding(indices) = change.kind else {
            return None;
        };
        let line = report.findings[indices[side]?].stdout_line?;
        let sample = &report.collection.samples[change.probe];
        let text = sample.stdout.text.lines().nth(line.checked_sub(1)?)?;
        Some((line, text.trim()))
    }

    fn evidence(&self, change: &Change, side: usize) -> View {
        let report = &self.reports[side];
        let probe = &report.target.probes[change.probe];
        let sample = &report.collection.samples[change.probe];
        let mut fields = vec![
            detail(
                "value",
                format!("{} · {}", SIDES[side], self.value(change, side)),
            ),
            detail("command", display_output(&probe.command)),
            detail("sampling", display_output(&probe.sampling)),
        ];
        if let Some((line, evidence)) = self.observation_line(change, side) {
            fields.push(detail(
                "evidence-label",
                format!("{} output line {line}", probe.id),
            ));
            fields.push(selectable("evidence", evidence));
        } else {
            fields.push(detail("check-status", check_status(report, change.probe)));
            for (key, label, output) in [
                ("stdout", "Output excerpt", &sample.stdout.text),
                ("stderr", "Standard error excerpt", &sample.stderr.text),
            ] {
                if !output.is_empty() {
                    fields.push(detail(format!("{key}-label"), label));
                    fields.push(selectable(key, output));
                }
            }
            if sample.stdout.text.is_empty() && sample.stderr.text.is_empty() {
                fields.push(detail("no-output", "No output captured."));
            }
        }
        StackPanel::new().spacing(4.0).keyed_children(fields)
    }

    pub fn openrouter_preview(&self) -> String {
        let mut preview = String::from("Linux performance triage comparison\n\n");
        let endpoint_matches = same_target(&self.reports[0].target, &self.reports[1].target);
        preview.push_str(if endpoint_matches {
            "Same recorded SSH endpoint\n"
        } else {
            "Different recorded SSH endpoints\n"
        });
        for (side, report) in SIDES.into_iter().zip(&self.reports) {
            let _ = writeln!(preview, "{side}: {}", coverage_summary(report));
            for probe in 0..report.collection.samples.len() {
                if !check_complete(report, probe) {
                    let id = &report.target.probes[probe].id;
                    let _ = writeln!(preview, "{side} {id}: {}", check_status(report, probe));
                }
            }
        }
        if let Some(change) = self.selected.and_then(|index| self.changes.get(index)) {
            let _ = writeln!(preview, "\nSelected evidence · {}", self.title(change));
            for (side, label) in SIDES.into_iter().enumerate() {
                let report = &self.reports[side];
                let probe = &report.target.probes[change.probe];
                let sample = &report.collection.samples[change.probe];
                let _ = writeln!(
                    preview,
                    "{label} {}: {}\nCommand: {}\nSampling: {}",
                    probe.id,
                    clip(&self.value(change, side), 192),
                    clip(&probe.command, 192),
                    clip(&probe.sampling, 192),
                );
                if let Some((line, text)) = self.observation_line(change, side) {
                    let _ = writeln!(
                        preview,
                        "{label} {} stdout line {line}: {}",
                        probe.id,
                        clip(text, 512),
                    );
                } else {
                    for (stream, text) in [
                        ("stdout", &sample.stdout.text),
                        ("stderr", &sample.stderr.text),
                    ] {
                        if !text.is_empty() {
                            let _ = writeln!(
                                preview,
                                "{label} {} {stream} excerpt:\n{}",
                                probe.id,
                                clip(text, 512),
                            );
                        }
                    }
                }
            }
        }
        preview.push_str("\nChanges in check coverage and observations\n");
        if self.changes.is_empty() {
            preview.push_str("No changes in check coverage or reported observations.\n");
        }
        for change in &self.changes {
            let probe = &self.reports[0].target.probes[change.probe];
            let _ = writeln!(
                preview,
                "{} · {}: Before {} · After {}{}",
                probe.id,
                self.title(change),
                clip(&self.value(change, 0), 120),
                clip(&self.value(change, 1), 120),
                change
                    .delta
                    .as_ref()
                    .map_or_else(String::new, |delta| format!(" · {delta}")),
            );
        }
        preview
    }

    pub fn fields(
        &self,
        idle: bool,
        export_idle: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> Vec<KeyedView> {
        let mut fields = vec![
            KeyedView::new(
                "compare-back",
                Button::new()
                    .is_enabled(idle)
                    .on_click(ctx.callback(|_| message(Action::BackFromComparison)))
                    .content("Back to report"),
            ),
            KeyedView::new(
                "compare-export",
                Button::new()
                    .is_enabled(idle && export_idle)
                    .on_click(ctx.callback(|_| message(Action::ExportComparison)))
                    .content("Export comparison…"),
            ),
            KeyedView::new(
                "compare-openrouter",
                Button::new()
                    .is_enabled(idle)
                    .on_click(ctx.callback(|_| message(Action::OpenRouterComparison)))
                    .content("Review comparison with OpenRouter…"),
            ),
            KeyedView::new("compare-sources", columns(self.source(0), self.source(1))),
        ];
        if self.changes.is_empty() {
            fields.push(detail(
                "compare-empty",
                "No changes in check coverage or reported observations.",
            ));
            return fields;
        }
        let marker = self.reports[1].clone();
        fields.push(KeyedView::new(
            "compare-changes",
            ListView::new()
                .automation_name("Triage report changes")
                .max_height(240.0)
                .selected_index(self.selected)
                .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                    index.map_or(Message::ControlIdle, |index| {
                        message(Action::ComparisonChange(marker.clone(), index))
                    })
                }))
                .collection_slot(
                    ListViewSlot::Items,
                    self.changes.iter().enumerate().map(|(index, change)| {
                        let mut title = self.title(change);
                        if let Some(delta) = &change.delta {
                            title.push_str(" · ");
                            title.push_str(delta);
                        }
                        KeyedView::new(
                            index.to_string(),
                            ListViewItem::new().automation_name(&title).content(
                                TextBlock::new()
                                    .text(title)
                                    .text_wrapping(TextWrapping::Wrap),
                            ),
                        )
                    }),
                ),
        ));
        if let Some(change) = self.selected.and_then(|index| self.changes.get(index)) {
            fields.push(KeyedView::new(
                "compare-evidence",
                columns(self.evidence(change, 0), self.evidence(change, 1)),
            ));
        }
        fields
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

fn check_complete(report: &TriageReport, probe: usize) -> bool {
    let sample = &report.collection.samples[probe];
    sample.status == TriageSampleStatus::Ok
        && !sample.stdout.at_byte_limit
        && !sample.stderr.at_byte_limit
}

fn coverage_summary(report: &TriageReport) -> String {
    let count = report.collection.samples.len();
    let complete = (0..count)
        .filter(|&index| check_complete(report, index))
        .count();
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

fn check_status(report: &TriageReport, probe: usize) -> String {
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

fn columns(before: View, after: View) -> View {
    Grid::new()
        .columns([GridLength::Star(1.0), GridLength::Star(1.0)])
        .column_spacing(12.0)
        .children((
            Border::new().content(before),
            Border::new().grid_column(1).content(after),
        ))
}

fn selectable(key: &str, output: &str) -> KeyedView {
    let excerpt = output.char_indices().nth(2048).map_or_else(
        || display_output(output),
        |(end, _)| format!("{}…", display_output(&output[..end])),
    );
    KeyedView::new(
        key,
        TextBlock::new()
            .text(excerpt)
            .text_wrapping(TextWrapping::Wrap)
            .is_text_selection_enabled(true),
    )
}
