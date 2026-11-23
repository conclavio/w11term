use super::{Entry, EntryKind, FileDrawer, ListedKind, PAGE_SIZE};
use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SortOrder {
    #[default]
    Name,
    NewestFirst,
    LargestFirst,
}

impl SortOrder {
    pub(super) fn sort(self, entries: &mut [Entry]) {
        let directory = |e: &Entry| e.kind == ListedKind::Known(EntryKind::Directory);
        entries.sort_unstable_by(|a, b| {
            directory(b)
                .cmp(&directory(a))
                .then_with(|| match self {
                    Self::Name => Ordering::Equal,
                    Self::NewestFirst => b.mtime.cmp(&a.mtime),
                    Self::LargestFirst => b.size.cmp(&a.size),
                })
                .then_with(|| a.name.cmp(&b.name))
        });
    }
}

impl FileDrawer {
    pub(super) fn set_sort_order(&mut self, order: SortOrder) {
        if self.sort_order == order {
            return;
        }
        self.sort_order = order;
        order.sort(&mut self.entries);
        self.update_matches();
        self.page = self
            .selected
            .as_ref()
            .and_then(|name| self.matching_entries().position(|e| &e.name == name))
            .map_or(0, |index| index / PAGE_SIZE);
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ListedEntry, directory_entries};
    use super::*;
    use russh_sftp::protocol::{File, FileAttributes};
    use serde_json::json;

    fn entry(name: &str, permissions: Option<u32>, mtime: Option<u32>) -> File {
        File::new(
            name,
            FileAttributes {
                permissions,
                atime: mtime,
                mtime,
                ..FileAttributes::empty()
            },
        )
    }

    #[test]
    fn sorts_wire_mtimes_inside_directory_groups_with_literal_name_ties_and_missing_times_last() {
        let directory = Some(0o040755);
        let file = Some(0o100644);
        let files = vec![
            entry("z epoch", file, Some(0)),
            entry("z dir", directory, None),
            entry("b stale", file, None),
            entry("雪", None, Some(42)),
            entry("a dir", directory, Some(2)),
            entry("A", Some(0o120777), Some(42)),
            entry("a newest", file, Some(u32::MAX)),
            entry(".dot", file, Some(42)),
        ];
        let wire = json!({ "entries": files });
        let names = |order| {
            directory_entries(&wire, order)
                .unwrap()
                .into_iter()
                .map(|e| e.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(SortOrder::default()),
            [
                "a dir", "z dir", ".dot", "A", "a newest", "b stale", "z epoch", "雪"
            ]
        );
        assert_eq!(
            names(SortOrder::NewestFirst),
            [
                "a dir", "z dir", "a newest", ".dot", "A", "雪", "z epoch", "b stale"
            ]
        );
    }

    #[test]
    fn sorts_reported_file_sizes_with_directories_first_and_unreported_sizes_last() {
        let wire = json!({"entries": [
            {"filename":"empty","attrs":{"permissions":0o100644,"size":0}},
            {"filename":"z-link","attrs":{"permissions":0o120777,"size":u64::MAX}},
            {"filename":"z-tie","attrs":{"permissions":0o100644,"size":10}},
            {"filename":"z-folder","attrs":{"permissions":0o040755,"size":500}},
            {"filename":"a-unknown","attrs":{"size":900}},
            {"filename":"big","attrs":{"permissions":0o100644,"size":u64::MAX}},
            {"filename":"a-directory","attrs":{"permissions":0o040755}},
            {"filename":"a-tie","attrs":{"permissions":0o100644,"size":10}},
            {"filename":"file-without-size","attrs":{"permissions":0o100644}},
        ]});
        let names: Vec<_> = directory_entries(&wire, SortOrder::LargestFirst)
            .unwrap()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(
            names,
            [
                "a-directory",
                "z-folder",
                "big",
                "a-tie",
                "z-tie",
                "empty",
                "a-unknown",
                "file-without-size",
                "z-link",
            ]
        );
    }

    #[test]
    fn sorting_cached_entries_keeps_the_selected_filtered_row_and_its_current_listing_identity() {
        let mut drawer = FileDrawer {
            open: true,
            ..Default::default()
        };
        let files = (0..100)
            .map(|i| {
                let prefix = if i % 2 == 0 { "雪" } else { "other" };
                entry(&format!("{prefix}-{i:02}"), Some(0o100644), Some(i))
            })
            .collect::<Vec<_>>();
        let reply = json!({"entries": files});
        drawer.accept_listing(
            None,
            "/logs".into(),
            directory_entries(&reply, drawer.sort_order).unwrap(),
        );
        drawer.set_filter("雪".into());
        let listed = ListedEntry {
            parent: "/logs".into(),
            revision: drawer.listing_revision,
            name: "雪-02".into(),
            kind: ListedKind::Known(EntryKind::File),
        };
        drawer.selected = Some(listed.name.clone());
        assert!(drawer.check_selected(&listed).is_ok());
        assert_eq!(drawer.page, 0);
        drawer.set_sort_order(SortOrder::NewestFirst);
        assert_eq!(drawer.page, 1);
        assert!(drawer.check_selected(&listed).is_ok());
        assert_eq!(drawer.filter, "雪");
        assert_eq!(drawer.matching_entries().next().unwrap().name, "雪-98");
        assert!(!drawer.busy());
        drawer.set_sort_order(SortOrder::Name);
        assert_eq!(drawer.page, 0);
        assert!(drawer.check_selected(&listed).is_ok());
        drawer.set_page(1); // no selection
        drawer.set_sort_order(SortOrder::NewestFirst);
        assert_eq!(drawer.page, 0);
        assert!(drawer.selected.is_none());
    }

    #[test]
    fn sorting_preference_is_per_pane_and_survives_new_listings_and_reconnects() {
        let wire = json!({"entries": [
            entry("old", Some(0o100644), Some(1)), entry("recent", Some(0o100644), Some(20))
        ]});
        let mut files = FileDrawer::default();
        let other = FileDrawer::default();
        files.set_sort_order(SortOrder::NewestFirst);
        for (generation, path) in [(1, "/work"), (1, "/work"), (1, "/new"), (2, "/home")] {
            files.observe_connection(generation);
            files.accept_listing(
                None,
                path.into(),
                directory_entries(&wire, files.sort_order).unwrap(),
            );
            assert_eq!(files.matching_entries().next().unwrap().name, "recent");
        }
        assert_eq!(other.sort_order, SortOrder::Name);
    }
}
