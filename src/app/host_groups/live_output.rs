use super::*;
use regex::Regex;
use std::{collections::BTreeMap, fmt::Write as _, io::Write as _};
use w11term::{
    events::{
        GroupCompletionState, GroupOutputAlert, GroupTranscriptHistory, GroupTranscriptLine,
        GroupTranscriptSnapshot,
    },
    search::{MAX_QUERY_BYTES, compile_regex},
};

const CONTEXT_LINES: usize = 2;

#[derive(Clone)]
pub enum Action {
    Open,
    Back,
    ViewRun(u64),
    Pause,
    Resume,
    Search(String),
    UseRegex(bool),
    MatchCase(bool),
    ShowContext(bool),
    OldestFirst(bool),
    ShowElapsed(bool),
    FilterHost(Option<u64>),
    ClearFilters,
    OutputAlert(bool),
    CopyShown,
    SaveShown,
}

pub(super) struct State {
    pub(super) submission: u64,
    paused: Option<GroupTranscriptHistory>,
    filter: Filter,
    timeline: Timeline,
    copy_feedback: Option<String>,
}

#[derive(Default)]
struct Timeline {
    oldest_first: bool,
    show_elapsed: bool,
}

impl Timeline {
    fn status(&self, paused: bool) -> &'static str {
        match (paused, self.oldest_first) {
            (false, false) => "Newest first",
            (false, true) => "Oldest first",
            (true, false) => "Paused · newest first",
            (true, true) => "Paused · oldest first",
        }
    }
}

#[derive(Default)]
struct Filter {
    query: String,
    matcher: Option<Result<Regex, String>>,
    regex: bool,
    sensitive: bool,
    show_context: bool,
    host: Option<HostFilter>,
}

struct HostFilter {
    pane: u64,
    address: String,
    label: String,
}

impl HostFilter {
    fn matches(&self, pane: u64, address: &str) -> bool {
        self.pane == pane && self.address == address
    }
}

impl From<&Target> for HostFilter {
    fn from(target: &Target) -> Self {
        Self {
            pane: target.pane,
            address: target.names.address().into(),
            label: target.name.clone(),
        }
    }
}

impl Filter {
    fn configure(&mut self, query: String, regex: bool, sensitive: bool) {
        if (self.query.as_str(), self.regex, self.sensitive) == (query.as_str(), regex, sensitive) {
            return;
        }
        self.matcher = if query.is_empty() {
            None
        } else if query.len() > MAX_QUERY_BYTES {
            Some(Err(format!(
                "Search text exceeds {MAX_QUERY_BYTES} UTF-8 bytes"
            )))
        } else {
            let pattern = if regex {
                Cow::Borrowed(query.as_str())
            } else {
                Cow::Owned(regex::escape(&query))
            };
            Some(compile_regex(&pattern, sensitive))
        };
        self.query = query;
        self.regex = regex;
        self.sensitive = sensitive;
    }

    fn set_query(&mut self, query: String) {
        self.configure(query, self.regex, self.sensitive);
    }

    fn error(&self) -> Option<&str> {
        self.matcher.as_ref()?.as_ref().err().map(String::as_str)
    }

    fn filters_lines(&self) -> bool {
        self.host.is_some() || self.matcher.is_some()
    }

    fn active(&self) -> bool {
        self.filters_lines() || self.regex || self.sensitive || self.show_context
    }

    fn includes_host(&self, line: &GroupTranscriptLine, address: &str) -> bool {
        self.host
            .as_ref()
            .is_none_or(|host| host.matches(line.pane, address))
    }

    fn matches_text(&self, line: &GroupTranscriptLine) -> bool {
        self.matcher.as_ref().is_none_or(|result| {
            result
                .as_ref()
                .is_ok_and(|pattern| pattern.is_match(&line.text))
        })
    }

    fn includes_context(&self) -> bool {
        self.show_context && self.matcher.is_some()
    }
}

