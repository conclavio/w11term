use super::*;

const MAX_ALIGNMENT_CELLS: usize = 1_000_000;
const MAX_CHANGED_LINES: usize = 256;
const MAX_LINE_BYTES: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edit {
    Removed,
    Added,
}

struct Change {
    edit: Edit,
    line: usize,
    text: String,
}

#[derive(Default)]
pub struct Difference {
    changes: Vec<Change>,
    unaligned: bool,
    limited: bool,
}

impl Difference {
    fn build(before: &str, after: &str) -> Self {
        let before: Vec<_> = before.split_inclusive('\n').collect();
        let after: Vec<_> = after.split_inclusive('\n').collect();
        let prefix = before
            .iter()
            .zip(&after)
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = before[prefix..]
            .iter()
            .rev()
            .zip(after[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let before = &before[prefix..before.len() - suffix];
        let after = &after[prefix..after.len() - suffix];
        let mut difference = Self::default();
        if before.is_empty() || after.is_empty() {
            let changes = before
                .iter()
                .enumerate()
                .map(|(i, text)| (Edit::Removed, i, *text))
                .chain(
                    after
                        .iter()
                        .enumerate()
                        .map(|(i, text)| (Edit::Added, i, *text)),
                );
            for (edit, index, text) in changes {
                if !difference.push(edit, prefix + index + 1, text) {
                    break;
                }
            }
        } else if (before.len() + 1).saturating_mul(after.len() + 1) <= MAX_ALIGNMENT_CELLS {
            difference.align(before, after, prefix);
        } else {
            difference.unaligned = true;
            let removed = before
                .len()
                .min(MAX_CHANGED_LINES - after.len().min(MAX_CHANGED_LINES / 2));
            let added = after.len().min(MAX_CHANGED_LINES - removed);
            difference.limited = removed < before.len() || added < after.len();
            for (index, text) in before[..removed].iter().enumerate() {
                difference.push(Edit::Removed, prefix + index + 1, text);
            }
            for (index, text) in after[..added].iter().enumerate() {
                difference.push(Edit::Added, prefix + index + 1, text);
            }
        }
        difference
    }

    fn align<'a>(&mut self, before: &[&'a str], after: &[&'a str], prefix: usize) {
        let mut ids = HashMap::new();
        let mut tokens = |lines: &[&'a str]| {
            lines
                .iter()
                .map(|&line| {
                    let next = ids.len();
                    *ids.entry(line).or_insert(next)
                })
                .collect::<Vec<_>>()
        };
        let (before_ids, after_ids) = (tokens(before), tokens(after));
        // Each cell holds the longest common subsequence of the two suffixes.
        let width = after.len() + 1;
        let index = |i: usize, j: usize| i * width + j;
        let mut common = vec![0_u32; (before.len() + 1) * width];
        for i in (0..before.len()).rev() {
            for j in (0..after.len()).rev() {
                common[index(i, j)] = if before_ids[i] == after_ids[j] {
                    1 + common[index(i + 1, j + 1)]
                } else {
                    common[index(i + 1, j)].max(common[index(i, j + 1)])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < before.len() || j < after.len() {
            if i < before.len() && j < after.len() && before_ids[i] == after_ids[j] {
                i += 1;
                j += 1;
                continue;
            }
            let removing = j == after.len()
                || (i < before.len() && common[index(i + 1, j)] >= common[index(i, j + 1)]);
            let appended = if removing {
                i += 1;
                self.push(Edit::Removed, prefix + i, before[i - 1])
            } else {
                j += 1;
                self.push(Edit::Added, prefix + j, after[j - 1])
            };
            if !appended {
                break;
            }
        }
    }

    fn push(&mut self, edit: Edit, line: usize, text: &str) -> bool {
        if self.changes.len() == MAX_CHANGED_LINES {
            self.limited = true;
            return false;
        }
        let (text, ending) = if let Some(text) = text.strip_suffix("\r\n") {
            (text, "")
        } else if let Some(text) = text.strip_suffix('\n') {
            (text, " [LF]")
        } else {
            (text, " [no newline]")
        };
        let end = text.floor_char_boundary(text.len().min(MAX_LINE_BYTES));
        let clipped = if end < text.len() { "…" } else { "" };
        let trailing_spaces = text.len() - text.trim_end_matches(' ').len();
        let trailing = if trailing_spaces > 0 {
            let unit = if trailing_spaces == 1 {
                "space"
            } else {
                "spaces"
            };
            format!(" [{trailing_spaces} trailing {unit}]")
        } else {
            String::new()
        };
        self.changes.push(Change {
            edit,
            line,
            text: format!("{}{clipped}{trailing}{ending}", &text[..end]),
        });
        true
    }
}

pub(super) struct Panel {
    reference: String,
    selected: String,
    pub(super) result: Option<Result<Difference, &'static str>>,
}

impl Panel {
    pub(super) fn view(
        &self,
        command: &str,
        action: impl Fn(Action) -> Message + 'static,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let text = |value: &str| {
            TextBlock::new()
                .text(value)
                .text_wrapping(TextWrapping::Wrap)
        };
        let content = match &self.result {
            None => text("Comparing output lines…").into(),
            Some(Err(message)) => text(message).into(),
            Some(Ok(diff)) if diff.changes.is_empty() => {
                text("Output is identical. The exit codes differ.").into()
            }
            Some(Ok(diff)) => StackPanel::new().spacing(5.0).children((
                if diff.unaligned {
                    text("The changed sections are too large to align. Lines are shown by source.")
                        .into()
                } else {
                    View::empty()
                },
                if diff.limited {
                    text("More changed lines are available in the full output.").into()
                } else {
                    View::empty()
                },
                StackPanel::new()
                    .spacing(4.0)
                    .keyed_children(diff.changes.iter().enumerate().map(|(index, change)| {
                        let verb = match change.edit {
                            Edit::Removed => "Removed",
                            Edit::Added => "Added",
                        };
                        KeyedView::new(
                            index,
                            text(&format!("{verb} line {} · {}", change.line, change.text)),
                        )
                    })),
            )),
        };
        Grid::new()
            .margin(16.0)
            .row_spacing(8.0)
            .rows([GridLength::Auto, GridLength::STAR])
            .children((
                StackPanel::new().spacing(6.0).children((
                    TextBlock::new()
                        .font_size(24.0)
                        .text("SSH group output differences"),
                    TextBlock::new()
                        .text(format!("Submitted · {command}"))
                        .text_trimming(TextTrimming::CharacterEllipsis),
                    text(&self.reference),
                    text(&self.selected),
                    Button::new()
                        .on_click(ctx.callback(move |_| action(Action::CloseDiff)))
                        .content("Back to comparison"),
                )),
                ScrollViewer::new().grid_row(1).content(content),
            ))
    }
}

impl TerminalApp {
    pub(super) fn open_group_diff(
        &mut self,
        id: TabId,
        submission: u64,
        revision: u64,
        pane: u64,
        ctx: &ComponentContext<Self>,
    ) {
        let Some(page) = self.host_group_mut(id) else {
            return;
        };
        let state = &mut page.comparison;
        if state.revision != revision {
            return;
        }
        let Some(open) = state.open.as_mut().filter(|open| {
            open.submission == submission && open.diff.is_none() && !open.updating_reference
        }) else {
            return;
        };
        let Some(Ok(comparison)) = &open.comparison else {
            return;
        };
        let Some(reference) = comparison.reference.filter(|&reference| reference != pane) else {
            return;
        };
        let (Some(before), Some(after)) = (comparison.group(reference), comparison.group(pane))
        else {
            return;
        };
        let reference_member = comparison
            .preferred
            .and_then(|pane| before.members.iter().find(|member| member.pane == pane))
            .unwrap_or(&before.members[0]);
        open.diff = Some(Panel {
            reference: format!(
                "Reference · {} · Exit {}",
                reference_member.name, before.exit_code
            ),
            selected: format!(
                "Selected · {} · Exit {}",
                after.members[0].name, after.exit_code
            ),
            result: None,
        });
        let before = before.output.clone();
        let after = after.output.clone();
        state.revision += 1;
        let revision = state.revision;
        let ready = move |result| {
            Message::HostGroup(super::super::Action::Compare(
                id,
                submission,
                Action::DiffReady { revision, result },
            ))
        };
        ctx.spawn_background_with_rejection(
            move |cancel| {
                let result = if cancel.is_cancelled() {
                    Err("Output comparison cancelled.")
                } else {
                    Ok(Difference::build(before.text(), after.text()))
                };
                ready(result)
            },
            ready(Err("Could not start the output comparison.")),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligns_insertions_and_removals_and_keeps_original_line_numbers() {
        let result = Difference::build(
            "same\r\nold\r\nanchor\r\ntail\r\n",
            "same\r\nnew\r\nanchor\r\ninserted\r\ntail\r\n",
        );
        let edits: Vec<_> = result
            .changes
            .iter()
            .map(|change| (change.edit, change.line, change.text.as_str()))
            .collect();
        assert_eq!(
            edits,
            [
                (Edit::Removed, 2, "old"),
                (Edit::Added, 2, "new"),
                (Edit::Added, 4, "inserted")
            ],
        );
        assert!(!result.unaligned && !result.limited);
        assert!(Difference::build("same", "same").changes.is_empty());
        let result = Difference::build("same\r\n", "same \r\n");
        assert_eq!(result.changes[1].text, "same  [1 trailing space]");
    }

    #[test]
    fn aligns_a_late_small_change_and_marks_line_endings() {
        let common = "same\r\n".repeat(10_000);
        let result = Difference::build(&format!("{common}end\r\n"), &format!("{common}end"));
        assert_eq!(result.changes.len(), 2);
        assert_eq!(result.changes[0].line, 10_001);
        assert_eq!(result.changes[1].text, "end [no newline]");
        assert!(!result.unaligned && !result.limited);
    }

    #[test]
    fn shows_both_sources_when_the_changed_sections_exceed_the_alignment_budget() {
        let result = Difference::build(&"east\r\n".repeat(1100), &"west\r\n".repeat(1100));
        assert!(result.unaligned && result.limited);
        assert_eq!(result.changes.len(), MAX_CHANGED_LINES);
        assert_eq!(
            result.changes[MAX_CHANGED_LINES / 2 - 1].edit,
            Edit::Removed
        );
        assert_eq!(result.changes[MAX_CHANGED_LINES / 2].edit, Edit::Added);

        let result = Difference::build("", &"雪".repeat(MAX_LINE_BYTES));
        assert!(result.changes[0].text.ends_with("… [no newline]"));
    }
}
