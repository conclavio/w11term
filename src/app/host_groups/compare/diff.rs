use super::*;
use std::io::{self, Write};

const MAX_ALIGNMENT_CELLS: usize = 1_000_000;
const MAX_CHANGED_LINES: usize = 256;
const MAX_LINE_BYTES: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edit {
    Removed,
    Added,
}

impl Edit {
    fn verb(self) -> &'static str {
        match self {
            Self::Removed => "Removed",
            Self::Added => "Added",
        }
    }
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

struct ChangedSections<'a> {
    before: Vec<&'a str>,
    after: Vec<&'a str>,
    prefix: usize,
}

impl<'a> ChangedSections<'a> {
    fn new(before: &'a str, after: &'a str) -> Self {
        let mut before: Vec<_> = before.split_inclusive('\n').collect();
        let mut after: Vec<_> = after.split_inclusive('\n').collect();
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
        before.truncate(before.len() - suffix);
        after.truncate(after.len() - suffix);
        Self {
            before,
            after,
            prefix,
        }
    }

    fn before(&self) -> &[&'a str] {
        &self.before[self.prefix..]
    }

    fn after(&self) -> &[&'a str] {
        &self.after[self.prefix..]
    }

    fn unaligned(&self) -> bool {
        !self.before().is_empty()
            && !self.after().is_empty()
            && (self.before().len() + 1).saturating_mul(self.after().len() + 1)
                > MAX_ALIGNMENT_CELLS
    }

    fn visit<E>(&self, mut line: impl FnMut(Edit, usize, &str) -> Result<(), E>) -> Result<(), E> {
        let before = self.before();
        let after = self.after();
        if before.is_empty() || after.is_empty() || self.unaligned() {
            return self.visit_sources(before.len(), after.len(), line);
        }
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
            if removing {
                i += 1;
                line(Edit::Removed, self.prefix + i, before[i - 1])?;
            } else {
                j += 1;
                line(Edit::Added, self.prefix + j, after[j - 1])?;
            }
        }
        Ok(())
    }

    fn visit_sources<E>(
        &self,
        removed: usize,
        added: usize,
        mut line: impl FnMut(Edit, usize, &str) -> Result<(), E>,
    ) -> Result<(), E> {
        for (index, text) in self.before().iter().take(removed).enumerate() {
            line(Edit::Removed, self.prefix + index + 1, text)?;
        }
        for (index, text) in self.after().iter().take(added).enumerate() {
            line(Edit::Added, self.prefix + index + 1, text)?;
        }
        Ok(())
    }
}

fn line_parts(text: &str) -> (&str, &'static str) {
    if let Some(text) = text.strip_suffix("\r\n") {
        (text, "")
    } else if let Some(text) = text.strip_suffix('\n') {
        (text, " [LF]")
    } else {
        (text, " [no newline]")
    }
}

impl Difference {
    pub(super) fn build(before: &str, after: &str) -> Self {
        let sections = ChangedSections::new(before, after);
        let mut difference = Self {
            unaligned: sections.unaligned(),
            ..Self::default()
        };
        if difference.unaligned {
            let before = sections.before().len();
            let after = sections.after().len();
            let removed = before.min(MAX_CHANGED_LINES - after.min(MAX_CHANGED_LINES / 2));
            let added = after.min(MAX_CHANGED_LINES - removed);
            difference.limited = removed < before || added < after;
            let _ = sections.visit_sources(removed, added, |edit, line, text| {
                difference.push(edit, line, text)
            });
        } else {
            let _ = sections.visit(|edit, line, text| difference.push(edit, line, text));
        }
        difference
    }

    fn push(&mut self, edit: Edit, line: usize, text: &str) -> Result<(), ()> {
        if self.changes.len() == MAX_CHANGED_LINES {
            self.limited = true;
            return Err(());
        }
        let (text, ending) = line_parts(text);
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
        Ok(())
    }
}

pub(super) struct Panel {
    reference: Side,
    selected: Side,
    pub(super) result: Option<Result<Difference, &'static str>>,
}

#[derive(Clone)]
struct Side {
    pane: u64,
    heading: String,
    output: report::Output,
}

pub(super) fn write_changes(
    mut file: impl Write,
    before: &str,
    after: &str,
    identical: &str,
) -> io::Result<()> {
    let sections = ChangedSections::new(before, after);
    if sections.before().is_empty() && sections.after().is_empty() {
        return writeln!(file, "{identical}\r");
    }
    if sections.unaligned() {
        writeln!(
            file,
            "Line alignment limit reached. The full unmatched sections follow by source.\r"
        )?;
    }
    sections.visit(|edit, line, text| {
        let (body, ending) = line_parts(text);
        writeln!(file, "{} line {line} · {body}{ending}\r", edit.verb())
    })
}

