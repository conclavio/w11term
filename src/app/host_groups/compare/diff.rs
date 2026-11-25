use super::*;
use std::io::{self, Write};

pub(super) use crate::app::text_diff::{Difference, write_changes};

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
