use super::*;
use std::collections::VecDeque;
use w11term::diagnostic_history::{Artifact, Kind, Store};

pub(super) const LABEL: &str = "Diagnostics overview";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Drawer {
    Overview,
    Triage,
    FlameGraph,
    PacketCapture,
}

pub enum Action {
    Open,
    Close,
    OpenRecent(Arc<Artifact>),
    ForgetRecent(Arc<Artifact>),
    RecentReady(Result<Vec<Artifact>, String>),
}

enum Edit {
    Load,
    Remember(Artifact),
    Forget(Arc<Artifact>),
}

#[derive(Default)]
pub(super) struct Recent {
    entries: Vec<Arc<Artifact>>,
    edits: VecDeque<Edit>,
    pending: bool,
    error: Option<String>,
}

pub(super) struct Activity {
    endpoint: String,
    state: ActivityState,
}

enum ActivityState {
    Running {
        ticket: Arc<()>,
        stopping: bool,
        progress: Option<String>,
    },
    Finished(String),
}

impl Activity {
    pub(super) fn start(ticket: &Arc<()>, user: &str, host: &str, port: u16) -> Self {
        Self {
            endpoint: ssh_endpoint(user, host, port),
            state: ActivityState::Running {
                ticket: ticket.clone(),
                stopping: false,
                progress: None,
            },
        }
    }

    pub(super) fn stopping(&mut self) {
        if let ActivityState::Running { stopping, .. } = &mut self.state {
            *stopping = true;
        }
    }

    pub(super) fn record_progress(&mut self, ticket: &Arc<()>, update: String) {
        if let ActivityState::Running {
            ticket: active,
            progress,
            ..
        } = &mut self.state
            && Arc::ptr_eq(active, ticket)
        {
            *progress = Some(update);
        }
    }

    pub(super) fn running_progress(&self) -> Option<&str> {
        match &self.state {
            ActivityState::Running { progress, .. } => progress.as_deref(),
            ActivityState::Finished(_) => None,
        }
    }

    pub(super) fn finish<T>(
        &mut self,
        ticket: &Arc<()>,
        result: &Result<Option<T>, String>,
        completed: impl FnOnce(&T) -> Option<String>,
    ) {
        if matches!(&self.state, ActivityState::Running { ticket: active, .. } if Arc::ptr_eq(active, ticket))
        {
            self.state = ActivityState::Finished(match result {
                Ok(Some(outcome)) => completed(outcome).unwrap_or_else(|| "Completed".into()),
                Ok(None) => "Cancelled".into(),
                Err(error) => format!("Failed · {error}"),
            });
        }
    }

    fn summary(&self) -> &str {
        match &self.state {
            ActivityState::Running {
                stopping: false,
                progress,
                ..
            } => progress.as_deref().unwrap_or("Running"),
            ActivityState::Running { stopping: true, .. } => {
                "Stop requested · saving collected data"
            }
            ActivityState::Finished(summary) => summary,
        }
    }
}

fn ssh_endpoint(user: &str, host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("{user}@[{host}]:{port}")
    } else {
        format!("{user}@{host}:{port}")
    }
}

pub(super) fn menu() -> MenuItem {
    MenuItem::submenu(
        "diagnostics",
        "Diagnostics",
        [
            MenuItem::item("diagnostics-overview", LABEL),
            MenuItem::separator("diagnostics-tools"),
            MenuItem::item("linux-triage", triage::LABEL),
            MenuItem::item("cpu-flame-graph", flame_graph::LABEL),
            MenuItem::item("packet-capture", packet_capture::LABEL),
            MenuItem::item("syscall-trace-viewer", syscall_view::LABEL),
        ],
    )
}

