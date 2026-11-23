use super::*;
use std::collections::BTreeMap;
use w11term::events::GroupTranscriptSnapshot;

#[derive(Clone, Copy)]
pub enum Action {
    Open,
    Back,
    Pause,
    Resume,
}

pub(super) struct State {
    pub(super) submission: u64,
    paused: Option<GroupTranscriptSnapshot>,
}

impl Submission {
    fn output_snapshot(&self) -> GroupTranscriptSnapshot {
        let sent: BTreeSet<_> = self
            .results
            .iter()
            .filter(|delivery| matches!(delivery.status, DeliveryStatus::Sent))
            .map(|delivery| delivery.target.pane)
            .collect();
        let mut snapshot = self.transcript.snapshot();
        snapshot.lines.retain(|line| sent.contains(&line.pane));
        snapshot
    }
}

impl HostGroup {
    pub(super) fn update_live_output(&mut self, id: u64, action: Action) {
        let Some(submission) = self.viewed_submission(id) else {
            return;
        };
        if matches!(action, Action::Open) && self.live_output.is_none() && !self.saved.is_editing()
        {
            self.live_output = Some(State {
                submission: id,
                paused: None,
            });
            return;
        }
        let pause = matches!(action, Action::Pause).then(|| submission.output_snapshot());
        let Some(output) = self
            .live_output
            .as_mut()
            .filter(|output| output.submission == id)
        else {
            return;
        };
        match action {
            Action::Back => self.live_output = None,
            Action::Pause if output.paused.is_none() => {
                output.paused = pause;
            }
            Action::Resume => output.paused = None,
            _ => {}
        }
    }
}

impl TerminalApp {
    pub(super) fn host_group_live_output(
        &self,
        id: TabId,
        output: &State,
        submission: &Submission,
        ctx: &ViewContext<Self>,
    ) -> View {
        let snapshot = output
            .paused
            .as_ref()
            .map_or_else(|| Cow::Owned(submission.output_snapshot()), Cow::Borrowed);
        let names: BTreeMap<_, _> = submission
            .results
            .iter()
            .map(|delivery| (delivery.target.pane, delivery.target.name.as_str()))
            .collect();
        let lines: Vec<_> = snapshot
            .lines
            .iter()
            .rev()
            .filter_map(|line| {
                let pane = line.pane;
                let name = names.get(&pane)?;
                let shortened = if line.shortened { "…" } else { "" };
                let text = format!("{}{shortened}", line.text);
                Some(KeyedView::new(
                    line.id.to_string(),
                    StackPanel::new().spacing(2.0).children((
                        Button::new()
                            .automation_name(format!("Focus output pane {pane}"))
                            .is_enabled(self.tab_for_pane(pane).is_some())
                            .on_click(ctx.callback(move |_| {
                                Message::HostGroup(super::Action::Visit(id, pane))
                            }))
                            .content(*name),
                        TextBlock::new()
                            .text(text)
                            .text_wrapping(TextWrapping::Wrap),
                    )),
                ))
            })
            .collect();
        let submission_id = submission.id;
        let (pause_label, pause_action) = if output.paused.is_some() {
            ("Resume output", Action::Resume)
        } else {
            ("Pause output", Action::Pause)
        };
        let status = if output.paused.is_some() {
            "Paused · newest first"
        } else {
            "Newest first"
        };
        let retention = if snapshot.omitted == 0 {
            View::empty()
        } else {
            TextBlock::new()
                .text("Older lines left the live output. Use pane history for the full command.")
                .text_wrapping(TextWrapping::Wrap)
                .into()
        };
        let content = if lines.is_empty() {
            TextBlock::new()
                .text("No marked output yet. Prompt-only shells show output in their panes.")
                .text_wrapping(TextWrapping::Wrap)
                .into()
        } else {
            StackPanel::new().spacing(8.0).keyed_children(lines)
        };
        Grid::new()
            .margin(Thickness::uniform(16.0))
            .rows([GridLength::Auto, GridLength::STAR])
            .children((
                StackPanel::new().spacing(6.0).children((
                    TextBlock::new()
                        .font_size(24.0)
                        .text("SSH group live output"),
                    TextBlock::new().text(format!("Submitted · {}", submission.text)),
                    StackPanel::new()
                        .spacing(8.0)
                        .orientation(Orientation::Horizontal)
                        .children((
                            Button::new()
                                .on_click(ctx.callback(move |_| {
                                    Message::HostGroup(super::Action::LiveOutput(
                                        id,
                                        submission_id,
                                        Action::Back,
                                    ))
                                }))
                                .content("Back to results"),
                            Button::new()
                                .on_click(ctx.callback(move |_| {
                                    Message::HostGroup(super::Action::LiveOutput(
                                        id,
                                        submission_id,
                                        pause_action,
                                    ))
                                }))
                                .content(pause_label),
                        )),
                    TextBlock::new().text(status),
                    retention,
                )),
                ScrollViewer::new().grid_row(1).content(content),
            ))
    }
}
