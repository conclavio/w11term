use super::RunningCommandTarget;
use regex::Regex;
use serde::Serialize;
use tokio::sync::oneshot;
use wezterm_term::{LiveCommandOutput, LiveOutputCursor};

#[derive(Debug, Serialize)]
pub struct RunningOutputMatch {
    pub id: u64,
    pub start_token: u64,
    pub connection_generation: u64,
    pub text: String,
}

pub type RunningOutputResult = Result<RunningOutputMatch, &'static str>;

#[derive(Debug)]
pub(super) struct Waiter {
    pub(super) target: RunningCommandTarget,
    pub(super) reply: oneshot::Sender<RunningOutputResult>,
    pub(super) pattern: Regex,
    pub(super) cursor: LiveOutputCursor,
}

impl Waiter {
    pub(super) fn observe(
        &mut self,
        output: &LiveCommandOutput<'_>,
    ) -> Option<RunningOutputResult> {
        let batch = output.read(&mut self.cursor);
        if batch.gap {
            return Some(Err(
                "running command output is incomplete: earlier rows were lost or reflowed",
            ));
        }
        for line in batch.lines {
            if line.shortened {
                return Some(Err(
                    "running command output is incomplete: a line exceeded 1024 UTF-8 bytes",
                ));
            }
            if self.pattern.is_match(&line.text) {
                return Some(Ok(RunningOutputMatch {
                    id: self.target.id,
                    connection_generation: self.target.generation,
                    start_token: self.target.start_token,
                    text: line.text,
                }));
            }
        }
        output
            .is_finishing()
            .then_some(Err("command finished without a matching output line"))
    }
}
