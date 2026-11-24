use super::{CommandLink, GroupExecution, GroupOutput, Submission, execution_caption};
use anyhow::{Context, Result};
use std::{fmt::Write as _, io, sync::Arc};
use w11term::events::GroupCommandResult;
use wezterm_term::CompletedCommand;

mod json;

const OUTPUT_END: &str = "\r\n";
const OUTPUT_SHORTENED: &str = "[Output shortened to fit the clipboard]\r\n";
const OUTPUT_PREVIEW: &str = "  Output preview · full output unavailable\r\n";

pub(super) struct Report {
    id: u64,
    label: String,
    kind: json::Kind,
    sections: Vec<Section>,
}

struct Section {
    heading: String,
    metadata: json::Recipient,
    output: Option<CapturedOutput>,
}

enum CapturedOutput {
    Empty,
    Captured(Output),
    Unavailable(&'static str),
}

impl Section {
    fn text_output(&self) -> Option<&Output> {
        match &self.output {
            Some(CapturedOutput::Captured(output)) => Some(output),
            _ => None,
        }
    }
}

pub(super) struct Output {
    result: Arc<GroupCommandResult>,
    retained: Option<Arc<CompletedCommand>>,
}

impl Output {
    pub(super) fn capture(
        result: Arc<GroupCommandResult>,
        retained: impl FnOnce(CommandLink) -> Option<Arc<CompletedCommand>>,
    ) -> Option<Self> {
        let GroupOutput::Captured { truncated, .. } = &result.output else {
            return None;
        };
        let full = truncated
            .then(|| retained(result.link))
            .flatten()
            .filter(|command| command.output.is_ok());
        Some(Self {
            result,
            retained: full,
        })
    }

    pub(super) fn complete(&self) -> Option<&str> {
        let preview = matches!(
            &self.result.output,
            GroupOutput::Captured {
                truncated: true,
                ..
            }
        );
        (!preview || self.retained.is_some()).then(|| self.text())
    }

    pub(super) fn text(&self) -> &str {
        if let Some(retained) = &self.retained
            && let Ok(text) = &retained.output
        {
            return text;
        }
        match &self.result.output {
            GroupOutput::Captured { text, .. } => text,
            GroupOutput::Unavailable(_) => "",
        }
    }
}

impl Report {
    pub(super) fn capture(
        submission: &Submission,
        mut retained: impl FnMut(u64, CommandLink) -> Option<Arc<CompletedCommand>>,
    ) -> Self {
        let sections = submission
            .results
            .iter()
            .map(|delivery| {
                let mut heading = format!("\r\n{}\r\n", delivery.description());
                if let Some(command) = submission.content.host_command(delivery.target.pane) {
                    let _ = writeln!(heading, "  {command}\r");
                }
                let execution = delivery.execution();
                if let Some(execution) = &execution {
                    let _ = writeln!(heading, "  {}\r", execution_caption(execution));
                }
                if let Some(interrupt) = delivery.interrupt_caption() {
                    let _ = writeln!(heading, "  {interrupt}\r");
                }
                let output = if let Some(GroupExecution::Finished(result)) = &execution {
                    match &result.output {
                        GroupOutput::Captured { text, .. } if text.is_empty() => {
                            heading.push_str("  No output captured\r\n");
                            Some(CapturedOutput::Empty)
                        }
                        GroupOutput::Captured { .. } => {
                            let output = Output::capture(result.clone(), |link| {
                                retained(delivery.target.pane, link)
                            });
                            if let Some(captured) = &output {
                                heading.push_str(if captured.complete().is_some() {
                                    "  Output\r\n"
                                } else {
                                    OUTPUT_PREVIEW
                                });
                            }
                            output.map(CapturedOutput::Captured)
                        }
                        GroupOutput::Unavailable(reason) => {
                            let _ = writeln!(heading, "  Output unavailable · {reason}\r");
                            Some(CapturedOutput::Unavailable(reason))
                        }
                    }
                } else {
                    None
                };
                Section {
                    heading,
                    metadata: json::Recipient::capture(
                        delivery,
                        &submission.content,
                        execution.as_ref(),
                    ),
                    output,
                }
            })
            .collect();

        Self {
            id: submission.id,
            label: submission.content.label().to_owned(),
            kind: if submission.content.command().is_some() {
                json::Kind::Command
            } else {
                json::Kind::Environment
            },
            sections,
        }
    }

    fn text_title(&self) -> String {
        format!("Submitted · {}\r\n", self.label)
    }

