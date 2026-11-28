use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{ErrorKind, Read},
    path::PathBuf,
};

const LABEL: &str = "saved command reference pages";
const MAX_PAGES: usize = 64;
const MAX_BYTES: u64 = 32 * 1024;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Entry {
    pub platform: String,
    pub name: String,
}

pub fn valid_page_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !matches!(name, "." | "..")
        && !name
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '\\'))
}

impl Entry {
    fn valid(&self) -> bool {
        valid_page_name(&self.name)
            && !self.name.bytes().any(|b| b.is_ascii_uppercase())
            && !self.platform.is_empty()
            && self.platform.len() <= 32
            && self
                .platform
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    }
}

#[derive(Serialize, Deserialize)]
struct SavedPages {
    version: u8,
    pages: Vec<Entry>,
}

pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn user() -> Result<Self> {
        Ok(Self {
            path: crate::keys::data_dir()?
                .join("command-reference")
                .join("saved-pages.json"),
        })
    }

    fn load(&self) -> Result<SavedPages> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(SavedPages {
                    version: 1,
                    pages: Vec::new(),
                });
            }
            Err(error) => return Err(error).context("Opening saved command reference pages"),
        };
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_BYTES,
            "Saved command reference pages file is too large"
        );
        let data: SavedPages =
            serde_json::from_slice(&bytes).context("Reading saved command reference pages")?;
        let mut seen = BTreeSet::new();
        ensure!(
            data.version == 1
                && data.pages.len() <= MAX_PAGES
                && data
                    .pages
                    .iter()
                    .all(|entry| entry.valid() && seen.insert(entry)),
            "Saved command reference pages file has invalid entries"
        );
        Ok(data)
    }

    pub fn list(&self) -> Result<Vec<Entry>> {
        Ok(self.load()?.pages)
    }

    pub fn set_saved(&self, page: Entry, saved: bool) -> Result<Vec<Entry>> {
        ensure!(page.valid(), "Invalid command reference page");
        let result = crate::stored_json::update(&self.path, LABEL, || {
            let mut data = self.load()?;
            if saved && !data.pages.contains(&page) {
                ensure!(
                    data.pages.len() < MAX_PAGES,
                    "Remove a saved page before saving another"
                );
                data.pages.push(page);
            } else if !saved {
                data.pages.retain(|entry| *entry != page);
            }
            Ok(data)
        })?;
        Ok(result.pages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(platform: &str, name: &str) -> Entry {
        Entry {
            platform: platform.into(),
            name: name.into(),
        }
    }

    #[test]
    fn updates_use_the_latest_disk_state_and_keep_platforms_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ref").join("saved-pages.json");
        let first = Store { path: path.clone() };
        let later = Store { path };
        assert!(first.list().unwrap().is_empty());
        assert!(later.list().unwrap().is_empty());
        let common = entry("common", "rg");
        let linux = entry("linux", "rg");
        first.set_saved(common.clone(), true).unwrap();
        later.set_saved(linux.clone(), true).unwrap();
        assert_eq!(
            first.set_saved(common.clone(), true).unwrap(),
            [common.clone(), linux.clone()]
        );
        first.set_saved(linux.clone(), false).unwrap();
        assert_eq!(
            later.set_saved(linux, false).unwrap().as_slice(),
            std::slice::from_ref(&common)
        );
        assert_eq!(first.list().unwrap(), [common]);
    }

    #[test]
    fn malformed_catalogs_are_preserved_when_an_update_fails() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store {
            path: dir.path().join("saved-pages.json"),
        };
        for bytes in [
            r#"{"version":2,"pages":[]}"#,
            r#"{"version":1,"pages":[{"platform":"linux","name":"../rg"}]}"#,
            r#"{"version":1,"pages":[{"platform":"linux","name":"rg"},{"platform":"linux","name":"rg"}]}"#,
        ] {
            std::fs::write(&store.path, bytes).unwrap();
            assert!(store.set_saved(entry("linux", "jq"), true).is_err());
            assert_eq!(std::fs::read_to_string(&store.path).unwrap(), bytes);
        }
    }
}
