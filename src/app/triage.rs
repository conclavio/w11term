use super::*;
use std::num::NonZeroU32;
use std::path::PathBuf;
use w11term::ssh::{TriagePlan, TriageReport, TriageResource, TriageSampleStatus};

pub(super) const LABEL: &str = "Linux performance triage";
mod processes;
use flame_graph::Connection;

pub enum Action {
    Open,
    Close,
    Review,
    Run,
    OpenReport,
    SaveReport,
    Sample(usize),
    Resource(usize),
    Finding(usize),
    Profile(NonZeroU32),
    Finished(Arc<()>, Result<Option<Outcome>, String>),
}

pub enum Outcome {
    Report {
        report: Arc<TriageReport>,
        path: Option<PathBuf>,
        source: Option<Connection>,
    },
    Saved(PathBuf),
}

enum Content {
    Review { pane: u64, plan: Box<TriagePlan> },
    Report(Arc<TriageReport>),
}

#[derive(Default)]
pub(super) struct Panel {
    pub(super) open: bool,
    content: Option<Content>,
    sample: usize,
    resource: TriageResource,
    evidence_line: Option<usize>,
    status: Option<String>,
    pending: Option<Arc<()>>,
    path: Option<PathBuf>,
    profile_source: Option<Connection>,
    busy_processes: Vec<processes::Process>,
}

fn message(action: Action) -> Message {
    Message::Triage(action)
}

fn detail(key: impl Into<String>, text: impl Into<String>) -> KeyedView {
    KeyedView::new(
        key.into(),
        TextBlock::new()
            .text(text.into())
            .text_wrapping(TextWrapping::Wrap),
    )
}

fn endpoint(plan: &TriagePlan) -> String {
    format!(
        "{}@{}:{}\nHost key {}",
        plan.user, plan.host, plan.port, plan.host_key_fingerprint
    )
}

fn status_label(status: TriageSampleStatus) -> &'static str {
    match status {
        TriageSampleStatus::Ok => "OK",
        TriageSampleStatus::Missing => "Utility missing",
        TriageSampleStatus::TimedOut => "Timed out",
        TriageSampleStatus::Incomplete => "Incomplete",
        TriageSampleStatus::OutputLimit => "Output limit",
        TriageSampleStatus::PermissionDenied => "Permission denied",
        TriageSampleStatus::Failed => "Failed",
    }
}

fn display_output(text: &str) -> String {
    let mut display = String::with_capacity(text.len());
    for c in text.chars() {
        if (c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
            || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            display.extend(c.escape_default());
        } else {
            display.push(c);
        }
    }
    display
}

impl Panel {
    fn begin(&mut self, status: &str) -> Arc<()> {
        let ticket = Arc::new(());
        self.pending = Some(ticket.clone());
        self.status = Some(status.into());
        ticket
    }