struct ShownLine<'a> {
    line: &'a GroupTranscriptLine,
    name: &'a str,
    matched: bool,
}

fn retain_with_context(rows: Vec<ShownLine<'_>>) -> Vec<ShownLine<'_>> {
    let mut by_host = BTreeMap::<_, Vec<_>>::new();
    for (index, row) in rows.iter().enumerate() {
        by_host.entry(row.line.pane).or_default().push(index);
    }
    let mut shown = vec![false; rows.len()];
    for positions in by_host.values() {
        for (offset, &index) in positions.iter().enumerate() {
            if rows[index].matched {
                let start = offset.saturating_sub(CONTEXT_LINES);
                let end = (offset + CONTEXT_LINES + 1).min(positions.len());
                for &neighbor in &positions[start..end] {
                    shown[neighbor] = true;
                }
            }
        }
    }
    rows.into_iter()
        .zip(shown)
        .filter_map(|(row, show)| show.then_some(row))
        .collect()
}

impl Submission {
    fn output_panes(&self) -> BTreeSet<u64> {
        self.results
            .iter()
            .filter(|delivery| matches!(delivery.status, DeliveryStatus::Sent))
            .map(|delivery| delivery.target.pane)
            .collect()
    }

    fn output_snapshot(&self, pane: Option<u64>) -> GroupTranscriptSnapshot {
        let sent = self.output_panes();
        if let Some(pane) = pane {
            return if sent.contains(&pane) {
                self.transcript.snapshot_for_pane(pane)
            } else {
                GroupTranscriptSnapshot::default()
            };
        }
        let mut snapshot = self.transcript.snapshot();
        snapshot.lines.retain(|line| sent.contains(&line.pane));
        snapshot
    }

    fn output_history(&self) -> GroupTranscriptHistory {
        let sent = self.output_panes();
        let mut history = self.transcript.history();
        history.retain_panes(|pane| sent.contains(&pane));
        history
    }

    fn output_hosts(&self) -> BTreeMap<u64, (&str, &str)> {
        self.results
            .iter()
            .map(|delivery| {
                let target = &delivery.target;
                (target.pane, (target.name.as_str(), target.names.address()))
            })
            .collect()
    }
}

impl State {
    fn snapshot(
        &self,
        submission: &Submission,
        hosts: &BTreeMap<u64, (&str, &str)>,
    ) -> Cow<'_, GroupTranscriptSnapshot> {
        let pane = match &self.filter.host {
            None => None,
            Some(host)
                if hosts
                    .get(&host.pane)
                    .is_some_and(|&(_, address)| host.address == address) =>
            {
                Some(host.pane)
            }
            Some(_) => return Cow::Owned(GroupTranscriptSnapshot::default()),
        };
        self.paused.as_ref().map_or_else(
            || Cow::Owned(submission.output_snapshot(pane)),
            |history| history.scoped(pane),
        )
    }

    fn shown<'a>(
        &'a self,
        snapshot: &'a GroupTranscriptSnapshot,
        hosts: &'a BTreeMap<u64, (&'a str, &'a str)>,
    ) -> Vec<ShownLine<'a>> {
        let rows = snapshot.lines.iter().rev().filter_map(|line| {
            let &(name, address) = hosts.get(&line.pane)?;
            self.filter.includes_host(line, address).then(|| ShownLine {
                line,
                name,
                matched: self.filter.matches_text(line),
            })
        });
        let mut shown = if self.filter.includes_context() {
            retain_with_context(rows.collect())
        } else {
            rows.filter(|row| row.matched).collect()
        };
        if self.timeline.oldest_first {
            shown.reverse();
        }
        shown
    }

    fn excerpt(&self, submission: &Submission) -> Option<String> {
        let hosts = submission.output_hosts();
        let snapshot = self.snapshot(submission, &hosts);
        format_excerpt(
            submission.content.label(),
            snapshot.omitted,
            self.shown(&snapshot, &hosts),
            self.timeline.show_elapsed,
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

fn elapsed_label(elapsed_ms: u64) -> String {
    format!("+{}.{:03} s", elapsed_ms / 1000, elapsed_ms % 1000)
}

fn format_excerpt(
    label: &str,
    omitted: u64,
    lines: Vec<ShownLine<'_>>,
    show_elapsed: bool,
) -> Option<String> {
    if lines.is_empty() {
        return None;
    }
    let mut text = format!("Submitted · {label}\r\n");
    for ShownLine { line, name, .. } in lines {
        if show_elapsed {
            let _ = write!(text, "{} · ", elapsed_label(line.elapsed_ms));
        }
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
                timeline: Timeline::default(),
                copy_feedback: None,
            }));
            return;
        }
        if let Action::ViewRun(run) = action {
            if run != id
                && self.submissions.iter().any(|item| item.id == run)
                && let Some(output) = self
                    .live_output
                    .as_mut()
                    .filter(|output| output.submission == id)
            {
                output.submission = run;
                output.paused = None;
                output.copy_feedback = None;
                self.viewing = run;
                self.result_filter = progress::Filter::All;
                self.recipient_search.clear();
                self.copy_feedback.clear();
            }
            return;
        }
        let host_filter = match action {
            Action::FilterHost(Some(pane)) => {
                let Some(delivery) = submission
                    .results
                    .iter()
                    .find(|delivery| delivery.target.pane == pane)
                else {
                    return;
                };
                Some(HostFilter::from(&delivery.target))
            }
            _ => None,
        };
        let pause = matches!(action, Action::Pause).then(|| submission.output_history());
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
            Action::UseRegex(regex) => {
                output
                    .filter
                    .configure(output.filter.query.clone(), regex, output.filter.sensitive)
            }
            Action::MatchCase(sensitive) => {
                output
                    .filter
                    .configure(output.filter.query.clone(), output.filter.regex, sensitive)
            }
            Action::ShowContext(show) => output.filter.show_context = show,
            Action::OldestFirst(enabled) => output.timeline.oldest_first = enabled,
            Action::ShowElapsed(enabled) => output.timeline.show_elapsed = enabled,
            Action::FilterHost(_) => output.filter.host = host_filter,
            Action::ClearFilters => output.filter = Filter::default(),
            _ => {}
        }
    }
}