    pub(super) fn clipboard(&self, limit: usize) -> Result<String> {
        let mut report = self.text_title();
        let outputs = self.sections.iter().filter_map(Section::text_output);
        let output_count = outputs.clone().count();
        let text_bytes: usize = outputs.map(|output| output.text().len()).sum();
        let metadata_bytes = report.len()
            + output_count * OUTPUT_END.len()
            + self
                .sections
                .iter()
                .map(|section| section.heading.len())
                .sum::<usize>();
        let mut available = limit
            .checked_sub(metadata_bytes)
            .context("Host statuses exceed the clipboard limit")?;
        let shorten = text_bytes > available;
        if shorten {
            available = available
                .checked_sub(output_count * OUTPUT_SHORTENED.len())
                .context("Host statuses and output notices exceed the clipboard limit")?;
        }
        let mut outputs_left = output_count;
        for section in &self.sections {
            report.push_str(&section.heading);
            if let Some(output) = section.text_output() {
                let text = output.text();
                let end = if shorten {
                    text.floor_char_boundary(text.len().min(available / outputs_left))
                } else {
                    text.len()
                };
                report.push_str(&text[..end]);
                report.push_str(OUTPUT_END);
                if end < text.len() {
                    report.push_str(OUTPUT_SHORTENED);
                }
                available -= end;
                outputs_left -= 1;
            }
        }
        Ok(report)
    }

