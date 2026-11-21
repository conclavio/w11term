//! Profile persistence preserves unrelated JSON fields and replaces the file atomically.
use crate::options::{SshOptions, load_profiles};
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeMap, io::Write, path::PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct ProfileSnapshot {
    pub name: String,
    pub options: SshOptions,
    raw: serde_json::Value,
}

pub struct ProfileStore {
    path: PathBuf,
}

#[derive(Clone)]
pub struct LocalProfileSnapshot {
    pub name: String,
    pub options: crate::options::LocalOptions,
    raw: Option<serde_json::Value>,
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

    pub fn catalog(&self) -> Result<crate::options::Profiles> {
        load_profiles(&self.path)
    }

    pub fn inspect_local(&self, name: &str) -> Result<LocalProfileSnapshot> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => br#"{"profiles":{}}"#.to_vec(),
            Err(e) => return Err(e.into()),
        };
        let raw: serde_json::Value = serde_json::from_slice(&bytes)?;
        let options = crate::options::parse_profiles(&bytes, &self.path)?
            .local
            .remove(name)
            .context("Local profile was removed or its backend changed")?;
        Ok(LocalProfileSnapshot {
            name: name.into(),
            options,
            raw: raw["profiles"].get(name).cloned(),
        })
    }

    pub fn save_local(
        &self,
        original: Option<&LocalProfileSnapshot>,
        name: &str,
        shell: &crate::options::LocalOptions,
    ) -> Result<()> {
        ensure!(
            !name.is_empty() && name == name.trim() && !name.chars().any(char::is_control),
            "Enter a local profile name"
        );
        shell.validate()?;
        self.modify(|profiles, _| {
            if let Some(base) = original {
                ensure!(
                    profiles.get(&base.name) == base.raw.as_ref(),
                    "Local profile changed on disk; reopen the editor before saving"
                );
            }
            ensure!(
                original.is_some_and(|base| base.name == name)
                    || (!profiles.contains_key(name)
                        && !crate::options::LocalOptions::installed().contains_key(name)),
                "Profile {name} already exists"
            );
            let mut entry = original
                .and_then(|base| base.raw.clone())
                .unwrap_or_else(|| serde_json::json!({}));
            let fields = entry
                .as_object_mut()
                .context("Local profile must be an object")?;
            fields.insert("backend".into(), "local".into());
            let stored = original.filter(|base| base.raw.is_some());
            if stored.is_none_or(|base| base.options.executable != shell.executable) {
                fields.insert(
                    "executable".into(),
                    serde_json::to_value(&shell.executable)?,
                );
            }
            fields.insert("args".into(), serde_json::to_value(&shell.args)?);
            if stored.is_none_or(|base| base.options.cwd != shell.cwd) {
                fields.insert("cwd".into(), serde_json::to_value(&shell.cwd)?);
            }
            if let Some(base) = original
                && base.name != name
            {
                profiles.remove(&base.name);
            }
            profiles.insert(name.into(), entry);
            Ok(())
        })
    }

    /// Both the editor and the conflict check use the same read of profiles.json.
    pub fn inspect(&self, name: &str) -> Result<(BTreeMap<String, SshOptions>, ProfileSnapshot)> {
        let (catalog, snapshot) = self.inspect_catalog(name)?;
        Ok((catalog.profiles, snapshot))
    }

    pub fn inspect_catalog(
        &self,
        name: &str,
    ) -> Result<(crate::options::Profiles, ProfileSnapshot)> {
        let bytes = std::fs::read(&self.path)?;
        let catalog = crate::options::parse_profiles(&bytes, &self.path)?;
        let options = catalog
            .profiles
            .get(name)
            .with_context(|| format!("Profile {name} was removed"))?
            .clone();
        let raw: serde_json::Value = serde_json::from_slice(&bytes)?;
        let snapshot = ProfileSnapshot {
            name: name.into(),
            options,
            raw: raw["profiles"][name].clone(),
        };
        Ok((catalog, snapshot))
    }

    pub fn add(&self, name: &str, options: &SshOptions) -> Result<()> {
        self.write(None, name, options, None, None)
    }
    pub fn update(&self, name: &str, options: &SshOptions) -> Result<()> {
        self.write(Some(name), name, options, None, None)
    }
    /// Edit/rename only the version loaded by the editor, preserving unrelated JSON.
    pub fn edit(
        &self,
        original: &str,
        name: &str,
        options: &SshOptions,
        expected: &SshOptions,
    ) -> Result<()> {
        self.write(Some(original), name, options, Some(expected), None)
    }

    pub fn duplicate(
        &self,
        base: &ProfileSnapshot,
        name: &str,
        options: &SshOptions,
    ) -> Result<()> {
        self.write(None, name, options, Some(&base.options), Some(base))
    }

    pub fn delete(&self, base: &ProfileSnapshot) -> Result<()> {
        self.modify(|profiles, _| {
            ensure!(
                profiles.get(&base.name) == Some(&base.raw),
                "Profile changed on disk; reopen the editor before deleting"
            );
            profiles.remove(&base.name);
            Ok(())
        })
    }
    fn write(
        &self,
        original: Option<&str>,
        name: &str,
        options: &SshOptions,
        expected: Option<&SshOptions>,
        copy: Option<&ProfileSnapshot>,
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
        options.validate_route()?;
        self.modify(|profiles, before| {
            if let Some(copy) = copy {
                ensure!(
                    profiles.get(&copy.name) == Some(&copy.raw),
                    "Source profile changed on disk; reopen the editor before duplicating"
                );
            } else if let (Some(expected), Some(original)) = (expected, original) {
                let current = crate::options::parse_profiles(
                    before.context("Profile file was removed; close and reopen the editor")?,
                    &self.path,
                )?;
                ensure!(
                    current.profiles.get(original) == Some(expected),
                    "Profile changed on disk; close and reopen the editor before saving"
                );
            }
            if let Some(original) = original {
                let current = profiles
                    .get(original)
                    .with_context(|| format!("Profile {original} was removed"))?;
                ensure!(
                    current.get("backend").and_then(|v| v.as_str()) != Some("local"),
                    "Profile {original} is now a local shell; reopen the profile"
                );
            }
            ensure!(
                original == Some(name)
                    || (!profiles.contains_key(name)
                        && !crate::options::LocalOptions::installed().contains_key(name)),
                "Profile {name} already exists"
            );
            let mut value = original
                .or_else(|| copy.map(|base| base.name.as_str()))
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
            let keep_route =
                expected.is_some_and(|previous| previous.proxy_jump == options.proxy_jump);
            if options.terminal_type.is_default() {
                object.remove("terminal_type");
            }
            if !keep_route && options.proxy_jump.is_empty() {
                object.remove("proxy_jump");
            }
            for (key, value) in serde_json::to_value(options)?.as_object().unwrap() {
                if (key == "identity" && keep_identity) || (key == "proxy_jump" && keep_route) {
                    continue;
                }
                let mut value = value.clone();
                if key == "player"
                    && expected.is_some_and(|previous| {
                        previous.player.device_key == options.player.device_key
                    })
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
            Ok(())
        })
    }

    fn modify(
        &self,
        change: impl FnOnce(
            &mut serde_json::Map<String, serde_json::Value>,
            Option<&[u8]>,
        ) -> Result<()>,
    ) -> Result<()> {
        let parent = self
            .path
            .parent()
            .context("Configuration has no parent directory")?;
        std::fs::create_dir_all(parent)?;
        let mut lock_path = self.path.as_os_str().to_owned();
        lock_path.push(".w11term.lock");
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        lock.try_lock()
            .context("Another window is updating profiles; try again")?;
        let before = match std::fs::read(&self.path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let mut document: serde_json::Value = match &before {
            Some(bytes) => serde_json::from_slice(bytes).context("Reading existing profiles")?,
            None => serde_json::json!({"profiles": {}}),
        };
        let profiles = document
            .get_mut("profiles")
            .and_then(|v| v.as_object_mut())
            .context("Configuration must contain a profiles object")?;
        change(profiles, before.as_deref())?;
        // Validate the serialized profile, including its selected authentication mode.
        let _ = crate::options::parse_profiles(&serde_json::to_vec(&document)?, &self.path)?;
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
            terminal_type: Default::default(),
            user: "root".into(),
            port: 22,
            authentication: Authentication::Identity {
                identity: "local-key".into(),
            },
            host_key: None,
            proxy_jump: Vec::new(),
            durable: false,
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
    fn duplicate_preserves_keys_and_metadata_and_delete_checks_the_complete_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        let key = dir.path().join("key");
        std::fs::write(&key, b"fixture").unwrap();
        let source = serde_json::json!({"host":"db.test","auth":"identity","identity":"key","user":"pilot","player":{"device_key":"device"},"custom":{"x":42}});
        let mut doc = serde_json::json!({"theme":"dark", "profiles":{"db":source.clone()}});
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        let store = ProfileStore::new(path.clone()).unwrap();
        let (_, base) = store.inspect("db").unwrap();
        let mut copy = base.options.clone();
        copy.user = "admin".into();
        assert!(store.duplicate(&base, "db", &copy).is_err());
        store.duplicate(&base, "other", &copy).unwrap();
        doc = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(doc["profiles"]["db"], source);
        assert_eq!(doc["profiles"]["other"]["custom"], source["custom"]);
        assert_eq!(doc["profiles"]["other"]["identity"], "key");
        assert_eq!(doc["profiles"]["other"]["player"]["device_key"], "device");
        assert_eq!(doc["profiles"]["other"]["user"], "admin");
        doc["profiles"]["db"]["custom"]["x"] = 9.into();
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        assert!(store.delete(&base).is_err());
        assert!(store.duplicate(&base, "stale", &copy).is_err());
        let (_, fresh) = store.inspect("db").unwrap();
        store.delete(&fresh).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["theme"], "dark");
        assert_eq!(saved["profiles"]["other"], doc["profiles"]["other"]);
        assert!(saved["profiles"].get("db").is_none());
        assert_eq!(std::fs::read(&key).unwrap(), b"fixture");
    }
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
        changed.terminal_type = "xterm-sixel".to_owned().try_into().unwrap();
        store.edit("typo", "correct", &changed, &original).unwrap();
        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(document["theme"], "dark");
        assert_eq!(document["profiles"]["correct"]["custom"], 42);
        assert_eq!(document["profiles"]["correct"]["identity"], "keys/id");
        assert_eq!(
            document["profiles"]["correct"]["terminal_type"],
            "xterm-sixel"
        );
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
        keyless.terminal_type = Default::default();
        store
            .edit("correct", "correct", &keyless, &changed)
            .unwrap();
        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(document["profiles"]["correct"].get("identity").is_none());
        assert!(
            document["profiles"]["correct"]
                .get("terminal_type")
                .is_none()
        );
        assert_eq!(store.load().unwrap()["other"].host, "other.test");
    }
}

