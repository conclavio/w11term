use super::*;
use w11term::{
    interaction::PasteWarning,
    session::{ClipboardPaste, PendingPaste},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Origin {
    pub(super) pane: u64,
    pub(super) group: TabId,
    pub(super) connection: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket {
    serial: u64,
    origin: Origin,
}

pub enum Action {
    Start(Option<Origin>, bool),
    Reply(Ticket, Result<Option<PendingPaste>, String>),
    Closed(Ticket, ContentDialogResult),
}

#[derive(Default)]
pub(super) struct State {
    pub(super) input_origin: Rc<Cell<Option<Origin>>>,
    serial: u64,
    ticket: Option<Ticket>,
    dialog: Option<PendingPaste>,
}

impl State {
    fn start(&mut self, origin: Origin) -> Ticket {
        self.serial += 1;
        let ticket = Ticket {
            serial: self.serial,
            origin,
        };
        self.ticket = Some(ticket);
        ticket
    }

    pub(super) fn clear(&mut self) {
        self.ticket = None;
        self.dialog = None;
    }

    pub(super) fn is_open(&self) -> bool {
        self.dialog.is_some()
    }

    pub(super) fn connection_changed(&mut self, pane: u64, generation: u64, blocked: bool) {
        if self.ticket.is_some_and(|t| {
            t.origin.pane == pane && (t.origin.connection != generation || blocked)
        }) {
            self.clear();
        }
    }
}

fn read_clipboard() -> anyhow::Result<String> {
    #[cfg(feature = "ui-smoke")]
    if let Some(path) = std::env::var_os("W11TERM_TEST_PASTE_FILE") {
        return std::fs::read_to_string(path).context("Reading the clipboard test fixture");
    }
    w11term::clipboard::read()
}

fn send(
    sender: &tokio::sync::mpsc::UnboundedSender<Command>,
    ticket: Ticket,
    request: ClipboardPaste,
    ctx: &ComponentContext<TerminalApp>,
) {
    let (reply, receive) = tokio::sync::oneshot::channel();
    if sender
        .send(Command::ClipboardPaste(request, reply))
        .is_err()
    {
        ctx.sender().send(Message::Clipboard(Action::Reply(
            ticket,
            Err("The pane closed before it could paste.".into()),
        )));
        return;
    }
    ctx.spawn_background_with_rejection(
        move |_| {
            let result = receive.blocking_recv().unwrap_or_else(|_| {
                Err("The connection closed before acknowledging paste.".into())
            });
            Message::Clipboard(Action::Reply(ticket, result))
        },
        Message::Clipboard(Action::Reply(
            ticket,
            Err("Unable to receive the paste result.".into()),
        )),
    );
}

impl TerminalApp {
    pub(super) fn clipboard_menu(&self) -> MenuItem {
        let can_paste = matches!(
            self.tabs.active().map(|tab| &tab.page),
            Some(Page::Terminal(live)) if !live.read_only.get()
        );
        MenuItem::submenu(
            "clipboard",
            "Clipboard",
            [
                command_menu_item("paste", "Paste", can_paste),
                command_menu_item("paste-raw", "Paste raw (no bracketed paste)", can_paste),
                MenuItem::separator("clipboard-selection"),
                command_menu_item(
                    "copy-selection",
                    "Copy selection    Ctrl+Shift+C",
                    self.active_selection(),
                ),
                self.binding_menu_enabled(
                    "select-all",
                    KeyAction::SelectAll.binding().label,
                    KeyAction::SelectAll,
                    self.copy_mode_target().is_some(),
                ),
            ],
        )
    }

    pub(super) fn paste_origin(&self) -> Option<Origin> {
        let Page::Terminal(tab) = &self.tabs.active()?.page else {
            return None;
        };
        Some(Origin {
            pane: tab.session.pane_id,
            group: self.tabs.active_group_id()?,
            connection: tab.session.frames.load().connection_generation,
        })
    }

    // Each native invocation is scoped to one window, group, pane and connection.
    // Re-check them before presenting the dialog and before consuming its payload.
    fn paste_target(&self, origin: Origin) -> Option<&LiveTab> {
        self.pane_paste_target(self.tabs.active_id()?, origin)
    }

    pub(super) fn pane_paste_target(&self, id: TabId, origin: Origin) -> Option<&LiveTab> {
        if self.tabs.active_group_id() != Some(origin.group)
            || self.tabs.group_id(id) != Some(origin.group)
            || !self.tabs.visible(id)
            || self
                .outgoing
                .as_ref()
                .is_some_and(|transfer| transfer.includes_group(origin.group))
            || self.title_editor.is_some()
            || self.export.pending.is_some()
            || self.picker.is_some()
            || self.switcher.is_some()
        {
            return None;
        }
        let Page::Terminal(tab) = &self.tabs.get(id)?.page else {
            return None;
        };
        let frame = tab.session.frames.load();
        (tab.session.pane_id == origin.pane
            && frame.connection_generation == origin.connection
            && frame.prompt.is_none()
            && !tab.read_only.get()
            && !tab.files.awaiting_native_result()
            && !tab.files.filter_editing()
            && !tab.device_busy
            && !tab.browsing_sessions())
        .then_some(tab)
    }

    fn paste_window_is_foreground(&self) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::{GA_ROOT, GetAncestor, GetForegroundWindow};
        self.keyboard.hwnd().is_some_and(|hwnd| unsafe {
            let foreground = GetForegroundWindow();
            foreground == hwnd
                || (!foreground.is_invalid() && GetAncestor(foreground, GA_ROOT) == hwnd)
        })
    }

    pub(super) fn discard_inactive_paste(&mut self) {
        if self
            .clipboard
            .ticket
            .is_some_and(|ticket| self.paste_target(ticket.origin).is_none())
        {
            self.cancel_clipboard_request();
        }
    }

    fn cancel_clipboard_request(&mut self) {
        let was_open = self.clipboard.is_open();
        self.clipboard.clear();
        if was_open {
            let other_modal = self.title_editor.is_some() || self.tabs.active().is_some_and(|tab| matches!(
                &tab.page, Page::Terminal(live) if live.session.frames.load().prompt.is_some() || live.files.entry_dialog_open() || live.files.filter_editing()));
            self.keyboard.set_modal(other_modal);
        }
    }

    pub(super) fn paste(&mut self, raw: bool, ctx: &ComponentContext<Self>) {
        self.paste_from(self.paste_origin(), raw, None, ctx);
    }

    pub(super) fn paste_saved_text(
        &mut self,
        origin: Origin,
        text: String,
        ctx: &ComponentContext<Self>,
    ) {
        self.paste_from(Some(origin), false, Some(text), ctx);
    }

    fn begin_paste(
        &mut self,
        origin: Origin,
    ) -> Option<(tokio::sync::mpsc::UnboundedSender<Command>, Ticket)> {
        if self.clipboard.is_open() {
            return None;
        }
        let sender = self.paste_target(origin)?.session.input.clone();
        Some((sender, self.clipboard.start(origin)))
    }

    pub(super) fn paste_history_text(
        &mut self,
        origin: Origin,
        command: u64,
        text: String,
        ctx: &ComponentContext<Self>,
    ) {
        let Some((sender, ticket)) = self.begin_paste(origin) else {
            return;
        };
        send(
            &sender,
            ticket,
            ClipboardPaste::InsertHistory {
                text,
                connection: origin.connection,
                command,
            },
            ctx,
        );
        self.clear_selection();
    }

    fn paste_from(
        &mut self,
        origin: Option<Origin>,
        raw: bool,
        saved_text: Option<String>,
        ctx: &ComponentContext<Self>,
    ) {
        let Some(origin) = origin else { return };
        let Some((sender, ticket)) = self.begin_paste(origin) else {
            return;
        };
        let preserve_text = saved_text.is_some();
        let text = match saved_text.map_or_else(read_clipboard, Ok) {
            Ok(text) => text,
            Err(error) => {
                self.clipboard.clear();
                self.window_error = Some(format!("Couldn’t read the clipboard. {error:#}"));
                return;
            }
        };
        let mut preferences = self.appearance.read().unwrap().interaction.clone();
        if preserve_text {
            preferences.trim_paste = false;
        }
        send(
            &sender,
            ticket,
            ClipboardPaste::New {
                text,
                raw,
                connection: origin.connection,
                preferences,
            },
            ctx,
        );
        self.clear_selection();
    }

    pub(super) fn clipboard_action(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        let ticket = match &action {
            Action::Start(origin, raw) => return self.paste_from(*origin, *raw, None, ctx),
            Action::Reply(t, _) | Action::Closed(t, _) => *t,
        };
        if self.clipboard.ticket != Some(ticket) {
            return;
        }
        let Some(tab) = self.paste_target(ticket.origin) else {
            self.cancel_clipboard_request();
            return;
        };
        let sender = tab.session.input.clone();
        match action {
            Action::Start(..) => unreachable!("handled before worker replies"),
            Action::Reply(_, Ok(Some(p)))
                if p.connection() == ticket.origin.connection
                    && self.paste_window_is_foreground() =>
            {
                self.clipboard.dialog = Some(p);
                self.keyboard.set_modal(true);
            }
            Action::Reply(_, result) => {
                self.clipboard.clear();
                if let Err(error) = result {
                    self.window_error = Some(error);
                }
            }
            Action::Closed(_, result) => {
                let Some(p) = self.clipboard.dialog.take() else {
                    return;
                };
                self.keyboard.set_modal(false);
                if result == ContentDialogResult::Primary && self.paste_window_is_foreground() {
                    send(&sender, ticket, ClipboardPaste::Confirm(p), ctx);
                } else {
                    self.clipboard.clear();
                }
            }
        }
    }

    pub(super) fn paste_dialog(&self, ctx: &ViewContext<Self>) -> Option<KeyedView> {
        let ticket = self.clipboard.ticket?;
        let pending = self.clipboard.dialog.as_ref()?;
        let tab = self.paste_target(ticket.origin)?;
        let reason = match pending.warning {
            PasteWarning::MultiLine => "The clipboard contains multiple lines.",
            PasteWarning::Large => "The clipboard exceeds 5,120 UTF-16 code units.",
            PasteWarning::LargeMultiLine => {
                "The clipboard contains multiple lines and exceeds 5,120 UTF-16 code units."
            }
            PasteWarning::ControlCharacters => {
                "The clipboard contains control or escape characters."
            }
        };
        let destination = format!(
            "Paste into {} (pane {}).",
            source_label(&tab.source),
            ticket.origin.pane
        );
        Some(KeyedView::new(
            format!("confirm-paste-{}", ticket.serial),
            ContentDialog::new()
                .title("Confirm paste")
                .primary_button_text(if pending.is_raw() {
                    "Paste raw"
                } else {
                    "Paste"
                })
                .close_button_text("Cancel")
                .is_open(true)
                .on_closed(ctx.callback(move |r| Message::Clipboard(Action::Closed(ticket, r))))
                .content(
                    StackPanel::new().spacing(8.0).children((
                        TextBlock::new()
                            .text(destination)
                            .text_wrapping(TextWrapping::Wrap),
                        TextBlock::new()
                            .text(reason)
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
    fn a_late_clipboard_reply_cannot_claim_a_new_request_on_the_same_connection() {
        let mut state = State::default();
        let origin = Origin {
            pane: 3,
            group: 5,
            connection: 7,
        };
        let old = state.start(origin);
        state.clear();
        let current = state.start(origin);
        assert_ne!(old, current);
        assert_eq!(state.ticket, Some(current));
        state.connection_changed(4, 8, true);
        assert_eq!(state.ticket, Some(current));
        state.connection_changed(3, 7, false);
        assert_eq!(state.ticket, Some(current));
        state.connection_changed(3, 8, false);
        assert!(state.ticket.is_none());
        state.start(Origin {
            connection: 8,
            ..origin
        });
        state.connection_changed(3, 8, true);
        assert!(state.ticket.is_none());
    }
}