    pub(super) fn write_to(&self, file: &mut impl io::Write) -> io::Result<()> {
        file.write_all(self.text_title().as_bytes())?;
        for section in &self.sections {
            file.write_all(section.heading.as_bytes())?;
            if let Some(output) = section.text_output() {
                file.write_all(output.text().as_bytes())?;
                file.write_all(OUTPUT_END.as_bytes())?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::host_groups::{
        Delivery, DeliveryStatus, GroupInterruptTarget, SubmissionContent, Target, TargetState,
        filter::Names,
    };
    use std::{collections::BTreeMap, time::Instant};
    use tokio::sync::{oneshot, watch};

    fn captured(pane: u64, text: String, truncated: bool) -> Delivery {
        let capture = GroupCommandResult {
            link: CommandLink {
                generation: 2,
                id: 3,
            },
            exit_code: Some(17),
            output: GroupOutput::Captured { text, truncated },
        };
        let (_, execution) = watch::channel(GroupExecution::Finished(Arc::new(capture)));
        Delivery {
            target: Target {
                pane,
                name: format!("Host {pane}"),
                names: Names::new(&format!("Host {pane}"), "lab.example", "ops", 2222),
                profile: None,
                connected: true,
                state: TargetState::InUse,
            },
            status: DeliveryStatus::Sent,
            execution: Some(execution),
            interrupt: None,
        }
    }

    fn json(report: &Report) -> serde_json::Value {
        let mut bytes = Vec::new();
        report.write_json(&mut bytes).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn report_uses_exact_history_or_labels_the_retained_preview() {
        let submission = Submission {
            id: 1,
            content: SubmissionContent::Command("show".into()),
            recording: Default::default(),
            saved_recordings: Default::default(),
            transcript: Default::default(),
            results: vec![
                captured(7, "preview".into(), true),
                captured(8, "missing tail".into(), true),
            ],
        };
        let snapshot = Report::capture(&submission, |pane, link| {
            assert_eq!(
                link,
                CommandLink {
                    generation: 2,
                    id: 3
                }
            );
            (pane == 7).then(|| {
                Arc::new(CompletedCommand {
                    id: link.id,
                    command: Some("show".into()),
                    exit_code: Some(17),
                    output: Ok("full output\r\nsecond line".into()),
                })
            })
        });
        let report = snapshot.clipboard(1024).unwrap();
        assert!(report.contains(
            "Host 7 · Sent to SSH channel\r\n  Exit 17\r\n  Output\r\nfull output\r\nsecond line"
        ));
        assert!(report.contains("Host 8 · Sent to SSH channel\r\n  Exit 17\r\n  Output preview · full output unavailable\r\nmissing tail"));
        assert!(!report.contains("\r\npreview"));
        let mut file = Vec::new();
        snapshot.write_to(&mut file).unwrap();
        assert_eq!(file, report.as_bytes());
        let document = json(&snapshot);
        assert_eq!(document["schema_version"], 1);
        assert_eq!(document["submission_id"], 1);
        assert_eq!(document["kind"], "command");
        assert_eq!(document["command"], "show");
        let hosts = document["recipients"].as_array().unwrap();
        assert_eq!(hosts[0]["name"], "Host 7");
        assert_eq!(hosts[0]["address"], "ops@lab.example:2222");
        assert_eq!(hosts[0]["delivery"]["state"], "sent");
        assert_eq!(hosts[0]["execution"]["exit_code"], 17);
        assert_eq!(hosts[0]["execution"]["command_id"], 3);
        assert_eq!(hosts[0]["output"]["state"], "complete");
        assert_eq!(hosts[0]["output"]["text"], "full output\r\nsecond line");
        assert_eq!(hosts[1]["output"]["state"], "preview");
        assert_eq!(hosts[1]["output"]["text"], "missing tail");
    }

    #[test]
    fn report_limits_unicode_output_and_keeps_all_host_statuses() {
        let mut other = captured(9, String::new(), false);
        other.status = DeliveryStatus::Rejected("SSH connection closed".into());
        let submission = Submission {
            id: 1,
            content: SubmissionContent::Command("show".into()),
            recording: Default::default(),
            saved_recordings: Default::default(),
            transcript: Default::default(),
            results: vec![
                captured(7, "雪".repeat(400), false),
                captured(8, "月".repeat(400), false),
                other,
            ],
        };
        let snapshot = Report::capture(&submission, |_, _| None);
        let report = snapshot.clipboard(440).unwrap();
        assert!(report.len() <= 440);
        assert_eq!(report.matches(OUTPUT_SHORTENED).count(), 2);
        assert!(report.contains("雪雪") && report.contains("月月"));
        assert!(report.contains("Host 9 · Not sent · SSH connection closed"));
        assert!(snapshot.clipboard(100).is_err());
        let mut file = Vec::new();
        snapshot.write_to(&mut file).unwrap();
        let file = String::from_utf8(file).unwrap();
        assert!(file.contains(&"雪".repeat(400)) && file.contains(&"月".repeat(400)));
        assert!(!file.contains(OUTPUT_SHORTENED));
        let document = json(&snapshot);
        assert_eq!(
            document["recipients"][0]["output"]["text"],
            "雪".repeat(400)
        );
        let withheld = &document["recipients"][2];
        assert_eq!(withheld["delivery"]["state"], "rejected");
        assert_eq!(withheld["delivery"]["reason"], "SSH connection closed");
        assert!(withheld["execution"].is_null() && withheld["output"].is_null());
    }

    #[test]
    fn json_preserves_host_assignments_and_distinguishes_running_pending_and_empty_output() {
        let mut running = captured(8, String::new(), false);
        let (_, execution) = watch::channel(GroupExecution::Running(GroupInterruptTarget {
            generation: 1,
            serial: 4,
        }));
        running.execution = Some(execution);
        running.interrupt = Some(DeliveryStatus::Unconfirmed);
        let mut pending = captured(9, String::new(), false);
        let (_, reply) = oneshot::channel();
        pending.status = DeliveryStatus::Pending(Instant::now(), reply);
        let submission = Submission {
            id: 4,
            content: SubmissionContent::Environment(BTreeMap::from([
                (7, "export POLYSH_RANK=0".into()),
                (8, "export POLYSH_RANK=1".into()),
                (9, "export POLYSH_RANK=2".into()),
            ])),
            recording: Default::default(),
            saved_recordings: Default::default(),
            transcript: Default::default(),
            results: vec![captured(7, String::new(), false), running, pending],
        };
        let snapshot = Report::capture(&submission, |_, _| None);
        let document = json(&snapshot);
        assert_eq!(document["kind"], "environment");
        assert!(document["command"].is_null());
        let hosts = document["recipients"].as_array().unwrap();
        assert_eq!(hosts[0]["pane_id"], 7);
        assert_eq!(hosts[0]["command"], "export POLYSH_RANK=0");
        assert_eq!(
            hosts[0]["output"],
            serde_json::json!({"state": "complete", "text": ""})
        );
        assert_eq!(hosts[1]["execution"]["state"], "running");
        assert_eq!(hosts[1]["interrupt"]["state"], "unconfirmed");
        assert!(hosts[1]["output"].is_null());
        assert_eq!(hosts[2]["command"], "export POLYSH_RANK=2");
        assert_eq!(hosts[2]["delivery"]["state"], "pending");
        assert!(hosts[2]["execution"].is_null() && hosts[2]["output"].is_null());
    }
}
