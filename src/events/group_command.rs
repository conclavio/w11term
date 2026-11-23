use super::{CommandLink, application_id};
use std::sync::Arc;
use tokio::sync::watch;
use wezterm_term::{CompletedCommand, FinalTermPromptKind, FinalTermSemanticPrompt};

const PREVIEW_BYTES: usize = 8192;
const PREVIEW_LINES: usize = 32;

#[derive(Clone, Debug)]
pub enum GroupExecution {
    AwaitingMarkers,
    Running,
    Finished(Arc<GroupCommandResult>),
    Unavailable(&'static str),
}

#[derive(Debug)]
pub struct GroupCommandResult {
    pub link: CommandLink,
    pub exit_code: Option<i32>,
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
    generation: u64,
    expected: Arc<str>,
    status: watch::Sender<GroupExecution>,
    started: Option<Started>,
}

#[derive(Debug)]
struct Started {
    id: u64,
    aid: Option<[u8; 32]>,
}

impl GroupRun {
    pub(super) fn new(
        generation: u64,
        expected: Arc<str>,
        status: watch::Sender<GroupExecution>,
    ) -> Self {
        Self {
            generation,
            expected,
            status,
            started: None,
        }
    }

    pub(super) fn abandon(self, reason: &'static str) {
        self.status
            .send_replace(GroupExecution::Unavailable(reason));
    }

    pub(super) fn identify(&self, id: u64, label: Option<&str>) {
        if self
            .started
            .as_ref()
            .is_some_and(|started| started.id == id)
        {
            self.status
                .send_replace(if label == Some(self.expected.as_ref()) {
                    GroupExecution::Running
                } else {
                    GroupExecution::AwaitingMarkers
                });
        }
    }

    /// Keep observing only while this capture can still be attributed to the submitted line.
    pub(super) fn marker(
        &mut self,
        marker: &FinalTermSemanticPrompt,
        started: Option<(u64, Option<&str>)>,
        completed: Option<&CompletedCommand>,
    ) -> bool {
        use FinalTermSemanticPrompt::*;
        if self.status.is_closed() {
            return false;
        }
        if let MarkEndOfInputAndStartOfOutput { aid, .. } = marker
            && let Some((id, label)) = started
        {
            if self.started.is_some() {
                self.status.send_replace(GroupExecution::Unavailable(
                    "Overlapping shell markers interrupted the command capture.",
                ));
                return false;
            }
            self.started = Some(Started {
                id,
                aid: application_id(aid.as_deref()),
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
            let matched = self.started.as_ref().is_some_and(|started| {
                started.id == command.id
                    && command.command.as_deref() == Some(self.expected.as_ref())
                    && match marker {
                        CommandStatus { aid, .. } => started.aid == application_id(aid.as_deref()),
                        _ => fresh_prompt,
                    }
            });
            if matched {
                self.status
                    .send_replace(GroupExecution::Finished(Arc::new(GroupCommandResult {
                        link: CommandLink {
                            generation: self.generation,
                            id: command.id,
                        },
                        exit_code: command.exit_code,
                        output: command.into(),
                    })));
            } else {
                self.status.send_replace(GroupExecution::Unavailable(
                    "The shell did not confirm the submitted command line.",
                ));
            }
            return false;
        }
        if fresh_prompt {
            self.status.send_replace(GroupExecution::Unavailable(
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
    fn group_preview_bounds_lines_and_preserves_utf8() {
        let mut command = CompletedCommand {
            id: 7,
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
