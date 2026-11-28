use super::{
    Action as ViewerAction, Loaded, Outcome as ViewerOutcome, Panel, Profile, ProfileSource,
    capture, message, text,
};
use crate::app::*;
use std::{path::Path, path::PathBuf};
use w11term::{
    control::CpuCallGraph,
    ssh::{CpuProfileCaptureReport, CpuProfileReportTarget, verify_cpu_profile_report},
};

pub enum Action {
    Save,
    Verify,
    VerifyRaw,
    Expanded(bool),
}

pub enum Outcome {
    Saved,
    Verified(Verified),
    RawVerified(PathBuf),
}

pub struct Verified {
    path: PathBuf,
    pub(super) report: Arc<CpuProfileCaptureReport>,
    raw_path: Option<PathBuf>,
    expanded: bool,
}

enum Task {
    Save(Arc<CpuProfileCaptureReport>),
    Verify(ProfileSource),
    VerifyRaw(Arc<CpuProfileCaptureReport>),
}

impl Task {
    fn run(self, path: PathBuf) -> anyhow::Result<Outcome> {
        match self {
            Self::Save(report) => {
                report.save(&path)?;
                Ok(Outcome::Saved)
            }
            Self::Verify(source) => {
                let report = verify_for_profile(&source.profile, &source.path, &path)?;
                Ok(Outcome::Verified(Verified {
                    path,
                    report,
                    raw_path: None,
                    expanded: true,
                }))
            }
            Self::VerifyRaw(report) => {
                report.verify_raw_recording(&path)?;
                Ok(Outcome::RawVerified(path))
            }
        }
    }
}

pub(super) fn matches_profile(profile: &Profile, report: &CpuProfileCaptureReport) -> bool {
    profile.loaded_sha256() == Some(report.folded.sha256.as_str())
}

pub(super) fn verify_for_profile(
    profile: &Profile,
    folded: &Path,
    path: &Path,
) -> anyhow::Result<Arc<CpuProfileCaptureReport>> {
    let report = verify_cpu_profile_report(folded, path, None)?;
    anyhow::ensure!(
        matches_profile(profile, &report),
        "The folded profile changed after it was opened. Reopen it before verifying the report."
    );
    Ok(Arc::new(report))
}

impl TerminalApp {
    pub(super) fn cpu_report_action(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        let panel = &mut self.flame_graph;
        if !panel.open || panel.visible_graph().is_none() || panel.inspection.is_some() {
            return;
        }
        let Some(loaded) = &mut panel.loaded else {
            return;
        };
        if let Action::Expanded(expanded) = action {
            if let Some(verified) = &mut loaded.verified_report {
                verified.expanded = expanded;
            }
            return;
        }
        if panel.pending.is_some() {
            return;
        }
        let Some(owner) = self.keyboard.hwnd().map(|window| window.0 as usize) else {
            return;
        };
        let report_picker = |save| file_picker::Purpose::CaptureReport {
            capture: loaded.path.clone(),
            kind: file_picker::CaptureReportKind::CpuProfile,
            save,
        };
        let (purpose, task, status) = match action {
            Action::Save => {
                let Some(report) = capture_report(loaded) else {
                    return;
                };
                (
                    report_picker(true),
                    Task::Save(report.clone()),
                    "Choosing where to save the CPU profile report…",
                )
            }
            Action::Verify => {
                loaded.verified_report = None;
                (
                    report_picker(false),
                    Task::Verify(loaded.source()),
                    "Choosing a CPU profile report to verify…",
                )
            }
            Action::VerifyRaw => {
                let Some(verified) = &mut loaded.verified_report else {
                    return;
                };
                if verified.report.raw_perf_data.is_none() {
                    return;
                }
                verified.raw_path = None;
                (
                    file_picker::Purpose::VerifyRawPerfData(verified.path.clone()),
                    Task::VerifyRaw(verified.report.clone()),
                    "Choosing a raw perf recording to verify…",
                )
            }
            Action::Expanded(_) => return,
        };
        let ticket = panel.begin(status);
        let rejected = message(ViewerAction::Finished(
            ticket.clone(),
            Err("CPU profile report worker unavailable".into()),
        ));
        ctx.spawn_background_with_rejection(
            move |scope| {
                let result = (|| -> anyhow::Result<_> {
                    let Some(file) = file_picker::pick(owner, purpose)? else {
                        return Ok(None);
                    };
                    if scope.is_cancelled() {
                        return Ok(None);
                    }
                    Ok(Some(ViewerOutcome::Report(task.run(file.path)?)))
                })()
                .map_err(|error| format!("{error:#}"));
                message(ViewerAction::Finished(ticket, result))
            },
            rejected,
        );
    }
}

