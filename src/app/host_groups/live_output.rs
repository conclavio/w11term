use super::*;
use regex::{Regex, RegexBuilder};
use std::collections::BTreeMap;
use w11term::{
    events::{GroupTranscriptLine, GroupTranscriptSnapshot},
    search::MAX_QUERY_BYTES,
};

#[derive(Clone)]
pub enum Action {
    Open,
    Back,
    Pause,
    Resume,
    Search(String),
    FilterHost(Option<u64>),
    ClearFilters,
}

pub(super) struct State {
    pub(super) submission: u64,
    paused: Option<GroupTranscriptSnapshot>,
    filter: Filter,
}

#[derive(Default)]
struct Filter {
    query: String,
    matcher: Option<Regex>,
    pane: Option<u64>,
}

impl Filter {
    fn set_query(&mut self, mut query: String) {
        query.truncate(query.floor_char_boundary(query.len().min(MAX_QUERY_BYTES)));
        if self.query == query {
            return;
        }
        self.matcher = if query.is_empty() {
            None
        } else {
            RegexBuilder::new(&regex::escape(&query))
                .case_insensitive(true)
                .build()
                .ok()
        };
        self.query = query;
    }

    fn active(&self) -> bool {
        self.pane.is_some() || !self.query.is_empty()
    }

    fn includes(&self, line: &GroupTranscriptLine) -> bool {
        self.pane.is_none_or(|pane| pane == line.pane)
            && (self.query.is_empty()
                || self
                    .matcher
                    .as_ref()
                    .is_some_and(|query| query.is_match(&line.text)))
    }
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
            self.live_output = Some(Box::new(State {
                submission: id,
                paused: None,
                filter: Filter::default(),
            }));
            return;
        }
        if let Action::FilterHost(Some(pane)) = action
            && !submission
                .results
                .iter()
                .any(|delivery| delivery.target.pane == pane)
        {
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
            Action::Search(query) => output.filter.set_query(query),
            Action::FilterHost(pane) => output.filter.pane = pane,
            Action::ClearFilters => output.filter = Filter::default(),
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
            .filter(|line| output.filter.includes(line))
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
        let send =
            move |action| Message::HostGroup(super::Action::LiveOutput(id, submission_id, action));
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
        let panes: Vec<_> = std::iter::once(None)
            .chain(
                submission
                    .results
                    .iter()
                    .map(|delivery| Some(delivery.target.pane)),
            )
            .collect();
        let hosts = ComboBox::new()
            .grid_column(1)
            .automation_name("Filter live output by host")
            .max_width(320.0)
            .items_source(
                std::iter::once("All output hosts").chain(
                    submission
                        .results
                        .iter()
                        .map(|delivery| delivery.target.name.as_str()),
                ),
            )
            .selected_index(panes.iter().position(|&pane| pane == output.filter.pane))
            .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                index
                    .and_then(|index| panes.get(index))
                    .map_or(Message::ControlIdle, |&pane| send(Action::FilterHost(pane)))
            }));
        let search = Grid::new()
            .columns([GridLength::STAR, GridLength::Auto, GridLength::Auto])
            .column_spacing(8.0)
            .children((
                TextBox::new()
                    .automation_name("Find in live output")
                    .placeholder_text("Find text in live output")
                    .text(&output.filter.query)
                    .on_text_changed(ctx.callback(move |query| send(Action::Search(query)))),
                hosts,
                Button::new()
                    .grid_column(2)
                    .is_enabled(output.filter.active())
                    .on_click(ctx.callback(move |_| send(Action::ClearFilters)))
                    .content("Clear output filters"),
            ));
        let status = if output.filter.active() {
            Cow::Owned(format!(
                "{status} · {} of {} retained lines",
                lines.len(),
                snapshot.lines.len()
            ))
        } else {
            Cow::Borrowed(status)
        };
        let retention = if snapshot.omitted == 0 {
            View::empty()
        } else {
            TextBlock::new()
                .text("Older lines left the live output. Use pane history for the full command.")
                .text_wrapping(TextWrapping::Wrap)
                .into()
        };
        let content = if snapshot.lines.is_empty() {
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
                    TextBlock::new().text(format!("Submitted · {}", submission.content.label())),
                    StackPanel::new()
                        .spacing(8.0)
                        .orientation(Orientation::Horizontal)
                        .children((
                            Button::new()
                                .on_click(ctx.callback(move |_| send(Action::Back)))
                                .content("Back to results"),
                            Button::new()
                                .on_click(ctx.callback(move |_| send(pause_action.clone())))
                                .content(pause_label),
                            output_recording::button(id, submission, ctx),
                        )),
                    search,
                    TextBlock::new().text(status.as_ref()),
                    output_recording::status_view(id, submission, ctx),
                    retention,
                )),
                ScrollViewer::new().grid_row(1).content(content),
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_output_filters_match_literal_unicode_text_and_source_together() {
        let line = |pane, text: &str| GroupTranscriptLine {
            id: pane,
            pane,
            text: text.into(),
            shortened: false,
        };
        let east = line(1, "Result [ÉTAT]");
        let west = line(2, "result [état]");
        let other = line(1, "Result ÉTAT");
        let mut filter = Filter::default();
        assert!(filter.includes(&other));
        filter.set_query("[état]".into());
        assert!(filter.includes(&east) && filter.includes(&west));
        assert!(!filter.includes(&other));
        filter.pane = Some(2);
        assert!(!filter.includes(&east));
        assert!(filter.includes(&west));
        filter.set_query(String::new());
        assert!(!filter.includes(&east));
        assert!(filter.includes(&west));
        assert!(filter.active());
    }
}
