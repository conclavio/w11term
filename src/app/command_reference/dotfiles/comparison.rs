use super::*;
use crate::app::text_diff::{Difference, Edit as LineEdit};

#[derive(Deserialize)]
#[serde(from = "FragmentReport")]
pub(super) struct Comparison {
    baseline: Baseline,
    changes: Vec<FragmentDiff>,
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Baseline {
    Inactive,
    Verified,
    Unavailable,
}

#[derive(Deserialize)]
struct FragmentReport {
    baseline: Baseline,
    fragments: Vec<Fragment>,
}

#[derive(Deserialize)]
struct Fragment {
    role: String,
    before: Option<String>,
    after: Option<String>,
}

struct FragmentDiff {
    role: String,
    lines: String,
}

impl From<FragmentReport> for Comparison {
    fn from(report: FragmentReport) -> Self {
        let changes = report
            .fragments
            .into_iter()
            .filter_map(|fragment| {
                let lines = diff_text(
                    fragment.before.as_deref().unwrap_or_default(),
                    fragment.after.as_deref().unwrap_or_default(),
                    report.baseline == Baseline::Unavailable,
                )?;
                Some(FragmentDiff {
                    role: fragment.role,
                    lines,
                })
            })
            .collect();
        Self {
            baseline: report.baseline,
            changes,
        }
    }
}

pub(in crate::app) fn diff_text(before: &str, after: &str, proposed_only: bool) -> Option<String> {
    let diff = Difference::compare_files(before, after);
    if diff.changes.is_empty() {
        return None;
    }
    let mut lines = Vec::with_capacity(diff.changes.len() + 2);
    if diff.unaligned {
        lines.push("Changed sections are too large to align. Lines are grouped by version.".into());
    }
    if diff.limited {
        lines.push(
            "This view shows the first changed lines. The CLI preview includes the complete text."
                .into(),
        );
    }
    lines.extend(diff.changes.iter().map(|change| {
        let verb = if proposed_only && change.edit == LineEdit::Added {
            "Proposed"
        } else {
            change.edit.verb()
        };
        format!("{verb} line {} · {}", change.line, change.text)
    }));
    Some(lines.join("\n"))
}

impl Comparison {
    pub(super) fn describe(&self, fields: &mut Vec<KeyedView>) {
        if self.baseline == Baseline::Unavailable {
            fields.push(detail(
                "fragment-baseline",
                "The active managed fragments could not be verified. Only proposed lines are shown.".into(),
            ));
        } else if self.changes.is_empty() {
            fields.push(detail(
                "fragment-baseline",
                "No managed fragment lines will change.".into(),
            ));
        }
        for change in &self.changes {
            let name = match change.role.as_str() {
                "environment" => "Environment fragment",
                "prompt" => "Prompt fragment",
                _ => &change.role,
            };
            fields.push(code_details(
                format!("fragment-changes-{}", change.role),
                &format!("{name} changes"),
                &change.lines,
            ));
        }
    }
}

pub(in crate::app) fn code_details(key: String, label: &str, code: &str) -> KeyedView {
    KeyedView::new(
        key,
        View::component::<CodeDetails>(Code {
            label: label.into(),
            text: code.into(),
        }),
    )
}

#[derive(Clone, PartialEq)]
struct Code {
    label: String,
    text: String,
}

#[derive(Default)]
struct CodeDetails {
    expanded: bool,
}

impl Component for CodeDetails {
    type Input = Code;
    type Message = bool;

    fn create(_: &Code, _: &ComponentContext<Self>) -> Self {
        Self::default()
    }

    fn update(&mut self, expanded: bool, _: &ComponentContext<Self>) {
        self.expanded = expanded;
    }

    fn view(&self, code: &Code, ctx: &mut ViewContext<Self>) -> View {
        Expander::new()
            .automation_name(&code.label)
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .is_expanded(self.expanded)
            .on_is_expanded_changed(ctx.callback(|expanded| expanded))
            .slots([
                SlotView::new(ExpanderSlot::Header, TextBlock::new().text(&code.label)),
                SlotView::new(
                    ExpanderSlot::Content,
                    TextBlock::new()
                        .text_wrapping(TextWrapping::Wrap)
                        .is_text_selection_enabled(true)
                        .text(&code.text),
                ),
            ])
    }
}
