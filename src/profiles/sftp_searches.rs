use super::{BookmarkPeer, CatalogEdit, ProfileStore, bookmark_directory};
use crate::sftp::{FilenameFilter, FilenameSort, MAX_SEARCH_PATH_BYTES};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub(super) const FIELD: &str = "sftp_searches";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedSftpSearch {
    pub root: String,
    pub query: String,
    #[serde(default)]
    pub filter: FilenameFilter,
    #[serde(default)]
    pub sort: FilenameSort,
}

impl SavedSftpSearch {
    fn validate(&self) -> Result<()> {
        ensure!(
            bookmark_directory(&self.root) == Some(self.root.as_str())
                && !self.root.contains('\\')
                && self.root.len() <= MAX_SEARCH_PATH_BYTES,
            "Choose a canonical remote search folder"
        );
        ensure!(
            self.filter.valid_query(&self.query),
            "Enter a filename pattern or select a result type"
        );
        if self.filter.regex && !self.query.is_empty() {
            crate::search::compile_regex(&self.query, self.filter.match_case)
                .map_err(|error| anyhow::anyhow!("Invalid filename regex. {error}"))?;
        }
        Ok(())
    }
}

fn check_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && name == name.trim()
            && !name.chars().any(char::is_control),
        "Enter a search name up to 128 bytes without control characters"
    );
    Ok(())
}

type Searches = BTreeMap<String, SavedSftpSearch>;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Group {
    peer: BookmarkPeer,
    searches: Searches,
}

fn read(profile: &Value) -> Result<Vec<Group>> {
    let Some(raw) = profile.get(FIELD) else {
        return Ok(Vec::new());
    };
    let groups = Vec::<Group>::deserialize(raw).context("Reading saved SFTP searches")?;
    let mut peers = BTreeSet::new();
    for group in &groups {
        ensure!(
            peers.insert(&group.peer),
            "Duplicate SSH peer in saved searches"
        );
        for (name, search) in &group.searches {
            check_name(name)?;
            search.validate()?;
        }
    }
    Ok(groups)
}

impl ProfileStore {
    pub fn list_sftp_searches(&self, profile: &str, peer: &BookmarkPeer) -> Result<Searches> {
        let (_, snapshot) = self.inspect(profile)?;
        peer.check_profile(&snapshot.options)?;
        Ok(read(&snapshot.raw)?
            .into_iter()
            .find(|group| &group.peer == peer)
            .map(|group| group.searches)
            .unwrap_or_default())
    }

    pub fn save_sftp_search(
        &self,
        profile: &str,
        peer: &BookmarkPeer,
        name: &str,
        search: SavedSftpSearch,
    ) -> Result<Searches> {
        check_name(name)?;
        search.validate()?;
        self.edit_sftp_searches(profile, peer, |entries| {
            ensure!(
                !entries.contains_key(name),
                "A saved search already uses this name"
            );
            entries.insert(name.into(), search);
            Ok(())
        })
    }

    pub fn remove_sftp_search(
        &self,
        profile: &str,
        peer: &BookmarkPeer,
        name: &str,
        expected: &SavedSftpSearch,
    ) -> Result<Searches> {
        self.edit_sftp_searches(profile, peer, |entries| {
            ensure!(
                entries.get(name) == Some(expected),
                "The saved search changed. Reopen saved searches."
            );
            entries.remove(name);
            Ok(())
        })
    }

