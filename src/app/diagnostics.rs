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
                let host = &options.host;
                if host.contains(':') && !host.starts_with('[') {
                    format!("SSH pane · {}@[{host}]:{}", options.user, options.port)
                } else {
                    format!("SSH pane · {}@{host}:{}", options.user, options.port)
                }
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
                            .chain(rows),
                        ),
                    ),
                ),
            ))
    }
}
