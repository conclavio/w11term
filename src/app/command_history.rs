use super::*;
use w11term::control::CommandStatus;
use w11term::saved_commands::{Commands, Entry, valid_command_text};
use w11term::session::history_text_allowed;
use w11term::terminal::Snapshot;
use wezterm_term::CompletedCommand;

pub(super) struct HistoryPage {
    pane: u64,
    selected: Option<(u64, u64)>, // Connection generation and command id.
    query: String,
    status: CommandStatus,
    feedback: String,
    save: Option<SaveDraft>,
    showing: Option<Arc<()>>,
}

struct SaveDraft {
    target: (u64, u64),
    name: String,
}

pub enum Action {
    Open,
    InPane(u64, HistoryAction),
    OutputShown {
        page: TabId,
        pane: u64,
        target: (u64, u64),
        request: Arc<()>,
        result: Result<(), &'static str>,
    },
}

pub enum HistoryAction {
    Search(String),
    Status(CommandStatus),
    Refresh,
    Select(u64, u64),
    Copy(u64, u64, CopyPart),
    ExportOutput((u64, u64)),
    ShowOutput((u64, u64)),
    Insert((u64, u64)),
    BeginSave((u64, u64)),
    SaveName((u64, u64), String),
    Save((u64, u64)),
    CancelSave((u64, u64)),
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

const STATUS_OPTIONS: [(CommandStatus, &str); 4] = [
    (CommandStatus::All, "All commands"),
    (CommandStatus::Failed, "Failed"),
    (CommandStatus::Succeeded, "Succeeded"),
    (CommandStatus::Unknown, "Status unknown"),
];

fn matches_command(entry: &CompletedCommand, needle: &str, status: CommandStatus) -> bool {
    status.matches(entry.exit_code)
        && (needle.is_empty()
            || entry
                .command
                .as_ref()
                .is_some_and(|text| text.to_lowercase().contains(needle)))
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

fn saveable_command(frame: &Snapshot, target: (u64, u64)) -> Option<&str> {
    selected_command(frame, Some(target))?
        .command
        .as_deref()
        .filter(|text| valid_command_text(text))
}

fn newest(frame: &Snapshot, needle: &str, status: CommandStatus) -> Option<(u64, u64)> {
    frame
        .command_history
        .iter()
        .rev()
        .find(|entry| matches_command(entry, needle, status))
        .map(|c| (frame.connection_generation, c.id))
}

impl HistoryPage {
    fn keep_matching_selection(&mut self, frame: &Snapshot) {
        let needle = self.query.to_lowercase();
        if !selected_command(frame, self.selected)
            .is_some_and(|entry| matches_command(entry, &needle, self.status))
        {
            self.selected = newest(frame, &needle, self.status);
        }
    }
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

    fn history_insert_target(
        &self,
        pane: u64,
        frame: &Snapshot,
    ) -> Option<(TabId, clipboard_paste::Origin)> {
        let id = self.tab_for_pane(pane)?;
        let Page::Terminal(live) = &self.tabs.get(id)?.page else {
            return None;
        };
        if frame.alternate
            || frame.prompt.is_some()
            || live.read_only.get()
            || !live.session.events.idle_shell(frame.connection_generation)
        {
            return None;
        }
        Some((
            id,
            clipboard_paste::Origin {
                pane,
                group: self.tabs.group_id(id)?,
                connection: frame.connection_generation,
            },
        ))
    }

    fn show_history_output(
        &mut self,
        page_id: TabId,
        pane: u64,
        target: (u64, u64),
        ctx: &ComponentContext<Self>,
    ) {
        let Some(Page::Terminal(live)) = self
            .tab_for_pane(pane)
            .and_then(|id| self.tabs.get(id))
            .map(|tab| &tab.page)
        else {
            return;
        };
        let input = live.session.input.clone();
        let Some(Page::CommandHistory(page)) = self.tabs.get_mut(page_id).map(|tab| &mut tab.page)
        else {
            return;
        };
        let (reply, received) = tokio::sync::oneshot::channel();
        let request = Arc::new(());
        let complete = {
            let request = request.clone();
            move |result| {
                Message::History(Action::OutputShown {
                    page: page_id,
                    pane,
                    target,
                    request: request.clone(),
                    result,
                })
            }
        };
        page.showing = Some(request);
        let rejected = complete(Err("Could not locate the command output."));
        let task = ctx.spawn_background_with_rejection(
            move |_| {
                complete(
                    received
                        .blocking_recv()
                        .unwrap_or(Err("The source pane has closed.")),
                )
            },
            rejected,
        );
        if task.status() != ComponentTaskStatus::Rejected {
            let _ = input.send(Command::RevealCommand {
                connection: target.0,
                command: target.1,
                reply,
            });
        }
    }

    pub(super) fn handle_history(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        if let Action::OutputShown {
            page,
            pane,
            target,
            request,
            result,
        } = action
        {
            let active = self.tabs.active_id() == Some(page);
            let frame = self.history_frame(pane);
            let Some(Page::CommandHistory(view)) = self.tabs.get_mut(page).map(|tab| &mut tab.page)
            else {
                return;
            };
            if view.pane != pane
                || !view
                    .showing
                    .as_ref()
                    .is_some_and(|pending| Arc::ptr_eq(pending, &request))
            {
                return;
            }
            view.showing = None;
            if view.selected == Some(target) {
                match result {
                    Ok(())
                        if frame
                            .as_ref()
                            .is_some_and(|frame| frame.connection_generation == target.0) =>
                    {
                        if active && let Some(source) = self.tab_for_pane(pane) {
                            self.tabs.select(source);
                            self.sync_selection(ctx);
                            self.activate(ctx);
                        }
                    }
                    Ok(()) => view.feedback = "The source pane changed.".into(),
                    Err(error) => view.feedback = error.into(),
                }
            }
            return;
        }
        if matches!(action, Action::Open) {
            let Some(source) = self.tabs.active() else {
                return;
            };
            let Some(pane) = source.pane_id() else { return };
            let label = format!("History · {}", source.label);
            let selected = self
                .history_frame(pane)
                .and_then(|frame| newest(&frame, "", CommandStatus::All));
            let existing = self.tabs.entries().iter()
                .find(|tab| matches!(&tab.value.page, Page::CommandHistory(page) if page.pane == pane))
                .map(|tab| tab.id);
            let page = Page::CommandHistory(HistoryPage {
                pane,
                selected,
                query: String::new(),
                status: CommandStatus::All,
                feedback: String::new(),
                save: None,
                showing: None,
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
        if matches!(&action, HistoryAction::Status(status) if *status == page.status) {
            return;
        }
        page.feedback.clear();
        match action {
            HistoryAction::Search(query) => {
                page.query = query;
                page.keep_matching_selection(&frame);
            }
            HistoryAction::Status(status) => {
                page.status = status;
                page.keep_matching_selection(&frame);
            }
            HistoryAction::Refresh => {
                page.selected = newest(&frame, &page.query.to_lowercase(), page.status)
            }
            HistoryAction::Select(generation, command) => {
                let key = Some((generation, command));
                if selected_command(&frame, key).is_some_and(|entry| {
                    matches_command(entry, &page.query.to_lowercase(), page.status)
                }) {
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
            HistoryAction::ExportOutput(target) => {
                if page.selected == Some(target)
                    && let Some(output) = selected_command(&frame, Some(target))
                        .and_then(|entry| CopyPart::Output.text(entry))
                {
                    self.export_command_output(pane, target.1, output, ctx);
                }
                return;
            }
            HistoryAction::ShowOutput(target) => {
                if page.showing.is_none()
                    && page.selected == Some(target)
                    && selected_command(&frame, Some(target)).is_some()
                    && !frame.alternate
                {
                    self.show_history_output(id, pane, target, ctx);
                }
                return;
            }
            HistoryAction::Insert(target) => {
                if page.selected != Some(target) {
                    return;
                }
                let Some(text) = selected_command(&frame, Some(target))
                    .and_then(|entry| entry.command.as_ref())
                    .filter(|text| history_text_allowed(text, frame.bracketed_paste))
                else {
                    return;
                };
                if let Some((source, origin)) = self.history_insert_target(pane, &frame) {
                    self.tabs.select(source);
                    self.sync_selection(ctx);
                    self.activate(ctx);
                    self.paste_history_text(origin, target.1, text.clone(), ctx);
                }
                return;
            }
            HistoryAction::BeginSave(target) => {
                if page.selected == Some(target) && saveable_command(&frame, target).is_some() {
                    page.save = Some(SaveDraft {
                        target,
                        name: String::new(),
                    });
                }
            }
            HistoryAction::SaveName(target, name) => {
                if let Some(draft) = &mut page.save
                    && draft.target == target
                {
                    draft.name = name;
                }
            }
            HistoryAction::Save(target) => {
                if let Some(draft) = &page.save
                    && draft.target == target
                    && page.selected == Some(target)
                    && let Some(text) = saveable_command(&frame, target)
                {
                    let entry = Entry {
                        name: draft.name.clone(),
                        text: text.into(),
                    };
                    match Commands::path()
                        .and_then(|path| Commands::update(&path, None, Some(entry)))
                    {
                        Ok(_) => {
                            page.save = None;
                            page.feedback = "Saved command".into();
                        }
                        Err(error) => page.feedback = format!("{error:#}"),
                    }
                }
            }
            HistoryAction::CancelSave(target) => {
                if page
                    .save
                    .as_ref()
                    .is_some_and(|draft| draft.target == target)
                {
                    page.save = None;
                }
            }
            HistoryAction::Return => {
                if let Some(source) = self.tab_for_pane(pane) {
                    self.tabs.select(source);
                    self.sync_selection(ctx);
                    self.activate(ctx);
                }
                return;
            }
        }
        if page
            .save
            .as_ref()
            .is_some_and(|draft| Some(draft.target) != page.selected)
        {
            page.save = None;
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
            .filter(|c| matches_command(c, &needle, page.status))
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
        let save_target = page
            .selected
            .filter(|&target| saveable_command(&frame, target).is_some());
        let insert_target = page.selected.filter(|_| {
            selected
                .and_then(|entry| entry.command.as_deref())
                .is_some_and(|text| history_text_allowed(text, frame.bracketed_paste))
                && self.history_insert_target(pane, &frame).is_some()
        });
        let export_target = page.selected.filter(|_| {
            selected.is_some_and(|entry| entry.output.is_ok()) && self.export.pending.is_none()
        });
        let show_target = page
            .selected
            .filter(|_| selected.is_some() && !frame.alternate && page.showing.is_none());
        let save =
            match (&page.save, save_target) {
                (Some(draft), Some(target)) if draft.target == target => {
                    StackPanel::new().spacing(6.0).children((
                        TextBox::new()
                            .automation_name("Saved command name")
                            .placeholder_text("Saved command name")
                            .text(&draft.name)
                            .on_text_changed(ctx.callback(move |name| {
                                action(HistoryAction::SaveName(target, name))
                            })),
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(8.0)
                            .children((
                                Button::new()
                                    .is_enabled(!draft.name.trim().is_empty())
                                    .on_click(
                                        ctx.callback(move |_| action(HistoryAction::Save(target))),
                                    )
                                    .content("Save to commands"),
                                Button::new()
                                    .automation_name("Cancel saving command")
                                    .on_click(ctx.callback(move |_| {
                                        action(HistoryAction::CancelSave(target))
                                    }))
                                    .content("Cancel"),
                            )),
                    ))
                }
                (_, target) => Button::new()
                    .is_enabled(target.is_some())
                    .on_click(ctx.callback(move |_| {
                        target.map_or(Message::ControlIdle, |target| {
                            action(HistoryAction::BeginSave(target))
                        })
                    }))
                    .content("Save as command…"),
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
                StackPanel::new().grid_row(2).spacing(4.0).children((
                    TextBox::new()
                        .automation_name("Search command history")
                        .placeholder_text("Search command history")
                        .text(&page.query)
                        .on_text_changed(
                            ctx.callback(move |text| action(HistoryAction::Search(text))),
                        ),
                    ComboBox::new()
                        .automation_name("Command status")
                        .items_source(STATUS_OPTIONS.map(|(_, label)| label))
                        .selected_index(
                            STATUS_OPTIONS
                                .iter()
                                .position(|(status, _)| *status == page.status),
                        )
                        .horizontal_alignment(HorizontalAlignment::Stretch)
                        .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                            index
                                .and_then(|index| STATUS_OPTIONS.get(index))
                                .map_or(Message::ControlIdle, |&(status, _)| {
                                    action(HistoryAction::Status(status))
                                })
                        })),
                )),
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
                            Button::new()
                                .is_enabled(insert_target.is_some())
                                .on_click(ctx.callback(move |_| {
                                    insert_target.map_or(Message::ControlIdle, |target| {
                                        action(HistoryAction::Insert(target))
                                    })
                                }))
                                .content("Insert command"),
                            save,
                            Button::new()
                                .is_enabled(export_target.is_some())
                                .on_click(ctx.callback(move |_| {
                                    export_target.map_or(Message::ControlIdle, |target| {
                                        action(HistoryAction::ExportOutput(target))
                                    })
                                }))
                                .content(text_export::COMMAND_OUTPUT_LABEL),
                            Button::new()
                                .is_enabled(show_target.is_some())
                                .on_click(ctx.callback(move |_| {
                                    show_target.map_or(Message::ControlIdle, |target| {
                                        action(HistoryAction::ShowOutput(target))
                                    })
                                }))
                                .content("Show output in pane"),
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
        assert!(matches_command(&entry, "[ångström]", CommandStatus::Failed));
        assert!(!matches_command(
            &entry,
            "[ångström]",
            CommandStatus::Succeeded
        ));
        assert!(!matches_command(&entry, "archived", CommandStatus::All));
        assert!(!matches_command(&entry, ".*", CommandStatus::All));
        assert_eq!(CopyPart::Command.text(&entry), Some(command.as_str()));
        assert_eq!(CopyPart::Output.text(&entry), Some("archived result\r\n"));
        entry.command = None;
        assert!(matches_command(&entry, "", CommandStatus::Failed));
        assert!(!matches_command(&entry, "9", CommandStatus::All));
        assert_eq!(CopyPart::Command.text(&entry), None);
        entry.output = Err("Unavailable");
        assert_eq!(CopyPart::Output.text(&entry), None);
    }
}