pub(super) fn header(
    title: &str,
    close_name: &str,
    close: Callback<()>,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    Grid::new()
        .columns([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
        .column_spacing(8.0)
        .children((
            Button::new()
                .automation_name("Return to diagnostics overview")
                .on_click(ctx.callback(|_| Message::Diagnostics(Action::Open)))
                .content("Back"),
            TextBlock::new()
                .grid_column(1)
                .vertical_alignment(VerticalAlignment::Center)
                .text(title)
                .font_size(18.0)
                .text_wrapping(TextWrapping::Wrap),
            Button::new()
                .grid_column(2)
                .automation_name(close_name)
                .on_click(close)
                .content("Close"),
        ))
}

impl TerminalApp {
    pub(super) fn set_diagnostics_drawer(&mut self, drawer: Option<Drawer>) {
        self.diagnostics_open = drawer == Some(Drawer::Overview);
        self.triage.open = drawer == Some(Drawer::Triage);
        self.flame_graph.open = drawer == Some(Drawer::FlameGraph);
        self.packet_capture.open = drawer == Some(Drawer::PacketCapture);
    }

    pub(super) fn diagnostics_action(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        match action {
            Action::Open => {
                self.command_reference.close();
                self.set_diagnostics_drawer(Some(Drawer::Overview));
                self.edit_diagnostics_recent(Edit::Load, ctx);
            }
            Action::Close => self.diagnostics_open = false,
            Action::OpenRecent(artifact)
                if self.diagnostics_open && self.diagnostics_recent.entries.contains(&artifact) =>
            {
                self.open_recent_diagnostic(artifact, ctx);
            }
            Action::ForgetRecent(artifact) if self.diagnostics_open => {
                self.edit_diagnostics_recent(Edit::Forget(artifact), ctx);
            }
            Action::RecentReady(result) if self.diagnostics_recent.pending => {
                let recent = &mut self.diagnostics_recent;
                recent.pending = false;
                match result {
                    Ok(entries) => {
                        recent.entries = entries.into_iter().map(Arc::new).collect();
                        recent.error = None;
                    }
                    Err(error) => recent.error = Some(error),
                }
                self.write_next_diagnostics_edit(ctx);
            }
            _ => (),
        }
    }

    pub(super) fn remember_diagnostic(
        &mut self,
        kind: Kind,
        paths: impl IntoIterator<Item = std::path::PathBuf>,
        ctx: &ComponentContext<Self>,
    ) {
        self.edit_diagnostics_recent(
            Edit::Remember(Artifact {
                kind,
                paths: paths.into_iter().collect(),
            }),
            ctx,
        );
    }

    fn edit_diagnostics_recent(&mut self, edit: Edit, ctx: &ComponentContext<Self>) {
        self.diagnostics_recent.edits.push_back(edit);
        self.write_next_diagnostics_edit(ctx);
    }

    fn write_next_diagnostics_edit(&mut self, ctx: &ComponentContext<Self>) {
        let recent = &mut self.diagnostics_recent;
        if recent.pending {
            return;
        }
        let Some(edit) = recent.edits.pop_front() else {
            return;
        };
        recent.pending = true;
        ctx.spawn_background_with_rejection(
            move |_| {
                let result = (|| {
                    let store = Store::user()?;
                    match edit {
                        Edit::Load => store.list(),
                        Edit::Remember(artifact) => store.remember(&artifact),
                        Edit::Forget(artifact) => store.forget(&artifact),
                    }
                })()
                .map_err(|error: anyhow::Error| format!("{error:#}"));
                Message::Diagnostics(Action::RecentReady(result))
            },
            Message::Diagnostics(Action::RecentReady(Err(
                "Recent diagnostics worker unavailable".into(),
            ))),
        );
    }

    fn open_recent_diagnostic(&mut self, artifact: Arc<Artifact>, ctx: &ComponentContext<Self>) {
        match (artifact.kind, artifact.paths.as_slice()) {
            (Kind::TriageReport, [path]) => {
                self.set_diagnostics_drawer(Some(Drawer::Triage));
                self.triage_action(triage::Action::OpenReport(Some(path.clone())), ctx);
            }
            (Kind::CpuProfile, [path]) => {
                self.set_diagnostics_drawer(Some(Drawer::FlameGraph));
                self.flame_graph_action(flame_graph::Action::OpenFile(Some(path.clone())), ctx);
            }
            (Kind::PacketCapture, [path]) => {
                self.set_diagnostics_drawer(Some(Drawer::PacketCapture));
                self.packet_capture_action(
                    packet_capture::Action::OpenCaptureFile(Some(path.clone())),
                    ctx,
                );
            }
            (Kind::SyscallTrace, paths) => {
                self.syscall_view_action(syscall_view::Action::OpenRecent(paths.to_vec()), ctx);
            }
            _ => (),
        }
    }

    pub(super) fn with_diagnostics(&self, content: View, ctx: &ViewContext<Self>) -> View {
        let width = (self.width * 0.38).clamp(270.0, 460.0);
        Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Pixel(width)])
            .keyed_children([
                KeyedView::new("terminal", Border::new().content(content)),
                KeyedView::new(
                    "diagnostics",
                    Border::new()
                        .grid_column(1)
                        .background(Color::rgb(35, 35, 35))
                        .content(self.diagnostics_view(ctx)),
                ),
            ])
    }

    fn diagnostics_view(&self, ctx: &ViewContext<Self>) -> View {
        let connection = self.triage_session().and_then(Session::ssh_connection);
        let destination = match connection {
            Some(connection) => {
                let options = connection.options();
                let endpoint = ssh_endpoint(&options.user, &options.host, options.port);
                format!("SSH pane · {endpoint}")
            }
            None => "Select a connected Linux SSH pane to run diagnostics.".into(),
        };
        let row = |label: &str, hint: &str, action: fn() -> Message| {
            KeyedView::new(
                label,
                StackPanel::new().spacing(3.0).children((
                    Button::new()
                        .on_click(ctx.callback(move |_| action()))
                        .content(label),
                    TextBlock::new()
                        .text(hint)
                        .text_wrapping(TextWrapping::Wrap),
                )),
            )
        };
        let rows = [
            row(
                triage::LABEL,
                "Collect or open a Linux performance report.",
                || Message::Triage(triage::Action::Open),
            ),
            row(
                flame_graph::LABEL,
                "Profile CPU activity or open folded stacks.",
                || Message::FlameGraph(flame_graph::Action::Open),
            ),
            row(
                packet_capture::LABEL,
                "Record packets and save a Wireshark capture.",
                || Message::PacketCapture(packet_capture::Action::Open),
            ),
            row(
                syscall_view::LABEL,
                "Record, inspect, or compare Linux syscall traces.",
                || Message::SyscallView(syscall_view::Action::Show),
            ),
        ];
        let activity = self.diagnostics_activity_rows(ctx);
        let recent = self.diagnostics_recent_rows(ctx);
        Grid::new()
            .rows([GridLength::Auto, GridLength::Star(1.0)])
            .children((
                Grid::new()
                    .columns([GridLength::Star(1.0), GridLength::Auto])
                    .column_spacing(8.0)
                    .margin(12.0)
                    .children((
                        TextBlock::new()
                            .automation_name("Diagnostics overview panel")
                            .text("Diagnostics")
                            .font_size(18.0),
                        Button::new()
                            .grid_column(1)
                            .automation_name("Close diagnostics overview")
                            .on_click(ctx.callback(|_| Message::Diagnostics(Action::Close)))
                            .content("Close"),
                    )),
                Border::new().grid_row(1).padding(12.0).content(
                    ScrollViewer::new().content(
                        StackPanel::new().spacing(12.0).keyed_children(
                            std::iter::once(KeyedView::new(
                                "target",
                                TextBlock::new()
                                    .text(triage::display_output(&destination))
                                    .text_wrapping(TextWrapping::Wrap),
                            ))
                            .chain(activity)
                            .chain(rows)
                            .chain(recent),
                        ),
                    ),
                ),
            ))
    }

    fn diagnostics_recent_rows(&self, ctx: &ViewContext<Self>) -> Vec<KeyedView> {
        let recent = &self.diagnostics_recent;
        if recent.entries.is_empty() && recent.error.is_none() {
            return Vec::new();
        }
        let mut rows = vec![KeyedView::new(
            "recent-diagnostics",
            TextBlock::new().text("Recent files").font_size(16.0),
        )];
        if let Some(error) = &recent.error {
            rows.push(KeyedView::new(
                "recent-error",
                TextBlock::new()
                    .text(triage::display_output(error))
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        rows.extend(recent.entries.iter().enumerate().map(|(index, artifact)| {
            let path = &artifact.paths[0];
            let filename = path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy();
            let filename = triage::display_output(&filename);
            let kind = match artifact.kind {
                Kind::TriageReport => "Linux triage report",
                Kind::CpuProfile => "Folded CPU profile",
                Kind::PacketCapture => "Packet capture",
                Kind::SyscallTrace => "Syscall trace",
            };
            let count = artifact.paths.len();
            let scope = if count > 1 {
                format!(" · {count} files")
            } else {
                String::new()
            };
            let open = artifact.clone();
            let forget = artifact.clone();
            KeyedView::new(
                format!("recent-{index}"),
                StackPanel::new().spacing(3.0).children((
                    Grid::new()
                        .columns([GridLength::Star(1.0), GridLength::Auto])
                        .column_spacing(6.0)
                        .children((
                            Button::new()
                                .automation_name(format!("Reopen {filename}"))
                                .horizontal_alignment(HorizontalAlignment::Stretch)
                                .on_click(ctx.callback(move |_| {
                                    Message::Diagnostics(Action::OpenRecent(open.clone()))
                                }))
                                .content(
                                    TextBlock::new()
                                        .text(&filename)
                                        .text_trimming(TextTrimming::CharacterEllipsis),
                                ),
                            Button::new()
                                .grid_column(1)
                                .automation_name(format!("Forget {filename}"))
                                .is_enabled(!recent.pending)
                                .on_click(ctx.callback(move |_| {
                                    Message::Diagnostics(Action::ForgetRecent(forget.clone()))
                                }))
                                .content("Forget"),
                        )),
                    TextBlock::new()
                        .text(triage::display_output(&format!(
                            "{kind}{scope} · {}",
                            path.display()
                        )))
                        .text_wrapping(TextWrapping::Wrap),
                )),
            )
        }));
        rows
    }

    fn diagnostics_activity_rows(&self, ctx: &ViewContext<Self>) -> Vec<KeyedView> {
        let row = |label, key, activity: &Activity, open: Callback<()>| {
            let item = KeyedView::new(
                key,
                StackPanel::new().spacing(3.0).children((
                    Button::new()
                        .automation_name(format!("Open {label} activity"))
                        .on_click(open)
                        .content(format!("{label} activity")),
                    TextBlock::new()
                        .text(triage::display_output(&activity.endpoint))
                        .text_wrapping(TextWrapping::Wrap),
                    TextBlock::new()
                        .text(triage::display_output(activity.summary()))
                        .text_wrapping(TextWrapping::Wrap),
                )),
            );
            (matches!(activity.state, ActivityState::Finished(_)), item)
        };
        let drawers: [(_, _, fn() -> Message); 3] = [
            (triage::LABEL, self.triage.activity.as_ref(), || {
                Message::Triage(triage::Action::Open)
            }),
            (
                flame_graph::LABEL,
                self.flame_graph.activity.as_ref(),
                || Message::FlameGraph(flame_graph::Action::Open),
            ),
            (
                packet_capture::LABEL,
                self.packet_capture.activity.as_ref(),
                || Message::PacketCapture(packet_capture::Action::Open),
            ),
        ];
        let mut rows: Vec<_> = drawers
            .into_iter()
            .filter_map(|(label, activity, open)| {
                Some(row(
                    label,
                    format!("activity-{label}"),
                    activity?,
                    ctx.callback(move |_| open()),
                ))
            })
            .collect();
        for tab in self.tabs.entries() {
            if let Page::SyscallTrace(page) = &tab.value.page
                && let Some(activity) = page.diagnostics_activity()
            {
                let id = tab.id;
                rows.push(row(
                    "Syscall capture",
                    format!("activity-syscall-{id}"),
                    activity,
                    ctx.callback(move |_| Message::SyscallView(syscall_view::Action::Resume(id))),
                ));
            }
        }
        if rows.is_empty() {
            return Vec::new();
        }
        rows.sort_by_key(|(finished, _)| *finished);
        std::iter::once(KeyedView::new(
            "activity-heading",
            TextBlock::new().text("Activity").font_size(16.0),
        ))
        .chain(rows.into_iter().map(|(_, row)| row))
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_retains_original_endpoint_and_ignores_unrelated_completion() {
        let ticket = Arc::new(());
        let mut activity = Activity::start(&ticket, "pilot", "2001:db8::1", 2222);
        assert_eq!(activity.endpoint, "pilot@[2001:db8::1]:2222");
        assert_eq!(activity.summary(), "Running");
        activity.record_progress(&Arc::new(()), "Unrelated".into());
        assert_eq!(activity.summary(), "Running");
        activity.record_progress(&ticket, "4 of 10 checks finished".into());
        assert_eq!(activity.summary(), "4 of 10 checks finished");
        activity.stopping();
        let saved = Ok(Some("Saved"));
        let summary = |outcome: &&str| Some((*outcome).into());
        activity.finish(&Arc::new(()), &saved, summary);
        assert_eq!(activity.summary(), "Stop requested · saving collected data");
        activity.finish(&ticket, &saved, summary);
        activity.stopping();
        activity.finish(&ticket, &Err("Late callback".into()), summary);
        activity.record_progress(&ticket, "Late progress".into());
        assert_eq!(activity.summary(), "Saved");
    }
}