    fn view(&self, active: Option<Connection>, ctx: &ViewContext<TerminalApp>) -> View {
        let idle = self.pending.is_none();
        let send = |action: fn() -> Action| ctx.callback(move |_| message(action()));
        let button = |label: &str, enabled, action: fn() -> Action| {
            Button::new()
                .is_enabled(enabled)
                .on_click(send(action))
                .content(label)
        };
        let mut fields = vec![
            KeyedView::new(
                "files",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        button("Open report", idle, || Action::OpenReport),
                        button(
                            "Save report",
                            idle && matches!(self.content, Some(Content::Report(_))),
                            || Action::SaveReport,
                        ),
                    )),
            ),
            KeyedView::new(
                "review",
                button("Review selected SSH pane", idle && active.is_some(), || {
                    Action::Review
                }),
            ),
        ];
        if let Some(status) = &self.status {
            fields.push(detail("status", display_output(status)));
        }
        match &self.content {
            Some(Content::Review { pane, plan }) => {
                let ready = active
                    == Some(Connection {
                        pane: *pane,
                        generation: plan.connection_generation,
                    });
                fields.extend([
                    detail("endpoint", endpoint(plan)),
                    detail(
                        "bounds",
                        format!(
                            "{}-second deadline · {} KiB per output stream",
                            plan.deadline_seconds,
                            plan.max_bytes_per_stream / 1024
                        ),
                    ),
                    KeyedView::new("run", button("Run triage", idle && ready, || Action::Run)),
                    detail(
                        "commands",
                        plan.probes
                            .iter()
                            .map(|probe| probe.command.as_str())
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                ]);
                if !ready {
                    fields.push(detail(
                        "stale",
                        "Return to the reviewed SSH connection or review the selected pane.",
                    ));
                }
            }
            Some(Content::Report(report)) => self.report_view(report, active, &mut fields, ctx),
            None if self.status.is_none() => {
                fields.push(detail(
                    "empty",
                    "Select a connected Linux SSH pane or open a saved report.",
                ));
            }
            None => (),
        }
        Grid::new()
            .rows([GridLength::Auto, GridLength::Star(1.0)])
            .keyed_children([
                KeyedView::new(
                    "heading",
                    Grid::new()
                        .columns([GridLength::Star(1.0), GridLength::Auto])
                        .column_spacing(8.0)
                        .margin(12.0)
                        .children((
                            TextBlock::new()
                                .text(LABEL)
                                .font_size(18.0)
                                .text_wrapping(TextWrapping::Wrap),
                            Button::new()
                                .grid_column(1)
                                .automation_name("Close performance triage")
                                .on_click(send(|| Action::Close))
                                .content("Close"),
                        )),
                ),
                KeyedView::new(
                    "body",
                    Border::new().grid_row(1).padding(12.0).content(
                        ScrollViewer::new()
                            .content(StackPanel::new().spacing(8.0).keyed_children(fields)),
                    ),
                ),
            ])
    }

    fn report_view(
        &self,
        report: &TriageReport,
        active: Option<Connection>,
        fields: &mut Vec<KeyedView>,
        ctx: &ViewContext<TerminalApp>,
    ) {
        let collection = &report.collection;
        let ok = collection
            .samples
            .iter()
            .filter(|sample| {
                sample.status == TriageSampleStatus::Ok
                    && !sample.stdout.at_byte_limit
                    && !sample.stderr.at_byte_limit
            })
            .count();
        fields.extend([
            detail("report-endpoint", display_output(&endpoint(&report.target))),
            detail(
                "report-summary",
                format!("{ok} of {} checks complete", collection.samples.len()),
            ),
        ]);
        if let Some(elapsed) = collection
            .finished_unix_ms
            .checked_sub(collection.started_unix_ms)
        {
            fields.push(detail(
                "elapsed",
                format!("Collected in {:.1} seconds", elapsed as f64 / 1000.0),
            ));
        }
        self.findings_view(report, fields, ctx);
        self.profile_buttons(active, fields, ctx);
        fields.push(KeyedView::new(
            "samples",
            ComboBox::new()
                .automation_name("Triage samples")
                .items_source(
                    collection
                        .samples
                        .iter()
                        .map(|sample| format!("{} · {}", sample.id, status_label(sample.status))),
                )
                .selected_index(self.sample)
                .on_selection_changed(ctx.callback(|index: Option<usize>| {
                    index.map_or(Message::ControlIdle, |index| message(Action::Sample(index)))
                })),
        ));
        let Some((probe, sample)) = report
            .target
            .probes
            .get(self.sample)
            .zip(collection.samples.get(self.sample))
        else {
            return;
        };
        fields.push(detail("command", display_output(&probe.command)));
        fields.push(detail("sampling", display_output(&probe.sampling)));
        fields.push(detail(
            "sample-status",
            sample.exit_status.map_or_else(
                || status_label(sample.status).into(),
                |exit| format!("{} · Exit {exit}", status_label(sample.status)),
            ),
        ));
        if let Some((line, text)) = self.evidence_line.and_then(|line| {
            sample
                .stdout
                .text
                .lines()
                .nth(line.checked_sub(1)?)
                .map(|text| (line, text))
        }) {
            fields.push(detail(
                "evidence-line",
                format!("Evidence · {} output line {line}", sample.id),
            ));
            fields.push(KeyedView::new(
                "evidence",
                TextBlock::new()
                    .text(display_output(text.trim()))
                    .text_wrapping(TextWrapping::Wrap)
                    .is_text_selection_enabled(true),
            ));
        }
        for (key, label, output) in [
            ("stdout", "Output", &sample.stdout),
            ("stderr", "Standard error", &sample.stderr),
        ] {
            if !output.text.is_empty() {
                fields.push(detail(
                    format!("{key}-label"),
                    if output.at_byte_limit {
                        format!("{label} · Byte limit reached")
                    } else {
                        label.into()
                    },
                ));
                fields.push(KeyedView::new(
                    key,
                    TextBlock::new()
                        .text(display_output(&output.text))
                        .text_wrapping(TextWrapping::Wrap)
                        .is_text_selection_enabled(true),
                ));
            }
        }
    }

    fn profile_buttons(
        &self,
        active: Option<Connection>,
        fields: &mut Vec<KeyedView>,
        ctx: &ViewContext<TerminalApp>,
    ) {
        if self.resource != TriageResource::Cpu || self.busy_processes.is_empty() {
            return;
        }
        let ready = active.is_some() && active == self.profile_source && self.pending.is_none();
        fields.push(detail("busy-processes", "Busy processes during triage"));
        for process in &self.busy_processes {
            let pid = process.pid;
            let label = display_output(&format!(
                "{} · PID {pid} · {:.1}% peak CPU",
                process.command, process.peak_cpu
            ));
            fields.push(KeyedView::new(
                format!("profile-pid-{pid}"),
                Button::new()
                    .automation_name(format!("Profile {label}"))
                    .is_enabled(ready)
                    .on_click(ctx.callback(move |_| message(Action::Profile(pid))))
                    .content(
                        TextBlock::new()
                            .text(label)
                            .text_wrapping(TextWrapping::Wrap),
                    ),
            ));
        }
    }

    fn findings_view(
        &self,
        report: &TriageReport,
        fields: &mut Vec<KeyedView>,
        ctx: &ViewContext<TerminalApp>,
    ) {
        let selected = TriageResource::ALL
            .iter()
            .position(|resource| *resource == self.resource);
        fields.push(detail("findings-label", "Findings by resource"));
        fields.push(KeyedView::new(
            "findings-resources",
            ComboBox::new()
                .automation_name("Triage resources")
                .items_source(TriageResource::ALL.map(|resource| resource.label()))
                .selected_index(selected)
                .on_selection_changed(ctx.callback(|index: Option<usize>| {
                    index.map_or(Message::ControlIdle, |index| {
                        message(Action::Resource(index))
                    })
                })),
        ));
        let findings = report
            .findings
            .iter()
            .enumerate()
            .filter(|(_, finding)| finding.resource == self.resource);
        let mut has_findings = false;
        for (index, finding) in findings {
            has_findings = true;
            fields.push(KeyedView::new(
                format!("finding-{index}"),
                Button::new()
                    .automation_name(format!("Show evidence for {}", finding.label))
                    .on_click(ctx.callback(move |_| message(Action::Finding(index))))
                    .content(
                        TextBlock::new()
                            .text(display_output(&format!(
                                "{} · {}",
                                finding.label, finding.value
                            )))
                            .text_wrapping(TextWrapping::Wrap),
                    ),
            ));
        }
        if !has_findings {
            fields.push(detail(
                "no-findings",
                "No recognized measurements for this resource.",
            ));
        }
    }
}

