use super::Events;
use tokio::sync::oneshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingSshAttempt(u64);

#[derive(Debug, Default)]
pub(super) struct Attempts {
    next: u64,
    current: Option<(PendingSshAttempt, oneshot::Sender<()>)>,
}

pub(crate) struct AttemptGuard {
    events: Events,
    id: PendingSshAttempt,
}

impl Events {
    pub(crate) fn begin_ssh_attempt(&self) -> (AttemptGuard, oneshot::Receiver<()>) {
        let mut state = self.state.lock().unwrap();
        let attempts = &mut state.ssh_attempts;
        attempts.next += 1;
        let id = PendingSshAttempt(attempts.next);
        let (cancel, cancelled) = oneshot::channel();
        attempts.current = Some((id, cancel));
        (
            AttemptGuard {
                events: self.clone(),
                id,
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
            .map(|(id, _)| *id)
    }

    pub fn cancel_ssh_attempt(&self, id: PendingSshAttempt) -> bool {
        let Some((_, cancel)) = self.take_ssh_attempt(id) else {
            return false;
        };
        // Publication may already have observed the claimed token and exited.
        let _ = cancel.send(());
        true
    }

    fn take_ssh_attempt(
        &self,
        id: PendingSshAttempt,
    ) -> Option<(PendingSshAttempt, oneshot::Sender<()>)> {
        self.state
            .lock()
            .unwrap()
            .ssh_attempts
            .current
            .take_if(|(current, _)| *current == id)
    }
}

impl AttemptGuard {
    /// Retire the cancellation offer before publishing an established shell.
    /// If cancellation already claimed the offer, the shell must stay unpublished.
    pub(crate) fn finish(&self) -> bool {
        self.events.take_ssh_attempt(self.id).is_some()
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
        assert!(events.cancel_ssh_attempt(old));
        assert_eq!(cancelled.try_recv(), Ok(()));
        assert!(!first.finish());

        let (second, mut cancelled) = events.begin_ssh_attempt();
        let current = events.pending_ssh_attempt().unwrap();
        drop(first);
        assert_ne!(old, current);
        assert!(!events.cancel_ssh_attempt(old));
        assert_eq!(
            cancelled.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        );
        assert!(second.finish());
        assert!(!events.cancel_ssh_attempt(current));
        assert_eq!(
            cancelled.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        );
        assert_eq!(events.pending_ssh_attempt(), None);
    }
}
