use super::capture::text;
use super::*;
use w11term::control::SyscallOptions;
use w11term::ssh::{SyscallCapture, SyscallCaptureReport, verify_syscall_capture_report};

pub enum Action {
    Expanded(TabId, bool),
    Save(TabId),
    Verify(TabId),
    Repeat(TabId),
    Finished(TabId, Arc<()>, Result<Option<Outcome>, String>),
}

pub enum Outcome {
    Saved(PathBuf),
    Verified(PathBuf, Box<SyscallCaptureReport>),
}

#[derive(Default)]
pub(super) struct ViewState {
    pub(super) capture: Option<Arc<SyscallCapture>>,
    verified: Option<(PathBuf, Box<SyscallCaptureReport>)>,
    expanded: bool,
}

impl TerminalApp {
    pub(super) fn syscall_report_action(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        match action {
            Action::Expanded(id, expanded) => {
                if let Some(page) = self.syscall_page(id) {
                    page.report.expanded = expanded;
                }
            }
            Action::Save(id) => self.choose_syscall_report(id, true, ctx),
            Action::Verify(id) => self.choose_syscall_report(id, false, ctx),
            Action::Repeat(id) => {
                if self.tabs.active_id() != Some(id) {
                    return;
                }
                let options = self
                    .syscall_page(id)
                    .filter(|page| page.pending.is_none() && !page.capture.open)
                    .and_then(|page| page.report.options().cloned());
                if let Some(options) = options
                    && let Err(error) = self.repeat_syscall_capture(options, ctx)
                    && let Some(page) = self.syscall_page(id)
                {
                    page.status = Some(format!("{error:#}"));
                }
            }
            Action::Finished(id, ticket, result) => {
                let Some(page) = self.syscall_page(id) else {
                    return;
                };
                if !page.finish(&ticket) {
                    return;
                }
                match result {
                    Ok(Some(Outcome::Saved(path))) => {
                        page.status = Some(format!(
                            "Saved syscall capture report · {}",
                            safe_text(&path.display().to_string())
                        ));
                    }
                    Ok(Some(Outcome::Verified(path, report))) => {
                        page.status = Some(format!(
                            "Trace file matches report · {} bytes verified",
                            report.bytes
                        ));
                        page.report.verified = Some((path, report));
                    }
                    Ok(None) => {}
                    Err(error) => page.status = Some(error),
                }
            }
        }
    }

    fn choose_syscall_report(&mut self, id: TabId, save: bool, ctx: &ComponentContext<Self>) {
        if self.tabs.active_id() != Some(id) {
            return;
        }
        let Some(owner) = self.keyboard.hwnd().map(|window| window.0 as usize) else {
            return;
        };
        let Some(page) = self
            .syscall_page(id)
            .filter(|page| page.pending.is_none() && !page.capture.open)
        else {
            return;
        };
        let Some(trace) = page.loaded.as_ref().filter(|trace| trace.file_count() == 1) else {
            return;
        };
        let trace = trace.first_path().to_owned();
        let capture = if save {
            let Some(capture) = page.report.capture.as_ref() else {
                return;
            };
            Some(capture.clone())
        } else {
            page.report.verified = None;
            None
        };
        let purpose = file_picker::Purpose::CaptureReport {
            capture: trace.clone(),
            kind: file_picker::CaptureFileKind::Syscall,
            save,
        };
        let ticket = page.begin("Choosing a syscall capture report file…");
        let rejected = send(super::Action::Report(Action::Finished(
            id,
            ticket.clone(),
            Err("Syscall capture report worker unavailable".into()),
        )));
        ctx.spawn_background_with_rejection(
            move |scope| {
                let result = (|| -> anyhow::Result<_> {
                    let Some(file) = file_picker::pick(owner, purpose)? else {
                        return Ok(None);
                    };
                    if scope.is_cancelled() {
                        return Ok(None);
                    }
                    Ok(Some(if let Some(capture) = capture {
                        capture.save_report(&file.path)?;
                        Outcome::Saved(file.path)
                    } else {
                        let report = verify_syscall_capture_report(&trace, &file.path)?;
                        Outcome::Verified(file.path, Box::new(report))
                    }))
                })()
                .map_err(|error| format!("{error:#}"));
                send(super::Action::Report(Action::Finished(id, ticket, result)))
            },
            rejected,
        );
    }
}

