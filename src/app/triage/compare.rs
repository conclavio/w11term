use super::*;
use w11term::ssh::TriageFinding;

const SIDES: [&str; 2] = ["Before", "After"];

#[derive(Clone, Copy)]
enum Kind {
    Check,
    Finding([Option<usize>; 2]),
}

struct Change {
    probe: usize,
    kind: Kind,
    delta: Option<String>,
}

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

    fn source(&self, side: usize) -> View {
        let report = &self.reports[side];
        let name = self.paths[side]
            .as_ref()
            .and_then(|path| path.file_name())
            .map_or_else(
                || "Current collection".into(),
                |name| name.to_string_lossy(),
            );
        let complete = (0..report.collection.samples.len())
            .filter(|&index| check_complete(report, index))
            .count();
        let status = if report.collection.stopped_by_user {
            " · Stopped by request".into()
        } else if report.collection.supervisor_status != 0 {
            format!(" · Collector exit {}", report.collection.supervisor_status)
        } else {
            String::new()
        };
        StackPanel::new().spacing(4.0).keyed_children([
            detail("file", display_output(&format!("{} · {name}", SIDES[side]))),
            detail("target", display_output(&endpoint(&report.target))),
            detail(
                "coverage",
                format!(
                    "{complete} of {} checks complete{status}",
                    report.collection.samples.len()
                ),
            ),
        ])
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
        let line = match change.kind {
            Kind::Check => None,
            Kind::Finding(indices) => {
                indices[side].and_then(|index| report.findings[index].stdout_line)
            }
        };
        if let Some((line, evidence)) = line.and_then(|line| {
            sample
                .stdout
                .text
                .lines()
                .nth(line.checked_sub(1)?)
                .map(|text| (line, text))
        }) {
            fields.push(detail(
                "evidence-label",
                format!("{} output line {line}", probe.id),
            ));
            fields.push(selectable("evidence", evidence.trim()));
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

    pub fn fields(&self, ctx: &ViewContext<TerminalApp>) -> Vec<KeyedView> {
        let mut fields = vec![
            KeyedView::new(
                "compare-back",
                Button::new()
                    .on_click(ctx.callback(|_| message(Action::BackFromComparison)))
                    .content("Back to report"),
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
