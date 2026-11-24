use super::*;
use w11term::events::{PendingSshAttempt, SshAttemptOutcome, SshLoginResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attempt {
    pub(super) pane: u64,
    pub(super) token: PendingSshAttempt,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedLogin {
    pub host: String,
    exit: ExitedPane,
}

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
    pub(super) fn exit(self) -> Option<ExitedPane> {
        match self {
            Self::Restartable(exit) => Some(exit),
            Self::Failed(exit) | Self::Stopped(exit) => exit,
            _ => None,
        }
    }
}

impl HostGroup {
    pub(super) fn login_permits(&mut self, limit: Option<usize>) -> Option<Arc<Semaphore>> {
        let limit = limit?;
        let (current, permits) = self
            .login_queue
            .get_or_insert_with(|| (limit, Arc::new(Semaphore::new(limit))));
        if *current != limit {
            *current = limit;
            *permits = Arc::new(Semaphore::new(limit));
        }
        Some(permits.clone())
    }
}

impl TerminalApp {
    pub(super) fn pending_attempts(&self, page: &HostGroup, shown: &[&Target]) -> Vec<Attempt> {
        shown
            .iter()
            .filter(|target| !target.connected && page.selected.contains(&target.pane))
            .filter_map(|target| {
                let session = self.direct_ssh_session(target.pane)?;
                Some(Attempt {
                    pane: target.pane,
                    token: session.events.pending_ssh_attempt()?,
                })
            })
            .collect()
    }

    pub(super) fn stop_pending_host_group(&mut self, id: TabId, offered: &[Attempt]) {
        if offered.is_empty() || self.host_group_command_target() != Some(id) {
            return;
        }
        let Some(Page::HostGroup(page)) = self.tabs.get(id).map(|entry| &entry.page) else {
            return;
        };
        let roster = self.host_group_targets();
        let pending = self
            .pending_attempts(page, &page.shown(&roster).collect::<Vec<_>>())
            .into_iter()
            .map(|attempt| (attempt.pane, attempt.token))
            .collect::<BTreeMap<_, _>>();
        let stopped = offered
            .iter()
            .filter(|attempt| {
                pending.get(&attempt.pane) == Some(&attempt.token)
                    && self
                        .direct_ssh_session(attempt.pane)
                        .is_some_and(|session| session.events.cancel_ssh_attempt(attempt.token))
            })
            .count();
        let skipped = offered.len() - stopped;
        let mut feedback = Vec::new();
        if stopped > 0 {
            feedback.push(format!("Stop requested for {}.", host_count(stopped)));
        }
        if skipped > 0 {
            feedback.push(format!(
                "{} changed before the stop request.",
                host_count(skipped)
            ));
        }
        if let Some(page) = self.host_group_mut(id) {
            page.reconnect_feedback = feedback.join(" ");
        }
        self.refresh_open_commands();
    }

    pub(super) fn reconnect_host_group(&mut self, id: TabId, request: Request) {
        if self.host_group_command_target() != Some(id) || request.exits().is_empty() {
            return;
        }
        let roster = self.host_group_targets();
        let Some(page) = self.host_group_mut(id) else {
            return;
        };
        let eligible = request.eligible(page, &roster);
        let login_permits = match request {
            Request::Selection(_) => {
                page.login_permits(page.roster_preferences.login_limit.concurrency())
            }
            Request::Host(_) => None,
        };
        let exits = request.exits();
        let confirmed = exits
            .iter()
            .copied()
            .filter(|exit| eligible.get(&exit.pane) == Some(exit))
            .collect::<Vec<_>>();
        self.restart_group_exits(id, &confirmed, exits.len() - confirmed.len(), login_permits);
    }

    pub(in crate::app) fn report_failed_logins(
        &self,
        id: TabId,
        results: &[SshLoginResult],
    ) -> Vec<FailedLogin> {
        let Some(Page::HostGroup(page)) = self.tabs.get(id).map(|entry| &entry.page) else {
            return Vec::new();
        };
        results
            .iter()
            .filter_map(|result| {
                if !page.includes_pane(result.pane)
                    || !matches!(result.outcome, SshAttemptOutcome::Failed(_))
                {
                    return None;
                }
                let token = self
                    .direct_ssh_session(result.pane)?
                    .events
                    .failed_ssh_restart_token(result.attempt)?;
                Some(FailedLogin {
                    host: result.host.clone(),
                    exit: ExitedPane {
                        pane: result.pane,
                        token,
                    },
                })
            })
            .collect()
    }

    pub(in crate::app) fn restart_report_failed_logins(
        &mut self,
        id: TabId,
        recipients: &[FailedLogin],
    ) {
        self.tabs.select(id);
        let Some(page) = self.host_group_mut(id) else {
            return;
        };
        let permits = page.login_permits(page.roster_preferences.login_limit.concurrency());
        let exits = recipients
            .iter()
            .map(|recipient| recipient.exit)
            .collect::<Vec<_>>();
        self.restart_group_exits(id, &exits, 0, permits);
    }

    fn restart_group_exits(
        &mut self,
        id: TabId,
        exits: &[ExitedPane],
        mut skipped: usize,
        login_permits: Option<Arc<Semaphore>>,
    ) {
        let mut started = 0;
        let mut feedback = Vec::new();
        for &exit in exits {
            match self.restart_exited_pane(exit, login_permits.clone()) {
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
    pending: Vec<Attempt>,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    let visible_exit = shown.iter().any(|target| target.state.exit().is_some());
    if !visible_exit
        && pending.is_empty()
        && page.reconnect_feedback.is_empty()
        && !page.connection_notice.is_armed()
    {
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
    let notice = page.connection_notice.button(id, &pending, ctx);
    let stop = if pending.is_empty() {
        View::empty()
    } else {
        let count = pending.len();
        Button::new()
            .automation_name("Stop selected SSH attempts")
            .on_click(
                ctx.callback(move |_| Message::HostGroup(Action::StopPending(id, pending.clone()))),
            )
            .content(format!("Stop selected SSH attempts ({count})"))
    };
    StackPanel::new().spacing(4.0).children((
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .children((button, stop, notice)),
        feedback,
    ))
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
