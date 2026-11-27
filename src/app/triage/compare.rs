use super::*;
use crate::app::openrouter_assistance::clip;
use std::fmt::Write as _;
use w11term::triage_comparison::{
    Change, Comparison as TriageComparison, SIDES, check_complete, check_status, coverage_summary,
};

pub struct Comparison {
    pub(super) model: Arc<TriageComparison>,
    selected: Option<usize>,
}

impl Comparison {
    pub fn new(reports: [Arc<TriageReport>; 2], paths: [Option<PathBuf>; 2]) -> Self {
        let model = Arc::new(TriageComparison::new(reports, paths));
        let selected = (!model.changes().is_empty()).then_some(0);
        Self { model, selected }
    }

    pub fn select(&mut self, marker: &Arc<TriageReport>, index: usize) {
        if Arc::ptr_eq(&self.model.reports()[1], marker) && index < self.model.changes().len() {
            self.selected = Some(index);
        }
    }

    fn source(&self, side: usize) -> View {
        let report = &self.model.reports()[side];
        let name = self.model.source_name(side);
        StackPanel::new().spacing(4.0).keyed_children([
            detail("file", display_output(&format!("{} · {name}", SIDES[side]))),
            detail("target", display_output(&endpoint(&report.target))),
            detail("coverage", coverage_summary(report)),
        ])
    }

    fn evidence(&self, change: &Change, side: usize) -> View {
        let report = &self.model.reports()[side];
        let probe = &report.target.probes[change.probe];
        let sample = &report.collection.samples[change.probe];
        let mut fields = vec![
            detail(
                "value",
                format!("{} · {}", SIDES[side], self.model.value(change, side)),
            ),
            detail("command", display_output(&probe.command)),
            detail("sampling", display_output(&probe.sampling)),
        ];
        if let Some((line, evidence)) = self.model.observation_line(change, side) {
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
        let endpoint_matches = self.model.same_endpoint();
        preview.push_str(if endpoint_matches {
            "Same recorded SSH endpoint\n"
        } else {
            "Different recorded SSH endpoints\n"
        });
        for (side, report) in SIDES.into_iter().zip(self.model.reports()) {
            let _ = writeln!(preview, "{side}: {}", coverage_summary(report));
            for probe in 0..report.collection.samples.len() {
                if !check_complete(report, probe) {
                    let id = &report.target.probes[probe].id;
                    let _ = writeln!(preview, "{side} {id}: {}", check_status(report, probe));
                }
            }
        }
        if let Some(change) = self
            .selected
            .and_then(|index| self.model.changes().get(index))
        {
            let _ = writeln!(
                preview,
                "\nSelected evidence · {}",
                self.model.title(change)
            );
            for (side, label) in SIDES.into_iter().enumerate() {
                let report = &self.model.reports()[side];
                let probe = &report.target.probes[change.probe];
                let sample = &report.collection.samples[change.probe];
                let _ = writeln!(
                    preview,
                    "{label} {}: {}\nCommand: {}\nSampling: {}",
                    probe.id,
                    clip(&self.model.value(change, side), 192),
                    clip(&probe.command, 192),
                    clip(&probe.sampling, 192),
                );
                if let Some((line, text)) = self.model.observation_line(change, side) {
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
        if self.model.changes().is_empty() {
            preview.push_str("No changes in check coverage or reported observations.\n");
        }
        for change in self.model.changes() {
            let probe = &self.model.reports()[0].target.probes[change.probe];
            let _ = writeln!(
                preview,
                "{} · {}: Before {} · After {}{}",
                probe.id,
                self.model.title(change),
                clip(&self.model.value(change, 0), 120),
                clip(&self.model.value(change, 1), 120),
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
        if self.model.changes().is_empty() {
            fields.push(detail(
                "compare-empty",
                "No changes in check coverage or reported observations.",
            ));
            return fields;
        }
        let marker = self.model.reports()[1].clone();
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
                    self.model
                        .changes()
                        .iter()
                        .enumerate()
                        .map(|(index, change)| {
                            let mut title = self.model.title(change);
                            if let Some(delta) = change.delta {
                                let _ = write!(title, " · {delta}");
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
        if let Some(change) = self
            .selected
            .and_then(|index| self.model.changes().get(index))
        {
            fields.push(KeyedView::new(
                "compare-evidence",
                columns(self.evidence(change, 0), self.evidence(change, 1)),
            ));
        }
        fields
    }
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
