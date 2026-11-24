use super::*;
use regex::{Regex, RegexBuilder};
use std::{collections::BTreeMap, fmt::Write as _, io::Write as _};
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
    CopyShown,
    SaveShown,
}

pub(super) struct State {
    pub(super) submission: u64,
    paused: Option<GroupTranscriptSnapshot>,
    filter: Filter,
    copy_feedback: Option<String>,
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

    fn output_names(&self) -> BTreeMap<u64, &str> {
        self.results
            .iter()
            .map(|delivery| (delivery.target.pane, delivery.target.name.as_str()))
            .collect()
    }
}

impl State {
    fn snapshot(&self, submission: &Submission) -> Cow<'_, GroupTranscriptSnapshot> {
        self.paused
            .as_ref()
            .map_or_else(|| Cow::Owned(submission.output_snapshot()), Cow::Borrowed)
    }

    fn shown<'a>(
        &'a self,
        snapshot: &'a GroupTranscriptSnapshot,
        names: &'a BTreeMap<u64, &'a str>,
    ) -> impl Iterator<Item = (&'a GroupTranscriptLine, &'a str)> {
        snapshot.lines.iter().rev().filter_map(move |line| {
            let &name = names.get(&line.pane)?;
            self.filter.includes(line).then_some((line, name))
        })
    }

    fn excerpt(&self, submission: &Submission) -> Option<String> {
        let snapshot = self.snapshot(submission);
        let names = submission.output_names();
        format_excerpt(
            submission.content.label(),
            snapshot.omitted,
            self.shown(&snapshot, &names),
        )
    }
}

fn displayed_line(line: &GroupTranscriptLine) -> Cow<'_, str> {
    if line.shortened {
        Cow::Owned(format!("{}…", line.text))
    } else {
        Cow::Borrowed(&line.text)
    }
}

fn format_excerpt<'a>(
    label: &str,
    omitted: u64,
    lines: impl Iterator<Item = (&'a GroupTranscriptLine, &'a str)>,
) -> Option<String> {
    let mut lines = lines.peekable();
    lines.peek()?;
    let mut text = format!("Submitted · {label}\r\n");
    for (line, name) in lines {
        let _ = writeln!(text, "{name} · {}\r", displayed_line(line));
    }
    if omitted > 0 {
        text.push_str("[Older lines are outside the retained output.]\r\n");
    }
    Some(text)
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
                copy_feedback: None,
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
        output.copy_feedback = None;
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
    pub(super) fn share_group_live_output(
        &mut self,
        id: TabId,
        submission_id: u64,
        save: bool,
        ctx: &ComponentContext<Self>,
    ) {
        if save && self.export.pending.is_some() {
            return;
        }
        let Some(page) = self.host_group_mut(id) else {
            return;
        };
        let Some(submission) = page.viewed_submission(submission_id) else {
            return;
        };
        let Some(text) = page
            .live_output
            .as_ref()
            .filter(|output| output.submission == submission_id)
            .and_then(|output| output.excerpt(submission))
        else {
            return;
        };
        if save {
            self.begin_text_export(
                text_export::Subject::HostGroupShownOutput(submission_id),
                move |file| file.write_all(text.as_bytes()),
                ctx,
            );
        } else if let Some(output) = &mut page.live_output {
            output.copy_feedback = Some(match w11term::clipboard::write(&text) {
                Ok(()) => "Copied shown output".into(),
                Err(error) => format!("Could not copy shown output. {error}"),
            });
        }
    }

    pub(super) fn host_group_live_output(
        &self,
        id: TabId,
        output: &State,
        submission: &Submission,
        ctx: &ViewContext<Self>,
    ) -> View {
        let snapshot = output.snapshot(submission);
        let names = submission.output_names();
        let lines: Vec<_> = output
            .shown(&snapshot, &names)
            .map(|(line, name)| {
                let pane = line.pane;
                KeyedView::new(
                    line.id.to_string(),
                    StackPanel::new().spacing(2.0).children((
                        Button::new()
                            .automation_name(format!("Focus output pane {pane}"))
                            .is_enabled(self.tab_for_pane(pane).is_some())
                            .on_click(ctx.callback(move |_| {
                                Message::HostGroup(super::Action::Visit(id, pane))
                            }))
                            .content(name),
                        TextBlock::new()
                            .text(displayed_line(line).as_ref())
                            .text_wrapping(TextWrapping::Wrap),
                    )),
                )
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
        let can_share = !lines.is_empty();
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
                    Grid::new()
                        .columns([GridLength::STAR, GridLength::Auto, GridLength::Auto])
                        .column_spacing(8.0)
                        .children((
                            TextBlock::new()
                                .text(status.as_ref())
                                .text_wrapping(TextWrapping::Wrap),
                            Button::new()
                                .grid_column(1)
                                .is_enabled(can_share)
                                .on_click(ctx.callback(move |_| send(Action::CopyShown)))
                                .content("Copy shown output"),
                            Button::new()
                                .grid_column(2)
                                .is_enabled(can_share && self.export.pending.is_none())
                                .on_click(ctx.callback(move |_| send(Action::SaveShown)))
                                .content("Save shown output…"),
                        )),
                    output
                        .copy_feedback
                        .as_deref()
                        .map_or_else(View::empty, |message| {
                            TextBlock::new()
                                .text(message)
                                .text_wrapping(TextWrapping::Wrap)
                                .into()
                        }),
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

    #[test]
    fn excerpt_keeps_visible_host_labels_order_and_retention_notices() {
        let line = |id, pane, text: &str, shortened| GroupTranscriptLine {
            id,
            pane,
            text: text.into(),
            shortened,
        };
        let snapshot = GroupTranscriptSnapshot {
            omitted: 2,
            lines: VecDeque::from([
                line(1, 7, "first witness", false),
                line(2, 9, "wrong host witness", false),
                line(3, 7, "other content", false),
                line(4, 7, "雪 WITNESS", true),
            ]),
        };
        let mut output = State {
            submission: 1,
            paused: Some(snapshot),
            filter: Filter {
                pane: Some(7),
                ..Filter::default()
            },
            copy_feedback: None,
        };
        let snapshot = output.paused.as_ref().unwrap();
        let names = BTreeMap::from([(7, "East"), (9, "West")]);
        output.filter.set_query("witness".into());
        assert_eq!(
            format_excerpt("show", snapshot.omitted, output.shown(snapshot, &names)).as_deref(),
            Some(
                "Submitted · show\r\nEast · 雪 WITNESS…\r\nEast · first witness\r\n[Older lines are outside the retained output.]\r\n"
            ),
        );
        output.filter.set_query("missing".into());
        assert!(format_excerpt("show", snapshot.omitted, output.shown(snapshot, &names)).is_none());
    }
}
