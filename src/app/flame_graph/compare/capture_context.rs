use super::{Action, ProfileSide};
use crate::app::flame_graph::{Action as ViewerAction, message, report, text};
use crate::app::*;
use w11term::ssh::CpuProfileCaptureReport;

pub(super) struct Context {
    pub(super) reports: [Option<Arc<CpuProfileCaptureReport>>; 2],
    pub(super) expanded: bool,
}

impl Context {
    pub(super) fn new(reports: [Option<Arc<CpuProfileCaptureReport>>; 2]) -> Self {
        let expanded = reports.iter().any(Option::is_some);
        Self { reports, expanded }
    }

    pub(super) fn view(&self, idle: bool, ctx: &ViewContext<TerminalApp>) -> KeyedView {
        let mut fields = Vec::new();
        if let [Some(before), Some(after)] = &self.reports {
            fields.push(text("sampling-comparison", compare_sampling(before, after)));
        }
        for (side, key, title, button) in [
            (
                ProfileSide::Before,
                "before",
                "Before capture",
                "Verify Before report…",
            ),
            (
                ProfileSide::After,
                "after",
                "After capture",
                "Verify After report…",
            ),
        ] {
            let mut details = vec![
                text("side", title),
                KeyedView::new(
                    "verify-report",
                    Button::new()
                        .is_enabled(idle)
                        .on_click(ctx.callback(move |_| {
                            message(ViewerAction::Compare(Action::VerifyReport(side)))
                        }))
                        .content(button),
                ),
            ];
            if let Some(report) = &self.reports[side.index()] {
                details.push(KeyedView::new(
                    "capture-details",
                    ScrollViewer::new().max_height(146.0).content(
                        StackPanel::new()
                            .spacing(4.0)
                            .keyed_children(report::capture_fields(report)),
                    ),
                ));
            } else {
                details.push(text("no-report", "Capture report unavailable"));
            }
            fields.push(KeyedView::new(
                key,
                StackPanel::new().spacing(4.0).keyed_children(details),
            ));
        }
        KeyedView::new(
            "compare-capture-settings",
            Expander::new()
                .automation_name("CPU comparison capture settings")
                .is_expanded(self.expanded)
                .on_is_expanded_changed(ctx.callback(|expanded| {
                    message(ViewerAction::Compare(Action::CaptureSettingsExpanded(
                        expanded,
                    )))
                }))
                .slots([
                    SlotView::new(
                        ExpanderSlot::Header,
                        TextBlock::new().text("Capture settings"),
                    ),
                    SlotView::new(
                        ExpanderSlot::Content,
                        StackPanel::new().spacing(8.0).keyed_children(fields),
                    ),
                ]),
        )
    }
}

fn compare_sampling(before: &CpuProfileCaptureReport, after: &CpuProfileCaptureReport) -> String {
    let before = report::Sampling::of(before);
    let after = report::Sampling::of(after);
    let changed: Vec<_> = [
        (before.event != after.event, "event"),
        (before.seconds != after.seconds, "duration"),
        (before.frequency != after.frequency, "frequency"),
        (before.call_graph != after.call_graph, "unwind mode"),
    ]
    .into_iter()
    .filter_map(|(different, field)| different.then_some(field))
    .collect();
    if changed.is_empty() {
        "Sampling settings match".into()
    } else {
        format!("Sampling settings differ · {}", changed.join(", "))
    }
}
