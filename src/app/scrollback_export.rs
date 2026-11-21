use super::*;
use std::path::PathBuf;
use tokio::sync::oneshot;

pub(super) const LABEL: &str = "Save scrollback as text…";

#[derive(Default)]
pub(super) struct Export {
    pub pending: Option<u64>,
    pub status: Option<String>,
}

pub enum Action {
    Start(Option<u64>),
    Captured(u64, Result<String, String>),
    Finished(u64, Result<Option<PathBuf>, String>),
    Dismiss,
}

impl TerminalApp {
    pub(super) fn export_scrollback(&mut self, action: Action, context: &ComponentContext<Self>) {
        match action {
            Action::Start(Some(pane)) if self.export.pending.is_none() => {
                // Use the pane named by the rendered action. Never resolve the active pane again.
                let live = self.tab_for_pane(pane).and_then(|id| self.tabs.get(id));
                let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = live
                else {
                    self.export.status = Some(format!("Pane {pane} is no longer in this window."));
                    return;
                };
                let (reply, result) = oneshot::channel();
                if tab.session.input.send(Command::ExportText(reply)).is_err() {
                    self.export.status = Some(format!(
                        "Pane {pane} has stopped. Its scrollback is unavailable."
                    ));
                    return;
                }
                self.export.pending = Some(pane);
                self.export.status = None;
                context.spawn_background_with_rejection(
                    move |_| {
                        let text = result
                            .blocking_recv()
                            .map_err(|_| "The terminal stopped during capture".to_owned())
                            .and_then(|text| text.map_err(str::to_owned));
                        Message::Export(Action::Captured(pane, text))
                    },
                    Message::Export(Action::Finished(
                        pane,
                        Err("Scrollback capture is unavailable".into()),
                    )),
                );
            }
            Action::Captured(pane, result) if self.export.pending == Some(pane) => {
                let text = match result {
                    Ok(text) => text,
                    Err(error) => return self.export.finish(pane, Err(error)),
                };
                let Some(owner) = self.keyboard.hwnd() else {
                    return self
                        .export
                        .finish(pane, Err("The window is unavailable".into()));
                };
                let owner = owner.0 as usize;
                context.spawn_background_with_rejection(
                    move |cancel| {
                        let save = || -> anyhow::Result<Option<PathBuf>> {
                            if cancel.is_cancelled() {
                                return Ok(None);
                            }
                            let Some(file) =
                                file_picker::pick(owner, file_picker::Purpose::Scrollback)?
                            else {
                                return Ok(None);
                            };
                            w11term::transcript::save(&file.path, &text, file.overwrite)?;
                            Ok(Some(file.path))
                        };
                        Message::Export(Action::Finished(
                            pane,
                            save().map_err(|e| format!("{e:#}")),
                        ))
                    },
                    Message::Export(Action::Finished(
                        pane,
                        Err("File picker unavailable".into()),
                    )),
                );
            }
            Action::Finished(pane, result) if self.export.pending == Some(pane) => {
                self.export.finish(pane, result);
            }
            Action::Dismiss => self.export.status = None,
            _ => {}
        }
    }
}

impl Export {
    fn finish(&mut self, pane: u64, result: Result<Option<PathBuf>, String>) {
        self.pending = None;
        self.status = match result {
            Ok(Some(path)) => Some(format!(
                "Saved scrollback for pane {pane} to {}",
                path.display()
            )),
            Ok(None) => None,
            Err(error) => Some(format!(
                "Could not save scrollback for pane {pane}: {error}"
            )),
        };
    }
}
