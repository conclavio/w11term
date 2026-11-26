use super::{EventLinks, Events, PaneProcessState, SshConnectionPhase, State};
use serde::{Deserialize, Serialize};
use std::{
    num::NonZeroU64,
    sync::{Arc, MutexGuard},
};
use tokio::sync::{oneshot, watch};
use tokio::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PendingSshAttempt(NonZeroU64);

impl From<NonZeroU64> for PendingSshAttempt {
    fn from(id: NonZeroU64) -> Self {
        Self(id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PendingSshRetry(NonZeroU64);

impl From<NonZeroU64> for PendingSshRetry {
    fn from(id: NonZeroU64) -> Self {
        Self(id)
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SshConnectionSnapshot {
    pub restart_token: Option<u64>,
    pub phase: Option<SshConnectionPhase>,
    pub attempt: Option<PendingSshAttempt>,
    pub retry: Option<PendingSshRetry>,
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
    retry: Option<RetryOffer>,
}

#[derive(Debug)]
struct RetryOffer {
    id: PendingSshRetry,
    until: Instant,
    request: oneshot::Sender<()>,
}

#[derive(Debug, Default)]
pub(super) struct Attempts {
    next: u64,
    current: Option<ActiveAttempt>,
    next_retry: u64,
}

impl Attempts {
    fn pending_retry(&self) -> Option<PendingSshRetry> {
        let retry = self.current.as_ref()?.retry.as_ref()?;
        (Instant::now() < retry.until).then_some(retry.id)
    }
}

pub(crate) struct RetryWait {
    events: Events,
    id: PendingSshRetry,
    requested: oneshot::Receiver<()>,
}

impl RetryWait {
    pub(crate) async fn requested(wait: Option<&mut Self>) {
        if let Some(wait) = wait
            && (&mut wait.requested).await.is_ok()
        {
            return;
        }
        std::future::pending().await
    }
}

impl Drop for RetryWait {
    fn drop(&mut self) {
        self.events.retire_ssh_retry(self.id, false);
    }
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
            retry: None,
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

    pub fn pending_ssh_retry(&self) -> Option<PendingSshRetry> {
        self.state.lock().unwrap().ssh_attempts.pending_retry()
    }

    pub(crate) fn connection_snapshot(&self) -> SshConnectionSnapshot {
        let state = self.state.lock().unwrap();
        SshConnectionSnapshot {
            restart_token: state.restart_token,
            phase: state.ssh_connection_phase.clone(),
            attempt: state
                .ssh_attempts
                .current
                .as_ref()
                .map(|attempt| attempt.id),
            retry: state.ssh_attempts.pending_retry(),
        }
    }

    pub(crate) async fn wait_ssh_connected(
        &self,
        after_generation: u64,
        deadline: Instant,
    ) -> anyhow::Result<u64> {
        use SshConnectionPhase::*;
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let state = self.state.lock().unwrap();
                anyhow::ensure!(!state.restart_closed, "pane is closed");
                let generation = state.ssh_connection_generation;
                match state.ssh_connection_phase.as_ref() {
                    None => anyhow::bail!("pane does not use SSH"),
                    Some(Connected) if generation > after_generation => return Ok(generation),
                    Some(Failed(reason)) => anyhow::bail!("SSH connection failed: {reason}"),
                    Some(Stopped) => anyhow::bail!("SSH connection attempt stopped"),
                    Some(Disconnected)
                        if matches!(state.pane_process, PaneProcessState::Exited(_)) =>
                    {
                        anyhow::bail!("SSH pane is disconnected");
                    }
                    _ => {}
                }
            }
            anyhow::ensure!(Instant::now() < deadline, "wait-ssh-connected timed out");
            let _ = tokio::time::timeout_at(deadline, changed).await;
        }
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

    pub fn retry_ssh_now(&self, retry: PendingSshRetry) -> bool {
        self.retire_ssh_retry(retry, true)
    }

    fn retire_ssh_retry(&self, id: PendingSshRetry, requested: bool) -> bool {
        let mut state = self.state.lock().unwrap();
        let Some(retry) = state.ssh_attempts.current.as_mut().and_then(|attempt| {
            attempt
                .retry
                .take_if(|retry| retry.id == id && (!requested || Instant::now() < retry.until))
        }) else {
            return false;
        };
        let accepted = requested && retry.request.send(()).is_ok();
        Self::record_ssh_retry(&mut state, id, false, accepted);
        self.notify_ssh_control(state);
        accepted
    }

    fn record_ssh_retry(state: &mut State, id: PendingSshRetry, pending: bool, requested: bool) {
        Self::append_locked(
            state,
            "ssh-retry-wait",
            serde_json::json!({"retry_token": id, "pending": pending, "requested": requested}),
            EventLinks::default(),
        );
    }

    fn take_ssh_attempt(&self, id: PendingSshAttempt) -> Option<ActiveAttempt> {
        let mut state = self.state.lock().unwrap();
        let attempt = state
            .ssh_attempts
            .current
            .take_if(|attempt| attempt.id == id)?;
        if let Some(retry) = &attempt.retry {
            Self::record_ssh_retry(&mut state, retry.id, false, false);
        }
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
        self.notify_ssh_control(state);
    }

    fn notify_ssh_control(&self, state: MutexGuard<'_, State>) {
        let inbox = state.inbox.as_ref().map(|(_, _, inbox)| inbox.clone());
        drop(state);
        self.changed.notify_waiters();
        if let Some(inbox) = inbox {
            inbox.refresh();
        }
    }
}

impl AttemptGuard {
    pub(crate) fn offer_retry(&self, until: Instant) -> Option<RetryWait> {
        let mut state = self.events.state.lock().unwrap();
        let attempts = &mut state.ssh_attempts;
        let current = attempts.current.as_mut().filter(|attempt| {
            attempt.id == self.id && attempt.retry.is_none() && Instant::now() < until
        })?;
        attempts.next_retry += 1;
        let id = PendingSshRetry(
            NonZeroU64::new(attempts.next_retry).expect("SSH retry ID starts at one"),
        );
        let (request, requested) = oneshot::channel();
        current.retry = Some(RetryOffer { id, until, request });
        Events::record_ssh_retry(&mut state, id, true, false);
        self.events.notify_ssh_control(state);
        Some(RetryWait {
            events: self.events.clone(),
            id,
            requested,
        })
    }

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

    #[tokio::test(start_paused = true)]
    async fn retry_tokens_expire_and_cannot_advance_a_later_wait() {
        use std::time::Duration;
        let events = Events::default();
        let (attempt, _) = events.begin_ssh_attempt();
        let mut first = attempt
            .offer_retry(Instant::now() + Duration::from_secs(2))
            .unwrap();
        let old = events.pending_ssh_retry().unwrap();
        assert_eq!(events.connection_snapshot().retry, Some(old));
        assert!(events.retry_ssh_now(old));
        assert_eq!(first.requested.try_recv(), Ok(()));
        assert!(!events.retry_ssh_now(old));

        let second = attempt
            .offer_retry(Instant::now() + Duration::from_secs(4))
            .unwrap();
        let current = events.pending_ssh_retry().unwrap();
        assert_ne!(old, current);
        drop(first);
        assert!(!events.retry_ssh_now(old));
        assert_eq!(events.pending_ssh_retry(), Some(current));
        tokio::time::advance(Duration::from_secs(4)).await;
        assert!(!events.retry_ssh_now(current));
        assert_eq!(events.pending_ssh_retry(), None);
        drop(second);

        let _third = attempt
            .offer_retry(Instant::now() + Duration::from_secs(8))
            .unwrap();
        let third = events.pending_ssh_retry().unwrap();
        assert!(events.cancel_ssh_attempt(events.pending_ssh_attempt().unwrap()));
        assert_eq!(events.connection_snapshot().retry, None);
        assert!(!events.retry_ssh_now(third));
        let events: Vec<_> = events
            .since(0)
            .events
            .into_iter()
            .filter(|event| event.kind == "ssh-retry-wait")
            .map(|event| event.data)
            .collect();
        assert_eq!(events, [
            (old, true, false), (old, false, true),
            (current, true, false), (current, false, false),
            (third, true, false), (third, false, false),
        ].map(|(retry_token, pending, requested)| {
            serde_json::json!({ "retry_token": retry_token, "pending": pending, "requested": requested })
        }));
    }

    #[test]
    fn cancelling_and_publishing_an_attempt_are_mutually_exclusive() {
        let events = Events::default();
        let (first, mut cancelled) = events.begin_ssh_attempt();
        let old = events.pending_ssh_attempt().unwrap();
        assert_eq!(events.connection_snapshot().attempt, Some(old));
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
        assert_eq!(events.connection_snapshot().attempt, None);
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
        events.pane_exited(
            1,
            Some(SshConnectionPhase::Failed("first failure".into())),
            true,
        );
        assert_eq!(events.failed_ssh_restart_token(first_id), Some(1));
        events.restart_exited(&commands, 1).unwrap();
        assert_eq!(events.failed_ssh_restart_token(first_id), None);

        let (second, _stop) = events.begin_ssh_attempt();
        let second_id = events.pending_ssh_attempt().unwrap();
        assert!(second.finish(SshAttemptOutcome::Failed("later failure".into())));
        events.pane_exited(
            2,
            Some(SshConnectionPhase::Failed("later failure".into())),
            true,
        );
        assert_eq!(events.failed_ssh_restart_token(first_id), None);
        assert_eq!(events.failed_ssh_restart_token(second_id), Some(2));
    }

    #[tokio::test(start_paused = true)]
    async fn connection_wait_ignores_the_previous_generation_and_transient_disconnect() {
        use SshConnectionPhase::*;
        use std::time::Duration;

        let events = Events::default();
        events.set_ssh_connected(1);
        let deadline = Instant::now() + Duration::from_secs(10);
        assert_eq!(events.wait_ssh_connected(0, deadline).await.unwrap(), 1);
        let reconnect = async {
            for phase in [Disconnected, Reconnecting { attempt: Some(1) }] {
                tokio::time::sleep(Duration::from_secs(1)).await;
                events.set_ssh_connection_phase(phase);
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
            events.set_ssh_connected(2);
        };
        let (result, ()) = tokio::join!(events.wait_ssh_connected(1, deadline), reconnect);
        assert_eq!(result.unwrap(), 2);
        let result = events.wait_ssh_connected(2, deadline).await.unwrap_err();
        assert_eq!(result.to_string(), "wait-ssh-connected timed out");
    }

    #[tokio::test(start_paused = true)]
    async fn connection_wait_wakes_when_a_disconnect_settles_or_the_pane_closes() {
        use SshConnectionPhase::*;
        use std::time::Duration;

        for (closed, expected) in [
            (false, "SSH pane is disconnected"),
            (true, "pane is closed"),
        ] {
            let events = Events::default();
            events.set_ssh_connection_phase(Disconnected);
            let deadline = Instant::now() + Duration::from_secs(30);
            let end = async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                if closed {
                    events.close_restart();
                } else {
                    events.pane_exited(1, Some(Disconnected), false);
                }
            };
            let (result, ()) = tokio::join!(
                tokio::time::timeout(
                    Duration::from_secs(2),
                    events.wait_ssh_connected(0, deadline)
                ),
                end
            );
            assert_eq!(result.unwrap().unwrap_err().to_string(), expected);
        }
    }
}