impl TerminalApp {
    pub(super) fn triage_session(&self) -> Option<&Session> {
        let Page::Terminal(live) = &self.tabs.active()?.page else {
            return None;
        };
        Some(&live.session)
    }

    pub(super) fn with_triage(&self, content: View, ctx: &ViewContext<Self>) -> View {
        let active = self.profile_connection();
        let width = (self.width * 0.4).clamp(265.0, 520.0);
        Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Pixel(width)])
            .keyed_children([
                KeyedView::new("terminal", Border::new().content(content)),
                KeyedView::new(
                    "triage",
                    Border::new()
                        .grid_column(1)
                        .background(Color::rgb(35, 35, 35))
                        .content(self.triage.view(active, ctx)),
                ),
            ])
    }

    pub(super) fn triage_action(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        if let Action::Profile(pid) = action {
            if let Some(source) = self.triage.profile_source
                && self.triage.pending.is_none()
                && self
                    .triage
                    .busy_processes
                    .iter()
                    .any(|process| process.pid == pid)
            {
                self.show_triage_process_profile(source, pid);
            }
            return;
        }
        if matches!(action, Action::Open) {
            self.command_reference.close();
            self.flame_graph.open = false;
            self.triage.open = true;
            if self.triage.content.is_some() || self.triage.pending.is_some() {
                return;
            }
        }
        if matches!(action, Action::Open | Action::Review) {
            if self.triage.pending.is_some() {
                return;
            }
            let target = self
                .triage_session()
                .map(|session| (session.pane_id, session.triage_target()));
            match target {
                Some((pane, Ok(target))) => {
                    self.triage.content = Some(Content::Review {
                        pane,
                        plan: Box::new(target.preview()),
                    });
                    self.triage.status = None;
                }
                Some((_, Err(error))) if matches!(action, Action::Review) => {
                    self.triage.status = Some(format!("{error:#}"));
                }
                _ => (),
            }
            return;
        }
        if matches!(action, Action::Run) {
            let Some(Content::Review { pane, plan }) = &self.triage.content else {
                return;
            };
            let Some(session) = self
                .triage_session()
                .filter(|session| session.pane_id == *pane)
            else {
                return;
            };
            let Ok(target) = session.triage_target() else {
                return;
            };
            if self.triage.pending.is_some()
                || target.connection_generation() != plan.connection_generation
            {
                return;
            }
            let source = Connection {
                pane: *pane,
                generation: plan.connection_generation,
            };
            let ticket = self.triage.begin("Collecting Linux performance samples…");
            let rejected = message(Action::Finished(
                ticket.clone(),
                Err("Triage worker unavailable".into()),
            ));
            ctx.spawn_background_with_rejection(
                move |_| {
                    let result = target
                        .collect_blocking()
                        .map(|report| {
                            Some(Outcome::Report {
                                report: Arc::new(report),
                                path: None,
                                source: Some(source),
                            })
                        })
                        .map_err(|error| format!("{error:#}"));
                    message(Action::Finished(ticket, result))
                },
                rejected,
            );
            return;
        }
        let panel = &mut self.triage;
        match action {
            Action::Close => panel.open = false,
            Action::Sample(index) => {
                if let Some(Content::Report(report)) = &panel.content
                    && index < report.collection.samples.len()
                    && index != panel.sample
                {
                    panel.sample = index;
                    panel.evidence_line = None;
                }
            }
            Action::Resource(index) => {
                if let Some(resource) = TriageResource::ALL.get(index) {
                    panel.resource = *resource;
                }
            }
            Action::Finding(index) => {
                if let Some(Content::Report(report)) = &panel.content
                    && let Some(finding) = report.findings.get(index)
                    && let Some(sample) = report
                        .collection
                        .samples
                        .iter()
                        .position(|sample| sample.id == finding.sample_id)
                {
                    panel.sample = sample;
                    panel.evidence_line = finding.stdout_line;
                }
            }
            Action::OpenReport | Action::SaveReport if panel.pending.is_none() => {
                let Some(owner) = self.keyboard.hwnd().map(|window| window.0 as usize) else {
                    return;
                };
                let report = if matches!(action, Action::SaveReport) {
                    let Some(Content::Report(report)) = &panel.content else {
                        return;
                    };
                    Some(report.clone())
                } else {
                    None
                };
                let path = panel
                    .path
                    .clone()
                    .unwrap_or_else(|| self.store.path().into());
                let purpose = if report.is_some() {
                    file_picker::Purpose::SaveTriageReport(path)
                } else {
                    file_picker::Purpose::OpenTriageReport(path)
                };
                let ticket = panel.begin("Choosing a triage report file…");
                let rejected = message(Action::Finished(
                    ticket.clone(),
                    Err("Triage file picker unavailable".into()),
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
                            Ok(Some(match report {
                                Some(report) => {
                                    report.save(&file.path)?;
                                    Outcome::Saved(file.path)
                                }
                                None => Outcome::Report {
                                    report: Arc::new(TriageReport::load(&file.path)?),
                                    path: Some(file.path),
                                    source: None,
                                },
                            }))
                        })()
                        .map_err(|error| format!("{error:#}"));
                        message(Action::Finished(ticket, result))
                    },
                    rejected,
                );
            }
            Action::Finished(ticket, result)
                if panel
                    .pending
                    .take_if(|pending| Arc::ptr_eq(pending, &ticket))
                    .is_some() =>
            {
                panel.status = None;
                match result {
                    Ok(Some(Outcome::Report {
                        report,
                        path,
                        source,
                    })) => {
                        if let Some(path) = path {
                            panel.status = Some("Opened Linux triage report".into());
                            panel.path = Some(path);
                        }
                        panel.busy_processes =
                            source.map_or_else(Vec::new, |_| processes::busy_processes(&report));
                        panel.profile_source = source;
                        panel.content = Some(Content::Report(report));
                        panel.sample = 0;
                        panel.resource = TriageResource::default();
                        panel.evidence_line = None;
                    }
                    Ok(Some(Outcome::Saved(path))) => {
                        panel.path = Some(path);
                        panel.status = Some("Saved Linux triage report".into());
                    }
                    Ok(None) => (),
                    Err(error) => panel.status = Some(error),
                }
            }
            _ => (),
        }
    }
}