impl ViewState {
    fn options(&self) -> Option<&SyscallOptions> {
        self.capture
            .as_deref()
            .map(SyscallCapture::options)
            .or_else(|| {
                self.verified
                    .as_ref()
                    .map(|(_, report)| &report.reviewed.options)
            })
    }

    pub(super) fn view(&self, id: TabId, idle: bool, ctx: &ViewContext<TerminalApp>) -> KeyedView {
        let mut buttons = Vec::new();
        if self.capture.is_some() {
            buttons.push(KeyedView::new(
                "save",
                Button::new()
                    .is_enabled(idle)
                    .on_click(ctx.callback(move |_| send(super::Action::Report(Action::Save(id)))))
                    .content("Save capture report…"),
            ));
        }
        buttons.push(KeyedView::new(
            "verify",
            Button::new()
                .is_enabled(idle)
                .on_click(ctx.callback(move |_| send(super::Action::Report(Action::Verify(id)))))
                .content("Verify capture report…"),
        ));
        if self.options().is_some() {
            buttons.push(KeyedView::new(
                "repeat",
                Button::new()
                    .is_enabled(idle)
                    .on_click(
                        ctx.callback(move |_| send(super::Action::Report(Action::Repeat(id)))),
                    )
                    .content("Repeat capture…"),
            ));
        }
        let mut fields = vec![KeyedView::new(
            "actions",
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .keyed_children(buttons),
        )];
        if let Some((path, verified)) = &self.verified {
            fields.push(KeyedView::new(
                "details",
                ScrollViewer::new().max_height(176.0).content(
                    StackPanel::new()
                        .spacing(4.0)
                        .keyed_children(details(path, verified)),
                ),
            ));
        }
        KeyedView::new(
            "capture-report",
            Expander::new()
                .automation_name("Syscall capture report")
                .is_expanded(self.expanded)
                .on_is_expanded_changed(ctx.callback(move |expanded| {
                    send(super::Action::Report(Action::Expanded(id, expanded)))
                }))
                .slots([
                    SlotView::new(
                        ExpanderSlot::Header,
                        TextBlock::new().text("Capture report"),
                    ),
                    SlotView::new(
                        ExpanderSlot::Content,
                        StackPanel::new().spacing(6.0).keyed_children(fields),
                    ),
                ]),
        )
    }
}

fn details(path: &Path, report: &SyscallCaptureReport) -> Vec<KeyedView> {
    let reviewed = &report.reviewed;
    let options = &reviewed.options;
    let args = serde_json::to_string(&reviewed.strace_arguments).unwrap_or_default();
    vec![
        text(
            "report",
            format!("Report · {}", preview(&path.display().to_string(), 512)),
        ),
        text(
            "endpoint",
            format!(
                "Recorded SSH {}@{}:{}",
                preview(&reviewed.user, 256),
                preview(&reviewed.host, 256),
                reviewed.port
            ),
        ),
        text(
            "host-key",
            format!("Host key {}", preview(&reviewed.host_key_fingerprint, 256)),
        ),
        text(
            "program",
            format!("Executable {}", preview(&reviewed.program, 512)),
        ),
        text(
            "directory",
            format!("Working directory {}", preview(&reviewed.directory, 512)),
        ),
        text(
            "limits",
            format!(
                "{} seconds · {} MiB · {} bytes per string",
                options.seconds, options.max_mib, options.string_size
            ),
        ),
        text(
            "outcome",
            capture::outcome_label(&report.stop_reason, report.command_exit_status),
        ),
        text("hash", format!("SHA-256 {}", report.sha256)),
        text("arguments", format!("strace {}", preview(&args, 4096))),
    ]
}

fn preview(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let mut text = safe_text(&chars.by_ref().take(limit).collect::<String>());
    if chars.next().is_some() {
        text.push('…');
    }
    text
}
