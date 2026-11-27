use super::*;

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

    pub(super) fn diagnostics_action(&mut self, action: Action) {
        match action {
            Action::Open => {
                self.command_reference.close();
                self.set_diagnostics_drawer(Some(Drawer::Overview));
            }
            Action::Close => self.diagnostics_open = false,
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
                            .chain(rows),
                        ),
                    ),
                ),
            ))
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
