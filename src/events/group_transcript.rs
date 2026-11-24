use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;
use wezterm_term::LiveOutputBatch;

const TRANSCRIPT_LINES: usize = 128;
const RECORDING_QUEUE: usize = 1024;

#[derive(Clone, Debug, Default)]
pub struct GroupTranscript(Arc<Mutex<Transcript>>);

#[derive(Debug, Default)]
struct Transcript {
    snapshot: GroupTranscriptSnapshot,
    recording: Option<broadcast::Sender<GroupTranscriptLine>>,
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
}

pub struct GroupTranscriptRecording {
    transcript: GroupTranscript,
}

impl Drop for GroupTranscriptRecording {
    fn drop(&mut self) {
        // Close the only sender. The file writer can drain its last queued
        // lines before it reports that the recording has stopped.
        self.transcript.0.lock().unwrap().recording = None;
    }
}

pub struct GroupTranscriptStream {
    pub history: GroupTranscriptSnapshot,
    pub incoming: broadcast::Receiver<GroupTranscriptLine>,
}

impl GroupTranscript {
    pub fn host(&self, pane: u64) -> GroupTranscriptHost {
        GroupTranscriptHost {
            transcript: self.clone(),
            pane,
        }
    }

    pub fn snapshot(&self) -> GroupTranscriptSnapshot {
        self.0.lock().unwrap().snapshot.clone()
    }

    pub fn start_recording(&self) -> Option<(GroupTranscriptRecording, GroupTranscriptStream)> {
        let mut transcript = self.0.lock().unwrap();
        if transcript.recording.is_some() {
            return None;
        }
        // Take the history and subscribe under the same lock so the first live
        // line comes immediately after the last retained line.
        let (send, incoming) = broadcast::channel(RECORDING_QUEUE);
        transcript.recording = Some(send);
        let stream = GroupTranscriptStream {
            history: transcript.snapshot.clone(),
            incoming,
        };
        let recording = GroupTranscriptRecording {
            transcript: self.clone(),
        };
        Some((recording, stream))
    }
}

impl GroupTranscriptHost {
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
            let _ = recording.send(line.clone());
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
        let (recording, mut stream) = transcript.start_recording().unwrap();
        assert!(transcript.start_recording().is_none());
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
        assert!(transcript.start_recording().is_some());
    }
}