impl Panel {
    pub(super) fn view(
        &self,
        command: &str,
        revision: u64,
        can_save: bool,
        action: impl Fn(Action) -> Message + Copy + 'static,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let text = |value: &str| {
            TextBlock::new()
                .text(value)
                .text_wrapping(TextWrapping::Wrap)
        };
        let content = result_view(
            self.result.as_ref(),
            "Output is identical. The exit codes differ.",
        );
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
                    text(&self.reference.heading),
                    text(&self.selected.heading),
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            Button::new()
                                .on_click(ctx.callback(move |_| action(Action::CloseDiff)))
                                .content("Back to comparison"),
                            Button::new()
                                .is_enabled(can_save)
                                .on_click(
                                    ctx.callback(move |_| action(Action::SaveDiff { revision })),
                                )
                                .content("Save full differences…"),
                        )),
                )),
                ScrollViewer::new().grid_row(1).content(content),
            ))
    }
}

impl TerminalApp {
    pub(super) fn save_group_diff(
        &mut self,
        id: TabId,
        submission_id: u64,
        revision: u64,
        ctx: &ComponentContext<Self>,
    ) {
        if self.export.pending.is_some() {
            return;
        }
        let Some(page) = self.host_group_mut(id) else {
            return;
        };
        if page.comparison.revision != revision {
            return;
        }
        let Some(submission) = page.viewed_submission(submission_id) else {
            return;
        };
        let Some(panel) = page
            .comparison
            .open
            .as_ref()
            .filter(|open| open.submission == submission_id)
            .and_then(|open| open.diff.as_ref())
        else {
            return;
        };
        let (reference, selected) = (panel.reference.clone(), panel.selected.clone());
        let command = submission.content.label().to_owned();
        self.begin_text_export(
            text_export::Subject::HostGroupDiff {
                submission: submission_id,
                reference: reference.pane,
                selected: selected.pane,
            },
            move |file| {
                let mut file = io::BufWriter::new(file);
                writeln!(file, "Submitted · {command}\r")?;
                writeln!(file, "{}\r\n{}\r", reference.heading, selected.heading)?;
                write_changes(
                    &mut file,
                    reference.output.text(),
                    selected.output.text(),
                    "Output is identical. The exit codes differ.",
                )?;
                file.flush()
            },
            ctx,
        );
    }

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
            reference: Side {
                pane: reference_member.pane,
                heading: format!(
                    "Reference · {} · Exit {}",
                    reference_member.name, before.exit_code
                ),
                output: before.output.clone(),
            },
            selected: Side {
                pane,
                heading: format!(
                    "Selected · {} · Exit {}",
                    after.members[0].name, after.exit_code
                ),
                output: after.output.clone(),
            },
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

pub(super) fn result_view(
    result: Option<&Result<Difference, &'static str>>,
    identical: &str,
) -> View {
    let text = |value: &str| {
        TextBlock::new()
            .text(value)
            .text_wrapping(TextWrapping::Wrap)
    };
    match result {
        None => text("Comparing output lines…").into(),
        Some(Err(message)) => text(message).into(),
        Some(Ok(diff)) if diff.changes.is_empty() => text(identical).into(),
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
                    let verb = change.edit.verb();
                    KeyedView::new(
                        index,
                        text(&format!("{verb} line {} · {}", change.line, change.text)),
                    )
                })),
        )),
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

    #[test]
    fn export_includes_every_line_and_marks_an_unaligned_difference() {
        let before = "east\r\n".repeat(1100);
        let mut after = "west\r\n".repeat(1100);
        let final_line = "雪".repeat(MAX_LINE_BYTES);
        after.push_str(&final_line);
        after.push(' ');
        let mut bytes = Vec::new();
        write_changes(&mut bytes, &before, &after, "Identical.").unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("Line alignment limit reached."));
        assert_eq!(text.matches("Removed line ").count(), 1100);
        assert_eq!(text.matches("Added line ").count(), 1101);
        assert!(text.ends_with(&format!("Added line 1101 · {final_line}  [no newline]\r\n")));
    }

    #[test]
    fn export_reuses_line_alignment_and_preserves_source_whitespace() {
        let mut bytes = Vec::new();
        write_changes(
            &mut bytes,
            "old \r\ncommon\r\n",
            "new\ncommon\r\n",
            "Identical.",
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "Removed line 1 · old \r\nAdded line 1 · new [LF]\r\n"
        );
    }
}
