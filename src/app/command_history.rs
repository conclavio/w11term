use super::*;
use w11term::terminal::Snapshot;
use wezterm_term::CompletedCommand;

pub(super) struct HistoryPage {
    pane: u64,
    selected: Option<(u64, u64)>, // Connection generation and command id.
    query: String,
    feedback: String,
}

pub enum Action {
    Open,
    InPane(u64, HistoryAction),
}

pub enum HistoryAction {
    Search(String),
    Refresh,
    Select(u64, u64),
    Copy(u64, u64, CopyPart),
    Return,
}

#[derive(Clone, Copy)]
pub enum CopyPart {
    Command,
    Output,
}

impl CopyPart {
    fn text(self, entry: &CompletedCommand) -> Option<&str> {
        match self {
            Self::Command => entry.command.as_deref(),
            Self::Output => entry.output.as_ref().ok().map(String::as_str),
        }
    }
}

fn matches_command(entry: &CompletedCommand, needle: &str) -> bool {
    needle.is_empty()
        || entry
            .command
            .as_ref()
            .is_some_and(|text| text.to_lowercase().contains(needle))
}

fn selected_command(frame: &Snapshot, selected: Option<(u64, u64)>) -> Option<&CompletedCommand> {
    let (generation, id) = selected?;
    (frame.connection_generation == generation).then_some(())?;
    frame
        .command_history
        .iter()
        .find(|c| c.id == id)
        .map(AsRef::as_ref)
}

fn newest(frame: &Snapshot, needle: &str) -> Option<(u64, u64)> {
    frame
        .command_history
        .iter()
        .rev()
        .find(|entry| matches_command(entry, needle))
        .map(|c| (frame.connection_generation, c.id))
}

impl TerminalApp {
    #[cfg(feature = "ui-smoke")]
    pub(super) fn history_smoke_select_older(&mut self, ctx: &ComponentContext<Self>) {
        let Page::CommandHistory(page) = &self.tabs.active().unwrap().page else {
            panic!("expected history page")
        };
        let frame = self.history_frame(page.pane).unwrap();
        assert_eq!(
            selected_command(&frame, page.selected)
                .unwrap()
                .command
                .as_deref(),
            Some("status")
        );
        let first = frame.command_history.front().unwrap().id;
        self.update(
            Message::History(Action::InPane(
                page.pane,
                HistoryAction::Select(frame.connection_generation, first),
            )),
            ctx,
        );
    }

    #[cfg(feature = "ui-smoke")]
    pub(super) fn history_smoke_verify_older(&self) {
        let Page::CommandHistory(page) = &self.tabs.active().unwrap().page else {
            panic!("expected history page")
        };
        let frame = self.history_frame(page.pane).unwrap();
        let selected = selected_command(&frame, page.selected).unwrap();
        assert_eq!(selected.command.as_deref(), Some("first"));
        assert_eq!(selected.output.as_deref(), Ok("first result\r\n"));
        assert_eq!(selected.exit_code, Some(0));
    }

    fn history_frame(&self, pane: u64) -> Option<Arc<Snapshot>> {
        let tab = self.tabs.get(self.tab_for_pane(pane)?)?;
        match &tab.page {
            Page::Terminal(live) => Some(live.session.frames.load()),
            _ => None,
        }
    }