pub(super) fn finish(panel: &mut Panel, result: Outcome) {
    let Some(loaded) = &mut panel.loaded else {
        return;
    };
    panel.status = Some(match result {
        Outcome::Saved => "Saved CPU profile report".into(),
        Outcome::Verified(verified) => {
            let bytes = verified.report.folded.bytes;
            loaded.verified_report = Some(verified);
            format!("Folded profile matches report · {bytes} bytes verified")
        }
        Outcome::RawVerified(path) => {
            if let Some(verified) = &mut loaded.verified_report {
                verified.raw_path = Some(path);
            }
            "Raw perf recording matches report".into()
        }
    });
}

pub(super) fn capture_report(loaded: &Loaded) -> Option<&Arc<CpuProfileCaptureReport>> {
    loaded.capture.as_ref()?.summary.capture_report.as_ref()
}

fn send(action: Action) -> Message {
    message(ViewerAction::Report(action))
}

pub(super) fn fields(
    loaded: &Loaded,
    idle: bool,
    ctx: &ViewContext<TerminalApp>,
    fields: &mut Vec<KeyedView>,
) {
    if capture_report(loaded).is_some() {
        fields.push(KeyedView::new(
            "save-capture-report",
            Button::new()
                .is_enabled(idle)
                .on_click(ctx.callback(|_| send(Action::Save)))
                .content("Save capture report…"),
        ));
    }
    fields.push(KeyedView::new(
        "verify-capture-report",
        Button::new()
            .is_enabled(idle)
            .on_click(ctx.callback(|_| send(Action::Verify)))
            .content("Verify capture report…"),
    ));
    if let Some(verified) = &loaded.verified_report {
        let mut content = vec![KeyedView::new(
            "repeat-from-report",
            Button::new()
                .is_enabled(idle)
                .on_click(
                    ctx.callback(|_| message(ViewerAction::Capture(capture::Action::RepeatReport))),
                )
                .content("Repeat from report…"),
        )];
        if verified.report.raw_perf_data.is_some() {
            content.push(KeyedView::new(
                "verify-raw",
                Button::new()
                    .is_enabled(idle)
                    .on_click(ctx.callback(|_| send(Action::VerifyRaw)))
                    .content("Verify raw perf recording…"),
            ));
        }
        content.push(KeyedView::new(
            "details",
            ScrollViewer::new().max_height(176.0).content(
                StackPanel::new()
                    .spacing(4.0)
                    .keyed_children(verified.details()),
            ),
        ));
        fields.push(KeyedView::new(
            "capture-report",
            Expander::new()
                .automation_name("CPU profile capture report")
                .is_expanded(verified.expanded)
                .on_is_expanded_changed(ctx.callback(|expanded| send(Action::Expanded(expanded))))
                .slots([
                    SlotView::new(
                        ExpanderSlot::Header,
                        TextBlock::new().text("Capture report"),
                    ),
                    SlotView::new(
                        ExpanderSlot::Content,
                        StackPanel::new().spacing(6.0).keyed_children(content),
                    ),
                ]),
        ));
    }
}

