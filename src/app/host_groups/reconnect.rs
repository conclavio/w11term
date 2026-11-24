use super::*;

pub enum Request {
    Host(ExitedPane),
    Selection(Vec<ExitedPane>),
}

impl Request {
    fn exits(&self) -> &[ExitedPane] {
        match self {
            Self::Host(exit) => std::slice::from_ref(exit),
            Self::Selection(exits) => exits,
        }
    }

    fn eligible(&self, page: &HostGroup, roster: &[Target]) -> BTreeMap<u64, ExitedPane> {
        page.shown(roster)
            .filter(|target| matches!(self, Self::Host(_)) || page.selected.contains(&target.pane))
            .filter_map(|target| target.state.exit().map(|exit| (exit.pane, exit)))
            .collect()
    }
}

impl TargetState {
    fn exit(self) -> Option<ExitedPane> {
        match self {
            Self::Restartable(exit) => Some(exit),
            Self::Failed(exit) => exit,
            _ => None,
        }
    }
}

impl TerminalApp {
    pub(super) fn reconnect_host_group(&mut self, id: TabId, request: Request) {
        if self.host_group_command_target() != Some(id) || request.exits().is_empty() {
            return;
        }
        let roster = self.host_group_targets();
        let Some(page) = self.host_group_mut(id) else {
            return;
        };
        let eligible = request.eligible(page, &roster);
        let mut started = 0;
        let mut skipped = 0;
        let mut feedback = Vec::new();
        for &exit in request.exits() {
            if eligible.get(&exit.pane) != Some(&exit) {
                skipped += 1;
                continue;
            }
            match self.restart_exited_pane(exit) {
                Ok(true) => started += 1,
                Ok(false) => skipped += 1,
                Err(error) => feedback.push(format!(
                    "Could not reconnect pane {} · {error:#}.",
                    exit.pane
                )),
            }
        }
        if started > 0 {
            feedback.insert(
                0,
                format!("Reconnect requested for {}.", host_count(started)),
            );
        }
        if skipped > 0 {
            feedback.push(format!("{} changed before reconnect.", host_count(skipped)));
        }
        if let Some(page) = self.host_group_mut(id) {
            page.reconnect_feedback = feedback.join(" ");
        }
        self.refresh_open_commands();
    }
}

fn host_count(count: usize) -> String {
    let noun = if count == 1 { "host" } else { "hosts" };
    format!("{count} {noun}")
}

pub(super) fn selected_exits(page: &HostGroup, shown: &[&Target]) -> Vec<ExitedPane> {
    shown
        .iter()
        .filter(|target| page.selected.contains(&target.pane))
        .filter_map(|target| target.state.exit())
        .collect()
}

pub(super) fn host_button(id: TabId, state: TargetState, ctx: &ViewContext<TerminalApp>) -> View {
    let Some(exit) = state.exit() else {
        return View::empty();
    };
    Button::new()
        .automation_name(format!("Reconnect SSH pane {}", exit.pane))
        .on_click(
            ctx.callback(move |_| Message::HostGroup(Action::Reconnect(id, Request::Host(exit)))),
        )
        .content("Reconnect")
}

pub(super) fn selection_controls(
    id: TabId,
    page: &HostGroup,
    shown: &[&Target],
    ctx: &ViewContext<TerminalApp>,
) -> View {
    let visible_exit = shown.iter().any(|target| target.state.exit().is_some());
    if !visible_exit && page.reconnect_feedback.is_empty() {
        return View::empty();
    }
    let exits = selected_exits(page, shown);
    let button = if visible_exit {
        let caption = if exits.is_empty() {
            "Reconnect selected".to_owned()
        } else {
            format!("Reconnect selected ({})", exits.len())
        };
        Button::new()
            .automation_name("Reconnect selected")
            .is_enabled(!exits.is_empty())
            .on_click(ctx.callback(move |_| {
                Message::HostGroup(Action::Reconnect(id, Request::Selection(exits.clone())))
            }))
            .content(caption)
    } else {
        View::empty()
    };
    let feedback = if page.reconnect_feedback.is_empty() {
        View::empty()
    } else {
        TextBlock::new()
            .text(&page.reconnect_feedback)
            .text_wrapping(TextWrapping::Wrap)
            .into()
    };
    StackPanel::new().spacing(4.0).children((button, feedback))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(pane: u64, name: &str, state: TargetState) -> Target {
        Target {
            pane,
            name: name.into(),
            names: Default::default(),
            profile: None,
            connected: false,
            state,
        }
    }

    #[test]
    fn requested_exits_must_still_match_the_visible_panes_and_their_exit_tokens() {
        let exit = |pane, token| ExitedPane { pane, token };
        let active = [exit(1, 2), exit(2, 4), exit(3, 1), exit(4, 1)];
        let roster = [
            target(1, "prod west", TargetState::Restartable(active[0])),
            target(2, "prod west", TargetState::Restartable(active[1])),
            target(3, "prod east", TargetState::Restartable(active[2])),
            target(4, "prod west", TargetState::Restartable(active[3])),
            target(5, "prod west", TargetState::Offline),
        ];
        let mut page = HostGroup {
            selected: BTreeSet::from([1, 2, 3, 5]),
            ..Default::default()
        };
        page.filter.set_query("west".into());
        let request = Request::Selection(vec![
            exit(1, 1),
            active[1],
            active[2],
            active[3],
            exit(5, 1),
        ]);
        let eligible = request.eligible(&page, &roster);
        assert_eq!(eligible.get(&1), Some(&active[0]));
        assert_ne!(eligible.get(&1), Some(&request.exits()[0]));
        assert_eq!(eligible.get(&2), Some(&active[1]));
        assert_eq!(eligible.len(), 2);
        let individual = Request::Host(active[3]);
        assert_eq!(
            individual.eligible(&page, &roster).get(&4),
            Some(&active[3])
        );
    }
}
