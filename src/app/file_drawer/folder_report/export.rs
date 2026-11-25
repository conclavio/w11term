use super::*;
use serde::Serialize;
use std::io::{self, Write};
use text_export::{Export, FolderComparisonTarget, Subject};
use w11term::sftp::{FolderChange, FolderDifference, FolderEntry};

pub(super) fn controls(
    target: FolderComparisonTarget,
    export: &Export,
    context: &ViewContext<TerminalApp>,
) -> Vec<KeyedView> {
    let mut rows = vec![KeyedView::new(
        "save-comparison",
        Button::new()
            .is_enabled(export.pending.is_none())
            .on_click(context.callback(move |_| {
                Message::Export(text_export::Action::SaveFolderComparison(target))
            }))
            .content("Save comparison as JSON…"),
    )];
    if let Some(status) = export.folder_status(target) {
        rows.push(KeyedView::new(
            "export-status",
            TextBlock::new()
                .automation_id("sftp-folder-comparison-export-status")
                .text(status)
                .text_wrapping(TextWrapping::Wrap),
        ));
    }
    rows
}

#[derive(Serialize)]
struct Endpoint<'a> {
    host: &'a str,
    port: u16,
    user: &'a str,
}

#[derive(Serialize)]
struct Root<'a> {
    origin: &'static str,
    path: &'a str,
}

#[derive(Serialize)]
struct Counts {
    reference_only: usize,
    current_only: usize,
    type_changed: usize,
    metadata_changed: usize,
    needs_inspection: usize,
    matching: usize,
}

#[derive(Serialize)]
struct Entry {
    kind: &'static str,
    size_bytes: Option<u64>,
    modified_unix_seconds: Option<u32>,
}

impl From<FolderEntry> for Entry {
    fn from(entry: FolderEntry) -> Self {
        Self {
            kind: entry.kind_key(),
            size_bytes: entry.size,
            modified_unix_seconds: entry.mtime,
        }
    }
}

#[derive(Serialize)]
struct Difference<'a> {
    path: &'a str,
    change: &'static str,
    reference: Option<Entry>,
    current: Option<Entry>,
}

impl<'a> From<&'a FolderDifference> for Difference<'a> {
    fn from(row: &'a FolderDifference) -> Self {
        Self {
            path: &row.path,
            change: match row.change {
                FolderChange::ReferenceOnly => "reference_only",
                FolderChange::CurrentOnly => "current_only",
                FolderChange::Type => "type_changed",
                FolderChange::Metadata => "metadata_changed",
                FolderChange::Unknown => "needs_inspection",
            },
            reference: row.reference.map(Entry::from),
            current: row.current.map(Entry::from),
        }
    }
}

#[derive(Serialize)]
struct JsonReport<'a> {
    schema_version: u32,
    ssh: Endpoint<'a>,
    reference: Root<'a>,
    current: Root<'a>,
    counts: Counts,
    differences: Vec<Difference<'a>>,
}

fn write_report(
    file: impl Write,
    endpoint: Endpoint<'_>,
    report: &FolderComparison,
) -> io::Result<()> {
    let report = JsonReport {
        schema_version: 1,
        ssh: endpoint,
        reference: Root {
            origin: match report.reference_origin {
                FolderOrigin::Remote => "remote",
                FolderOrigin::Windows => "windows",
            },
            path: &report.reference_root,
        },
        current: Root {
            origin: "remote",
            path: &report.current_root,
        },
        counts: Counts {
            reference_only: report.reference_only,
            current_only: report.current_only,
            type_changed: report.types_changed,
            metadata_changed: report.metadata_changed,
            needs_inspection: report.unknown,
            matching: report.matching,
        },
        differences: report.differences.iter().map(Difference::from).collect(),
    };
    let mut output = io::BufWriter::new(file);
    serde_json::to_writer_pretty(&mut output, &report)?;
    output.write_all(b"\n")?;
    output.flush()
}

