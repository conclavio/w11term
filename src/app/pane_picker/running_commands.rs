use super::*;
use command_palette::{CommandId, Row, Search};
use tokio::sync::oneshot::{self, error::TryRecvError};
use w11term::{
    events::RunningCommandTarget,
    session::{RunningCommandOutput, RunningCommandSample},
};

#[derive(Default)]
pub(super) struct OutputPreview {
    target: Option<(u64, RunningCommandTarget)>,
    pending: Option<oneshot::Receiver<Result<RunningCommandSample, &'static str>>>,
    output: Option<RunningCommandOutput>,
}

impl OutputPreview {
    fn sample(
        &mut self,
        target: (u64, RunningCommandTarget),
        input: &tokio::sync::mpsc::UnboundedSender<Command>,
        tick: bool,
    ) {
        if self.target != Some(target) {
            *self = Self {
                target: Some(target),
                ..Self::default()
            };
        } else if !tick {
            return;
        }
        if let Some(pending) = &mut self.pending {
            match pending.try_recv() {
                Ok(Ok(RunningCommandSample {
                    output: RunningCommandOutput::Updating,
                    ..
                })) if matches!(self.output, Some(RunningCommandOutput::Text { .. })) => {}
                Ok(Ok(reply)) => self.output = Some(reply.output),
                Ok(Err(_)) | Err(TryRecvError::Closed) => {
                    self.output = Some(RunningCommandOutput::Unavailable);
                }
                Err(TryRecvError::Empty) => return,
            }
        }
        let (reply, receiver) = oneshot::channel();
        if input
            .send(Command::ReadRunningOutput(target.1.start_token, reply))
            .is_ok()
        {
            self.pending = Some(receiver);
        } else {
            self.pending = None;
            self.output = Some(RunningCommandOutput::Unavailable);
        }
    }

    fn view(&self) -> View {
        use RunningCommandOutput::*;
        let status = match &self.output {
            None => "Reading command output…",
            Some(Unavailable) => "Command output is unavailable.",
            Some(Updating) => "Waiting for a synchronized update…",
            Some(AlternateScreen) => "This command is using the alternate screen.",
            Some(Text {
                text,
                earlier_omitted: true,
            }) if text.is_empty() => "Earlier command output is outside the preview.",
            Some(Text { text, .. }) if text.is_empty() => "No command output yet.",
            Some(Text {
                text,
                earlier_omitted,
            }) => {
                let heading = if *earlier_omitted {
                    "Recent command output · Earlier rows omitted"
                } else {
                    "Recent command output"
                };
                return StackPanel::new().spacing(4.0).children((
                    TextBlock::new().text(heading),
                    ScrollViewer::new()
                        .max_height(88.0)
                        .horizontal_scroll_bar_visibility(ScrollBarVisibility::Auto)
                        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                        .content(
                            TextBlock::new()
                                .automation_id("recent-command-output")
                                .text(text)
                                .text_wrapping(TextWrapping::NoWrap)
                                .is_text_selection_enabled(true),
                        ),
                ));
            }
        };
        TextBlock::new().text(status).into()
    }
}

impl Picker {
    pub(super) fn selected_running_command(&self) -> Option<(u64, RunningCommandTarget)> {
        if self.mode != Mode::Commands(Search::RunningCommands) {
            return None;
        }
        match self.commands.get(self.selected)?.id {
            CommandId::FocusRunningCommand(pane, target) => Some((pane, target)),
            _ => None,
        }
    }

    fn running_command_watch_action(
        &self,
        app: &TerminalApp,
    ) -> Option<(u64, RunningCommandTarget, bool)> {
        let (pane, target) = self.selected_running_command()?;
        let (_, session) = app.running_command_session(pane, target)?;
        let watching = session.events.running_command_watch(target.generation)?;
        Some((pane, target, !watching))
    }

    pub(in crate::app) fn running_command_watch_button(
        &self,
        app: &TerminalApp,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let Some((pane, target, arm)) = self.running_command_watch_action(app) else {
            return View::empty();
        };
        let launch = Arc::clone(&self.launch);
        let label = if arm {
            "Notify when selected command finishes"
        } else {
            "Cancel selected command notification"
        };
        Button::new()
            .automation_name(label)
            .on_click(ctx.callback(move |_| {
                Message::WatchRunningCommand(Arc::clone(&launch), pane, target, arm)
            }))
            .content(format!("{label}  (F8)"))
    }

    pub(in crate::app) fn running_output_open(&self) -> bool {
        self.running_output.is_some()
    }

    pub(in crate::app) fn running_output_view(
        &self,
        app: &TerminalApp,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let target = self
            .selected_running_command()
            .filter(|&(pane, capture)| app.running_command_session(pane, capture).is_some());
        let output = self.running_output.as_ref();
        if target.is_none() && output.is_none() {
            return View::empty();
        }
        let label = if output.is_some() {
            "Hide recent command output"
        } else {
            "Show recent command output"
        };
        StackPanel::new().spacing(4.0).children((
            Button::new()
                .automation_name(label)
                .on_click(ctx.callback(|_| Message::Shortcut(Shortcut::PickerRunningOutput)))
                .content(format!("{label}  (F9)")),
            output
                .filter(|output| target.is_some() && output.target == target)
                .map_or_else(View::empty, OutputPreview::view),
        ))
    }
}

