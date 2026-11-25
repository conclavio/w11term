use super::*;

#[derive(Default)]
pub(super) struct TransferQueue {
    pub(super) remaining: VecDeque<(Operation, TransferFile)>,
    pub(super) overwrite_files: bool,
    pub(super) batch_id: Option<TransferId>,
    pub(super) total: usize,
    pub(super) completed: usize,
}

impl TransferQueue {
    pub(super) fn completed_one(&mut self) {
        self.completed += 1;
    }

    pub(super) fn next_item(&self) -> usize {
        self.completed + 1
    }

    pub(super) fn finish(&mut self, status: &mut String) {
        if self.total > 1 {
            use std::fmt::Write;
            let _ = write!(
                status,
                " {} of {} items completed.",
                self.completed, self.total
            );
        }
        *self = Self::default();
    }
}
