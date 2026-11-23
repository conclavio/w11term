//! Profile persistence preserves unrelated JSON fields and replaces the file atomically.
use crate::options::{Profiles, SshOptions, load_profiles};
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeMap, io::Write, path::PathBuf};

mod bookmarks;
pub use bookmarks::{BookmarkPeer, bookmark_directory};
mod folder_permissions;
pub(crate) use folder_permissions::{FolderEpoch, FolderGrantGate, FolderMutationPermit};

#[derive(Clone, Debug, PartialEq)]
pub struct ProfileSnapshot {
    pub name: String,
    pub options: SshOptions,
    raw: serde_json::Value,
    catalog_path: PathBuf,
}

pub struct ProfileStore {
    path: PathBuf,
}

const SHARE_TOKEN: &str = "folder_share_id";
const SHARE_EPOCH: &str = "folder_share_epoch";
const STARTUP_PROFILE: &str = "startup_profile";
const SSH_HOST_GROUPS: &str = "ssh_host_groups";

#[derive(Default)]
struct CatalogEdit {
    order: Option<Vec<String>>,
    startup: Option<Option<String>>, // None leaves the setting untouched.
    ssh_host_groups: Option<BTreeMap<String, Vec<String>>>,
}

#[derive(Clone)]
pub struct LocalProfileSnapshot {
    pub name: String,
    pub options: crate::options::LocalOptions,
    raw: Option<serde_json::Value>,
    removal: Option<LocalProfileRemoval>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LocalProfileRemoval {
    Delete,
    RestoreDefaults,
}

impl LocalProfileSnapshot {
    pub fn removal(&self) -> Option<LocalProfileRemoval> {
        self.removal
    }

    fn check_current(
        &self,
        entries: &serde_json::Map<String, serde_json::Value>,
        catalog: &Profiles,
    ) -> Result<()> {
        ensure!(
            entries.get(&self.name) == self.raw.as_ref()
                && catalog.local.get(&self.name) == Some(&self.options),
            "Local profile changed on disk; reopen the editor before saving"
        );
        Ok(())
    }
}

/// Maintain an explicit ordering when a profile changes identity. With no
/// explicit order, ordinary edits continue using the alphabetical defaults.
fn order_after_edit(
    catalog: &Profiles,
    old: Option<&str>,
    new: Option<&str>,
) -> Option<Vec<String>> {
    if !catalog.has_explicit_order || old == new {
        return None;
    }
    let mut order = catalog.profile_order.clone();
    if let Some(index) = old.and_then(|name| order.iter().position(|entry| entry == name)) {
        if let Some(new) = new {
            order[index] = new.into();
        } else {
            order.remove(index);
        }
    } else if let Some(new) = new {
        order.push(new.into());
    }
    Some(order)
}

fn profile_identity_edit(catalog: &Profiles, old: Option<&str>, new: Option<&str>) -> CatalogEdit {
    CatalogEdit {
        order: order_after_edit(catalog, old, new),
        startup: (old.is_some() && old != new && catalog.startup_profile.as_deref() == old)
            .then(|| new.map(str::to_owned)),
        ..Default::default()
    }
}

fn ssh_profile_identity_edit(
    catalog: &Profiles,
    old: Option<&str>,
    new: Option<&str>,
) -> CatalogEdit {
    let mut edit = profile_identity_edit(catalog, old, new);
    if let Some(old) = old
        && Some(old) != new
        && catalog
            .ssh_host_groups
            .values()
            .any(|members| members.iter().any(|name| name == old))
    {
        let mut groups = catalog.ssh_host_groups.clone();
        for members in groups
            .values_mut()
            .filter(|members| members.iter().any(|name| name == old))
        {
            let mut seen = std::collections::HashSet::new();
            members.retain_mut(|name| {
                if name == old {
                    let Some(new) = new else { return false };
                    *name = new.to_owned();
                }
                seen.insert(name.clone())
            });
        }
        edit.ssh_host_groups = Some(groups);
    }
    edit
}

impl ProfileStore {
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

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

    pub fn save_ssh_host_group(
        &self,
        name: &str,
        members: &[String],
        original: Option<(&str, &[String])>,
    ) -> Result<Profiles> {
        ensure!(
            !name.is_empty() && name == name.trim() && !name.chars().any(char::is_control),
            "Enter a group name on one line"
        );
        self.modify(|_, catalog| {
            let mut groups = catalog.ssh_host_groups.clone();
            if let Some((old, expected)) = original {
                ensure!(
                    groups.get(old).map(Vec::as_slice) == Some(expected),
                    "The SSH group changed on disk. Reopen it before saving."
                );
                groups.remove(old);
            }
            ensure!(
                !groups.contains_key(name),
                "An SSH group named {name} already exists"
            );
            catalog.resolve_ssh_host_group_members(members)?;
            groups.insert(name.to_owned(), members.to_vec());
            Ok(CatalogEdit {
                ssh_host_groups: Some(groups),
                ..Default::default()
            })
        })
    }

