use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use wezterm_term::LiveOutputBatch;

const TRANSCRIPT_LINES: usize = 128;

#[derive(Clone, Debug, Default)]
pub struct GroupTranscript(Arc<Mutex<GroupTranscriptSnapshot>>);

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

impl GroupTranscript {
    pub fn host(&self, pane: u64) -> GroupTranscriptHost {
        GroupTranscriptHost {
            transcript: self.clone(),
            pane,
        }
    }

    pub fn snapshot(&self) -> GroupTranscriptSnapshot {
        self.0.lock().unwrap().clone()
    }
}

impl GroupTranscriptHost {
    pub(super) fn append(&self, batch: LiveOutputBatch) {
        if batch.lines.is_empty() && !batch.gap {
            return;
        }
        let mut snapshot = self.transcript.0.lock().unwrap();
        if batch.gap {
            snapshot.push(
                self.pane,
                "A portion of this host’s output was skipped.",
                false,
            );
        }
        for line in batch.lines {
            snapshot.push(self.pane, line.text, line.shortened);
        }
    }
}

impl GroupTranscriptSnapshot {
    fn push(&mut self, pane: u64, text: impl Into<Arc<str>>, shortened: bool) {
        let id = self.lines.back().map_or(1, |line| line.id + 1);
        if self.lines.len() == TRANSCRIPT_LINES {
            self.lines.pop_front();
            self.omitted += 1;
        }
        self.lines.push_back(GroupTranscriptLine {
            id,
            pane,
            text: text.into(),
            shortened,
        });
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
}