impl Verified {
    fn details(&self) -> Vec<KeyedView> {
        let report = &self.report;
        let mut fields = vec![text(
            "report",
            format!("Report · {}", display_path(&self.path)),
        )];
        fields.extend(capture_fields(report));
        fields.extend([
            text(
                "totals",
                format!(
                    "{} · {} distinct stacks · {} unresolved frames",
                    super::sample_count(report.samples),
                    report.distinct_stacks,
                    report.unresolved_frames
                ),
            ),
            text(
                "folded-hash",
                format!("Folded SHA-256 {}", report.folded.sha256),
            ),
        ]);
        if let Some(raw) = &report.raw_perf_data {
            fields.push(text(
                "raw",
                if let Some(path) = &self.raw_path {
                    format!("Verified raw recording · {}", display_path(path))
                } else {
                    format!("Recorded raw file · {}", preview(&raw.file_name, 512))
                },
            ));
            fields.push(text(
                "raw-hash",
                format!("Raw SHA-256 {}", preview(&raw.sha256, 64)),
            ));
        } else if report.reviewed.raw_perf_data_requested {
            fields.push(text("raw", "Raw recording was requested"));
        }
        fields
    }
}

pub(super) struct Sampling<'a> {
    pub seconds: u8,
    pub frequency: u8,
    pub call_graph: CpuCallGraph,
    pub event: &'a str,
}

impl<'a> Sampling<'a> {
    pub(super) fn of(report: &'a CpuProfileCaptureReport) -> Self {
        let (seconds, frequency, call_graph) = match &report.reviewed.target {
            CpuProfileReportTarget::Process { options, .. } => {
                (options.seconds, options.frequency, options.call_graph)
            }
            CpuProfileReportTarget::Command { options, .. } => {
                (options.seconds, options.frequency, options.call_graph)
            }
        };
        Self {
            seconds,
            frequency,
            call_graph,
            event: &report.reviewed.event,
        }
    }

    fn label(&self) -> String {
        let unwind = match self.call_graph {
            CpuCallGraph::Fp => "Frame pointers",
            CpuCallGraph::Dwarf => "DWARF",
        };
        format!(
            "{} seconds · {} Hz · {unwind} · {}",
            self.seconds,
            self.frequency,
            preview(self.event, 64),
        )
    }
}

pub(super) fn capture_fields(report: &CpuProfileCaptureReport) -> Vec<KeyedView> {
    let reviewed = &report.reviewed;
    let mut fields = vec![
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
    ];
    match &reviewed.target {
        CpuProfileReportTarget::Process {
            process_name,
            process_start_ticks,
            options,
        } => {
            fields.push(text(
                "process",
                format!(
                    "PID {} · {} · start ticks {process_start_ticks}",
                    options.target_pid,
                    preview(process_name, 256)
                ),
            ));
        }
        CpuProfileReportTarget::Command {
            program,
            directory,
            options,
        } => {
            let arguments = serde_json::to_string(&options.arguments).unwrap_or_default();
            fields.extend([
                text(
                    "executable",
                    format!("Executable {}", preview(program, 512)),
                ),
                text(
                    "directory",
                    format!("Working directory {}", preview(directory, 512)),
                ),
                text(
                    "arguments",
                    format!("Arguments {}", preview(&arguments, 4096)),
                ),
            ]);
        }
    }
    fields.push(text("sampling", Sampling::of(report).label()));
    if let Some(workload) = &report.workload {
        fields.push(text("outcome", workload.to_string()));
    } else if report.stopped_by_user {
        fields.push(text("outcome", "Stopped by request"));
    }
    fields
}

fn display_path(path: &Path) -> String {
    preview(&path.to_string_lossy(), 512)
}

fn preview(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let mut shown = diagnostics::safe_text(&chars.by_ref().take(limit).collect::<String>());
    if chars.next().is_some() {
        shown.push('…');
    }
    shown
}