    pub fn delete_ssh_host_group(&self, name: &str, expected: &[String]) -> Result<Profiles> {
        self.modify(|_, catalog| {
            ensure!(
                catalog.ssh_host_groups.get(name).map(Vec::as_slice) == Some(expected),
                "The SSH group changed on disk. Reopen it before deleting."
            );
            let mut groups = catalog.ssh_host_groups.clone();
            groups.remove(name);
            Ok(CatalogEdit {
                ssh_host_groups: Some(groups),
                ..Default::default()
            })
        })
    }

    pub fn set_startup_profile(
        &self,
        name: Option<&str>,
        expected: Option<&str>,
    ) -> Result<Profiles> {
        self.modify(|_, catalog| {
            ensure!(
                catalog.startup_profile.as_deref() == expected,
                "The startup profile changed on disk; choose again"
            );
            if let Some(name) = name {
                ensure!(
                    catalog.profiles.contains_key(name) || catalog.local.contains_key(name),
                    "Profile {name} is no longer available"
                );
            }
            Ok(CatalogEdit {
                startup: Some(name.map(str::to_owned)),
                ..Default::default()
            })
        })
    }

    /// Move one position relative to the catalog shown to the user. Validate
    /// membership and order while holding the same lock used by profile edits.
    pub fn move_profile(
        &self,
        name: &str,
        delta: isize,
        expected_order: &[String],
    ) -> Result<Profiles> {
        ensure!(
            matches!(delta, -1 | 1),
            "Move a profile up or down by one position"
        );
        self.modify(|_, catalog| {
            ensure!(
                catalog.profile_order == expected_order,
                "Profile order changed on disk; refresh before moving"
            );
            let mut order = catalog.profile_order.clone();
            let index = order
                .iter()
                .position(|entry| entry == name)
                .with_context(|| format!("Profile {name} is no longer available"))?;
            let next = index
                .checked_add_signed(delta)
                .filter(|&next| next < order.len())
                .context("This profile is already at the end in that direction")?;
            order.swap(index, next);
            Ok(CatalogEdit {
                order: Some(order),
                ..Default::default()
            })
        })
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
        let raw = raw["profiles"].get(name).cloned();
        let removal = raw.as_ref().map(|_| {
            if crate::options::LocalOptions::installed().contains_key(name) {
                LocalProfileRemoval::RestoreDefaults
            } else {
                LocalProfileRemoval::Delete
            }
        });
        Ok(LocalProfileSnapshot {
            name: name.into(),
            options,
            raw,
            removal,
        })
    }

    pub fn save_local(
        &self,
        original: Option<&LocalProfileSnapshot>,
        name: &str,
        shell: &crate::options::LocalOptions,
    ) -> Result<()> {
        self.write_local(original, name, shell, false)
    }

    pub fn duplicate_local(
        &self,
        source: &LocalProfileSnapshot,
        name: &str,
        shell: &crate::options::LocalOptions,
    ) -> Result<()> {
        self.write_local(Some(source), name, shell, true)
    }

    pub fn remove_local(&self, base: &LocalProfileSnapshot) -> Result<Profiles> {
        self.modify(|profiles, catalog| {
            base.check_current(profiles, catalog)?;
            let action = base
                .removal
                .context("This shell uses its installed defaults")?;
            let installed = crate::options::LocalOptions::installed().contains_key(&base.name);
            ensure!(
                installed == (action == LocalProfileRemoval::RestoreDefaults),
                "The installed shell changed; reopen the profile before continuing"
            );
            profiles.remove(&base.name);
            // A discovered profile comes back in the same position after its override is removed.
            Ok(if installed {
                CatalogEdit::default()
            } else {
                profile_identity_edit(catalog, Some(&base.name), None)
            })
        })
    }