impl TerminalApp {
    pub(super) fn set_group_output_alert(&self, id: TabId, run: u64, armed: bool) {
        if self.tabs.active_id() != Some(id) {
            return;
        }
        let Some(AppTab {
            label,
            page: Page::HostGroup(page),
        }) = self.tabs.get(id)
        else {
            return;
        };
        let Some(submission) = page.viewed_submission(run) else {
            return;
        };
        let Some(output) = page
            .live_output
            .as_ref()
            .filter(|output| output.submission == run)
        else {
            return;
        };
        if !armed {
            submission.transcript.cancel_output_alert();
            return;
        }
        let Some(Ok(pattern)) = &output.filter.matcher else {
            return;
        };
        let hosts = submission
            .results
            .iter()
            .filter_map(|delivery| {
                let target = &delivery.target;
                output
                    .filter
                    .host
                    .as_ref()
                    .is_none_or(|host| host.matches(target.pane, target.names.address()))
                    .then(|| (target.pane, target.name.clone()))
            })
            .collect();
        submission.transcript.arm_output_alert(GroupOutputAlert {
            inbox: self.inbox.clone(),
            tab: id,
            submission: run,
            source: label.clone(),
            hosts,
            pattern: pattern.clone(),
        });
    }

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
        page: &HostGroup,
        output: &State,
        submission: &Submission,
        ctx: &ViewContext<Self>,
    ) -> View {
        let output_hosts = submission.output_hosts();
        let snapshot = output.snapshot(submission, &output_hosts);
        let shown = output.shown(&snapshot, &output_hosts);
        let matches = shown.iter().filter(|row| row.matched).count();
        let lines: Vec<_> = shown
            .into_iter()
            .map(
                |ShownLine {
                     line,
                     name,
                     matched,
                 }| {
                    let pane = line.pane;
                    let host =
                        Button::new()
                            .automation_name(format!("Focus output pane {pane}"))
                            .is_enabled(self.tab_for_pane(pane).is_some())
                            .on_click(ctx.callback(move |_| {
                                Message::HostGroup(super::Action::Visit(id, pane))
                            }))
                            .content(name);
                    let source = if matched && !output.timeline.show_elapsed {
                        host
                    } else {
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(8.0)
                            .children((
                                if output.timeline.show_elapsed {
                                    TextBlock::new()
                                        .text(elapsed_label(line.elapsed_ms))
                                        .vertical_alignment(VerticalAlignment::Center)
                                        .into()
                                } else {
                                    View::empty()
                                },
                                host,
                                if matched {
                                    View::empty()
                                } else {
                                    TextBlock::new()
                                        .text("Context")
                                        .vertical_alignment(VerticalAlignment::Center)
                                        .into()
                                },
                            ))
                    };
                    KeyedView::new(
                        line.id.to_string(),
                        StackPanel::new().spacing(2.0).children((
                            source,
                            TextBlock::new()
                                .text(displayed_line(line).as_ref())
                                .text_wrapping(TextWrapping::Wrap),
                        )),
                    )
                },
            )
            .collect();
        let submission_id = submission.id;
        let send =
            move |action| Message::HostGroup(super::Action::LiveOutput(id, submission_id, action));
        let (pause_label, pause_action) = if output.paused.is_some() {
            ("Resume output", Action::Resume)
        } else {
            ("Pause output", Action::Pause)
        };
        let status = output.timeline.status(output.paused.is_some());
        let mut panes = vec![None];
        let mut host_labels = vec![Cow::Borrowed("All output hosts")];
        let mut selected_host = 0;
        for delivery in &submission.results {
            let target = &delivery.target;
            if output
                .filter
                .host
                .as_ref()
                .is_some_and(|host| host.matches(target.pane, target.names.address()))
            {
                selected_host = panes.len();
            }
            panes.push(Some(target.pane));
            host_labels.push(Cow::Borrowed(target.name.as_str()));
        }
        let host_absent = selected_host == 0 && output.filter.host.is_some();
        let output_alert = if submission.transcript.completion_state()
            == GroupCompletionState::Settled
        {
            View::empty()
        } else {
            let armed = submission.transcript.output_alert_armed();
            let can_arm = !host_absent && output.filter.matcher.as_ref().is_some_and(Result::is_ok);
            Button::new()
                .is_enabled(armed || can_arm)
                .on_click(ctx.callback(move |_| send(Action::OutputAlert(!armed))))
                .content(if armed {
                    "Cancel output alert"
                } else {
                    "Notify on next match"
                })
        };
        if let Some(host) = output.filter.host.as_ref().filter(|_| host_absent) {
            selected_host = host_labels.len();
            host_labels.push(Cow::Owned(format!("{} · absent from this run", host.label)));
        }
        let hosts = ComboBox::new()
            .grid_column(1)
            .automation_name("Filter live output by host")
            .max_width(320.0)
            .items_source(host_labels)
            .selected_index(Some(selected_host))
            .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                index
                    .and_then(|index| panes.get(index))
                    .map_or(Message::ControlIdle, |&pane| send(Action::FilterHost(pane)))
            }));
        let run_picker: View = if page.submissions.len() > 1 {
            let runs: Vec<_> = page.submissions.iter().rev().map(|run| run.id).collect();
            ComboBox::new()
                .automation_name("Live output run")
                .items_source(
                    page.submissions
                        .iter()
                        .rev()
                        .map(|run| format!("Run {} · {}", run.id, run.content.label())),
                )
                .selected_index(runs.iter().position(|&run| run == submission_id))
                .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                    index
                        .and_then(|index| runs.get(index))
                        .map_or(Message::ControlIdle, |&run| send(Action::ViewRun(run)))
                }))
                .into()
        } else {
            TextBlock::new()
                .text(format!("Submitted · {}", submission.content.label()))
                .into()
        };
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
        let search_options = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .children((
                CheckBox::new()
                    .automation_name("Regular expression in live output")
                    .is_checked(output.filter.regex)
                    .on_is_checked_changed(
                        ctx.callback(move |enabled| send(Action::UseRegex(enabled))),
                    )
                    .content("Regular expression"),
                CheckBox::new()
                    .automation_name("Match case in live output")
                    .is_checked(output.filter.sensitive)
                    .on_is_checked_changed(
                        ctx.callback(move |enabled| send(Action::MatchCase(enabled))),
                    )
                    .content("Match case"),
                CheckBox::new()
                    .automation_name("Show surrounding lines")
                    .is_checked(output.filter.show_context)
                    .on_is_checked_changed(
                        ctx.callback(move |enabled| send(Action::ShowContext(enabled))),
                    )
                    .content("Show surrounding lines"),
            ));
        let timeline_options = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .children((
                CheckBox::new()
                    .automation_name("Oldest first in live output")
                    .is_checked(output.timeline.oldest_first)
                    .on_is_checked_changed(
                        ctx.callback(move |enabled| send(Action::OldestFirst(enabled))),
                    )
                    .content("Oldest first"),
                CheckBox::new()
                    .automation_name("Show elapsed time in live output")
                    .is_checked(output.timeline.show_elapsed)
                    .on_is_checked_changed(
                        ctx.callback(move |enabled| send(Action::ShowElapsed(enabled))),
                    )
                    .content("Show elapsed time"),
            ));
        let status = if output.filter.includes_context() {
            Cow::Owned(format!(
                "{status} · {matches} {} · {} of {} retained lines shown",
                if matches == 1 { "match" } else { "matches" },
                lines.len(),
                snapshot.lines.len()
            ))
        } else if output.filter.filters_lines() {
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
        let content = if host_absent {
            TextBlock::new()
                .text("The selected host did not receive this run.")
                .into()
        } else if snapshot.lines.is_empty() {
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
                    run_picker,
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
                            output_alert,
                        )),
                    search,
                    search_options,
                    timeline_options,
                    output.filter.error().map_or_else(View::empty, |error| {
                        TextBlock::new()
                            .text(format!("Live output search · {error}"))
                            .text_wrapping(TextWrapping::Wrap)
                            .into()
                    }),
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
            elapsed_ms: 0,
            text: text.into(),
            shortened: false,
        };
        let east = line(1, "Result [ÉTAT]");
        let west = line(2, "result [état]");
        let other = line(1, "Result ÉTAT");
        let mut filter = Filter::default();
        assert!(filter.includes_host(&other, "east"));
        assert!(filter.matches_text(&other));
        filter.set_query("[état]".into());
        assert!(filter.matches_text(&east) && filter.matches_text(&west));
        assert!(!filter.matches_text(&other));
        filter.host = Some(HostFilter {
            pane: 2,
            address: "west".into(),
            label: "West".into(),
        });
        assert!(!filter.includes_host(&east, "east"));
        assert!(filter.includes_host(&west, "west"));
        assert!(!filter.includes_host(&west, "different destination"));
        filter.set_query(String::new());
        assert!(filter.matches_text(&other));
        assert!(!filter.includes_host(&east, "east"));
        assert!(filter.active());
    }

    #[test]
    fn regex_search_matches_case_on_request_and_rejects_invalid_and_oversized_patterns() {
        let line = GroupTranscriptLine {
            id: 1,
            pane: 2,
            elapsed_ms: 0,
            text: "Error 503".into(),
            shortened: false,
        };
        let pattern = "^ERROR [0-9]{3}$";
        let mut filter = Filter::default();
        filter.configure(pattern.into(), true, false);
        assert!(filter.matches_text(&line));
        filter.configure(pattern.into(), true, true);
        assert!(!filter.matches_text(&line));
        assert!(filter.error().is_none());
        filter.set_query("(".into());
        assert!(filter.error().is_some());
        assert!(!filter.matches_text(&line));
        filter.set_query("界".repeat(MAX_QUERY_BYTES));
        assert!(filter.error().unwrap().contains("UTF-8 bytes"));
        assert!(!filter.matches_text(&line));
        filter.set_query(String::new());
        assert!(filter.error().is_none());
        assert!(filter.active());
        assert!(filter.matches_text(&line));
    }

    #[test]
    fn context_uses_only_neighbors_from_matching_hosts_and_deduplicates_windows() {
        let lines = [
            (1, "outside context"),
            (1, "second before"),
            (1, "first before"),
            (2, "unrelated other host"),
            (1, "error first"),
            (1, "between errors"),
            (1, "error second"),
            (2, "unrelated later"),
            (1, "first after"),
            (1, "second after"),
            (1, "also outside context"),
        ];
        let snapshot = GroupTranscriptSnapshot {
            omitted: 0,
            lines: lines
                .into_iter()
                .enumerate()
                .map(|(id, (pane, text))| GroupTranscriptLine {
                    id: id as u64,
                    pane,
                    elapsed_ms: 0,
                    text: text.into(),
                    shortened: false,
                })
                .collect(),
        };
        let mut output = State {
            submission: 1,
            paused: None,
            filter: Filter {
                show_context: true,
                ..Default::default()
            },
            timeline: Timeline::default(),
            copy_feedback: None,
        };
        let hosts = BTreeMap::from([(1, ("East", "east")), (2, ("West", "west"))]);
        output.filter.set_query("error ".into());
        let shown = output.shown(&snapshot, &hosts);
        assert_eq!(
            shown
                .iter()
                .map(|row| (row.line.id, row.matched))
                .collect::<Vec<_>>(),
            [
                (9, false),
                (8, false),
                (6, true),
                (5, false),
                (4, true),
                (2, false),
                (1, false)
            ]
        );
        let excerpt = format_excerpt("run", 0, shown, false).unwrap();
        assert_eq!(excerpt.matches("between errors").count(), 1);
        assert!(!excerpt.contains("unrelated"));
        output.filter.configure("(".into(), true, false);
        assert!(output.shown(&snapshot, &hosts).is_empty());
    }

    #[test]
    fn excerpt_keeps_visible_host_labels_order_and_retention_notices() {
        let line = |id, pane, text: &str, shortened| GroupTranscriptLine {
            id,
            pane,
            elapsed_ms: id * 50,
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
            paused: None,
            filter: Filter {
                host: Some(HostFilter {
                    pane: 7,
                    address: "east".into(),
                    label: "East".into(),
                }),
                ..Filter::default()
            },
            timeline: Timeline::default(),
            copy_feedback: None,
        };
        let names = BTreeMap::from([(7, ("East", "east")), (9, ("West", "west"))]);
        output.filter.set_query("witness".into());
        assert_eq!(
            format_excerpt(
                "show",
                snapshot.omitted,
                output.shown(&snapshot, &names),
                false
            )
            .as_deref(),
            Some(
                "Submitted · show\r\nEast · 雪 WITNESS…\r\nEast · first witness\r\n[Older lines are outside the retained output.]\r\n"
            ),
        );
        output.timeline.oldest_first = true;
        output.timeline.show_elapsed = true;
        assert_eq!(
            format_excerpt(
                "show",
                snapshot.omitted,
                output.shown(&snapshot, &names),
                output.timeline.show_elapsed
            )
            .as_deref(),
            Some(
                "Submitted · show\r\n+0.050 s · East · first witness\r\n+0.200 s · East · 雪 WITNESS…\r\n[Older lines are outside the retained output.]\r\n"
            ),
        );
        output.filter.set_query("missing".into());
        assert!(
            format_excerpt(
                "show",
                snapshot.omitted,
                output.shown(&snapshot, &names),
                true
            )
            .is_none()
        );
    }
}
