//! Profile persistence preserves unrelated JSON fields and replaces the file atomically.
use crate::options::{SshOptions, load_profiles};
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeMap, io::Write, path::PathBuf};

pub struct ProfileStore {
    path: PathBuf,
}

impl ProfileStore {
    pub fn new(path: PathBuf) -> Result<Self> {
        let path = if path.is_absolute() {
            path
        } else {
            std::env::current_dir()?.join(path)
        };
        Ok(Self { path })
    }

    pub fn load(&self) -> Result<BTreeMap<String, SshOptions>> {
        Ok(load_profiles(&self.path)?.profiles)
    }

    pub fn add(&self, name: &str, options: &SshOptions) -> Result<()> {
        self.write(None, name, options, None)
    }
    pub fn update(&self, name: &str, options: &SshOptions) -> Result<()> {
        self.write(Some(name), name, options, None)
    }
    /// Edit/rename only the version loaded by the editor, preserving unrelated JSON.
    pub fn edit(
        &self,
        original: &str,
        name: &str,
        options: &SshOptions,
        expected: &SshOptions,
    ) -> Result<()> {
        self.write(Some(original), name, options, Some(expected))
    }
    fn write(
        &self,
        original: Option<&str>,
        name: &str,
        options: &SshOptions,
        expected: Option<&SshOptions>,
    ) -> Result<()> {
        ensure!(
            crate::assets::name(&options.player.bell),
            "Bell must be system, off, or a sound name (letters, digits, _ and -)"
        );
        ensure!(!name.trim().is_empty(), "Enter a profile name");
        ensure!(
            name == name.trim(),
            "Profile name cannot start or end with spaces"
        );
        ensure!(
            !options.host.trim().is_empty() && !options.user.trim().is_empty(),
            "Enter a host and user"
        );
        ensure!(options.port != 0, "Port must be between 1 and 65535");
        let before = match std::fs::read(&self.path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let mut document: serde_json::Value = match &before {
            Some(bytes) => serde_json::from_slice(bytes).context("Reading existing profiles")?,
            None => serde_json::json!({"profiles": {}}),
        };
        if let Some(expected) = expected {
            let current = crate::options::parse_profiles(
                before
                    .as_deref()
                    .context("Profile file was removed; close and reopen the editor")?,
                &self.path,
            )?;
            ensure!(
                current.profiles.get(original.unwrap()) == Some(expected),
                "Profile changed on disk; close and reopen the editor before saving"
            );
        }
        let profiles = document
            .get_mut("profiles")
            .and_then(|v| v.as_object_mut())
            .context("Configuration must contain a profiles object")?;
        if let Some(original) = original {
            ensure!(
                profiles.contains_key(original),
                "Profile {original} was removed"
            );
        }
        ensure!(
            original == Some(name) || !profiles.contains_key(name),
            "Profile {name} already exists"
        );
        let mut value = original
            .and_then(|original| profiles.get(original))
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let object = value.as_object_mut().context("profile must be an object")?;
        let keep_identity = expected
            .is_some_and(|previous| previous.authentication == options.authentication)
            && matches!(
                options.authentication,
                crate::options::Authentication::Identity { .. }
            );
        if !keep_identity {
            object.remove("identity");
        }
        for (key, value) in serde_json::to_value(options)?.as_object().unwrap() {
            if key == "identity" && keep_identity {
                continue;
            }
            let mut value = value.clone();
            if key == "player"
                && expected
                    .is_some_and(|previous| previous.player.device_key == options.player.device_key)
                && let Some(previous) = object
                    .get("player")
                    .and_then(|player| player.get("device_key"))
            {
                value["device_key"] = previous.clone();
            }
            object.insert(key.clone(), value);
        }
        if let Some(original) = original
            && original != name
        {
            profiles.remove(original);
        }
        profiles.insert(name.into(), value);
        // Validate the serialized profile, including its selected authentication mode.
        let _ = crate::options::parse_profiles(&serde_json::to_vec(&document)?, &self.path)?;
        let parent = self
            .path
            .parent()
            .context("Configuration has no parent directory")?;
        std::fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut file, &document)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        let current = match std::fs::read(&self.path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        ensure!(
            current == before,
            "Profiles changed while saving; try again"
        );
        file.persist(&self.path).context("Saving profiles")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::Authentication;

    #[test]
    fn saving_preserves_other_fields_and_refuses_to_overwrite_existing_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        std::fs::write(&path, r#"{"theme":"dark","profiles":{"existing":{"host":"example.test","user":"play","auth":"none","custom":42}}}"#).unwrap();
        let store = ProfileStore::new(path.clone()).unwrap();
        let options = SshOptions {
            host: "new.test".into(),
            user: "root".into(),
            port: 22,
            authentication: Authentication::Identity {
                identity: "local-key".into(),
            },
            host_key: None,
            player: Default::default(),
        };
        store.add("new", &options).unwrap();
        let saved = std::fs::read(&path).unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&saved).unwrap();
        assert_eq!(doc["theme"], "dark");
        assert_eq!(doc["profiles"]["existing"]["custom"], 42);
        assert_eq!(
            store.load().unwrap()["new"].authentication,
            Authentication::Identity {
                identity: dir.path().join("local-key")
            }
        );
        assert!(store.add("new", &options).is_err());
        assert_eq!(std::fs::read(path).unwrap(), saved);
    }
}

#[cfg(test)]
mod edit_tests {
    use super::*;
    use crate::options::Authentication;
    #[test]
    fn edit_renames_preserves_relative_keys_and_metadata_and_refuses_collisions_or_stale_edits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        std::fs::write(&path,r#"{"theme":"dark","profiles":{"typo":{"host":"old.test","user":"pilot","auth":"identity","identity":"keys/id","player":{"device_key":"keys/device"},"custom":42},"other":{"host":"other.test","user":"pilot","auth":"none"}}}"#).unwrap();
        let store = ProfileStore::new(path.clone()).unwrap();
        let original = store.load().unwrap()["typo"].clone();
        let mut changed = original.clone();
        changed.host = "fixed.test".into();
        changed.port = 6792;
        store.edit("typo", "correct", &changed, &original).unwrap();
        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(document["theme"], "dark");
        assert_eq!(document["profiles"]["correct"]["custom"], 42);
        assert_eq!(document["profiles"]["correct"]["identity"], "keys/id");
        assert_eq!(
            document["profiles"]["correct"]["player"]["device_key"],
            "keys/device"
        );
        assert!(document["profiles"].get("typo").is_none());
        assert_eq!(store.load().unwrap()["correct"], changed);
        let saved = std::fs::read(&path).unwrap();
        assert!(store.edit("correct", "other", &changed, &changed).is_err());
        assert!(
            store
                .edit("correct", "correct", &original, &original)
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), saved);
        let mut keyless = changed.clone();
        keyless.authentication = Authentication::None;
        store
            .edit("correct", "correct", &keyless, &changed)
            .unwrap();
        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(document["profiles"]["correct"].get("identity").is_none());
        assert_eq!(store.load().unwrap()["other"].host, "other.test");
    }
}
