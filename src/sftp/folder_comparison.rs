use super::*;
use std::collections::{BTreeMap, BTreeSet};

const TYPE_MASK: u32 = 0o170000;
const DIRECTORY: u32 = 0o040000;
const REGULAR: u32 = 0o100000;
const SYMBOLIC_LINK: u32 = 0o120000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FolderEntry {
    kind: u32,
    pub size: Option<u64>,
    pub mtime: Option<u32>,
}

impl FolderEntry {
    pub fn is_file(self) -> bool {
        self.kind == REGULAR
    }

    pub fn kind_label(self) -> &'static str {
        match self.kind {
            DIRECTORY => "Folder",
            REGULAR => "File",
            SYMBOLIC_LINK => "Symbolic link",
            _ => "Special entry",
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct FolderInventory {
    pub root: String,
    pub entries: BTreeMap<String, FolderEntry>,
}

#[derive(Default)]
pub(super) struct InventoryBuilder {
    entries: BTreeMap<String, FolderEntry>,
    path_bytes: usize,
}

impl InventoryBuilder {
    pub(super) fn insert(&mut self, path: &str, attrs: &FileAttributes) -> Result<()> {
        let kind = attrs.permissions.unwrap_or_default() & TYPE_MASK;
        ensure!(kind != 0, "The server did not report the type of {path}");
        self.path_bytes += path.len();
        ensure!(
            self.path_bytes <= directory::MAX_NAME_BYTES,
            "The folder inventory exceeded the path size limit"
        );
        let entry = FolderEntry {
            kind,
            size: attrs.size.filter(|_| kind == REGULAR),
            mtime: attrs.mtime.filter(|_| kind == REGULAR),
        };
        ensure!(
            self.entries.insert(path.to_owned(), entry).is_none(),
            "The server returned a duplicate folder entry for {path}"
        );
        Ok(())
    }

    pub(super) fn finish(self, root: String) -> FolderInventory {
        FolderInventory {
            root,
            entries: self.entries,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum FolderChange {
    ReferenceOnly,
    CurrentOnly,
    Type,
    Metadata,
    Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct FolderDifference {
    pub path: String,
    pub change: FolderChange,
    pub reference: Option<FolderEntry>,
    pub current: Option<FolderEntry>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct FolderComparison {
    pub reference_root: String,
    pub current_root: String,
    pub reference_only: usize,
    pub current_only: usize,
    pub types_changed: usize,
    pub metadata_changed: usize,
    pub unknown: usize,
    pub matching: usize,
    pub differences: Vec<FolderDifference>,
}

fn entry_change(reference: FolderEntry, current: FolderEntry) -> Option<FolderChange> {
    if reference.kind != current.kind {
        return Some(FolderChange::Type);
    }
    match reference.kind {
        DIRECTORY => None,
        REGULAR => {
            let size = reference.size.zip(current.size);
            let mtime = reference.mtime.zip(current.mtime);
            if size.is_some_and(|(before, after)| before != after)
                || mtime.is_some_and(|(before, after)| before != after)
            {
                Some(FolderChange::Metadata)
            } else if size.is_none() || mtime.is_none() {
                Some(FolderChange::Unknown)
            } else {
                None
            }
        }
        _ => Some(FolderChange::Unknown),
    }
}

pub(super) fn compare(reference: &FolderInventory, current: FolderInventory) -> FolderComparison {
    let mut report = FolderComparison {
        reference_root: reference.root.clone(),
        current_root: current.root,
        ..Default::default()
    };
    let paths: BTreeSet<_> = reference
        .entries
        .keys()
        .chain(current.entries.keys())
        .collect();
    for path in paths {
        let before = reference.entries.get(path).copied();
        let after = current.entries.get(path).copied();
        let change = match (before, after) {
            (Some(before), Some(after)) => entry_change(before, after),
            (Some(_), None) => Some(FolderChange::ReferenceOnly),
            (None, Some(_)) => Some(FolderChange::CurrentOnly),
            (None, None) => unreachable!("comparison paths come from the two inventories"),
        };
        let Some(change) = change else {
            report.matching += 1;
            continue;
        };
        match change {
            FolderChange::ReferenceOnly => report.reference_only += 1,
            FolderChange::CurrentOnly => report.current_only += 1,
            FolderChange::Type => report.types_changed += 1,
            FolderChange::Metadata => report.metadata_changed += 1,
            FolderChange::Unknown => report.unknown += 1,
        }
        report.differences.push(FolderDifference {
            path: path.clone(),
            change,
            reference: before,
            current: after,
        });
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_metadata_does_not_mask_a_known_difference_or_prove_a_match() {
        let file = FolderEntry {
            kind: REGULAR,
            size: Some(4),
            mtime: Some(7),
        };
        let partial = FolderEntry {
            mtime: None,
            ..file
        };
        assert_eq!(entry_change(file, file), None);
        assert_eq!(entry_change(file, partial), Some(FolderChange::Unknown));
        assert_eq!(
            entry_change(
                file,
                FolderEntry {
                    size: Some(5),
                    ..partial
                }
            ),
            Some(FolderChange::Metadata)
        );
        let link = FolderEntry {
            kind: SYMBOLIC_LINK,
            size: None,
            mtime: None,
        };
        assert_eq!(entry_change(link, link), Some(FolderChange::Unknown));
        assert_eq!(entry_change(file, link), Some(FolderChange::Type));
    }

    #[test]
    fn comparison_keeps_every_difference_in_path_order() {
        let reference = InventoryBuilder::default().finish("/a".into());
        let mut builder = InventoryBuilder::default();
        let attrs = FileAttributes {
            permissions: Some(REGULAR),
            ..Default::default()
        };
        for index in 0..250 {
            builder.insert(&format!("file{index:03}"), &attrs).unwrap();
        }
        let report = compare(&reference, builder.finish("/b".into()));
        assert_eq!(report.current_only, 250);
        assert_eq!(report.differences.len(), 250);
        assert_eq!(report.differences[0].path, "file000");
        assert_eq!(report.differences[249].path, "file249");
    }
}
