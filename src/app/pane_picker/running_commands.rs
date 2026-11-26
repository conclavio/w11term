use super::*;
use command_palette::{CommandId, Row, Search};
use std::time::Instant;

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
                    id: CommandId::FocusRunningCommand(pane, generation, running.id, started),
                    label: format!("{command} · {source}"),
                    chord: format!("{} s", running.elapsed_ms() / 1000),
                    enabled: true,
                },
            ));
        }
        rows.sort_unstable_by_key(|(order, _)| *order);
        rows.into_iter().map(|(_, row)| row).collect()
    }

    pub(in crate::app) fn focus_running_command(
        &mut self,
        pane: u64,
        generation: u64,
        command: u64,
        started: Instant,
        ctx: &ComponentContext<Self>,
    ) {
        let Some(id) = self.tab_for_pane(pane) else {
            return;
        };
        let Some(Page::Terminal(live)) = self.tabs.get(id).map(|tab| &tab.page) else {
            return;
        };
        if live
            .session
            .running_command_snapshot()
            .is_some_and(|(current, running)| {
                current == generation && running.id == command && running.started_at() == started
            })
            && self.tabs.select(id)
        {
            self.sync_selection(ctx);
            self.activate(ctx);
        }
    }
}
