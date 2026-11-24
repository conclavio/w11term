use super::{GroupExecution, Inbox};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, MutexGuard, OnceLock},
};
use tokio::sync::{broadcast, oneshot};
use wezterm_term::LiveOutputBatch;

const TRANSCRIPT_LINES: usize = 128;
const RECORDING_QUEUE: usize = 1024;

#[derive(Clone, Debug, Default)]
pub struct GroupTranscript(Arc<Mutex<Transcript>>);

#[derive(Debug, Default)]
struct Transcript {
    snapshot: GroupTranscriptSnapshot,
    recording: Option<Recording>,
    next_recording: u64,
    hosts: Vec<HostRecord>,
    active_hosts: usize,
    dispatch_finished: bool,
    completion_notice: Option<CompletionNotice>,
}

#[derive(Debug)]
struct CompletionNotice {
    inbox: Inbox,
    tab: u64,
    submission: u64,
    source: Arc<str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupCompletionState {
    Available,
    Armed,
    Settled,
}

#[derive(Debug)]
struct Recording {
    id: u64,
    mode: GroupRecordingMode,
    incoming: broadcast::Sender<GroupTranscriptLine>,
    ended: oneshot::Sender<GroupRecordingSummary>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupRecordingMode {
    Manual,
    UntilSettled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupRecordingEnd {
    Stopped,
    Settled,
}

#[derive(Debug)]
pub struct GroupRecordingSummary {
    pub end: GroupRecordingEnd,
    pub hosts: Vec<(u64, GroupRecordingOutcome)>,
}

#[derive(Debug)]
pub enum GroupRecordingOutcome {
    AwaitingDelivery,
    DeliveryUnconfirmed,
    Rejected(Arc<str>),
    Sent(Option<GroupExecution>),
}

#[derive(Debug)]
struct HostRecord {
    pane: u64,
    active: bool,
    receipt: GroupWriteReceipt,
    execution: Option<GroupExecution>,
}

impl HostRecord {
    fn succeeded(&self) -> bool {
        matches!(self.outcome(), GroupRecordingOutcome::Sent(Some(GroupExecution::Finished(result)))
            if result.exit_code == Some(0))
    }

    fn outcome(&self) -> GroupRecordingOutcome {
        match self.receipt.0.get() {
            Some(Ok(())) => {
                let execution = self.execution.clone().filter(|state| {
                    self.active
                        || matches!(
                            state,
                            GroupExecution::Finished(_)
                                | GroupExecution::PromptReturned
                                | GroupExecution::Unavailable(_)
                        )
                });
                GroupRecordingOutcome::Sent(execution)
            }
            Some(Err(reason)) => GroupRecordingOutcome::Rejected(reason.clone()),
            None if self.active => GroupRecordingOutcome::AwaitingDelivery,
            None => GroupRecordingOutcome::DeliveryUnconfirmed,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct GroupTranscriptSnapshot {
    pub omitted: u64,
    pub lines: VecDeque<GroupTranscriptLine>,
}

#[derive(Clone, Debug)]
pub struct GroupTranscriptLine {
    pub id: u64,
    pub pane: u64,
    pub text: Arc<str>,
    pub shortened: bool,
}

#[derive(Debug)]
pub struct GroupTranscriptHost {
    transcript: GroupTranscript,
    pane: u64,
    index: usize,
    verify_delivery: bool,
    receipt: GroupWriteReceipt,
    _execution_slot: Option<tokio::sync::OwnedSemaphorePermit>,
}

#[derive(Clone, Debug, Default)]
pub struct GroupWriteReceipt(Arc<OnceLock<Result<(), Arc<str>>>>);

impl GroupWriteReceipt {
    pub(crate) fn acknowledge(&self) {
        let _ = self.0.set(Ok(()));
    }

    /// Record a refusal before any bytes could be sent.
    pub fn reject(&self, reason: &str) {
        let _ = self.0.set(Err(reason.into()));
    }

    fn acknowledged(&self) -> bool {
        self.0.get().is_some_and(Result::is_ok)
    }
}

pub struct GroupTranscriptRecording {
    transcript: GroupTranscript,
    id: u64,
}

impl Drop for GroupTranscriptRecording {
    fn drop(&mut self) {
        let mut transcript = self.transcript.0.lock().unwrap();
        if transcript
            .recording
            .as_ref()
            .is_some_and(|recording| recording.id == self.id)
        {
            transcript.finish_recording(GroupRecordingEnd::Stopped);
        }
    }
}

pub struct GroupTranscriptStream {
    pub history: GroupTranscriptSnapshot,
    pub incoming: broadcast::Receiver<GroupTranscriptLine>,
    pub ended: oneshot::Receiver<GroupRecordingSummary>,
}

impl GroupTranscript {
    pub fn host(&self, pane: u64) -> GroupTranscriptHost {
        let mut transcript = self.0.lock().unwrap();
        debug_assert!(!transcript.dispatch_finished);
        let receipt = GroupWriteReceipt::default();
        let index = transcript.hosts.len();
        transcript.hosts.push(HostRecord {
            pane,
            active: true,
            receipt: receipt.clone(),
            execution: None,
        });
        transcript.active_hosts += 1;
        GroupTranscriptHost {
            transcript: self.clone(),
            pane,
            index,
            verify_delivery: false,
            receipt,
            _execution_slot: None,
        }
    }

    /// All hosts have been offered the submission. A host remains tracked
    /// while its command is queued or being observed by its session.
    pub fn finish_dispatch(&self) {
        let mut transcript = self.0.lock().unwrap();
        transcript.dispatch_finished = true;
        Self::finish_if_settled(transcript);
    }

    pub fn completion_state(&self) -> GroupCompletionState {
        let transcript = self.0.lock().unwrap();
        if transcript.settled() {
            GroupCompletionState::Settled
        } else if transcript.completion_notice.is_some() {
            GroupCompletionState::Armed
        } else {
            GroupCompletionState::Available
        }
    }

    /// Whether this host settled with a confirmed zero exit status.
    pub fn settled_host_succeeded(&self, pane: u64) -> Option<bool> {
        self.0
            .lock()
            .unwrap()
            .hosts
            .iter()
            .find(|host| host.pane == pane && !host.active)
            .map(HostRecord::succeeded)
    }

    pub fn has_unsuccessful_result(&self) -> bool {
        self.0
            .lock()
            .unwrap()
            .hosts
            .iter()
            .any(|host| !host.active && !host.succeeded())
    }

    pub fn set_completion_notice(
        &self,
        armed: bool,
        inbox: &Inbox,
        tab: u64,
        submission: u64,
        source: &str,
    ) {
        let mut transcript = self.0.lock().unwrap();
        if !transcript.settled() {
            transcript.completion_notice = armed.then(|| CompletionNotice {
                inbox: inbox.clone(),
                tab,
                submission,
                source: source.into(),
            });
        }
    }

    pub fn snapshot(&self) -> GroupTranscriptSnapshot {
        self.0.lock().unwrap().snapshot.clone()
    }

    pub fn start_recording(
        &self,
        mode: GroupRecordingMode,
    ) -> Option<(GroupTranscriptRecording, GroupTranscriptStream)> {
        let mut transcript = self.0.lock().unwrap();
        if transcript.recording.is_some() {
            return None;
        }
        // Take the history and subscribe under the same lock so the first live
        // line comes immediately after the last retained line.
        let (send, incoming) = broadcast::channel(RECORDING_QUEUE);
        let (finish, ended) = oneshot::channel();
        transcript.next_recording += 1;
        let id = transcript.next_recording;
        transcript.recording = Some(Recording {
            id,
            mode,
            incoming: send,
            ended: finish,
        });
        let stream = GroupTranscriptStream {
            history: transcript.snapshot.clone(),
            incoming,
            ended,
        };
        let recording = GroupTranscriptRecording {
            transcript: self.clone(),
            id,
        };
        Self::finish_if_settled(transcript);
        Some((recording, stream))
    }

    fn finish_if_settled(mut transcript: MutexGuard<'_, Transcript>) {
        if !transcript.settled() {
            return;
        }
        if transcript
            .recording
            .as_ref()
            .is_some_and(|recording| recording.mode == GroupRecordingMode::UntilSettled)
        {
            transcript.finish_recording(GroupRecordingEnd::Settled);
        }
        if let Some(notice) = transcript.completion_notice.take() {
            let succeeded = transcript
                .hosts
                .iter()
                .filter(|host| host.succeeded())
                .count();
            let attention = transcript.hosts.len() - succeeded;
            drop(transcript);
            notice.inbox.group_finished(
                notice.tab,
                notice.submission,
                &notice.source,
                succeeded,
                attention,
            );
        }
    }
}

impl Drop for GroupTranscriptHost {
    fn drop(&mut self) {
        let mut transcript = self.transcript.0.lock().unwrap();
        transcript.hosts[self.index].active = false;
        transcript.active_hosts -= 1;
        GroupTranscript::finish_if_settled(transcript);
    }
}

impl GroupTranscriptHost {
    pub fn hold_execution_slot(&mut self, permit: tokio::sync::OwnedSemaphorePermit) {
        self._execution_slot = Some(permit);
    }

    pub fn delivery_receipt(&self) -> GroupWriteReceipt {
        self.receipt.clone()
    }

    pub(crate) fn require_delivery(&mut self) -> GroupWriteReceipt {
        self.verify_delivery = true;
        self.delivery_receipt()
    }

    pub(crate) fn reject_delivery(&self, reason: &str) {
        self.receipt.reject(reason);
    }

    pub(super) fn set_execution(&self, execution: &GroupExecution) {
        self.transcript.0.lock().unwrap().hosts[self.index].execution = Some(execution.clone());
    }

    pub(super) fn cleared_for_output(&self) -> bool {
        !self.verify_delivery || self.receipt.acknowledged()
    }

    pub(super) fn append(&self, batch: LiveOutputBatch) {
        if batch.lines.is_empty() && !batch.gap {
            return;
        }
        let mut transcript = self.transcript.0.lock().unwrap();
        if batch.gap {
            transcript.push(
                self.pane,
                "A portion of this host’s output was skipped.",
                false,
            );
        }
        for line in batch.lines {
            transcript.push(self.pane, line.text, line.shortened);
        }
    }
}

impl Transcript {
    fn settled(&self) -> bool {
        self.dispatch_finished && self.active_hosts == 0
    }

    fn finish_recording(&mut self, reason: GroupRecordingEnd) {
        if let Some(recording) = self.recording.take() {
            // Publish the reason before closing the only line sender. The
            // writer reads it only after draining the remaining lines.
            let hosts = self
                .hosts
                .iter()
                .map(|host| (host.pane, host.outcome()))
                .collect();
            let _ = recording
                .ended
                .send(GroupRecordingSummary { end: reason, hosts });
        }
    }

    fn push(&mut self, pane: u64, text: impl Into<Arc<str>>, shortened: bool) {
        let snapshot = &mut self.snapshot;
        let id = snapshot.lines.back().map_or(1, |line| line.id + 1);
        if snapshot.lines.len() == TRANSCRIPT_LINES {
            snapshot.lines.pop_front();
            snapshot.omitted += 1;
        }
        let line = GroupTranscriptLine {
            id,
            pane,
            text: text.into(),
            shortened,
        };
        if let Some(recording) = &self.recording {
            let _ = recording.incoming.send(line.clone());
        }
        snapshot.lines.push_back(line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{CommandLink, GroupCommandResult, GroupOutput, NoticeTarget};
    use wezterm_term::LiveOutputLine;

    #[test]
    fn a_settled_prompt_without_an_exit_status_is_unsuccessful() {
        let group = GroupTranscript::default();
        let appliance = group.host(7);
        appliance.delivery_receipt().acknowledge();
        appliance.set_execution(&GroupExecution::PromptReturned);
        assert!(!group.has_unsuccessful_result());
        assert_eq!(group.settled_host_succeeded(7), None);
        drop(appliance);
        assert_eq!(group.settled_host_succeeded(7), Some(false));
        assert!(group.has_unsuccessful_result());
    }

    #[test]
    fn armed_completion_reports_the_final_recipient_outcomes_once() {
        let inbox = Inbox::new(|| {});
        let group = GroupTranscript::default();
        let east = group.host(1);
        let west = group.host(2);
        west.delivery_receipt().reject("Not ready");
        group.finish_dispatch();
        assert_eq!(group.completion_state(), GroupCompletionState::Available);
        group.set_completion_notice(true, &inbox, 8, 4, "Fleet");
        assert_eq!(group.completion_state(), GroupCompletionState::Armed);
        east.delivery_receipt().acknowledge();
        east.set_execution(&GroupExecution::Finished(Arc::new(GroupCommandResult {
            link: CommandLink {
                generation: 1,
                id: 2,
            },
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
            output: GroupOutput::Captured {
                text: String::new(),
                truncated: false,
            },
        })));
        assert_eq!(group.settled_host_succeeded(1), None);
        drop(east);
        assert_eq!(group.settled_host_succeeded(1), Some(true));
        assert_eq!(group.settled_host_succeeded(2), None);
        assert!(inbox.rows().is_empty());
        assert!(!group.has_unsuccessful_result());
        drop(west);
        assert!(group.has_unsuccessful_result());
        assert_eq!(group.completion_state(), GroupCompletionState::Settled);
        group.set_completion_notice(true, &inbox, 8, 4, "Fleet");
        let rows = inbox.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].target,
            NoticeTarget::GroupRun {
                tab: 8,
                submission: 4
            }
        );
        assert_eq!(
            rows[0].message,
            "Group run finished · 1 succeeded · 1 needs attention"
        );
        assert!(inbox.acknowledge_wake());
        assert!(inbox.mark_target_read(rows[0].target));
        assert_eq!(inbox.unread_count(), 0);
        assert!(!inbox.acknowledge_wake());
    }

    #[test]
    fn cancelling_the_completion_notice_keeps_a_settled_run_quiet() {
        let inbox = Inbox::new(|| {});
        let group = GroupTranscript::default();
        let host = group.host(1);
        group.finish_dispatch();
        group.set_completion_notice(true, &inbox, 8, 4, "Fleet");
        group.set_completion_notice(false, &inbox, 8, 4, "Fleet");
        drop(host);
        assert!(inbox.rows().is_empty());
    }

    #[test]
    fn live_transcript_keeps_recent_lines_in_arrival_order_and_snapshots_stay_fixed() {
        let transcript = GroupTranscript::default();
        let append = |pane, text: &str| {
            transcript.host(pane).append(LiveOutputBatch {
                lines: vec![LiveOutputLine {
                    text: text.to_owned(),
                    shortened: false,
                }],
                gap: false,
            });
        };
        append(2, "West first");
        let paused = transcript.snapshot();
        for _ in 0..TRANSCRIPT_LINES {
            append(1, "East output");
        }
        append(2, "West last");
        let current = transcript.snapshot();
        assert_eq!(current.omitted, 2);
        assert_eq!(current.lines.len(), TRANSCRIPT_LINES);
        assert_eq!(current.lines[0].id, 3);
        assert_eq!(current.lines[0].pane, 1);
        assert_eq!(current.lines.back().unwrap().pane, 2);
        assert_eq!(&*current.lines.back().unwrap().text, "West last");
        assert_eq!(paused.lines.len(), 1);
        assert_eq!(&*paused.lines[0].text, "West first");
    }

    #[test]
    fn recording_bridges_history_and_live_lines_and_drains_at_stop() {
        use tokio::sync::broadcast::error::TryRecvError;

        let transcript = GroupTranscript::default();
        let append = |pane, text: &str| {
            transcript.host(pane).append(LiveOutputBatch {
                lines: vec![LiveOutputLine {
                    text: text.to_owned(),
                    shortened: false,
                }],
                gap: false,
            });
        };
        append(1, "earlier");
        let (recording, mut stream) = transcript
            .start_recording(GroupRecordingMode::Manual)
            .unwrap();
        assert!(
            transcript
                .start_recording(GroupRecordingMode::Manual)
                .is_none()
        );
        append(2, "after launch");
        drop(recording);
        append(1, "after stop");
        assert_eq!(stream.history.lines.len(), 1);
        assert_eq!(stream.history.lines[0].id, 1);
        let line = stream.incoming.try_recv().unwrap();
        assert_eq!((line.id, line.pane, &*line.text), (2, 2, "after launch"));
        assert!(matches!(
            stream.incoming.try_recv(),
            Err(TryRecvError::Closed)
        ));
        assert_eq!(transcript.snapshot().lines.len(), 3);
        assert_eq!(
            stream.ended.try_recv().unwrap().end,
            GroupRecordingEnd::Stopped
        );
        assert!(
            transcript
                .start_recording(GroupRecordingMode::Manual)
                .is_some()
        );
    }

    #[test]
    fn automatic_recording_waits_for_dispatch_and_every_host_and_keeps_the_final_line() {
        use tokio::sync::broadcast::error::TryRecvError;

        let append = |host: &GroupTranscriptHost, text: &str| {
            host.append(LiveOutputBatch {
                lines: vec![LiveOutputLine {
                    text: text.to_owned(),
                    shortened: false,
                }],
                gap: false,
            });
        };
        let transcript = GroupTranscript::default();
        let (recording, mut stream) = transcript
            .start_recording(GroupRecordingMode::UntilSettled)
            .unwrap();
        let east = transcript.host(1);
        east.delivery_receipt().acknowledge();
        east.set_execution(&GroupExecution::PromptReturned);
        append(&east, "first");
        drop(east);
        assert_eq!(&*stream.incoming.try_recv().unwrap().text, "first");
        assert!(matches!(
            stream.incoming.try_recv(),
            Err(TryRecvError::Empty)
        ));

        let west = transcript.host(2);
        west.delivery_receipt().acknowledge();
        let rejected = transcript.host(3);
        rejected
            .delivery_receipt()
            .reject("The host requires authentication.");
        let unconfirmed = transcript.host(4);
        transcript.finish_dispatch();
        drop(rejected);
        drop(unconfirmed);
        assert!(matches!(
            stream.incoming.try_recv(),
            Err(TryRecvError::Empty)
        ));
        append(&west, "last");
        west.set_execution(&GroupExecution::Unavailable("The connection closed."));
        drop(west);
        assert_eq!(&*stream.incoming.try_recv().unwrap().text, "last");
        assert!(matches!(
            stream.incoming.try_recv(),
            Err(TryRecvError::Closed)
        ));
        let summary = stream.ended.try_recv().unwrap();
        assert_eq!(summary.end, GroupRecordingEnd::Settled);
        assert!(matches!(
            summary.hosts.as_slice(),
            [
                (1, GroupRecordingOutcome::Sent(Some(GroupExecution::PromptReturned))),
                (2, GroupRecordingOutcome::Sent(Some(GroupExecution::Unavailable("The connection closed.")))),
                (3, GroupRecordingOutcome::Rejected(reason)),
                (4, GroupRecordingOutcome::DeliveryUnconfirmed),
            ] if &**reason == "The host requires authentication."
        ));

        let (new_recording, mut manual) = transcript
            .start_recording(GroupRecordingMode::Manual)
            .unwrap();
        drop(recording);
        assert!(matches!(
            manual.incoming.try_recv(),
            Err(TryRecvError::Empty)
        ));
        drop(new_recording);
        assert_eq!(
            manual.ended.try_recv().unwrap().end,
            GroupRecordingEnd::Stopped
        );
    }
}
