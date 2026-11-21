use super::*;
use w11term::terminal::Snapshot;

pub(super) struct HistoryPage {
    pane: u64,
    selected: Option<(u64, u64)>, // Connection generation and command id.
    feedback: String,
}

pub enum Action {
    Open,
    Refresh,
    Select(u64, u64),
    Copy(u64, u64),
    Return,
}

fn selected_command(
    frame: &Snapshot,
    selected: Option<(u64, u64)>,
) -> Option<&wezterm_term::CompletedCommand> {
    let (generation, id) = selected?;
    (frame.connection_generation == generation).then_some(())?;
    frame
        .command_history
        .iter()
        .find(|c| c.id == id)
        .map(AsRef::as_ref)
}

fn newest(frame: &Snapshot) -> Option<(u64, u64)> {
    frame
        .command_history
        .back()
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
            Message::History(Action::Select(frame.connection_generation, first)),
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
            let selected = self.history_frame(pane).and_then(|frame| newest(&frame));
            let existing = self.tabs.entries().iter()
                .find(|tab| matches!(&tab.value.page, Page::CommandHistory(page) if page.pane == pane))
                .map(|tab| tab.id);
            let page = Page::CommandHistory(HistoryPage {
                pane,
                selected,
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
        let Some(frame) = self.history_frame(pane) else {
            return;
        };
        let Page::CommandHistory(page) = &mut self.tabs.get_mut(id).unwrap().page else {
            return;
        };
        page.feedback.clear();
        match action {
            Action::Refresh => page.selected = newest(&frame),
            Action::Select(generation, command) => page.selected = Some((generation, command)),
            Action::Copy(generation, command) => {
                let key = Some((generation, command));
                if page.selected == key
                    && let Some(entry) = selected_command(&frame, key)
                    && let Ok(text) = &entry.output
                {
                    page.feedback = match w11term::clipboard::write(text) {
                        Ok(()) => "Copied output".into(),
                        Err(error) => format!("Could not copy output · {error}"),
                    };
                }
            }
            Action::Return => {
                if let Some(source) = self.tab_for_pane(pane) {
                    self.tabs.select(source);
                    self.sync_selection(ctx);
                    self.activate(ctx);
                }
            }
            Action::Open => unreachable!(),
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
        let selected = selected_command(&frame, page.selected);
        let rows = frame.command_history.iter().rev().map(|entry| {
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
                    .on_click(
                        ctx.callback(move |_| Message::History(Action::Select(generation, id))),
                    )
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
            None => ("Select a command".into(), String::new()),
        };
        let copy_id = selected.filter(|c| c.output.is_ok()).map(|c| c.id);
        Grid::new()
            .margin(16.0)
            .row_spacing(8.0)
            .column_spacing(12.0)
            .rows([GridLength::Auto, GridLength::Auto, GridLength::STAR])
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
                            .on_click(ctx.callback(|_| Message::History(Action::Refresh)))
                            .content("Refresh history"),
                        Button::new()
                            .is_enabled(copy_id.is_some())
                            .on_click(ctx.callback(move |_| {
                                Message::History(Action::Copy(
                                    generation,
                                    copy_id.unwrap_or_default(),
                                ))
                            }))
                            .content("Copy output"),
                        Button::new()
                            .on_click(ctx.callback(|_| Message::History(Action::Return)))
                            .content("Return to pane"),
                        TextBlock::new().text(&page.feedback),
                    )),
                ScrollViewer::new()
                    .grid_row(2)
                    .content(StackPanel::new().spacing(4.0).keyed_children(rows)),
                ScrollViewer::new().grid_row(2).grid_column(1).content(
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