    pub(super) fn handle_history(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        if matches!(action, Action::Open) {
            let Some(source) = self.tabs.active() else {
                return;
            };
            let Some(pane) = source.pane_id() else { return };
            let label = format!("History · {}", source.label);
            let selected = self
                .history_frame(pane)
                .and_then(|frame| newest(&frame, ""));
            let existing = self.tabs.entries().iter()
                .find(|tab| matches!(&tab.value.page, Page::CommandHistory(page) if page.pane == pane))
                .map(|tab| tab.id);
            let page = Page::CommandHistory(HistoryPage {
                pane,
                selected,
                query: String::new(),
                feedback: String::new(),
            });
            if let Some(id) = existing {
                self.tabs.get_mut(id).unwrap().page = page;
                self.tabs.select(id);
            } else {
                self.tabs.insert(AppTab { label, page });
            }
            self.sync_selection(ctx);
            self.activate(ctx);
            return;
        }
        let Some(id) = self.tabs.active_id() else {
            return;
        };
        let Some(AppTab {
            page: Page::CommandHistory(history),
            ..
        }) = self.tabs.get(id)
        else {
            return;
        };
        let pane = history.pane;
        let Action::InPane(source, action) = action else {
            return;
        };
        if source != pane {
            return;
        }
        let Some(frame) = self.history_frame(pane) else {
            return;
        };
        let Page::CommandHistory(page) = &mut self.tabs.get_mut(id).unwrap().page else {
            return;
        };
        page.feedback.clear();
        match action {
            HistoryAction::Search(query) => {
                let needle = query.to_lowercase();
                if !selected_command(&frame, page.selected)
                    .is_some_and(|c| matches_command(c, &needle))
                {
                    page.selected = newest(&frame, &needle);
                }
                page.query = query;
            }
            HistoryAction::Refresh => page.selected = newest(&frame, &page.query.to_lowercase()),
            HistoryAction::Select(generation, command) => {
                let key = Some((generation, command));
                if selected_command(&frame, key)
                    .is_some_and(|entry| matches_command(entry, &page.query.to_lowercase()))
                {
                    page.selected = key;
                }
            }
            HistoryAction::Copy(generation, command, part) => {
                let key = Some((generation, command));
                if page.selected == key
                    && let Some(entry) = selected_command(&frame, key)
                    && let Some(text) = part.text(entry)
                {
                    page.feedback = match w11term::clipboard::write(text) {
                        Ok(()) => match part {
                            CopyPart::Command => "Copied command".into(),
                            CopyPart::Output => "Copied output".into(),
                        },
                        Err(error) => format!("Could not copy to clipboard · {error}"),
                    };
                }
            }
            HistoryAction::Return => {
                if let Some(source) = self.tab_for_pane(pane) {
                    self.tabs.select(source);
                    self.sync_selection(ctx);
                    self.activate(ctx);
                }
            }
        }
    }

