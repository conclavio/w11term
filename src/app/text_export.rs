use super::*;
use std::{fs::File, io, io::Write, path::PathBuf};
use tokio::sync::oneshot;

pub(super) const SCROLLBACK_LABEL: &str = "Save scrollback as text…";
pub(super) const COMMAND_OUTPUT_LABEL: &str = "Save output as text…";
pub(super) const GROUP_REPORT_LABEL: &str = "Save results…";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subject {
    Scrollback(u64),
    CommandOutput { pane: u64, command: u64 },
    HostGroupResults(u64),
}

impl Subject {
    fn label(self) -> String {
        match self {
            Self::Scrollback(pane) => format!("scrollback for pane {pane}"),
            Self::CommandOutput { pane, .. } => format!("command output from pane {pane}"),
            Self::HostGroupResults(_) => "SSH group results".into(),
        }
    }

    fn picker(self) -> file_picker::Purpose {
        match self {
            Self::Scrollback(_) => file_picker::Purpose::Scrollback,
            Self::CommandOutput { pane, command } => {
                file_picker::Purpose::CommandOutput(format!("pane-{pane}-command-{command}.txt"))
            }
            Self::HostGroupResults(submission) => {
                file_picker::Purpose::GroupReport(format!("ssh-group-submission-{submission}.txt"))
            }
        }
    }
}

#[derive(Default)]
pub(super) struct Export {
    pub pending: Option<Subject>,
    pub status: Option<String>,
}

pub enum Action {
    StartScrollback(Option<u64>),
    Captured(u64, Result<String, String>),
    Finished(Subject, Result<Option<PathBuf>, String>),
    Dismiss,
}

impl TerminalApp {
    pub(super) fn handle_text_export(&mut self, action: Action, context: &ComponentContext<Self>) {
        match action {
            Action::StartScrollback(Some(pane)) if self.export.pending.is_none() => {
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
                let subject = Subject::Scrollback(pane);
                self.export.pending = Some(subject);
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
                        subject,
                        Err("Scrollback capture is unavailable".into()),
                    )),
                );
            }
            Action::Captured(pane, result)
                if self.export.pending == Some(Subject::Scrollback(pane)) =>
            {
                let subject = Subject::Scrollback(pane);
                match result {
                    Ok(text) => self.save_export(
                        subject,
                        move |file| file.write_all(text.as_bytes()),
                        context,
                    ),
                    Err(error) => self.export.finish(subject, Err(error)),
                }
            }
            Action::Finished(subject, result) if self.export.pending == Some(subject) => {
                self.export.finish(subject, result);
            }
            Action::Dismiss => self.export.status = None,
            _ => {}
        }
    }

    pub(super) fn export_command_output(
        &mut self,
        pane: u64,
        command: u64,
        text: &str,
        context: &ComponentContext<Self>,
    ) {
        if self.export.pending.is_some() {
            return;
        }
        let subject = Subject::CommandOutput { pane, command };
        let text = text.to_owned();
        self.begin_text_export(
            subject,
            move |file| file.write_all(text.as_bytes()),
            context,
        );
    }

    pub(super) fn begin_text_export(
        &mut self,
        subject: Subject,
        write: impl FnOnce(&mut File) -> io::Result<()> + Send + 'static,
        context: &ComponentContext<Self>,
    ) {
        if self.export.pending.is_some() {
            return;
        }
        self.export.pending = Some(subject);
        self.export.status = None;
        self.save_export(subject, write, context);
    }

    fn save_export(
        &mut self,
        subject: Subject,
        write: impl FnOnce(&mut File) -> io::Result<()> + Send + 'static,
        context: &ComponentContext<Self>,
    ) {
        let Some(owner) = self.keyboard.hwnd() else {
            return self
                .export
                .finish(subject, Err("The window is unavailable".into()));
        };
        let owner = owner.0 as usize;
        context.spawn_background_with_rejection(
            move |cancel| {
                let save = || -> anyhow::Result<Option<PathBuf>> {
                    if cancel.is_cancelled() {
                        return Ok(None);
                    }
                    let Some(file) = file_picker::pick(owner, subject.picker())? else {
                        return Ok(None);
                    };
                    if cancel.is_cancelled() {
                        return Ok(None);
                    }
                    w11term::transcript::save_with(&file.path, file.overwrite, write)?;
                    Ok(Some(file.path))
                };
                Message::Export(Action::Finished(
                    subject,
                    save().map_err(|e| format!("{e:#}")),
                ))
            },
            Message::Export(Action::Finished(
                subject,
                Err("File picker unavailable".into()),
            )),
        );
    }
}

impl Export {
    fn finish(&mut self, subject: Subject, result: Result<Option<PathBuf>, String>) {
        self.pending = None;
        self.status = match result {
            Ok(Some(path)) => Some(format!("Saved {} to {}", subject.label(), path.display())),
            Ok(None) => None,
            Err(error) => Some(format!("Could not save {}. {error}", subject.label())),
        };
    }
}
