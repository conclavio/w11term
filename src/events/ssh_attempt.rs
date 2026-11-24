use super::Events;
use tokio::sync::{oneshot, watch};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingSshAttempt(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SshAttemptOutcome {
    Connected,
    Failed,
    Stopped,
    Closed,
}

pub struct SshAttemptCompletion(watch::Receiver<Option<SshAttemptOutcome>>);

impl SshAttemptCompletion {
    pub fn outcome(&self) -> Option<SshAttemptOutcome> {
        let closed = self.0.has_changed().is_err();
        (*self.0.borrow()).or(closed.then_some(SshAttemptOutcome::Closed))
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
        let id = PendingSshAttempt(attempts.next);
        let (cancel, cancelled) = oneshot::channel();
        let (completion, _) = watch::channel(None);
        attempts.current = Some(ActiveAttempt {
            id,
            cancel,
            completion: completion.clone(),
        });
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

    pub fn watch_ssh_attempt(&self, id: PendingSshAttempt) -> Option<SshAttemptCompletion> {
        let state = self.state.lock().unwrap();
        let attempt = state
            .ssh_attempts
            .current
            .as_ref()
            .filter(|attempt| attempt.id == id)?;
        Some(SshAttemptCompletion(attempt.completion.subscribe()))
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
        self.state
            .lock()
            .unwrap()
            .ssh_attempts
            .current
            .take_if(|attempt| attempt.id == id)
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
    }

    #[test]
    fn watching_distinguishes_failure_and_an_abandoned_attempt() {
        let events = Events::default();
        let (failed, _) = events.begin_ssh_attempt();
        let watched = events
            .watch_ssh_attempt(events.pending_ssh_attempt().unwrap())
            .unwrap();
        assert_eq!(watched.outcome(), None);
        assert!(failed.finish(SshAttemptOutcome::Failed));
        drop(failed);
        assert_eq!(watched.outcome(), Some(SshAttemptOutcome::Failed));

        let (closed, _) = events.begin_ssh_attempt();
        let watched = events
            .watch_ssh_attempt(events.pending_ssh_attempt().unwrap())
            .unwrap();
        drop(closed);
        assert_eq!(watched.outcome(), Some(SshAttemptOutcome::Closed));
    }
}
