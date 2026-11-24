use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
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
    hosts: usize,
    dispatch_finished: bool,
}

#[derive(Debug)]
struct Recording {
    id: u64,
    mode: GroupRecordingMode,
    incoming: broadcast::Sender<GroupTranscriptLine>,
    ended: oneshot::Sender<GroupRecordingEnd>,
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
    receipt: Option<GroupWriteReceipt>,
}

#[derive(Clone, Debug, Default)]
pub struct GroupWriteReceipt(Arc<AtomicBool>);

impl GroupWriteReceipt {
    pub(crate) fn acknowledge(&self) {
        self.0.store(true, Ordering::Release);
    }

    fn acknowledged(&self) -> bool {
        self.0.load(Ordering::Acquire)
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
    pub ended: oneshot::Receiver<GroupRecordingEnd>,
}

impl GroupTranscript {
    pub fn host(&self, pane: u64) -> GroupTranscriptHost {
        let mut transcript = self.0.lock().unwrap();
        debug_assert!(!transcript.dispatch_finished);
        transcript.hosts += 1;
        GroupTranscriptHost {
            transcript: self.clone(),
            pane,
            receipt: None,
        }
    }

    /// All hosts have been offered the submission. A host remains tracked
    /// while its command is queued or being observed by its session.
    pub fn finish_dispatch(&self) {
        let mut transcript = self.0.lock().unwrap();
        transcript.dispatch_finished = true;
        transcript.finish_if_settled();
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
        transcript.finish_if_settled();
        Some((recording, stream))
    }
}

impl Drop for GroupTranscriptHost {
    fn drop(&mut self) {
        let mut transcript = self.transcript.0.lock().unwrap();
        transcript.hosts -= 1;
        transcript.finish_if_settled();
    }
}

impl GroupTranscriptHost {
    pub(crate) fn require_delivery(&mut self) -> GroupWriteReceipt {
        let receipt = GroupWriteReceipt::default();
        self.receipt = Some(receipt.clone());
        receipt
    }

    pub(super) fn cleared_for_output(&self) -> bool {
        self.receipt
            .as_ref()
            .is_none_or(GroupWriteReceipt::acknowledged)
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
    fn finish_if_settled(&mut self) {
        if self.dispatch_finished
            && self.hosts == 0
            && self
                .recording
                .as_ref()
                .is_some_and(|recording| recording.mode == GroupRecordingMode::UntilSettled)
        {
            self.finish_recording(GroupRecordingEnd::Settled);
        }
    }

    fn finish_recording(&mut self, reason: GroupRecordingEnd) {
        if let Some(recording) = self.recording.take() {
            // Publish the reason before closing the only line sender. The
            // writer reads it only after draining the remaining lines.
            let _ = recording.ended.send(reason);
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
    use wezterm_term::LiveOutputLine;

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
        assert_eq!(stream.ended.try_recv().unwrap(), GroupRecordingEnd::Stopped);
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
        append(&east, "first");
        drop(east);
        assert_eq!(&*stream.incoming.try_recv().unwrap().text, "first");
        assert!(matches!(
            stream.incoming.try_recv(),
            Err(TryRecvError::Empty)
        ));

        let west = transcript.host(2);
        let rejected = transcript.host(3);
        transcript.finish_dispatch();
        drop(rejected);
        assert!(matches!(
            stream.incoming.try_recv(),
            Err(TryRecvError::Empty)
        ));
        append(&west, "last");
        drop(west);
        assert_eq!(&*stream.incoming.try_recv().unwrap().text, "last");
        assert!(matches!(
            stream.incoming.try_recv(),
            Err(TryRecvError::Closed)
        ));
        assert_eq!(stream.ended.try_recv().unwrap(), GroupRecordingEnd::Settled);

        let (new_recording, mut manual) = transcript
            .start_recording(GroupRecordingMode::Manual)
            .unwrap();
        drop(recording);
        assert!(matches!(
            manual.incoming.try_recv(),
            Err(TryRecvError::Empty)
        ));
        drop(new_recording);
        assert_eq!(manual.ended.try_recv().unwrap(), GroupRecordingEnd::Stopped);
    }
}
