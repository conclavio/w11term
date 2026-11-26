use super::*;
use command_palette::{CommandId, Row, Search};
use w11term::events::RunningCommandTarget;

impl Picker {
    fn selected_running_command(&self) -> Option<(u64, RunningCommandTarget)> {
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

    fn running_command_session(
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
