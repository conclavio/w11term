use super::{CommandLink, GroupTranscriptHost, application_id};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::watch;
use wezterm_term::{
    CompletedCommand, FinalTermPromptKind, FinalTermSemanticPrompt, LiveCommandOutput,
    LiveOutputCursor,
};

const PREVIEW_BYTES: usize = 8192;
const PREVIEW_LINES: usize = 32;

#[derive(Clone, Debug)]
pub enum GroupExecution {
    AwaitingMarkers,
    AwaitingPrompt,
    Running {
        target: GroupInterruptTarget,
        started_at: Instant,
    },
    PromptReturned,
    Finished(Arc<GroupCommandResult>),
    Unavailable(&'static str),
}

impl GroupExecution {
    pub fn shell_time(&self, now: Instant) -> Option<Duration> {
        match self {
            Self::Running { started_at, .. } => Some(now.saturating_duration_since(*started_at)),
            Self::Finished(result) => Some(result.duration),
            _ => None,
        }
    }
}

/// One verified group submission on a single shell stream. Capture IDs can
/// be reused after an incomplete command, so they cannot guard a write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupInterruptTarget {
    pub generation: u64,
    pub serial: u64,
}

#[derive(Debug)]
pub struct GroupCommandResult {
    pub link: CommandLink,
    pub exit_code: Option<i32>,
    pub duration: Duration,
    pub output: GroupOutput,
}

#[derive(Debug)]
pub enum GroupOutput {
    Captured { text: String, truncated: bool },
    Unavailable(&'static str),
}

impl From<&CompletedCommand> for GroupOutput {
    fn from(command: &CompletedCommand) -> Self {
        match &command.output {
            Ok(text) => {
                let max_bytes = text.floor_char_boundary(PREVIEW_BYTES.min(text.len()));
                let end = text[..max_bytes]
                    .match_indices('\n')
                    .nth(PREVIEW_LINES - 1)
                    .map_or(max_bytes, |(i, _)| i + 1);
                Self::Captured {
                    text: text[..end].to_owned(),
                    truncated: end < text.len(),
                }
            }
            Err(reason) => Self::Unavailable(reason),
        }
    }
}

#[derive(Debug)]
pub(super) struct GroupRun {
    target: GroupInterruptTarget,
    expected: Arc<str>,
    status: watch::Sender<GroupExecution>,
    started: Option<Started>,
    transcript: GroupTranscriptHost,
    output_cursor: LiveOutputCursor,
}

#[derive(Debug)]
struct Started {
    id: u64,
    aid: Option<[u8; 32]>,
    at: Instant,
}

impl GroupRun {
    pub(super) fn new(
        target: GroupInterruptTarget,
        expected: Arc<str>,
        status: watch::Sender<GroupExecution>,
        transcript: GroupTranscriptHost,
    ) -> Self {
        transcript.set_execution(&status.borrow());
        Self {
            target,
            expected,
            status,
            started: None,
            transcript,
            output_cursor: LiveOutputCursor::default(),
        }
    }

    pub(super) fn live_output(
        &mut self,
        id: u64,
        label: Option<&str>,
        output: LiveCommandOutput<'_>,
    ) {
        if self
            .started
            .as_ref()
            .is_some_and(|started| started.id == id)
            && label == Some(self.expected.as_ref())
            && self.is_running(self.target)
            && self.transcript.cleared_for_output()
        {
            self.transcript.append(output.read(&mut self.output_cursor));
        }
    }

    pub(super) fn abandon(self, reason: &'static str) {
        self.publish(GroupExecution::Unavailable(reason));
    }

    pub(super) fn prompt_returned(self) {
        self.publish(GroupExecution::PromptReturned);
    }

    pub(super) fn awaiting_prompt(&self) -> bool {
        matches!(*self.status.borrow(), GroupExecution::AwaitingPrompt)
    }

    pub(super) fn identify(&self, id: u64, label: Option<&str>) {
        if let Some(started) = self.started.as_ref().filter(|started| started.id == id) {
            self.publish(if label == Some(self.expected.as_ref()) {
                GroupExecution::Running {
                    target: self.target,
                    started_at: started.at,
                }
            } else {
                GroupExecution::AwaitingMarkers
            });
        }
    }

    fn publish(&self, execution: GroupExecution) {
        self.transcript.set_execution(&execution);
        self.status.send_replace(execution);
    }

    pub(super) fn is_running(&self, target: GroupInterruptTarget) -> bool {
        !self.status.is_closed()
            && matches!(*self.status.borrow(), GroupExecution::Running { target: current, .. } if current == target)
    }

