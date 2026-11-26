use serde_json::{Value, json};
use std::io::{self, Write};
use w11term::{
    events::{RecentSftpTransfer, SavedDownload, SftpTransferItem},
    options::SshOptions,
};

pub(super) struct Row {
    label: String,
    host: String,
    user: String,
    port: u16,
    result: RecentSftpTransfer,
}

impl Row {
    pub(super) fn capture(label: &str, ssh: &SshOptions, result: RecentSftpTransfer) -> Self {
        Self {
            label: label.into(),
            host: ssh.host.clone(),
            user: ssh.user.clone(),
            port: ssh.port,
            result,
        }
    }

    fn json(&self) -> Value {
        let result = &self.result;
        let saved = result.saved_download.as_ref().map(|saved| {
            let (kind, path) = match saved {
                SavedDownload::File(path) => ("file", path),
                SavedDownload::Folder(path) => ("folder", path),
                SavedDownload::BatchFolder(path) => ("batch-folder", path),
            };
            json!({ "kind": kind, "path": path.to_string_lossy() })
        });
        let details = result.details.as_ref().map(|details| {
            json!({
                "completed": details.completed.iter().map(item_json).collect::<Vec<_>>(),
                "omitted_completed": details.omitted_completed,
                "stopped": details.stopped.as_ref().map(item_json),
                "not_started": details.not_started,
            })
        });
        json!({
            "transfer_id": result.id,
            "finished_unix_ms": result.finished_unix_ms,
            "outcome": result.outcome,
            "message": result.message.as_ref(),
            "background": result.background,
            "pane": { "id": result.id.pane, "label": self.label },
            "ssh": { "host": self.host, "user": self.user, "port": self.port },
            "saved_download": saved,
            "details": details,
        })
    }
}

fn item_json(item: &SftpTransferItem) -> Value {
    json!({
        "operation": item.operation,
        "outcome": item.outcome,
        "remote": item.remote,
        "local": item.local.to_string_lossy(),
        "bytes_transferred": item.bytes_transferred,
    })
}

pub(super) fn write_json(rows: &[Row], mut output: impl Write) -> io::Result<()> {
    let document = json!({
        "schema_version": 1,
        "transfers": rows.iter().map(Row::json).collect::<Vec<_>>(),
    });
    serde_json::to_writer_pretty(&mut output, &document)?;
    output.write_all(b"\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::Path, sync::Arc, time::Instant};
    use w11term::{
        events::SftpTransferDetails,
        sftp::{TransferId, TransferOperation, TransferOutcome},
    };

    #[test]
    fn report_round_trips_item_paths_and_distinguishes_unavailable_values() {
        let local = r"C:\Data\Ångström's $report";
        let remote = "/var/tmp/Ångström's \"report\"";
        let row = Row {
            label: "Build host".into(),
            host: "example.test".into(),
            user: "ops".into(),
            port: 2222,
            result: RecentSftpTransfer {
                id: TransferId {
                    pane: 2,
                    generation: 3,
                    serial: 4,
                },
                finished_at: Instant::now(),
                finished_unix_ms: 12345,
                outcome: TransferOutcome::Failed,
                message: Arc::from("A later transfer failed"),
                background: true,
                saved_download: Some(SavedDownload::BatchFolder(Arc::from(Path::new(local)))),
                details: Some(Arc::new(SftpTransferDetails {
                    completed: Vec::new(),
                    omitted_completed: 2,
                    stopped: Some(SftpTransferItem {
                        operation: TransferOperation::DownloadFile,
                        local: Path::new(local).join("report"),
                        remote: remote.into(),
                        outcome: TransferOutcome::Failed,
                        bytes_transferred: None,
                    }),
                    not_started: 3,
                })),
            },
        };
        let mut bytes = Vec::new();
        write_json(&[row], &mut bytes).unwrap();
        let document: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(document["schema_version"], 1);
        let saved = &document["transfers"][0];
        assert_eq!(saved["transfer_id"], "2:3:4");
        assert_eq!(saved["saved_download"]["path"], local);
        assert_eq!(saved["details"]["stopped"]["remote"], remote);
        assert!(saved["details"]["stopped"]["bytes_transferred"].is_null());
        assert_eq!(saved["details"]["omitted_completed"], 2);
        assert_eq!(saved["details"]["not_started"], 3);
    }
}
