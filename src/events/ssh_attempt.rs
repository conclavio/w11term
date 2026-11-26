use super::{EventLinks, Events, SshConnectionPhase, State};
use serde::{Deserialize, Serialize};
use std::{
    num::NonZeroU64,
    sync::{Arc, MutexGuard},
};
use tokio::sync::{oneshot, watch};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PendingSshAttempt(NonZeroU64);

impl From<NonZeroU64> for PendingSshAttempt {
    fn from(id: NonZeroU64) -> Self {
        Self(id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SshAttemptOutcome {
    Connected,
    Failed(Arc<str>),
    Stopped,
    Closed,
}

pub struct SshAttemptCompletion(watch::Receiver<Option<SshAttemptOutcome>>);

impl SshAttemptCompletion {
    pub fn outcome(&self) -> Option<SshAttemptOutcome> {
        let closed = self.0.has_changed().is_err();
        self.0
            .borrow()
            .clone()
            .or(closed.then_some(SshAttemptOutcome::Closed))
    }
}

#[derive(Debug)]
struct ActiveAttempt {
    id: PendingSshAttempt,
    cancel: oneshot::Sender<()>,
    completion: watch::Sender<Option<SshAttemptOutcome>>,
}

#[derive(Debug, Default)]
pub(super) struct Attempts {
    next: u64,
    current: Option<ActiveAttempt>,
}

pub(crate) struct AttemptGuard {
    events: Events,
    id: PendingSshAttempt,
    completion: watch::Sender<Option<SshAttemptOutcome>>,
}

impl Events {
    pub(crate) fn begin_ssh_attempt(&self) -> (AttemptGuard, oneshot::Receiver<()>) {
        let mut state = self.state.lock().unwrap();
        let attempts = &mut state.ssh_attempts;
        attempts.next += 1;
        let id = PendingSshAttempt(
            NonZeroU64::new(attempts.next).expect("SSH attempt ID starts at one"),
        );
        let (cancel, cancelled) = oneshot::channel();
        let (completion, _) = watch::channel(None);
        attempts.current = Some(ActiveAttempt {
            id,
            cancel,
            completion: completion.clone(),
        });
        self.ssh_attempt_changed(state, id, true);
        (
            AttemptGuard {
                events: self.clone(),
                id,
                completion,
            },
            cancelled,
        )
    }

    pub fn pending_ssh_attempt(&self) -> Option<PendingSshAttempt> {
        self.state
            .lock()
            .unwrap()
            .ssh_attempts
            .current
            .as_ref()
            .map(|attempt| attempt.id)
    }

    pub(crate) fn connection_snapshot(
        &self,
    ) -> (
        Option<u64>,
        Option<SshConnectionPhase>,
        Option<PendingSshAttempt>,
    ) {
        let state = self.state.lock().unwrap();
        (
            state.restart_token,
            state.ssh_connection_phase.clone(),
            state
                .ssh_attempts
                .current
                .as_ref()
                .map(|attempt| attempt.id),
        )
    }

    pub fn watch_ssh_attempt(&self, id: PendingSshAttempt) -> Option<SshAttemptCompletion> {
        let state = self.state.lock().unwrap();
        let attempt = state
            .ssh_attempts
            .current
            .as_ref()
            .filter(|attempt| attempt.id == id)?;
        Some(SshAttemptCompletion(attempt.completion.subscribe()))
    }

    /// Only the exit of this attempt can be claimed for a report retry.
    pub fn failed_ssh_restart_token(&self, id: PendingSshAttempt) -> Option<u64> {
        let state = self.state.lock().unwrap();
        if state.ssh_attempts.next == id.0.get()
            && state.ssh_attempts.current.is_none()
            && matches!(
                state.ssh_connection_phase,
                Some(SshConnectionPhase::Failed(_))
            )
        {
            state.restart_token
        } else {
            None
        }
    }

    pub fn cancel_ssh_attempt(&self, id: PendingSshAttempt) -> bool {
        let Some(attempt) = self.take_ssh_attempt(id) else {
            return false;
        };
        // Publication may already have observed the claimed token and exited.
        let _ = attempt.cancel.send(());
        true
    }

    fn take_ssh_attempt(&self, id: PendingSshAttempt) -> Option<ActiveAttempt> {
        let mut state = self.state.lock().unwrap();
        let attempt = state
            .ssh_attempts
            .current
            .take_if(|attempt| attempt.id == id)?;
        self.ssh_attempt_changed(state, id, false);
        Some(attempt)
    }

    fn ssh_attempt_changed(
        &self,
        mut state: MutexGuard<'_, State>,
        id: PendingSshAttempt,
        pending: bool,
    ) {
        Self::append_locked(
            &mut state,
            "ssh-attempt",
            serde_json::json!({"attempt_token": id, "pending": pending}),
            EventLinks::default(),
        );
        let inbox = state.inbox.as_ref().map(|(_, _, inbox)| inbox.clone());
        drop(state);
        self.changed.notify_waiters();
        if let Some(inbox) = inbox {
            inbox.refresh();
        }
    }
}

impl AttemptGuard {
    /// Retire the cancellation offer before publishing an established shell.
    /// If cancellation already claimed the offer, the shell must stay unpublished.
    pub(crate) fn finish(&self, outcome: SshAttemptOutcome) -> bool {
        let completed = self.events.take_ssh_attempt(self.id).is_some();
        self.completion.send_replace(if completed {
            Some(outcome)
        } else {
            Some(SshAttemptOutcome::Stopped)
        });
        completed
    }
}

impl Drop for AttemptGuard {
    fn drop(&mut self) {
        self.events.take_ssh_attempt(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelling_and_publishing_an_attempt_are_mutually_exclusive() {
        let events = Events::default();
        let (first, mut cancelled) = events.begin_ssh_attempt();
        let old = events.pending_ssh_attempt().unwrap();
        assert_eq!(events.connection_snapshot().2, Some(old));
        let watching_first = events.watch_ssh_attempt(old).unwrap();
        assert!(events.cancel_ssh_attempt(old));
        assert_eq!(cancelled.try_recv(), Ok(()));
        assert!(!first.finish(SshAttemptOutcome::Connected));
        assert_eq!(watching_first.outcome(), Some(SshAttemptOutcome::Stopped));

        let (second, mut cancelled) = events.begin_ssh_attempt();
        let current = events.pending_ssh_attempt().unwrap();
        assert!(events.watch_ssh_attempt(old).is_none());
        let watching_second = events.watch_ssh_attempt(current).unwrap();
        drop(first);
        assert_ne!(old, current);
        assert!(!events.cancel_ssh_attempt(old));
        assert_eq!(
            cancelled.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        );
        assert!(second.finish(SshAttemptOutcome::Connected));
        assert_eq!(
            watching_second.outcome(),
            Some(SshAttemptOutcome::Connected)
        );
        assert!(!events.cancel_ssh_attempt(current));
        assert_eq!(
            cancelled.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        );
        assert_eq!(events.pending_ssh_attempt(), None);
        assert_eq!(events.connection_snapshot().2, None);
        let records: Vec<_> = events
            .since(0)
            .events
            .into_iter()
            .map(|event| (event.kind, event.data))
            .collect();
        assert_eq!(
            records,
            [(old, true), (old, false), (current, true), (current, false)].map(
                |(attempt_token, pending)| (
                    "ssh-attempt".into(),
                    serde_json::json!({ "attempt_token": attempt_token, "pending": pending })
                )
            )
        );
    }

    #[test]
    fn watching_distinguishes_failure_and_an_abandoned_attempt() {
        let events = Events::default();
        let (failed, _) = events.begin_ssh_attempt();
        let watched = events
            .watch_ssh_attempt(events.pending_ssh_attempt().unwrap())
            .unwrap();
        assert_eq!(watched.outcome(), None);
        let outcome = SshAttemptOutcome::Failed("connection refused".into());
        assert!(failed.finish(outcome.clone()));
        drop(failed);
        assert_eq!(watched.outcome(), Some(outcome));

        let (closed, _) = events.begin_ssh_attempt();
        let watched = events
            .watch_ssh_attempt(events.pending_ssh_attempt().unwrap())
            .unwrap();
        drop(closed);
        assert_eq!(watched.outcome(), Some(SshAttemptOutcome::Closed));
    }

    #[test]
    fn a_report_can_only_reconnect_its_original_failed_attempt() {
        let events = Events::default();
        let (commands, _worker) = tokio::sync::mpsc::unbounded_channel();
        let (first, _stop) = events.begin_ssh_attempt();
        let first_id = events.pending_ssh_attempt().unwrap();
        assert!(first.finish(SshAttemptOutcome::Failed("first failure".into())));
        events.pane_exited(1, Some(SshConnectionPhase::Failed("first failure".into())));
        assert_eq!(events.failed_ssh_restart_token(first_id), Some(1));
        events.restart_exited(&commands, 1).unwrap();
        assert_eq!(events.failed_ssh_restart_token(first_id), None);

        let (second, _stop) = events.begin_ssh_attempt();
        let second_id = events.pending_ssh_attempt().unwrap();
        assert!(second.finish(SshAttemptOutcome::Failed("later failure".into())));
        events.pane_exited(2, Some(SshConnectionPhase::Failed("later failure".into())));
        assert_eq!(events.failed_ssh_restart_token(first_id), None);
        assert_eq!(events.failed_ssh_restart_token(second_id), Some(2));
    }
}
