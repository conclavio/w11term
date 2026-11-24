use super::{reconnect::Attempt, *};
use w11term::events::{SshAttemptCompletion, SshAttemptOutcome};

pub enum Action {
    Arm(Vec<Attempt>),
    Cancel(u64),
}

#[derive(Default)]
pub(super) struct State {
    next: u64,
    watch: Option<Watch>,
}

struct Watch {
    sequence: u64,
    attempts: Vec<SshAttemptCompletion>,
}

impl State {
    pub(super) fn is_armed(&self) -> bool {
        self.watch.is_some()
    }

    pub(super) fn button(
        &self,
        id: TabId,
        pending: &[Attempt],
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        if let Some(watch) = &self.watch {
            let sequence = watch.sequence;
            Button::new()
                .on_click(ctx.callback(move |_| {
                    Message::HostGroup(super::Action::ConnectionNotice(
                        id,
                        Action::Cancel(sequence),
                    ))
                }))
                .content("Cancel login notification")
        } else if pending.is_empty() {
            View::empty()
        } else {
            let pending = pending.to_vec();
            let count = pending.len();
            Button::new()
                .automation_name("Notify when logins finish")
                .on_click(ctx.callback(move |_| {
                    Message::HostGroup(super::Action::ConnectionNotice(
                        id,
                        Action::Arm(pending.clone()),
                    ))
                }))
                .content(format!("Notify when logins finish ({count})"))
        }
    }

    fn arm(&mut self, attempts: Vec<SshAttemptCompletion>) {
        self.next += 1;
        self.watch = Some(Watch {
            sequence: self.next,
            attempts,
        });
    }

    fn poll(&mut self) -> Option<(u64, String)> {
        let watch = self.watch.as_ref()?;
        let (mut connected, mut failed, mut stopped, mut closed) = (0, 0, 0, 0);
        for attempt in &watch.attempts {
            match attempt.outcome()? {
                SshAttemptOutcome::Connected => connected += 1,
                SshAttemptOutcome::Failed => failed += 1,
                SshAttemptOutcome::Stopped => stopped += 1,
                SshAttemptOutcome::Closed => closed += 1,
            }
        }
        let sequence = watch.sequence;
        self.watch = None;
        let message = format!(
            "SSH logins finished · {connected} connected · {failed} failed · {stopped} stopped · {closed} closed"
        );
        Some((sequence, message))
    }
}

impl TerminalApp {
    pub(super) fn handle_connection_notice(
        &mut self,
        id: TabId,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        if self.host_group_command_target() != Some(id) {
            return;
        }
        match action {
            Action::Cancel(sequence) => {
                if let Some(page) = self.host_group_mut(id)
                    && page
                        .connection_notice
                        .watch
                        .as_ref()
                        .is_some_and(|watch| watch.sequence == sequence)
                {
                    page.connection_notice.watch = None;
                }
            }
            Action::Arm(offered) => {
                let Some(Page::HostGroup(page)) = self.tabs.get(id).map(|tab| &tab.page) else {
                    return;
                };
                if offered.is_empty() || page.connection_notice.is_armed() {
                    return;
                }
                let roster = self.host_group_targets();
                let current = self.pending_attempts(page, &page.shown(&roster).collect::<Vec<_>>());
                let attempts = if current == offered {
                    offered
                        .iter()
                        .map(|attempt| {
                            self.direct_ssh_session(attempt.pane)?
                                .events
                                .watch_ssh_attempt(attempt.token)
                        })
                        .collect::<Option<Vec<_>>>()
                } else {
                    None
                };
                if let Some(page) = self.host_group_mut(id) {
                    if let Some(attempts) = attempts {
                        page.connection_notice.arm(attempts);
                        page.reconnect_feedback.clear();
                    } else {
                        page.reconnect_feedback =
                            "The selected SSH attempts changed. Review the roster and try again."
                                .into();
                    }
                }
            }
        }
        self.update_host_group_timer(ctx);
    }

    pub(super) fn poll_connection_notices(&mut self) {
        let armed: Vec<_> = self.tabs.entries().iter().filter_map(|entry| {
            matches!(&entry.value.page, Page::HostGroup(page) if page.connection_notice.is_armed())
                .then_some(entry.id)
        }).collect();
        for id in armed {
            if let Some(AppTab {
                label,
                page: Page::HostGroup(page),
            }) = self.tabs.get_mut(id)
                && let Some((sequence, message)) = page.connection_notice.poll()
            {
                self.inbox
                    .group_connections_finished(id, sequence, label, message);
            }
        }
    }
}
