use super::*;
use serde::Serialize;
use std::{
    collections::HashMap,
    io::{self, BufWriter, Write},
};

#[derive(Serialize)]
struct HistoryExport {
    schema_version: u8,
    query: String,
    status: CommandStatus,
    order: &'static str,
    include_output: bool,
    expired_results: usize,
    commands: Vec<Capture>,
}

#[derive(Serialize)]
struct Capture {
    pane: Pane,
    id: u64,
    connection_generation: u64,
    command: Option<String>,
    reported_directory: Option<String>,
    exit_code: Option<i32>,
    started_unix_ms: Option<u64>,
    finished_unix_ms: u64,
    duration_ms: Option<u64>,
    output_available: bool,
    matched_output: bool,
}

#[derive(Clone, Serialize)]
struct Pane {
    id: u64,
    label: String,
    profile: Option<String>,
    ssh: Option<Endpoint>,
}

#[derive(Clone, Serialize)]
struct Endpoint {
    user: String,
    host: String,
    port: u16,
}

impl HistoryExport {
    fn capture(app: &TerminalApp, picker: &Picker) -> Self {
        let options = picker.window_history.options;
        let mut result = Self {
            schema_version: 1,
            query: picker.query.clone(),
            status: options.status,
            order: match options.order {
                HistoryOrder::Newest => "newest",
                HistoryOrder::Slowest => "slowest",
            },
            include_output: options.include_output,
            expired_results: 0,
            commands: Vec::with_capacity(picker.commands.len()),
        };
        let mut panes = HashMap::new();
        for row in &picker.commands {
            let command_palette::CommandId::OpenWindowCommand(pane, link, hit) = &row.id else {
                continue;
            };
            let pane = panes
                .entry(*pane)
                .or_insert_with(|| pane_snapshot(app, *pane));
            let Some((pane, frame)) = pane else {
                result.expired_results += 1;
                continue;
            };
            let Some(command) = frame.command_for_link(*link) else {
                result.expired_results += 1;
                continue;
            };
            result.commands.push(Capture {
                pane: pane.clone(),
                id: command.id,
                connection_generation: link.generation,
                command: command.command.clone(),
                reported_directory: command.reported_directory.as_deref().map(str::to_owned),
                exit_code: command.exit_code,
                started_unix_ms: command.timing.started_unix_ms,
                finished_unix_ms: command.timing.finished_unix_ms,
                duration_ms: command.timing.duration_ms,
                output_available: command.output.is_ok(),
                matched_output: hit.is_some(),
            });
        }
        result
    }

    fn write_json(&self, file: &mut std::fs::File) -> io::Result<()> {
        let mut output = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut output, self)?;
        output.write_all(b"\n")?;
        output.flush()
    }
}

fn pane_snapshot(app: &TerminalApp, pane: u64) -> Option<(Pane, Arc<Snapshot>)> {
    let tab = app.tabs.get(app.tab_for_pane(pane)?)?;
    let Page::Terminal(live) = &tab.page else {
        return None;
    };
    let ssh = match &live.source {
        Source::Ssh(ssh) => Some(Endpoint {
            user: ssh.user.clone(),
            host: ssh.host.clone(),
            port: ssh.port,
        }),
        _ => None,
    };
    Some((
        Pane {
            id: pane,
            label: tab
                .pane_name()
                .map_or_else(|| tab.label.clone(), |title| title.as_str().to_owned()),
            profile: tab.profile_name().map(str::to_owned),
            ssh,
        },
        live.session.frames.load(),
    ))
}

impl Picker {
    pub(in crate::app) fn history_export_button(
        &self,
        app: &TerminalApp,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let launch = self.launch.clone();
        Button::new()
            .style(ButtonStyle::Subtle)
            .horizontal_alignment(HorizontalAlignment::Right)
            .is_enabled(app.export.pending.is_none() && !self.commands.is_empty())
            .on_click(ctx.callback(move |_| Message::WindowHistory(Action::Save(launch.clone()))))
            .content("Save results as JSON…")
            .tooltip_with(title_tools::below("Save history results · Ctrl+S"))
    }
}

impl TerminalApp {
    pub(in crate::app) fn save_window_history(&mut self, ctx: &ComponentContext<Self>) {
        if self.export.pending.is_some() {
            return;
        }
        let Some(picker) = self.picker.as_ref().filter(|picker| {
            picker.mode == Mode::Commands(command_palette::Search::CommandHistory)
                && !picker.commands.is_empty()
        }) else {
            return;
        };
        let snapshot = HistoryExport::capture(self, picker);
        self.begin_text_export(
            text_export::Subject::WindowCommandHistory,
            move |file| snapshot.write_json(file),
            ctx,
        );
    }
}
