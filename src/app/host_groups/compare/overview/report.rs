use super::*;
use anyhow::{Result, ensure};
use serde::Serialize;
use std::{fmt::Write as _, io, io::Write as _};

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    earlier: Run,
    current: Run,
    counts: Counts,
    hosts: Vec<Recipient>,
}

#[derive(Serialize)]
struct Run {
    run_id: u64,
    label: String,
}

impl From<&Submission> for Run {
    fn from(run: &Submission) -> Self {
        Self {
            run_id: run.id,
            label: run.content.label().to_owned(),
        }
    }
}

#[derive(Serialize)]
struct Recipient {
    pane_id: u64,
    name: String,
    address: String,
    change: Change,
    earlier_exit_code: Option<i32>,
    current_exit_code: Option<i32>,
    detail: Option<String>,
}

impl Report {
    fn capture(earlier: &Submission, current: &Submission, rows: &[Row]) -> Self {
        Self {
            schema_version: 1,
            earlier: earlier.into(),
            current: current.into(),
            counts: Counts::capture(rows),
            hosts: rows
                .iter()
                .map(|row| {
                    let (earlier_exit_code, current_exit_code) = row
                        .sides
                        .as_ref()
                        .map_or((None, None), |(earlier, current)| {
                            (Some(earlier.exit_code), Some(current.exit_code))
                        });
                    Recipient {
                        pane_id: row.pane,
                        name: row.names.label.clone(),
                        address: row.names.address().to_owned(),
                        change: row.change,
                        earlier_exit_code,
                        current_exit_code,
                        detail: row.detail.clone(),
                    }
                })
                .collect(),
        }
    }

    fn text(&self, limit: usize) -> Result<String> {
        let mut text = format!(
            "SSH group run comparison\r\nEarlier · Run {} · {}\r\nCurrent · Run {} · {}\r\n{}\r\n",
            self.earlier.run_id,
            self.earlier.label,
            self.current.run_id,
            self.current.label,
            self.counts.caption(),
        );
        for host in &self.hosts {
            writeln!(
                text,
                "\r\n{} · {} · Pane {} · {}\r",
                host.name,
                host.address,
                host.pane_id,
                host.change.label()
            )?;
            if let Some(detail) = &host.detail {
                writeln!(text, "  {detail}\r")?;
            }
            ensure!(
                text.len() <= limit,
                "Comparison exceeds the clipboard limit"
            );
        }
        ensure!(
            text.len() <= limit,
            "Comparison exceeds the clipboard limit"
        );
        Ok(text)
    }

    fn write_json(&self, file: &mut impl io::Write) -> io::Result<()> {
        let mut file = io::BufWriter::new(file);
        serde_json::to_writer_pretty(&mut file, self)?;
        file.write_all(b"\n")?;
        file.flush()
    }
}

impl TerminalApp {
    fn capture_run_comparison_report(&self, tab: TabId, run: u64, revision: u64) -> Option<Report> {
        if self.tabs.active_id() != Some(tab) {
            return None;
        }
        let Page::HostGroup(page) = &self.tabs.get(tab)?.page else {
            return None;
        };
        let open = page
            .comparison
            .overview()
            .filter(|open| page.comparison.revision == revision && open.run == run)?;
        let ComparisonState::Ready(Ok(rows)) = &open.comparison else {
            return None;
        };
        let baseline = open.baseline?;
        let earlier = page.submissions.iter().find(|item| item.id == baseline)?;
        let current = page.viewed_submission(run)?;
        Some(Report::capture(earlier, current, rows))
    }

    pub(super) fn copy_run_comparison_report(&mut self, tab: TabId, run: u64, revision: u64) {
        let Some(report) = self.capture_run_comparison_report(tab, run, revision) else {
            return;
        };
        let copied = report
            .text(w11term::clipboard::MAX_TEXT)
            .and_then(|text| w11term::clipboard::write(&text));
        if let Some(page) = self.host_group_mut(tab)
            && let Some(open) = page.comparison.overview_mut(run, revision)
        {
            open.export_feedback = Some(match copied {
                Ok(()) => "Copied comparison".into(),
                Err(error) => format!("Could not copy comparison · {error}"),
            });
        }
    }

    pub(super) fn save_run_comparison_report(
        &mut self,
        tab: TabId,
        run: u64,
        revision: u64,
        ctx: &ComponentContext<Self>,
    ) {
        if self.export.pending.is_some() {
            return;
        }
        let Some(report) = self.capture_run_comparison_report(tab, run, revision) else {
            return;
        };
        self.begin_text_export(
            text_export::Subject::HostGroupRunComparison {
                baseline: report.earlier.run_id,
                run,
            },
            move |file| report.write_json(file),
            ctx,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_all_classifications_in_order_and_limits_clipboard_bytes() {
        let mut group = HostGroup::default();
        for label in ["before", "after"] {
            group.submit(
                SubmissionContent::Command(label.into()),
                Vec::new(),
                GroupTranscript::default(),
            );
        }
        let rows = [
            Row {
                pane: 6,
                names: filter::Names::new("雪", "test.example", "ops", 22),
                change: Change::Unchanged,
                sides: None,
                detail: None,
            },
            Row {
                pane: 9,
                names: filter::Names::new("Other", "test.example", "ops", 2222),
                change: Change::CurrentOnly,
                sides: None,
                detail: None,
            },
        ];
        let report = Report::capture(&group.submissions[0], &group.submissions[1], &rows);
        let text = report.text(4096).unwrap();
        assert!(text.contains("Earlier · Run 1 · before\r\nCurrent · Run 2 · after\r\n"));
        assert!(text.contains("雪 · ops@test.example:22 · Pane 6 · Unchanged"));
        assert!(report.text(text.len() - 1).is_err());
        let mut bytes = Vec::new();
        report.write_json(&mut bytes).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["counts"]["unchanged"], 1);
        assert_eq!(json["hosts"][0]["name"], "雪");
        assert_eq!(json["hosts"][0]["change"], "unchanged");
        assert_eq!(json["hosts"][1]["pane_id"], 9);
        assert_eq!(json["hosts"][1]["change"], "current_only");
    }
}