    fn write_local(
        &self,
        base: Option<&LocalProfileSnapshot>,
        name: &str,
        shell: &crate::options::LocalOptions,
        copy: bool,
    ) -> Result<()> {
        ensure!(
            !name.is_empty() && name == name.trim() && !name.chars().any(char::is_control),
            "Enter a local profile name"
        );
        shell.validate()?;
        self.modify(|profiles, catalog| {
            if let Some(base) = base {
                base.check_current(profiles, catalog)?;
            }
            let original = base.filter(|_| !copy);
            ensure!(
                original.is_some_and(|base| base.name == name)
                    || (!profiles.contains_key(name) && !catalog.local.contains_key(name)),
                "Profile {name} already exists"
            );
            let mut entry = base
                .and_then(|base| base.raw.clone())
                .unwrap_or_else(|| serde_json::json!({}));
            let fields = entry
                .as_object_mut()
                .context("Local profile must be an object")?;
            fields.insert("backend".into(), "local".into());
            let stored = base.filter(|base| base.raw.is_some());
            if stored.is_none_or(|base| base.options.executable != shell.executable) {
                fields.insert(
                    "executable".into(),
                    serde_json::to_value(&shell.executable)?,
                );
            }
            fields.insert("args".into(), serde_json::to_value(&shell.args)?);
            if shell.color_scheme.is_default() {
                fields.remove("color_scheme");
            } else {
                fields.insert(
                    "color_scheme".into(),
                    serde_json::to_value(shell.color_scheme)?,
                );
            }
            if shell.tab_color.is_default() {
                fields.remove("tab_color");
            } else {
                fields.insert("tab_color".into(), serde_json::to_value(shell.tab_color)?);
            }
            if shell.scrollback_lines.is_default() {
                fields.remove("scrollback_lines");
            } else {
                fields.insert(
                    "scrollback_lines".into(),
                    serde_json::to_value(shell.scrollback_lines)?,
                );
            }
            if shell.powershell_integration {
                fields.insert("powershell_integration".into(), true.into());
            } else {
                fields.remove("powershell_integration");
            }
            if stored.is_none_or(|base| base.options.cwd != shell.cwd) {
                fields.insert("cwd".into(), serde_json::to_value(&shell.cwd)?);
            }
            if let Some(base) = original
                && base.name != name
            {
                profiles.remove(&base.name);
            }
            profiles.insert(name.into(), entry);
            Ok(profile_identity_edit(
                catalog,
                original.map(|base| base.name.as_str()),
                Some(name),
            ))
        })
        .map(|_| ())
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
            catalog_path: std::fs::canonicalize(&self.path)?,
        };
        Ok((catalog, snapshot))
    }

    pub fn add(&self, name: &str, options: &SshOptions) -> Result<()> {
        self.add_to_catalog(name, options).map(|_| ())
    }
    /// Return the catalog committed by the same locked update.
    pub fn add_to_catalog(&self, name: &str, options: &SshOptions) -> Result<Profiles> {
        self.write(None, name, options, None, None)
    }
    pub fn update(&self, name: &str, options: &SshOptions) -> Result<()> {
        self.write(Some(name), name, options, None, None)
            .map(|_| ())
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
            .map(|_| ())
    }

    pub fn duplicate(
        &self,
        base: &ProfileSnapshot,
        name: &str,
        options: &SshOptions,
    ) -> Result<()> {
        self.write(None, name, options, Some(&base.options), Some(base))
            .map(|_| ())
    }