impl TerminalApp {
    pub(in crate::app) fn open_running_commands(&mut self, ctx: &ComponentContext<Self>) {
        self.start_picker(Mode::Commands(Search::RunningCommands), ctx);
    }

    pub(in crate::app) fn refresh_running_commands(&mut self, launch: &Arc<()>) {
        if self.picker.as_ref().is_some_and(|picker| {
            picker.mode == Mode::Commands(Search::RunningCommands)
                && Arc::ptr_eq(&picker.launch, launch)
        }) {
            self.refresh_open_commands();
            self.refresh_running_command_output(true);
            self.poll_interrupt_completion();
        }
    }

    pub(in crate::app) fn toggle_running_command_output(&mut self) {
        if let Some(picker) = &mut self.picker
            && picker.mode == Mode::Commands(Search::RunningCommands)
        {
            picker.running_output = if picker.running_output.is_some() {
                None
            } else if picker.selected_running_command().is_some() {
                Some(OutputPreview::default())
            } else {
                return;
            };
            self.refresh_running_command_output(false);
        }
    }

    pub(in crate::app) fn refresh_running_command_output(&mut self, tick: bool) {
        let Some(picker) = self.picker.as_ref().filter(|p| p.running_output_open()) else {
            return;
        };
        let target = picker.selected_running_command();
        let request = target.and_then(|(pane, capture)| {
            let (_, session) = self.running_command_session(pane, capture)?;
            Some(((pane, capture), session.input.clone()))
        });
        let output = self
            .picker
            .as_mut()
            .unwrap()
            .running_output
            .as_mut()
            .unwrap();
        if let Some((target, input)) = request {
            output.sample(target, &input, tick);
        } else {
            *output = OutputPreview::default();
        }
    }

    pub(super) fn running_command_rows(&self, query: &str) -> Vec<Row> {
        let needle = query.trim().to_lowercase();
        let mut rows = Vec::new();
        for tab in self.tabs.entries().iter().map(|entry| &entry.value) {
            let Page::Terminal(live) = &tab.page else {
                continue;
            };
            let Some((generation, running)) = live.session.running_command_snapshot() else {
                continue;
            };
            let pane = live.session.pane_id;
            let name = tab
                .pane_name()
                .map_or(tab.label.as_str(), |name| name.as_str());
            let source = match &live.source {
                Source::Ssh(ssh) => format!(
                    "{name} · {}@{}:{} · Pane {pane}",
                    ssh.user, ssh.host, ssh.port
                ),
                _ => format!("{name} · Pane {pane}"),
            };
            let matches = needle.is_empty()
                || source.to_lowercase().contains(&needle)
                || tab
                    .profile_name()
                    .is_some_and(|profile| profile.to_lowercase().contains(&needle))
                || running
                    .command
                    .as_deref()
                    .is_some_and(|command| command.to_lowercase().contains(&needle))
                || running
                    .reported_directory
                    .as_deref()
                    .and_then(|directory| {
                        directory_split::Directory::reported(&live.source, directory)
                    })
                    .is_some_and(|directory| directory.display().to_lowercase().contains(&needle));
            if !matches {
                continue;
            }
            let started = running.started_at();
            let command = running.command.as_deref().map_or_else(
                || "Command line unavailable".into(),
                inspector::target_label,
            );
            rows.push((
                (started, pane),
                Row {
                    id: CommandId::FocusRunningCommand(
                        pane,
                        RunningCommandTarget::new(generation, &running),
                    ),
                    label: format!("{command} · {source}"),
                    chord: format!("{} s", running.elapsed_ms() / 1000),
                    enabled: true,
                },
            ));
        }
        rows.sort_unstable_by_key(|(order, _)| *order);
        rows.into_iter().map(|(_, row)| row).collect()
    }

    pub(super) fn running_command_session(
        &self,
        pane: u64,
        target: RunningCommandTarget,
    ) -> Option<(TabId, &Session)> {
        let id = self.tab_for_pane(pane)?;
        let Page::Terminal(live) = &self.tabs.get(id)?.page else {
            return None;
        };
        let (generation, running) = live.session.running_command_snapshot()?;
        (RunningCommandTarget::new(generation, &running) == target).then_some((id, &live.session))
    }

    pub(in crate::app) fn watch_selected_running_command(
        &self,
        launch: &Arc<()>,
        pane: u64,
        target: RunningCommandTarget,
        armed: bool,
    ) {
        if self.picker.as_ref().is_some_and(|picker| {
            Arc::ptr_eq(&picker.launch, launch)
                && picker.selected_running_command() == Some((pane, target))
        }) && let Some((_, session)) = self.running_command_session(pane, target)
        {
            let _ = session
                .input
                .send(Command::WatchRunningCommand(target, armed));
        }
    }

    pub(in crate::app) fn toggle_selected_running_command_watch(&self) {
        if let Some(picker) = &self.picker
            && let Some((pane, target, arm)) = picker.running_command_watch_action(self)
        {
            self.watch_selected_running_command(&picker.launch, pane, target, arm);
        }
    }

    pub(in crate::app) fn focus_running_command(
        &mut self,
        pane: u64,
        target: RunningCommandTarget,
        ctx: &ComponentContext<Self>,
    ) {
        if let Some((id, _)) = self.running_command_session(pane, target)
            && self.tabs.select(id)
        {
            self.sync_selection(ctx);
            self.activate(ctx);
        }
    }
}