    /// Keep observing only while this capture can still be attributed to the submitted line.
    pub(super) fn marker(
        &mut self,
        marker: &FinalTermSemanticPrompt,
        started: Option<(u64, Option<&str>)>,
        completed: Option<&CompletedCommand>,
        now: Instant,
    ) -> bool {
        use FinalTermSemanticPrompt::*;
        if self.status.is_closed() {
            return false;
        }
        if let MarkEndOfInputAndStartOfOutput { aid, .. } = marker
            && let Some((id, label)) = started
        {
            if self.started.is_some() {
                self.publish(GroupExecution::Unavailable(
                    "Overlapping shell markers interrupted the command capture.",
                ));
                return false;
            }
            self.started = Some(Started {
                id,
                aid: application_id(aid.as_deref()),
                at: now,
            });
            self.identify(id, label);
            return true;
        }
        let fresh_prompt = matches!(
            marker,
            FreshLineAndStartPrompt { .. }
                | MarkEndOfCommandWithFreshLine { .. }
                | StartPrompt(FinalTermPromptKind::Initial)
                | MarkEndOfPromptAndStartOfInputUntilNextMarker
                | MarkEndOfPromptAndStartOfInputUntilEndOfLine
        );
        if let Some(command) = completed {
            let matched = self.started.as_ref().filter(|started| {
                started.id == command.id
                    && command.command.as_deref() == Some(self.expected.as_ref())
                    && match marker {
                        CommandStatus { aid, .. } => started.aid == application_id(aid.as_deref()),
                        _ => fresh_prompt,
                    }
            });
            if let Some(started) = matched {
                self.publish(GroupExecution::Finished(Arc::new(GroupCommandResult {
                    link: CommandLink {
                        generation: self.target.generation,
                        id: command.id,
                    },
                    exit_code: command.exit_code,
                    duration: now.saturating_duration_since(started.at),
                    output: command.into(),
                })));
            } else {
                self.publish(GroupExecution::Unavailable(
                    "The shell did not confirm the submitted command line.",
                ));
            }
            return false;
        }
        if fresh_prompt {
            self.publish(GroupExecution::Unavailable(
                "The shell returned to a prompt without a matching result.",
            ));
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_from_the_start_marker_even_when_the_command_is_identified_later() {
        use FinalTermSemanticPrompt::{CommandStatus, MarkEndOfInputAndStartOfOutput};
        let start = Instant::now();
        let finished = start + Duration::from_millis(1350);
        let (status, observation) = watch::channel(GroupExecution::AwaitingMarkers);
        let mut run = GroupRun::new(
            GroupInterruptTarget {
                generation: 1,
                serial: 3,
            },
            "report".into(),
            status,
            crate::events::GroupTranscript::default().host(1),
        );
        assert!(run.marker(
            &MarkEndOfInputAndStartOfOutput {
                aid: Some("shell".into()),
                command: None,
            },
            Some((7, None)),
            None,
            start,
        ));
        assert_eq!(observation.borrow().shell_time(start), None);
        run.identify(7, Some("report"));
        assert_eq!(
            observation
                .borrow()
                .shell_time(start + Duration::from_millis(600)),
            Some(Duration::from_millis(600)),
        );
        assert!(!run.marker(
            &CommandStatus {
                status: Some(17),
                aid: Some("shell".into()),
            },
            None,
            Some(&CompletedCommand {
                id: 7,
                finished_at: finished,
                command: Some("report".into()),
                exit_code: Some(17),
                output: Ok(String::new()),
            }),
            finished,
        ));
        assert!(matches!(
            &*observation.borrow(),
            GroupExecution::Finished(result) if result.exit_code == Some(17)
        ));
        assert_eq!(
            observation
                .borrow()
                .shell_time(finished + Duration::from_secs(60)),
            Some(Duration::from_millis(1350)),
        );
    }

    #[test]
    fn group_preview_bounds_lines_and_preserves_utf8() {
        let mut command = CompletedCommand {
            id: 7,
            finished_at: std::time::Instant::now(),
            command: None,
            exit_code: None,
            output: Ok("夜".repeat(PREVIEW_BYTES)),
        };
        assert!(
            matches!(GroupOutput::from(&command), GroupOutput::Captured { text, truncated: true }
            if text.len() <= PREVIEW_BYTES && text.chars().all(|c| c == '夜'))
        );
        command.output = Ok("row\r\n".repeat(100));
        assert!(
            matches!(GroupOutput::from(&command), GroupOutput::Captured { text, truncated: true }
            if text.lines().count() == PREVIEW_LINES)
        );
    }
}
