use super::*;
use std::path::Path;
use w11term::events::{SftpTransferDetails, SftpTransferItem};

const COMPLETED_ITEM_LIMIT: usize = 64;

pub(super) struct FinishedTransfer {
    pub(super) saved: Option<SavedDownload>,
    pub(super) details: Option<Arc<SftpTransferDetails>>,
}

#[derive(Default)]
pub(super) struct TransferQueue {
    pub(super) remaining: VecDeque<(Operation, TransferFile)>,
    pub(super) overwrite_files: bool,
    pub(super) batch_id: Option<TransferId>,
    pub(super) total: usize,
    pub(super) completed: usize,
    pub(super) download_folder: Option<Arc<Path>>,
    pub(super) saved_download: Option<SavedDownload>,
    pub(super) completed_items: Vec<SftpTransferItem>,
    pub(super) stopped_item: Option<SftpTransferItem>,
}

impl TransferQueue {
    pub(super) fn next_item(&self) -> usize {
        self.completed + 1
    }

    pub(super) fn settle_item(
        &mut self,
        file: &TransferFile,
        outcome: TransferOutcome,
        reply: Option<&Value>,
    ) {
        if outcome == TransferOutcome::Completed {
            self.completed += 1;
            if file.is_download() {
                self.saved_download = match &self.download_folder {
                    Some(folder) => Some(SavedDownload::BatchFolder(folder.clone())),
                    None => file.downloaded_item(),
                };
            }
            if self.completed_items.len() == COMPLETED_ITEM_LIMIT {
                return;
            }
        }
        let (operation, remote, local) = file.paths();
        let item = SftpTransferItem {
            operation,
            remote: reply
                .and_then(|reply| {
                    reply["resolved_remote"]
                        .as_str()
                        .or_else(|| reply["remote"].as_str())
                })
                .unwrap_or(remote)
                .into(),
            local: local.to_owned(),
            outcome,
            bytes_transferred: reply.and_then(|reply| reply["bytes_transferred"].as_u64()),
        };
        if outcome == TransferOutcome::Completed {
            self.completed_items.push(item);
        } else {
            self.stopped_item = Some(item);
        }
    }

    pub(super) fn finish(&mut self, status: &mut String) -> FinishedTransfer {
        if self.total > 1 {
            use std::fmt::Write;
            let _ = write!(
                status,
                " {} of {} items completed.",
                self.completed, self.total
            );
        }
        let finished = std::mem::take(self);
        let attempted = finished.completed + usize::from(finished.stopped_item.is_some());
        let details = (finished.total != 0 || attempted != 0).then(|| {
            Arc::new(SftpTransferDetails {
                omitted_completed: finished
                    .completed
                    .saturating_sub(finished.completed_items.len()),
                not_started: finished.total.max(1).saturating_sub(attempted),
                completed: finished.completed_items,
                stopped: finished.stopped_item,
            })
        });
        FinishedTransfer {
            saved: finished.saved_download,
            details,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn history_keeps_the_stopped_item_after_the_completed_item_limit() {
        let mut queue = TransferQueue {
            total: COMPLETED_ITEM_LIMIT + 5,
            ..Default::default()
        };
        let item = TransferFile::Upload {
            name: "run.bin".into(),
            local: r"C:\Reports\run.bin".into(),
            remote: "/jobs/run.bin".into(),
            atomic: false,
        };
        for _ in 0..COMPLETED_ITEM_LIMIT + 1 {
            queue.settle_item(
                &item,
                TransferOutcome::Completed,
                Some(&json!({"bytes_transferred": 17})),
            );
        }
        let stopped = TransferFile::Download {
            local: r"C:\Reports\next.bin".into(),
            remote: "/jobs/next.bin".into(),
            resume: false,
        };
        queue.settle_item(&stopped, TransferOutcome::Failed, None);
        let details = queue.finish(&mut String::new()).details.unwrap();
        assert_eq!(details.completed.len(), COMPLETED_ITEM_LIMIT);
        assert_eq!(details.omitted_completed, 1);
        assert_eq!(details.not_started, 3);
        assert_eq!(
            details.completed[0].operation,
            TransferOperation::UploadFile
        );
        assert_eq!(details.completed[0].local, Path::new(r"C:\Reports\run.bin"));
        assert_eq!(details.completed[0].remote, "/jobs/run.bin");
        assert_eq!(details.completed[0].bytes_transferred, Some(17));
        let stopped = details.stopped.as_ref().unwrap();
        assert_eq!(stopped.operation, TransferOperation::DownloadFile);
        assert_eq!(stopped.outcome, TransferOutcome::Failed);
        assert_eq!(stopped.remote, "/jobs/next.bin");
        assert_eq!(stopped.bytes_transferred, None);
        assert!(queue.finish(&mut String::new()).details.is_none());
    }
}