    pub fn delete(&self, base: &ProfileSnapshot) -> Result<()> {
        self.modify(|profiles, catalog| {
            ensure!(
                profiles.get(&base.name) == Some(&base.raw),
                "Profile changed on disk; reopen the editor before deleting"
            );
            profiles.remove(&base.name);
            Ok(ssh_profile_identity_edit(catalog, Some(&base.name), None))
        })
        .map(|_| ())
    }
    fn write(
        &self,
        original: Option<&str>,
        name: &str,
        options: &SshOptions,
        expected: Option<&SshOptions>,
        copy: Option<&ProfileSnapshot>,
    ) -> Result<Profiles> {
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
        self.modify(|profiles, catalog| {
            if let Some(copy) = copy {
                ensure!(
                    profiles.get(&copy.name) == Some(&copy.raw),
                    "Source profile changed on disk; reopen the editor before duplicating"
                );
            } else if let (Some(expected), Some(original)) = (expected, original) {
                ensure!(
                    catalog.profiles.get(original) == Some(expected),
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
                    || (!profiles.contains_key(name) && !catalog.local.contains_key(name)),
                "Profile {name} already exists"
            );
            let mut value = original
                .or_else(|| copy.map(|base| base.name.as_str()))
                .and_then(|original| profiles.get(original))
                .cloned()
                .unwrap_or(serde_json::json!({}));
            let object = value.as_object_mut().context("profile must be an object")?;
            if copy.is_some() {
                // A different profile starts with its own remote places.
                object.remove(bookmarks::FIELD);
            }
            if original != Some(name)
                || original
                    .and_then(|key| catalog.profiles.get(key))
                    .is_some_and(|old| {
                        !old.host.eq_ignore_ascii_case(&options.host)
                            || old.user != options.user
                            || old.port != options.port
                            || old.host_key != options.host_key
                            || old.host_key_alias != options.host_key_alias
                    })
            {
                object.remove(SHARE_TOKEN);
                // Distinguish a recreated profile even when its visible options
                // match a still-open folder picker from the old incarnation.
                object.insert(
                    SHARE_EPOCH.into(),
                    format!("{:032x}", rand::random::<u128>()).into(),
                );
            }
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
            let keep_forwards =
                expected.is_some_and(|previous| previous.local_forwards == options.local_forwards);
            if options.terminal_type.is_default() {
                object.remove("terminal_type");
            }
            if options.color_scheme.is_default() {
                object.remove("color_scheme");
            }
            if options.tab_color.is_default() {
                object.remove("tab_color");
            }
            if options.scrollback_lines.is_default() {
                object.remove("scrollback_lines");
            }
            if options.host_key_alias.is_none() {
                object.remove("host_key_alias");
            }
            if !keep_route && options.proxy_jump.is_empty() {
                object.remove("proxy_jump");
            }
            if !keep_forwards && options.local_forwards.is_empty() {
                object.remove("local_forwards");
            }
            for (key, value) in serde_json::to_value(options)?.as_object().unwrap() {
                if (key == "identity" && keep_identity)
                    || (key == "proxy_jump" && keep_route)
                    || (key == "local_forwards" && keep_forwards)
                {
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
            Ok(ssh_profile_identity_edit(catalog, original, Some(name)))
        })
    }

    pub(crate) fn lock(&self) -> Result<std::fs::File> {
        let lock = self.open_catalog_lock()?;
        lock.try_lock()
            .context("Another window is updating profiles; try again")?;
        Ok(lock)
    }

    fn open_catalog_lock(&self) -> Result<std::fs::File> {
        let parent = self
            .path
            .parent()
            .context("Configuration has no parent directory")?;
        std::fs::create_dir_all(parent)?;
        let mut lock_path = self.path.as_os_str().to_owned();
        lock_path.push(".w11term.lock");
        Ok(std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?)
    }

    fn modify(
        &self,
        change: impl FnOnce(
            &mut serde_json::Map<String, serde_json::Value>,
            &Profiles,
        ) -> Result<CatalogEdit>,
    ) -> Result<Profiles> {
        let _lock = self.lock()?;
        self.modify_locked(change)
    }

    fn modify_locked(
        &self,
        change: impl FnOnce(
            &mut serde_json::Map<String, serde_json::Value>,
            &Profiles,
        ) -> Result<CatalogEdit>,
    ) -> Result<Profiles> {
        let parent = self
            .path
            .parent()
            .context("Configuration has no parent directory")?;
        let before = match std::fs::read(&self.path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let bytes = before.as_deref().unwrap_or(br#"{"profiles":{}}"#);
        let catalog = crate::options::parse_profiles(bytes, &self.path)?;
        let mut document: serde_json::Value =
            serde_json::from_slice(bytes).context("Reading existing profiles")?;
        let fields = document
            .as_object_mut()
            .context("Configuration must be an object")?;
        let profiles = fields
            .get_mut("profiles")
            .and_then(|v| v.as_object_mut())
            .context("Configuration must contain a profiles object")?;
        let edit = change(profiles, &catalog)?;
        if let Some(order) = edit.order {
            fields.insert("profile_order".into(), serde_json::to_value(order)?);
        }
        match edit.startup {
            Some(Some(name)) => {
                fields.insert(STARTUP_PROFILE.into(), name.into());
            }
            Some(None) => {
                fields.remove(STARTUP_PROFILE);
            }
            None => {}
        }
        if let Some(groups) = edit.ssh_host_groups {
            if groups.is_empty() {
                fields.remove(SSH_HOST_GROUPS);
            } else {
                fields.insert(SSH_HOST_GROUPS.into(), serde_json::to_value(groups)?);
            }
        }
        // Return the catalog from the exact document being committed.
        let result = crate::options::parse_profiles(&serde_json::to_vec(&document)?, &self.path)?;
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
        Ok(result)
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
            scrollback_lines: Default::default(),
            color_scheme: Default::default(),
            tab_color: Default::default(),
            terminal_type: Default::default(),
            user: "root".into(),
            port: 22,
            authentication: Authentication::Identity {
                identity: "local-key".into(),
            },
            host_key: None,
            host_key_alias: None,
            proxy_jump: Vec::new(),
            local_forwards: Vec::new(),
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

    #[test]
    fn ssh_host_groups_save_edit_rename_and_delete_with_the_profile_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        std::fs::write(
            &path,
            r#"{"other":17,"profiles":{
            "East":{"host":"east.test","user":"u","auth":"none"},
            "West":{"host":"west.test","user":"u","auth":"none"},
            "Local":{"backend":"local","executable":"powershell.exe"},
            "Durable":{"host":"old.test","user":"u","auth":"none","durable":true},
            "Game":{"host":"game.test","user":"u","auth":"none","player":{"rustmush":true}}
        }}"#,
        )
        .unwrap();
        let store = ProfileStore::new(path.clone()).unwrap();
        assert!(store.catalog().unwrap().ssh_host_groups.is_empty());
        let ordered = ["West".into(), "East".into()];
        for disallowed in ["Local", "Durable", "Game", "Missing"] {
            assert!(
                store
                    .save_ssh_host_group("Fleet", &[disallowed.into()], None)
                    .is_err()
            );
        }
        assert!(
            store
                .save_ssh_host_group("Fleet", &["East".into(), "East".into()], None)
                .is_err()
        );
        store.save_ssh_host_group("Fleet", &ordered, None).unwrap();
        assert!(store.save_ssh_host_group("Fleet", &ordered, None).is_err());
        let second = ProfileStore::new(path.clone()).unwrap();
        assert_eq!(second.catalog().unwrap().ssh_host_groups["Fleet"], ordered);

        let (_, east) = second.inspect("East").unwrap();
        second
            .edit("East", "East Coast", &east.options, &east.options)
            .unwrap();
        let catalog = store.catalog().unwrap();
        let after_rename = catalog.ssh_host_groups["Fleet"].clone();
        assert_eq!(after_rename, ["West", "East Coast"]);
        assert_eq!(
            catalog
                .resolve_ssh_host_group_members(&after_rename)
                .unwrap()[1]
                .host,
            "east.test"
        );
        assert!(
            store
                .save_ssh_host_group("Stale", &ordered, Some(("Fleet", &ordered)))
                .is_err()
        );
        assert!(store.delete_ssh_host_group("Fleet", &ordered).is_err());

        store
            .save_ssh_host_group("Edge", &after_rename, Some(("Fleet", &after_rename)))
            .unwrap();
        let (_, west) = second.inspect("West").unwrap();
        second.delete(&west).unwrap();
        let remaining = store.catalog().unwrap();
        assert_eq!(remaining.ssh_host_groups["Edge"], ["East Coast"]);
        store
            .delete_ssh_host_group("Edge", &remaining.ssh_host_groups["Edge"])
            .unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(doc["other"], 17);
        assert!(doc.get(SSH_HOST_GROUPS).is_none());
        assert_eq!(doc["profiles"]["East Coast"]["host"], "east.test");
    }
}

#[cfg(test)]
mod order_tests {
    use super::*;
    use serde_json::{Value, json};

    fn fixture() -> (tempfile::TempDir, ProfileStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(dir.path().join("profiles.json")).unwrap();
        std::fs::write(
            store.path(),
            serde_json::to_vec(&json!({
                "theme":{"tone":"dark"},
                "profiles":{
                    "Alpha":{"host":"a.test","user":"one","auth":"none","extra":7},
                    "Zulu":{"host":"z.test","user":"two","auth":"none"},
                    "Lima":{"backend":"local","executable":"cmd.exe","extra":"local"}
                }
            }))
            .unwrap(),
        )
        .unwrap();
        (dir, store)
    }

    fn disk(store: &ProfileStore) -> Value {
        serde_json::from_slice(&std::fs::read(store.path()).unwrap()).unwrap()
    }

    #[test]
    fn startup_profile_follows_renames_and_clears_when_deleted() {
        let (_dir, store) = fixture();
        store.set_startup_profile(Some("Alpha"), None).unwrap();
        let first = disk(&store);
        assert_eq!(first[STARTUP_PROFILE], "Alpha");
        assert_eq!(first["theme"]["tone"], "dark");
        assert!(store.set_startup_profile(Some("Zulu"), None).is_err());
        assert!(
            store
                .set_startup_profile(Some("missing"), Some("Alpha"))
                .is_err()
        );
        assert_eq!(disk(&store), first);

        let base = store.inspect("Alpha").unwrap().1;
        store.duplicate(&base, "Alpha copy", &base.options).unwrap();
        assert_eq!(disk(&store)[STARTUP_PROFILE], "Alpha");
        store
            .edit("Alpha", "Renamed", &base.options, &base.options)
            .unwrap();
        assert_eq!(
            store.catalog().unwrap().startup_profile.as_deref(),
            Some("Renamed")
        );
        store.delete(&store.inspect("Renamed").unwrap().1).unwrap();
        assert!(disk(&store).get(STARTUP_PROFILE).is_none());

        store.set_startup_profile(Some("Lima"), None).unwrap();
        let base = store.inspect_local("Lima").unwrap();
        store
            .save_local(Some(&base), "Renamed local", &base.options)
            .unwrap();
        assert_eq!(disk(&store)[STARTUP_PROFILE], "Renamed local");
        let catalog = store
            .remove_local(&store.inspect_local("Renamed local").unwrap())
            .unwrap();
        assert!(catalog.startup_profile.is_none());
        assert!(disk(&store).get(STARTUP_PROFILE).is_none());
    }

    #[test]
    fn copy_local_preserves_relative_launch_paths_and_refuses_stale_or_occupied_entries() {
        let (_dir, store) = fixture();
        let mut doc = disk(&store);
        let local = json!({"backend":"local", "executable":"bin/shell.exe", "cwd":"build", "args":["two words"], "scrollback_lines":87, "extra":{"version":2}});
        doc["profiles"]["Lima"] = local.clone();
        doc["profile_order"] = json!(["Zulu", "Lima", "Alpha"]);
        std::fs::write(store.path(), doc.to_string()).unwrap();
        let source = store.inspect_local("Lima").unwrap();
        assert!(source.removal() == Some(LocalProfileRemoval::Delete));
        let copy = store.catalog().unwrap().copy_name("Lima");
        for taken in ["Alpha", "Lima"] {
            assert!(
                store
                    .duplicate_local(&source, taken, &source.options)
                    .is_err()
            );
        }
        let mut edited = source.options.clone();
        edited.args.push("-new".into());
        store.duplicate_local(&source, &copy, &edited).unwrap();
        let document = disk(&store);
        assert_eq!(document["profiles"]["Lima"], local);
        let mut expected_copy = local;
        expected_copy["args"] = json!(["two words", "-new"]);
        assert_eq!(document["profiles"][&copy], expected_copy);
        let catalog = store.catalog().unwrap();
        assert_eq!(catalog.copy_name("Lima"), "Lima copy 2");
        assert_eq!(catalog.profile_order.last(), Some(&copy));

        let mut changed = document;
        changed["profiles"]["Lima"]["extra"]["version"] = json!(3);
        std::fs::write(store.path(), changed.to_string()).unwrap();
        assert!(store.duplicate_local(&source, "another", &edited).is_err());
        assert!(store.remove_local(&source).is_err());
        assert_eq!(disk(&store), changed);
        let catalog = store
            .remove_local(&store.inspect_local(&copy).unwrap())
            .unwrap();
        assert!(!catalog.profile_order.contains(&copy));
        assert!(catalog.local.contains_key("Lima"));
        assert!(store.inspect_local(&copy).is_err());
    }

    #[test]
    fn removing_an_installed_override_restores_its_defaults_and_menu_position() {
        let (_dir, store) = fixture();
        let Some((name, defaults)) = crate::options::LocalOptions::installed().into_iter().next()
        else {
            return;
        };
        let pristine = store.inspect_local(&name).unwrap();
        assert!(pristine.removal().is_none());
        assert!(store.remove_local(&pristine).is_err());
        store
            .duplicate_local(&pristine, "discovered copy", &defaults)
            .unwrap();
        assert_eq!(store.catalog().unwrap().local["discovered copy"], defaults);
        let catalog = store.catalog().unwrap();
        let index = catalog
            .profile_order
            .iter()
            .position(|entry| entry == &name)
            .unwrap();
        let order = store
            .move_profile(
                &name,
                if index > 0 { -1 } else { 1 },
                &catalog.profile_order,
            )
            .unwrap()
            .profile_order;

        let mut edited = defaults.clone();
        edited.scrollback_lines = 0.try_into().unwrap();
        store.save_local(Some(&pristine), &name, &edited).unwrap();
        let base = store.inspect_local(&name).unwrap();
        assert!(base.removal() == Some(LocalProfileRemoval::RestoreDefaults));
        let restored = store.remove_local(&base).unwrap();
        assert_eq!(restored.profile_order, order);
        assert_eq!(restored.local[&name], defaults);
        assert!(disk(&store)["profiles"].get(&name).is_none());
        assert!(store.remove_local(&base).is_err());
    }

    #[test]
    fn schemes_and_tab_colors_survive_profile_copies_and_allow_resets() {
        use crate::options::ColorScheme::{Default, OneHalfDark, OneHalfLight};
        use crate::options::TabColor;
        let (_dir, store) = fixture();
        let catalog = store.catalog().unwrap();
        assert_eq!(catalog.source("Alpha").unwrap().color_scheme(), Default);
        assert_eq!(catalog.source("Lima").unwrap().color_scheme(), Default);
        assert_eq!(catalog.source("Alpha").unwrap().tab_color(), TabColor::None);
        assert_eq!(catalog.source("Lima").unwrap().tab_color(), TabColor::None);

        let mut ssh = catalog.profiles["Alpha"].clone();
        ssh.color_scheme = OneHalfLight;
        ssh.tab_color = TabColor::Red;
        store
            .edit("Alpha", "Alpha", &ssh, &catalog.profiles["Alpha"])
            .unwrap();
        let base = store.inspect("Alpha").unwrap().1;
        store.duplicate(&base, "SSH copy", &base.options).unwrap();
        let mut shell = catalog.local["Lima"].clone();
        shell.color_scheme = OneHalfDark;
        shell.tab_color = TabColor::Blue;
        store
            .save_local(Some(&store.inspect_local("Lima").unwrap()), "Lima", &shell)
            .unwrap();
        let base = store.inspect_local("Lima").unwrap();
        store
            .duplicate_local(&base, "Local copy", &base.options)
            .unwrap();

        let catalog = store.catalog().unwrap();
        assert_eq!(
            catalog.source("SSH copy").unwrap().color_scheme(),
            OneHalfLight
        );
        assert_eq!(
            catalog.source("Local copy").unwrap().color_scheme(),
            OneHalfDark
        );
        assert_eq!(
            catalog.source("SSH copy").unwrap().tab_color(),
            TabColor::Red
        );
        assert_eq!(
            catalog.source("Local copy").unwrap().tab_color(),
            TabColor::Blue
        );
        ssh.color_scheme = Default;
        ssh.tab_color = TabColor::None;
        store
            .edit("Alpha", "Alpha", &ssh, &catalog.profiles["Alpha"])
            .unwrap();
        shell.color_scheme = Default;
        shell.tab_color = TabColor::None;
        store.save_local(Some(&base), "Lima", &shell).unwrap();
        let entries = &disk(&store)["profiles"];
        assert!(entries["Alpha"].get("color_scheme").is_none());
        assert!(entries["Lima"].get("color_scheme").is_none());
        assert!(entries["Alpha"].get("tab_color").is_none());
        assert!(entries["Lima"].get("tab_color").is_none());
        assert_eq!(entries["SSH copy"]["tab_color"], "red");
        assert_eq!(entries["Local copy"]["tab_color"], "blue");
        assert_eq!(entries["SSH copy"]["color_scheme"], "one-half-light");
        assert_eq!(entries["Local copy"]["color_scheme"], "one-half-dark");
        assert_eq!(entries["SSH copy"]["extra"], 7);
        assert_eq!(entries["Local copy"]["extra"], "local");
    }

    #[test]
    fn scrollback_limits_round_trip_for_both_backends_and_can_return_to_default() {
        let (_dir, store) = fixture();
        for (limit, disk_value) in [(0, Some(0)), (32_767, Some(32_767)), (10_000, None)] {
            let previous = store.inspect("Alpha").unwrap().1;
            let mut ssh = previous.options.clone();
            ssh.scrollback_lines = limit.try_into().unwrap();
            store
                .edit("Alpha", "Alpha", &ssh, &previous.options)
                .unwrap();
            let previous = store.inspect_local("Lima").unwrap();
            let mut shell = previous.options.clone();
            shell.scrollback_lines = ssh.scrollback_lines;
            store.save_local(Some(&previous), "Lima", &shell).unwrap();

            let catalog = store.catalog().unwrap();
            let document = disk(&store);
            for name in ["Alpha", "Lima"] {
                assert_eq!(
                    catalog.source(name).unwrap().scrollback_lines().get(),
                    limit as usize
                );
                assert_eq!(
                    document["profiles"][name]
                        .get("scrollback_lines")
                        .and_then(Value::as_u64),
                    disk_value
                );
            }
            assert_eq!(document["profiles"]["Alpha"]["extra"], 7);
            assert_eq!(document["profiles"]["Lima"]["extra"], "local");
        }
    }

    #[test]
    fn malformed_scrollback_limits_report_the_profile() {
        let (_dir, store) = fixture();
        let original = disk(&store);
        for name in ["Alpha", "Lima"] {
            for invalid in [
                json!(-1),
                json!(32_768),
                json!(0.5),
                json!("20"),
                Value::Null,
            ] {
                let mut config = original.clone();
                config["profiles"][name]["scrollback_lines"] = invalid;
                let bytes = serde_json::to_vec(&config).unwrap();
                let error = crate::options::parse_profiles(&bytes, store.path()).unwrap_err();
                assert!(error.to_string().contains(name));
            }
        }
    }

    #[test]
    fn stale_live_ssh_settings_cannot_restore_an_earlier_scrollback_limit() {
        let (_dir, store) = fixture();
        let before = store.inspect("Alpha").unwrap().1.options;
        let mut edited = before.clone();
        edited.scrollback_lines = 0.try_into().unwrap();
        store.edit("Alpha", "Alpha", &edited, &before).unwrap();

        let mut stale = before.clone();
        stale.player.mute = true;
        assert!(store.edit("Alpha", "Alpha", &stale, &before).is_err());
        assert_eq!(store.inspect("Alpha").unwrap().1.options, edited);
    }

    #[test]
    fn move_is_one_step_and_stale_views_cannot_overwrite_the_next_edit() {
        let (_dir, store) = fixture();
        let original = store.catalog().unwrap();
        let mut doc = disk(&store);
        // An unrelated external edit made after rendering must survive the reorder.
        doc["theme"]["accent"] = json!("green");
        std::fs::write(store.path(), serde_json::to_vec(&doc).unwrap()).unwrap();
        let moved = store
            .move_profile("Zulu", -1, &original.profile_order)
            .unwrap();
        let mut expected = original.profile_order.clone();
        expected.swap(0, 1);
        assert_eq!(moved.profile_order, expected);
        assert_eq!(moved, store.catalog().unwrap());
        doc["profile_order"] = json!(expected);
        assert_eq!(disk(&store), doc);
        for (name, delta, order) in [
            ("Zulu", 1, &original.profile_order),
            ("Zulu", -1, &expected),
            (expected.last().unwrap(), 1, &expected),
            ("absent", 1, &expected),
            ("Alpha", 2, &expected),
        ] {
            assert!(store.move_profile(name, delta, order).is_err());
        }
        assert_eq!(disk(&store), doc);
        // A new unseen profile also invalidates the old view.
        store.add("External", &moved.profiles["Alpha"]).unwrap();
        assert!(store.move_profile("Alpha", -1, &expected).is_err());
        assert_eq!(
            store.catalog().unwrap().profile_order.last().unwrap(),
            "External"
        );
    }

    #[test]
    fn concurrent_moves_from_the_same_view_have_one_winner_and_rename_keeps_its_slot() {
        let (_dir, store) = fixture();
        let (_, base) = store.inspect("Alpha").unwrap();
        let order = store.catalog().unwrap().profile_order;
        let barrier = std::sync::Barrier::new(2);
        let move_one = |name, delta| {
            let other = ProfileStore::new(store.path().to_owned()).unwrap();
            barrier.wait();
            other.move_profile(name, delta, &order)
        };
        let results = std::thread::scope(|scope| {
            let first = scope.spawn(|| move_one("Alpha", 1));
            let second = move_one("Lima", -1);
            [first.join().unwrap(), second]
        });
        let mut winners = results.into_iter().filter_map(Result::ok);
        let last = winners.next().expect("one writer must succeed");
        assert!(winners.next().is_none());
        assert_eq!(store.catalog().unwrap(), last);
        store
            .edit("Alpha", "Renamed", &base.options, &base.options)
            .unwrap();
        let mut expected = last.profile_order;
        let at = expected.iter().position(|s| s == "Alpha").unwrap();
        expected[at] = "Renamed".into();
        assert_eq!(store.catalog().unwrap().profile_order, expected);
    }

    #[test]
    fn explicit_order_follows_ssh_and_local_edits_without_materializing_builtin_shells() {
        let (_dir, store) = fixture();
        let mut doc = disk(&store);
        doc["profile_order"] = json!(["Lima", "faded", "Zulu", "Lima"]);
        std::fs::write(store.path(), serde_json::to_vec(&doc).unwrap()).unwrap();
        let order = store.catalog().unwrap().profile_order;
        let (_, base) = store.inspect("Zulu").unwrap();
        store
            .edit("Zulu", "Remote", &base.options, &base.options)
            .unwrap();
        let local = store.inspect_local("Lima").unwrap();
        store
            .save_local(Some(&local), "Build", &local.options)
            .unwrap();
        let (_, fresh) = store.inspect("Remote").unwrap();
        store.duplicate(&fresh, "Copy", &fresh.options).unwrap();
        store.save_local(None, "New shell", &local.options).unwrap();
        let mut expected = order;
        expected[0] = "Build".into();
        expected[1] = "Remote".into();
        expected.extend(["Copy".into(), "New shell".into()]);
        assert_eq!(store.catalog().unwrap().profile_order, expected);
        let (_, duplicate) = store.inspect("Copy").unwrap();
        store.delete(&duplicate).unwrap();
        expected.retain(|n| n != "Copy");
        let saved = disk(&store);
        assert_eq!(saved["profile_order"], json!(expected));
        assert_eq!(saved["profiles"]["Build"]["extra"], "local");
        assert_eq!(saved["profiles"]["Alpha"]["extra"], 7);
        assert_eq!(saved["profiles"].as_object().unwrap().len(), 4);
        assert!(saved["profiles"].get("Zulu").is_none());
        assert!(saved["profiles"].get("Lima").is_none());
        // An installed local shell is still launchable solely through discovery.
        if let Some((builtin, _)) = crate::options::LocalOptions::installed().into_iter().next() {
            let moved = store.move_profile(&builtin, -1, &expected).unwrap();
            assert!(moved.local.contains_key(&builtin));
            assert!(disk(&store)["profiles"].get(&builtin).is_none());
        }
    }

    #[test]
    fn ordinary_profile_edits_keep_an_absent_order_implicit() {
        let (_dir, store) = fixture();
        let (_, base) = store.inspect("Zulu").unwrap();
        store
            .edit("Zulu", "Bravo", &base.options, &base.options)
            .unwrap();
        store.duplicate(&base, "unused", &base.options).unwrap_err();
        store.add("Aardvark", &base.options).unwrap();
        let catalog = store.catalog().unwrap();
        assert_eq!(&catalog.profile_order[..3], &["Aardvark", "Alpha", "Bravo"]);
        assert!(disk(&store).get("profile_order").is_none());
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

    #[test]
    fn forward_presets_survive_edit_and_duplicate_and_can_be_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        let forwards =
            serde_json::json!([{"local_port":8080, "remote_port":8000, "label":"preview"}]);
        let doc = serde_json::json!({"profiles":{"dev":{
            "host":"target", "user":"pilot", "auth":"agent", "local_forwards":forwards
        }}});
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        let store = ProfileStore::new(path.clone()).unwrap();
        let (_, old) = store.inspect("dev").unwrap();
        let mut edited = old.options.clone();
        assert_eq!(edited.local_forwards[0].local_port.get(), 8080);
        edited.port = 2222;
        store.edit("dev", "renamed", &edited, &old.options).unwrap();
        let (_, renamed) = store.inspect("renamed").unwrap();
        assert_eq!(renamed.raw["local_forwards"], forwards);
        store.duplicate(&renamed, "copy", &edited).unwrap();
        let (_, copy) = store.inspect("copy").unwrap();
        assert_eq!(copy.raw["local_forwards"], forwards);
        edited.local_forwards.clear();
        store.edit("copy", "copy", &edited, &copy.options).unwrap();
        let (_, copy) = store.inspect("copy").unwrap();
        assert!(copy.raw.get("local_forwards").is_none());
        assert!(copy.options.local_forwards.is_empty());
    }
}