    fn edit_sftp_searches(
        &self,
        profile: &str,
        peer: &BookmarkPeer,
        change: impl FnOnce(&mut Searches) -> Result<()>,
    ) -> Result<Searches> {
        let mut current = Searches::new();
        self.modify(|profiles, catalog| {
            let options = catalog
                .profiles
                .get(profile)
                .context("The saved SSH profile is no longer available")?;
            peer.check_profile(options)?;
            let entry = profiles
                .get_mut(profile)
                .context("The SSH profile has no saved entry")?;
            let mut groups = read(entry)?;
            let index = groups
                .iter()
                .position(|group| &group.peer == peer)
                .unwrap_or_else(|| {
                    groups.push(Group {
                        peer: peer.clone(),
                        searches: Searches::new(),
                    });
                    groups.len() - 1
                });
            change(&mut groups[index].searches)?;
            current = groups[index].searches.clone();
            if current.is_empty() {
                groups.remove(index);
            }
            let object = entry
                .as_object_mut()
                .context("The SSH profile must be an object")?;
            if groups.is_empty() {
                object.remove(FIELD);
            } else {
                object.insert(FIELD.into(), serde_json::to_value(groups)?);
            }
            Ok(CatalogEdit::default())
        })?;
        Ok(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sftp::EntryKind;
    use serde_json::json;

    fn peer(fingerprint: &str) -> BookmarkPeer {
        serde_json::from_value(json!({
            "host": "box.example", "user": "dev", "port": 22, "fingerprint": fingerprint
        }))
        .unwrap()
    }

    fn preset() -> SavedSftpSearch {
        SavedSftpSearch {
            root: "/srv/crew's 日本".into(),
            query: r"^live/Report.+\.log$".into(),
            filter: FilenameFilter {
                regex: true,
                match_case: true,
                match_path: true,
                kind: Some(EntryKind::File),
            },
            sort: FilenameSort::Newest,
        }
    }

    fn store(dir: &tempfile::TempDir) -> ProfileStore {
        let path = dir.path().join("profiles.json");
        std::fs::write(
            &path,
            json!({
                "profiles": {
                    "work": {"user":"dev","host":"Box.Example","auth":"none","custom":42},
                    "other": {"user":"dev","host":"Box.Example","auth":"none"}
                }
            })
            .to_string(),
        )
        .unwrap();
        ProfileStore::new(path).unwrap()
    }

    #[test]
    fn saved_searches_round_trip_keep_options_and_scope_to_the_profile_and_peer() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let key = peer("SHA256:first");
        let another = peer("SHA256:second");
        let saved = preset();
        store
            .save_sftp_search("work", &key, "Reports", saved.clone())
            .unwrap();
        let reopened = ProfileStore::new(store.path().into()).unwrap();
        assert_eq!(
            reopened.list_sftp_searches("work", &key).unwrap()["Reports"],
            saved
        );
        assert!(
            reopened
                .list_sftp_searches("work", &another)
                .unwrap()
                .is_empty()
        );
        assert!(
            reopened
                .list_sftp_searches("other", &key)
                .unwrap()
                .is_empty()
        );
        assert!(
            reopened
                .save_sftp_search("work", &key, "Reports", saved.clone())
                .is_err()
        );
        let (_, before) = store.inspect("work").unwrap();
        store
            .edit("work", "renamed", &before.options, &before.options)
            .unwrap();
        let (_, renamed) = store.inspect("renamed").unwrap();
        store.duplicate(&renamed, "copy", &renamed.options).unwrap();
        assert!(store.list_sftp_searches("copy", &key).unwrap().is_empty());
        assert_eq!(
            store.list_sftp_searches("renamed", &key).unwrap()["Reports"],
            saved
        );
        store
            .remove_sftp_search("renamed", &key, "Reports", &saved)
            .unwrap();
        let doc: Value = serde_json::from_slice(&std::fs::read(store.path()).unwrap()).unwrap();
        assert!(doc["profiles"]["renamed"].get(FIELD).is_none());
        assert_eq!(doc["profiles"]["renamed"]["custom"], 42);
    }

    #[test]
    fn older_presets_default_to_filename_matching() {
        let old: SavedSftpSearch = serde_json::from_value(json!({
            "root": "/srv", "query": "report", "filter": { "regex": false, "match_case": true }
        }))
        .unwrap();
        assert!(old.filter.match_case);
        assert!(!old.filter.match_path);
        assert!(
            serde_json::to_value(old).unwrap()["filter"]
                .get("match_path")
                .is_none()
        );
    }

    #[test]
    fn saved_searches_reject_invalid_presets_and_leave_a_newer_entry_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let key = peer("SHA256:first");
        let old = preset();
        assert!(
            store
                .save_sftp_search(
                    "work",
                    &key,
                    "Bad",
                    SavedSftpSearch {
                        query: "[".into(),
                        ..old.clone()
                    }
                )
                .is_err()
        );
        assert!(
            store
                .save_sftp_search(
                    "work",
                    &key,
                    "Bad",
                    SavedSftpSearch {
                        root: "/srv/../other".into(),
                        ..old.clone()
                    }
                )
                .is_err()
        );
        store
            .save_sftp_search("work", &key, "Reports", old.clone())
            .unwrap();
        store
            .remove_sftp_search("work", &key, "Reports", &old)
            .unwrap();
        let updated = SavedSftpSearch {
            query: "latest".into(),
            ..old.clone()
        };
        store
            .save_sftp_search("work", &key, "Reports", updated.clone())
            .unwrap();
        assert!(
            store
                .remove_sftp_search("work", &key, "Reports", &old)
                .is_err()
        );
        assert_eq!(
            store.list_sftp_searches("work", &key).unwrap()["Reports"],
            updated
        );
    }
}
