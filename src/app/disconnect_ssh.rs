use super::*;
use tokio::sync::oneshot;

pub(super) const LABEL: &str = "Disconnect SSH shell…";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pane: u64,
    generation: u64,
}

pub struct Review {
    target: Target,
    label: String,
}

pub enum Action {
    Review(Target),
    Closed(Arc<Review>, ContentDialogResult),
    Failed(Target, String),
}

impl TerminalApp {
    pub(super) fn disconnect_target(&self) -> Option<Target> {
        let Page::Terminal(live) = &self.tabs.active()?.page else {
            return None;
        };
        if !live.session.is_direct_ssh_shell()
            || live.read_only.get()
            || live.session.prompts.current().is_some()
            || live.session.frames.load().prompt.is_some()
        {
            return None;
        }
        Some(Target {
            pane: live.session.pane_id,
            generation: live.session.ssh_connection_generation()?,
        })
    }

    pub(super) fn disconnect_dialog(&self, context: &ViewContext<Self>) -> Option<View> {
        let review = self.disconnect_review.clone()?;
        let current = self.disconnect_target() == Some(review.target);
        let detail = if current {
            "Ending this shell may stop remote programs and file transfers. The pane and scrollback stay open."
        } else {
            "This SSH shell has changed. Close this dialog and review the current shell."
        };
        let content = StackPanel::new().spacing(8.0).children((
            TextBlock::new()
                .text(&review.label)
                .text_wrapping(TextWrapping::Wrap),
            TextBlock::new()
                .text(detail)
                .text_wrapping(TextWrapping::Wrap),
        ));
        Some(
            ContentDialog::new()
                .title("Disconnect SSH shell?")
                .primary_button_text("Disconnect shell")
                .is_primary_button_enabled(current)
                .close_button_text("Cancel")
                .is_open(true)
                .on_closed(context.callback(move |result| {
                    Message::DisconnectSsh(Action::Closed(review.clone(), result))
                }))
                .content(content),
        )
    }

    pub(super) fn handle_ssh_disconnect(
        &mut self,
        action: Action,
        context: &ComponentContext<Self>,
    ) {
        match action {
            Action::Review(target) => {
                if self.disconnect_target() != Some(target)
                    || self.disconnect_review.is_some()
                    || self.title_editor.is_some()
                    || self.clipboard.is_open()
                {
                    return;
                }
                let Some(AppTab {
                    page: Page::Terminal(live),
                    label,
                }) = self.tabs.active()
                else {
                    return;
                };
                if live.files.entry_dialog_open() {
                    return;
                }
                let pane = live
                    .pane_name
                    .as_ref()
                    .map_or(label.as_str(), |name| name.as_str());
                self.disconnect_review = Some(Arc::new(Review {
                    target,
                    label: pane.to_owned(),
                }));
                self.keyboard.set_modal(true);
            }
            Action::Closed(review, result) => {
                if self
                    .disconnect_review
                    .take_if(|open| Arc::ptr_eq(open, &review))
                    .is_none()
                {
                    return;
                }
                self.keyboard.set_modal(false);
                if let Some(id) = self.tabs.active_id() {
                    self.connection_ui(id);
                }
                if result != ContentDialogResult::Primary {
                    return;
                }
                if self.disconnect_target() != Some(review.target) {
                    self.window_error = Some("The SSH shell changed before disconnect.".into());
                    return;
                }
                let Some(Page::Terminal(live)) = self.tabs.active().map(|tab| &tab.page) else {
                    return;
                };
                let (reply, response) = oneshot::channel();
                if live
                    .session
                    .input
                    .send(Command::DisconnectSshShell {
                        connection: review.target.generation,
                        reply,
                    })
                    .is_err()
                {
                    self.window_error = Some("The SSH pane already closed.".into());
                    return;
                }
                let target = review.target;
                context.spawn_background(move |_| match response.blocking_recv() {
                    Ok(Ok(())) => Message::ControlIdle,
                    Ok(Err(error)) => Message::DisconnectSsh(Action::Failed(target, error)),
                    Err(_) => Message::DisconnectSsh(Action::Failed(
                        target,
                        "The SSH shell closed before acknowledging disconnect.".into(),
                    )),
                });
            }
            Action::Failed(target, error) => {
                if self.tab_for_pane(target.pane).is_some() {
                    self.window_error = Some(error);
                }
            }
        }
    }
}
