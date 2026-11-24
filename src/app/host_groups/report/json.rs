use super::{CapturedOutput, Report};
use crate::app::host_groups::{Delivery, DeliveryStatus, GroupExecution, SubmissionContent};
use serde::Serialize;
use std::io::{self, Write};

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Kind {
    Command,
    Environment,
}

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum DeliveryState {
    Pending,
    Sent,
    Rejected { reason: String },
    Unconfirmed,
}

impl From<&DeliveryStatus> for DeliveryState {
    fn from(status: &DeliveryStatus) -> Self {
        match status {
            DeliveryStatus::Pending(..) => Self::Pending,
            DeliveryStatus::Sent => Self::Sent,
            DeliveryStatus::Rejected(reason) => Self::Rejected {
                reason: reason.clone(),
            },
            DeliveryStatus::Unconfirmed => Self::Unconfirmed,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum Execution {
    AwaitingMarkers,
    AwaitingPrompt,
    Running,
    PromptReturned,
    Completed {
        exit_code: Option<i32>,
        connection_generation: u64,
        command_id: u64,
    },
    Unavailable {
        reason: &'static str,
    },
}

impl From<&GroupExecution> for Execution {
    fn from(execution: &GroupExecution) -> Self {
        match execution {
            GroupExecution::AwaitingMarkers => Self::AwaitingMarkers,
            GroupExecution::AwaitingPrompt => Self::AwaitingPrompt,
            GroupExecution::Running(_) => Self::Running,
            GroupExecution::PromptReturned => Self::PromptReturned,
            GroupExecution::Finished(result) => Self::Completed {
                exit_code: result.exit_code,
                connection_generation: result.link.generation,
                command_id: result.link.id,
            },
            GroupExecution::Unavailable(reason) => Self::Unavailable { reason },
        }
    }
}

#[derive(Serialize)]
pub(super) struct Recipient {
    pane_id: u64,
    name: String,
    profile: Option<String>,
    address: String,
    command: Option<String>,
    delivery: DeliveryState,
    execution: Option<Execution>,
    interrupt: Option<DeliveryState>,
}

impl Recipient {
    pub(super) fn capture(
        delivery: &Delivery,
        content: &SubmissionContent,
        execution: Option<&GroupExecution>,
    ) -> Self {
        let target = &delivery.target;
        Self {
            pane_id: target.pane,
            name: target.names.label.clone(),
            profile: target.profile.clone(),
            address: target.names.address().to_owned(),
            command: content.host_command(target.pane).map(str::to_owned),
            delivery: DeliveryState::from(&delivery.status),
            execution: execution.map(Execution::from),
            interrupt: delivery.interrupt.as_ref().map(DeliveryState::from),
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum Output<'a> {
    Complete { text: &'a str },
    Preview { text: &'a str },
    Unavailable { reason: &'static str },
}

impl<'a> From<&'a CapturedOutput> for Output<'a> {
    fn from(capture: &'a CapturedOutput) -> Self {
        match capture {
            CapturedOutput::Empty => Self::Complete { text: "" },
            CapturedOutput::Captured(output) => match output.complete() {
                Some(text) => Self::Complete { text },
                None => Self::Preview {
                    text: output.text(),
                },
            },
            CapturedOutput::Unavailable(reason) => Self::Unavailable { reason },
        }
    }
}

#[derive(Serialize)]
struct DocumentRecipient<'a> {
    #[serde(flatten)]
    recipient: &'a Recipient,
    output: Option<Output<'a>>,
}

#[derive(Serialize)]
struct Document<'a> {
    schema_version: u32,
    submission_id: u64,
    kind: Kind,
    label: &'a str,
    command: Option<&'a str>,
    recipients: Vec<DocumentRecipient<'a>>,
}

impl Report {
    pub(in crate::app::host_groups) fn write_json(
        &self,
        file: &mut impl io::Write,
    ) -> io::Result<()> {
        let document = Document {
            schema_version: 1,
            submission_id: self.id,
            kind: self.kind,
            label: &self.label,
            command: matches!(self.kind, Kind::Command).then_some(self.label.as_str()),
            recipients: self
                .sections
                .iter()
                .map(|section| DocumentRecipient {
                    recipient: &section.metadata,
                    output: section.output.as_ref().map(Output::from),
                })
                .collect(),
        };
        let mut buffered = io::BufWriter::new(file);
        serde_json::to_writer_pretty(&mut buffered, &document)?;
        buffered.write_all(b"\n")?;
        buffered.flush()
    }
}