#[cfg(test)]
mod jump_tests {
    use super::*;
    use crate::options::Authentication;

    #[test]
    fn edits_preserve_relative_jump_identities_and_allow_clearing_the_route() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        let doc = serde_json::json!({"profiles":{"destination":{
            "host":"target", "user":"pilot", "auth":"agent",
            "proxy_jump":[{"host":"gate","user":"pilot","auth":"identity",
                 "identity":"keys/jump","custom":"keep"}]
        }}});
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        let store = ProfileStore::new(path.clone()).unwrap();
        let old = store.load().unwrap()["destination"].clone();
        assert_eq!(
            old.proxy_jump[0].authentication,
            Authentication::Identity {
                identity: dir.path().join("keys/jump")
            }
        );
        let mut edited = old.clone();
        edited.port = 2200;
        store
            .edit("destination", "destination", &edited, &old)
            .unwrap();
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            raw["profiles"]["destination"]["proxy_jump"],
            doc["profiles"]["destination"]["proxy_jump"]
        );
        let (_, base) = store.inspect("destination").unwrap();
        store.duplicate(&base, "second", &edited).unwrap();
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            raw["profiles"]["second"]["proxy_jump"],
            doc["profiles"]["destination"]["proxy_jump"]
        );
        let mut direct = edited.clone();
        direct.proxy_jump.clear();
        store
            .edit("destination", "destination", &direct, &edited)
            .unwrap();
        assert_eq!(store.load().unwrap()["destination"], direct);
    }
}
