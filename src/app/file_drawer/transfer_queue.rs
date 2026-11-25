use super::*;
use std::path::Path;

#[derive(Default)]
pub(super) struct TransferQueue {
    pub(super) remaining: VecDeque<(Operation, TransferFile)>,
    pub(super) overwrite_files: bool,
    pub(super) batch_id: Option<TransferId>,
    pub(super) total: usize,
    pub(super) completed: usize,
    pub(super) download_folder: Option<Arc<Path>>,
    pub(super) saved_download: Option<SavedDownload>,
}

impl TransferQueue {
    pub(super) fn completed_one(&mut self) {
        self.completed += 1;
    }

    pub(super) fn next_item(&self) -> usize {
        self.completed + 1
    }

    pub(super) fn record_download(&mut self, file: &TransferFile) {
        if file.is_download() {
            self.saved_download = match &self.download_folder {
                Some(folder) => Some(SavedDownload::BatchFolder(folder.clone())),
                None => file.downloaded_item(),
            };
        }
    }

    pub(super) fn finish(&mut self, status: &mut String) -> Option<SavedDownload> {
        if self.total > 1 {
            use std::fmt::Write;
            let _ = write!(
                status,
                " {} of {} items completed.",
                self.completed, self.total
            );
        }
        std::mem::take(self).saved_download
    }
}
