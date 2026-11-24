use super::*;

#[derive(Clone, Debug, PartialEq)]
pub enum RosterCommand {
    Copy(BTreeSet<u64>),
    OpenPrompt { pane: u64, prompt: u64 },
    Reconnect(Vec<ExitedPane>),
    ReviewDisconnect(Vec<disconnect::Recipient>),
}

impl RosterCommand {
    pub(in crate::app) fn label(&self) -> &'static str {
        match self {
            Self::Copy(_) => "Copy shown SSH roster",
            Self::OpenPrompt { .. } => "Open next SSH sign-in or host-key prompt",
            Self::Reconnect(_) => "Reconnect selected SSH hosts",
            Self::ReviewDisconnect(_) => "Review disconnect for selected SSH hosts",
        }
    }

    pub(in crate::app) fn into_action(self, group: TabId) -> Action {
        match self {
            Self::Copy(offered) => Action::CopyRoster(group, offered),
            Self::OpenPrompt { pane, prompt } => Action::VisitPrompt(group, pane, prompt),
            Self::Reconnect(exits) => {
                Action::Reconnect(group, reconnect::Request::Selection(exits))
            }
            Self::ReviewDisconnect(recipients) => {
                Action::Disconnect(group, disconnect::Action::Review(recipients))
            }
        }
    }
}

impl TerminalApp {
    pub(in crate::app) fn roster_commands(&self, group: TabId) -> Vec<RosterCommand> {
        let Some(Page::HostGroup(page)) = self.tabs.get(group).map(|tab| &tab.page) else {
            return Vec::new();
        };
        let targets = self.host_group_targets();
        let mut shown: Vec<_> = page.shown(&targets).collect();
        if shown.is_empty() {
            return Vec::new();
        }
        let mut commands = vec![RosterCommand::Copy(
            shown.iter().map(|target| target.pane).collect(),
        )];
        let exits = reconnect::selected_exits(page, &shown);
        if !exits.is_empty() {
            commands.push(RosterCommand::Reconnect(exits));
        }
        if page.disconnect.can_review() {
            let recipients = self.disconnect_recipients(
                shown
                    .iter()
                    .copied()
                    .filter(|target| page.selected.contains(&target.pane)),
            );
            if !recipients.is_empty() {
                commands.push(RosterCommand::ReviewDisconnect(recipients));
            }
        }
        page.sort_shown(&mut shown);
        if let Some((pane, prompt)) = shown.into_iter().find_map(Target::pending_prompt) {
            commands.push(RosterCommand::OpenPrompt { pane, prompt });
        }
        commands
    }
}
