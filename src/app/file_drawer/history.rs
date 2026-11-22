use std::collections::VecDeque;

const HISTORY_LIMIT: usize = 32;

#[derive(Clone, Copy)]
pub enum HistoryDirection {
    Back,
    Forward,
}

#[derive(Default)]
pub(super) struct FolderHistory {
    directories: VecDeque<String>,
    current: usize,
}

impl FolderHistory {
    fn neighbor(&self, direction: HistoryDirection) -> Option<usize> {
        match direction {
            HistoryDirection::Back => self.current.checked_sub(1),
            HistoryDirection::Forward => {
                (self.current + 1 < self.directories.len()).then_some(self.current + 1)
            }
        }
    }

    pub(super) fn target(&self, direction: HistoryDirection) -> Option<&str> {
        self.neighbor(direction)
            .map(|index| self.directories[index].as_str())
    }

    /// Only call after REALPATH, READDIR and parsing have all succeeded.
    pub(super) fn listed(&mut self, path: &str, direction: Option<HistoryDirection>) {
        if let Some(index) = direction.and_then(|d| self.neighbor(d)) {
            self.current = index;
            if self.directories[index] != path {
                // A saved directory can have become a symlink. Keep its newly
                // resolved path in this slot and fold any adjacent duplicate.
                self.directories[index] = path.to_owned();
                if self.target(HistoryDirection::Back) == Some(path) {
                    self.directories.remove(self.current);
                    self.current -= 1;
                }
                if self.target(HistoryDirection::Forward) == Some(path) {
                    self.directories.remove(self.current + 1);
                }
            }
        } else if self.directories.get(self.current).map(String::as_str) != Some(path) {
            self.directories.truncate(self.current + 1);
            if self.directories.len() == HISTORY_LIMIT {
                self.directories.pop_front();
            }
            self.directories.push_back(path.into());
            self.current = self.directories.len() - 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FolderHistory, HISTORY_LIMIT, HistoryDirection::*};

    #[test]
    fn history_evicts_oldest_and_collapses_adjacent_redirects() {
        let mut history = FolderHistory::default();
        for i in 0..=HISTORY_LIMIT {
            history.listed(&format!("/{i}"), None);
        }
        for i in (1..HISTORY_LIMIT).rev() {
            let older = format!("/{i}");
            assert_eq!(history.target(Back), Some(older.as_str()));
            history.listed(&older, Some(Back));
        }
        assert_eq!(history.target(Back), None);
        assert_eq!(history.target(Forward), Some("/2"));

        // /2 now resolves to /3. There is only one stop for their destination.
        history.listed("/3", Some(Forward));
        assert_eq!(history.target(Back), Some("/1"));
        assert_eq!(history.target(Forward), Some("/4"));
        // /1 now points to the current folder.
        history.listed("/3", Some(Back));
        assert_eq!(history.target(Back), None);
        assert_eq!(history.target(Forward), Some("/4"));
    }
}