    pub(super) fn history_view(&self, page: &HistoryPage, ctx: &ViewContext<Self>) -> View {
        let Some(frame) = self.history_frame(page.pane) else {
            return TextBlock::new()
                .margin(16.0)
                .text("The source pane has closed")
                .into();
        };
        let generation = frame.connection_generation;
        let pane = page.pane;
        let action = move |request| Message::History(Action::InPane(pane, request));
        let needle = page.query.to_lowercase();
        let selected = selected_command(&frame, page.selected);
        let mut matches = frame
            .command_history
            .iter()
            .rev()
            .filter(|c| matches_command(c, &needle))
            .peekable();
        let has_matches = matches.peek().is_some();
        let rows = matches.map(|entry| {
            let id = entry.id;
            let name = entry.command.as_ref().map_or_else(
                || format!("Command {id}"),
                |s| {
                    s.chars()
                        .take(120)
                        .map(|c| if c.is_whitespace() { ' ' } else { c })
                        .collect()
                },
            );
            let status = entry
                .exit_code
                .map_or_else(|| "status unknown".into(), |code| format!("exit {code}"));
            let label = format!("{name} · {status}");
            let active = page.selected == Some((generation, id));
            KeyedView::new(
                format!("{generation}/{id}"),
                Button::new()
                    .style(if active {
                        ButtonStyle::Accent
                    } else {
                        ButtonStyle::Subtle
                    })
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .automation_name(if active {
                        format!("Selected, {label}")
                    } else {
                        label.clone()
                    })
                    .on_click(ctx.callback(move |_| action(HistoryAction::Select(generation, id))))
                    .content(label),
            )
        });
        let (heading, preview) = match selected {
            Some(entry) => {
                let heading = entry
                    .command
                    .clone()
                    .unwrap_or_else(|| format!("Command {}", entry.id));
                let preview = match &entry.output {
                    Err(why) => (*why).to_owned(),
                    Ok(text) if text.is_empty() => "No output".to_owned(),
                    Ok(text) => {
                        let end = text.floor_char_boundary(text.len().min(8192));
                        let mut view = text[..end].to_owned();
                        if end < text.len() {
                            view.push_str("\r\n… Copy output for the full text.");
                        }
                        view
                    }
                };
                (heading, preview)
            }
            None if frame.command_history.is_empty() => (
                "Waiting for a completed command".into(),
                "This pane needs a shell that sends OSC 133 command markers.".into(),
            ),
            None if !has_matches => ("No matching commands".into(), String::new()),
            None => ("Select a command".into(), String::new()),
        };
        let copy_button = |label: &'static str, part: CopyPart| {
            let id = selected
                .filter(|entry| part.text(entry).is_some())
                .map(|entry| entry.id);
            Button::new()
                .is_enabled(id.is_some())
                .on_click(ctx.callback(move |_| {
                    action(HistoryAction::Copy(
                        generation,
                        id.unwrap_or_default(),
                        part,
                    ))
                }))
                .content(label)
        };
        Grid::new()
            .margin(16.0)
            .row_spacing(8.0)
            .column_spacing(12.0)
            .rows([
                GridLength::Auto,
                GridLength::Auto,
                GridLength::Auto,
                GridLength::STAR,
            ])
            .columns([GridLength::Star(1.0), GridLength::Star(2.0)])
            .children((
                TextBlock::new()
                    .grid_column_span(2)
                    .font_size(24.0)
                    .text(format!("Command history · Pane {}", page.pane)),
                StackPanel::new()
                    .grid_row(1)
                    .grid_column_span(2)
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .on_click(ctx.callback(move |_| action(HistoryAction::Refresh)))
                            .content("Refresh history"),
                        copy_button("Copy command", CopyPart::Command),
                        copy_button("Copy output", CopyPart::Output),
                        Button::new()
                            .on_click(ctx.callback(move |_| action(HistoryAction::Return)))
                            .content("Return to pane"),
                        TextBlock::new().text(&page.feedback),
                    )),
                TextBox::new()
                    .grid_row(2)
                    .automation_name("Search commands")
                    .placeholder_text("Search commands")
                    .text(&page.query)
                    .on_text_changed(ctx.callback(move |text| action(HistoryAction::Search(text)))),
                ScrollViewer::new()
                    .grid_row(3)
                    .content(StackPanel::new().spacing(4.0).keyed_children(rows)),
                ScrollViewer::new()
                    .grid_row(2)
                    .grid_row_span(2)
                    .grid_column(1)
                    .content(
                        StackPanel::new().spacing(8.0).children((
                            TextBlock::new()
                                .font_size(18.0)
                                .text(heading)
                                .text_wrapping(TextWrapping::Wrap),
                            TextBlock::new()
                                .text(preview)
                                .text_wrapping(TextWrapping::Wrap),
                        )),
                    ),
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_search_uses_full_captured_input_and_keeps_output_separate() {
        let command = format!("{} [Ångström]\r\n  --final", "x".repeat(130));
        let mut entry = CompletedCommand {
            id: 9,
            command: Some(command.clone()),
            output: Ok("archived result\r\n".into()),
            exit_code: Some(17),
        };
        assert!(matches_command(&entry, "[ångström]"));
        assert!(!matches_command(&entry, "archived"));
        assert!(!matches_command(&entry, ".*"));
        assert_eq!(CopyPart::Command.text(&entry), Some(command.as_str()));
        assert_eq!(CopyPart::Output.text(&entry), Some("archived result\r\n"));
        entry.command = None;
        assert!(matches_command(&entry, ""));
        assert!(!matches_command(&entry, "9"));
        assert_eq!(CopyPart::Command.text(&entry), None);
        entry.output = Err("Unavailable");
        assert_eq!(CopyPart::Output.text(&entry), None);
    }
}
