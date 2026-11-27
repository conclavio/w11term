use super::*;
use std::{fs::File, io, io::Write, path::PathBuf};
use tokio::sync::oneshot;

pub(super) const SCROLLBACK_LABEL: &str = "Save scrollback as text…";
pub(super) const COPY_PANE_IMAGE_LABEL: &str = "Copy pane as image";
pub(super) const SAVE_PANE_IMAGE_LABEL: &str = "Save pane image…";
pub(super) const COPY_VIEWPORT_LABEL: &str = "Copy visible pane text";
pub(super) const COMMAND_OUTPUT_LABEL: &str = "Save output as text…";
pub(super) const GROUP_REPORT_LABEL: &str = "Save results…";
pub(super) const GROUP_JSON_LABEL: &str = "Save results as JSON…";
pub(super) const GROUP_SHOWN_REPORT_LABEL: &str = "Save shown results…";
pub(super) const GROUP_SHOWN_JSON_LABEL: &str = "Save shown results as JSON…";

pub use w11term::control::SshGroupReportFormat as GroupReportFormat;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FolderComparisonTarget {
    pub pane: u64,
    pub generation: u64,
    pub serial: u64,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subject {
    Scrollback(u64),
    PaneImage(u64),
    PaneText(u64),
    CommandOutput {
        pane: u64,
        command: u64,
    },
    CommandOutputDiff {
        reference_pane: u64,
        reference: u64,
        selected_pane: u64,
        selected: u64,
    },
    WindowCommandHistory,
    HostGroupResults {
        submission: u64,
        format: GroupReportFormat,
        shown: bool,
    },
    HostGroupShownOutput(u64),
    HostGroupDiff {
        submission: u64,
        reference: u64,
        selected: u64,
    },
    HostGroupRunDiff {
        baseline: u64,
        run: u64,
        pane: u64,
    },
    HostGroupRunComparison {
        baseline: u64,
        run: u64,
    },
    FolderComparison(FolderComparisonTarget),
    SyscallComparison,
    CpuComparison,
    TriageComparison,
    SftpTransferHistory,
    SshHostList,
    SshGroupDefinition,
    SshLoginReport(u64),
}

impl Subject {
    fn label(self) -> String {
        match self {
            Self::Scrollback(pane) => format!("scrollback for pane {pane}"),
            Self::PaneImage(pane) => format!("image of pane {pane}"),
            Self::PaneText(pane) => format!("visible text from pane {pane}"),
            Self::CommandOutput { pane, .. } => format!("command output from pane {pane}"),
            Self::CommandOutputDiff { .. } => "command output differences".into(),
            Self::WindowCommandHistory => "command history results".into(),
            Self::HostGroupResults { format, shown, .. } => {
                let scope = if shown { "shown " } else { "" };
                let format = if format == GroupReportFormat::Json {
                    "JSON "
                } else {
                    ""
                };
                format!("{scope}SSH group {format}results")
            }
            Self::HostGroupShownOutput(_) => "shown SSH group output".into(),
            Self::HostGroupDiff { .. } => "SSH group differences".into(),
            Self::HostGroupRunDiff { .. } => "SSH group run differences".into(),
            Self::HostGroupRunComparison { .. } => "SSH group run comparison".into(),
            Self::FolderComparison(_) => "folder comparison".into(),
            Self::SyscallComparison => "syscall comparison".into(),
            Self::CpuComparison => "CPU profile comparison".into(),
            Self::TriageComparison => "Linux triage comparison".into(),
            Self::SftpTransferHistory => "SFTP transfer history".into(),
            Self::SshHostList => "SSH host list".into(),
            Self::SshGroupDefinition => "SSH group definition".into(),
            Self::SshLoginReport(_) => "SSH login report".into(),
        }
    }

    fn picker(self) -> Option<file_picker::Purpose> {
        Some(match self {
            Self::Scrollback(_) => file_picker::Purpose::Scrollback,
            Self::PaneImage(_) => file_picker::Purpose::PaneImage,
            Self::PaneText(_) => return None,
            Self::CommandOutput { pane, command } => {
                file_picker::Purpose::CommandOutput(format!("pane-{pane}-command-{command}.txt"))
            }
            Self::CommandOutputDiff {
                reference_pane,
                reference,
                selected_pane,
                selected,
            } => file_picker::Purpose::CommandOutputDiff(format!(
                "command-diff-{reference_pane}-{reference}-to-{selected_pane}-{selected}.txt"
            )),
            Self::WindowCommandHistory => file_picker::Purpose::WindowCommandHistory,
            Self::HostGroupResults {
                submission,
                format,
                shown,
            } => {
                let scope = if shown { "-shown" } else { "" };
                let stem = format!("ssh-group-submission-{submission}{scope}");
                match format {
                    GroupReportFormat::Text => {
                        file_picker::Purpose::GroupReport(format!("{stem}.txt"))
                    }
                    GroupReportFormat::Json => {
                        file_picker::Purpose::GroupJsonReport(format!("{stem}.json"))
                    }
                }
            }
            Self::HostGroupShownOutput(submission) => file_picker::Purpose::GroupOutputExcerpt(
                format!("ssh-group-output-excerpt-{submission}.txt"),
            ),
            Self::HostGroupDiff {
                submission,
                reference,
                selected,
            } => file_picker::Purpose::GroupOutputDiff(format!(
                "ssh-group-diff-{submission}-{reference}-to-{selected}.txt"
            )),
            Self::HostGroupRunDiff {
                baseline,
                run,
                pane,
            } => file_picker::Purpose::GroupOutputDiff(format!(
                "ssh-group-run-diff-{baseline}-to-{run}-pane-{pane}.txt"
            )),
            Self::HostGroupRunComparison { baseline, run } => {
                file_picker::Purpose::GroupRunComparison(format!(
                    "ssh-group-run-comparison-{baseline}-to-{run}.json"
                ))
            }
            Self::FolderComparison(_) => file_picker::Purpose::FolderComparison,
            Self::SyscallComparison => file_picker::Purpose::SyscallComparison,
            Self::CpuComparison => file_picker::Purpose::CpuComparison,
            Self::TriageComparison => file_picker::Purpose::TriageComparison,
            Self::SftpTransferHistory => file_picker::Purpose::SftpTransferHistory,
            Self::SshHostList => file_picker::Purpose::SshHostList,
            Self::SshGroupDefinition => file_picker::Purpose::SshGroupDefinition,
            Self::SshLoginReport(sequence) => {
                file_picker::Purpose::SshLoginReport(format!("ssh-login-results-{sequence}.txt"))
            }
        })
    }
}

#[derive(Default)]
pub(super) struct Export {
    pub pending: Option<Subject>,
    pub status: Option<String>,
    folder_feedback: Option<(FolderComparisonTarget, String)>,
}

pub enum Action {
    StartScrollback(Option<u64>),
    StartPaneImage(Option<u64>, ImageDestination),
    CopyPaneText(Option<u64>),
    SaveFolderComparison(FolderComparisonTarget),
    CopiedPane(Subject, Result<bool, String>),
    Captured(u64, Result<String, String>),
    Finished(Subject, Result<Option<PathBuf>, String>),
    Dismiss,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageDestination {
    Clipboard,
    File,
}

impl ImageDestination {
    pub(super) fn from_label(label: &str) -> Option<Self> {
        match label {
            COPY_PANE_IMAGE_LABEL => Some(Self::Clipboard),
            SAVE_PANE_IMAGE_LABEL => Some(Self::File),
            _ => None,
        }
    }
}

enum PaneClipboard {
    Text(String),
    Image(image::RgbaImage),
}

impl TerminalApp {
    pub(super) fn handle_text_export(&mut self, action: Action, context: &ComponentContext<Self>) {
        match action {
            Action::SaveFolderComparison(target) if self.export.pending.is_none() => {
                self.save_folder_comparison(target, context);
            }
            Action::StartPaneImage(Some(pane), destination) if self.export.pending.is_none() => {
                self.export_pane_image(pane, destination, context);
            }
            Action::CopyPaneText(Some(pane)) if self.export.pending.is_none() => {
                let subject = Subject::PaneText(pane);
                if let Some(frame) = self.export_frame(pane, subject) {
                    self.copy_pane(
                        subject,
                        move || {
                            let text = w11term::terminal::viewport_text(&frame)
                                .map_err(anyhow::Error::msg)?;
                            anyhow::ensure!(!text.is_empty(), "The viewport contains no text");
                            Ok(PaneClipboard::Text(text))
                        },
                        context,
                    );
                }
            }
            Action::CopiedPane(subject, result) if self.export.pending == Some(subject) => {
                self.export.pending = None;
                self.export.status = Some(match result {
                    Ok(true) => format!("Copied {} to the clipboard", subject.label()),
                    Ok(false) => format!("Clipboard changed while capturing {}", subject.label()),
                    Err(error) => format!("Could not copy {}. {error}", subject.label()),
                });
            }
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
            Action::Dismiss => {
                self.export.status = None;
                self.export.folder_feedback = None;
            }
            _ => {}
        }
    }

    fn export_pane_image(
        &mut self,
        pane: u64,
        destination: ImageDestination,
        context: &ComponentContext<Self>,
    ) {
        let subject = Subject::PaneImage(pane);
        let Some(frame) = self.export_frame(pane, subject) else {
            return;
        };
        // Freeze the displayed viewport and appearance before showing the Save dialog.
        let font_size = self
            .tab_for_pane(pane)
            .and_then(|id| match &self.tabs.get(id)?.page {
                Page::Terminal(tab) => tab.font_size_override(),
                _ => None,
            });
        let mut appearance = self.appearance.read().unwrap().clone();
        if let Some(font_size) = font_size {
            appearance.font_size = font_size;
        }
        let render = move || crate::renderer::render_grid(&frame, frame.size, &appearance);
        match destination {
            ImageDestination::File => self.begin_text_export(
                subject,
                move |file| {
                    crate::renderer::write_grid_png(file, render().map_err(io::Error::other)?)
                },
                context,
            ),
            ImageDestination::Clipboard => {
                self.copy_pane(subject, move || render().map(PaneClipboard::Image), context)
            }
        }
    }

    fn export_frame(
        &mut self,
        pane: u64,
        subject: Subject,
    ) -> Option<Arc<w11term::terminal::Snapshot>> {
        let frame = self
            .tab_for_pane(pane)
            .and_then(|id| match &self.tabs.get(id)?.page {
                Page::Terminal(tab) if !tab.browsing_sessions() && !tab.device_busy => {
                    Some(tab.session.frames.load())
                }
                _ => None,
            });
        if frame.is_none() {
            self.export.status = Some(format!("The {} is unavailable", subject.label()));
        }
        frame
    }

    fn copy_pane(
        &mut self,
        subject: Subject,
        capture: impl FnOnce() -> anyhow::Result<PaneClipboard> + Send + 'static,
        context: &ComponentContext<Self>,
    ) {
        self.export.pending = Some(subject);
        self.export.status = None;
        let sequence = w11term::clipboard::sequence();
        context.spawn_background_with_rejection(
            move |cancel| {
                let copy = || -> anyhow::Result<bool> {
                    if cancel.is_cancelled() {
                        return Ok(false);
                    }
                    let contents = capture()?;
                    if cancel.is_cancelled() {
                        return Ok(false);
                    }
                    match contents {
                        PaneClipboard::Text(text) => {
                            w11term::clipboard::try_write_if_sequence(&text, sequence)
                        }
                        PaneClipboard::Image(grid) => {
                            w11term::clipboard::write_image_if_sequence(grid, sequence)
                        }
                    }
                };
                Message::Export(Action::CopiedPane(
                    subject,
                    copy().map_err(|error| format!("{error:#}")),
                ))
            },
            Message::Export(Action::CopiedPane(
                subject,
                Err("Pane capture is unavailable".into()),
            )),
        );
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
        if let Subject::FolderComparison(_) = subject {
            self.export.folder_feedback = None;
        }
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
                    let purpose = subject
                        .picker()
                        .context("This capture does not use a file picker")?;
                    let Some(file) = file_picker::pick(owner, purpose)? else {
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
    pub(super) fn folder_status(&self, target: FolderComparisonTarget) -> Option<&str> {
        self.folder_feedback
            .as_ref()
            .filter(|(reported, _)| *reported == target)
            .map(|(_, message)| message.as_str())
    }

    fn finish(&mut self, subject: Subject, result: Result<Option<PathBuf>, String>) {
        self.pending = None;
        self.status = match result {
            Ok(Some(path)) => Some(format!("Saved {} to {}", subject.label(), path.display())),
            Ok(None) => None,
            Err(error) => Some(format!("Could not save {}. {error}", subject.label())),
        };
        if let Subject::FolderComparison(target) = subject {
            self.folder_feedback = self.status.clone().map(|status| (target, status));
        }
    }
}