impl FileDrawer {
    fn captured_for_export(&self, target: FolderComparisonTarget) -> Option<Arc<FolderComparison>> {
        let panel = self.folder_report.as_ref()?;
        if !self.open
            || self.generation != target.generation
            || panel.serial != target.serial
            || panel.revision != target.revision
        {
            return None;
        }
        let State::Loaded(Ok(Report::Comparison(report))) = &panel.state else {
            return None;
        };
        Some(report.clone())
    }
}

impl TerminalApp {
    pub(in crate::app) fn save_folder_comparison(
        &mut self,
        target: FolderComparisonTarget,
        context: &ComponentContext<Self>,
    ) {
        let Some(tab) = self.tab_for_pane(target.pane) else {
            return;
        };
        if self.tabs.active_id() != Some(tab) {
            return;
        }
        let Some(AppTab {
            page: super::Page::Terminal(tab),
            ..
        }) = self.tabs.get(tab)
        else {
            return;
        };
        if tab.session.frames.load().connection_generation != target.generation {
            return;
        }
        let Source::Ssh(ssh) = &tab.source else {
            return;
        };
        let Some(report) = tab.files.captured_for_export(target) else {
            return;
        };
        let (host, port, user) = (ssh.host.clone(), ssh.port, ssh.user.clone());
        self.begin_text_export(
            Subject::FolderComparison(target),
            move |file| {
                write_report(
                    file,
                    Endpoint {
                        host: &host,
                        port,
                        user: &user,
                    },
                    &report,
                )
            },
            context,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report() -> FolderComparison {
        FolderComparison {
            reference_root: r"C:\baseline".into(),
            reference_origin: FolderOrigin::Windows,
            current_root: "/srv/site".into(),
            matching: 4,
            reference_only: 1,
            differences: vec![FolderDifference {
                path: "docs/雪.txt".into(),
                change: FolderChange::ReferenceOnly,
                reference: Some(
                    serde_json::from_value(json!({
                        "kind": 0o100000, "size": 23, "mtime": null,
                    }))
                    .unwrap(),
                ),
                current: None,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn json_preserves_origin_unicode_and_absent_metadata() {
        let mut bytes = Vec::new();
        write_report(
            &mut bytes,
            Endpoint {
                host: "test.example",
                port: 2222,
                user: "ops",
            },
            &report(),
        )
        .unwrap();
        let data: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(data["schema_version"], 1);
        assert_eq!(
            data["ssh"],
            json!({"host":"test.example","port":2222,"user":"ops"})
        );
        assert_eq!(
            data["reference"],
            json!({"origin":"windows","path":r"C:\baseline"})
        );
        assert_eq!(
            data["current"],
            json!({"origin":"remote","path":"/srv/site"})
        );
        assert_eq!(data["counts"]["matching"], 4);
        assert_eq!(data["counts"]["reference_only"], 1);
        assert_eq!(
            data["differences"],
            json!([{
                "path":"docs/雪.txt", "change":"reference_only",
                "reference":{"kind":"file","size_bytes":23,"modified_unix_seconds":null},
                "current":null,
            }])
        );
    }

    #[test]
    fn save_target_stays_bound_to_the_report_across_refresh() {
        let original = Arc::new(report());
        let mut files = FileDrawer {
            open: true,
            generation: 3,
            ..Default::default()
        };
        let serial = files.begin_folder_report(Kind::Comparison, "/srv/site".into());
        files.folder_report.as_mut().unwrap().state =
            State::Loaded(Ok(Report::Comparison(original.clone())));
        let target = FolderComparisonTarget {
            pane: 1,
            generation: 3,
            serial,
            revision: 0,
        };
        let captured = files.captured_for_export(target).unwrap();
        assert!(Arc::ptr_eq(&captured, &original));
        let panel = files.folder_report.as_mut().unwrap();
        panel.revision += 1;
        let mut replacement = report();
        replacement.current_root = "/replacement".into();
        panel.state = State::Loaded(Ok(Report::Comparison(Arc::new(replacement))));
        assert!(files.captured_for_export(target).is_none());
        assert_eq!(captured.current_root, "/srv/site");
        files.generation += 1;
        assert!(
            files
                .captured_for_export(FolderComparisonTarget {
                    revision: 1,
                    ..target
                })
                .is_none()
        );
    }
}
