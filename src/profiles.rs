//! Profile persistence preserves unrelated JSON fields and replaces the file atomically.
use crate::options::{Profiles, SshOptions, SshRecipientSets, SshRosterDefaults, load_profiles};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::Write,
    path::{Path, PathBuf},
};

mod bookmarks;
pub use bookmarks::{BookmarkPeer, bookmark_directory};
mod dotfiles;
pub use dotfiles::{
    DotfileRestoreStatus, DotfileRollbackReport, DotfileRolloutBaseline, DotfileRolloutProfile,
    DotfileRolloutReport, rollback_dotfile_rollout,
};
mod folder_permissions;
pub(crate) use folder_permissions::{FolderEpoch, FolderGrantGate, FolderMutationPermit};
mod group_file;
pub use group_file::{decode_group_file, encode_group_file, read_group_file, remap_group_profiles};
mod sftp_searches;
pub use sftp_searches::SavedSftpSearch;
mod tools;
pub use tools::{
    ToolManifestBaseline, ToolManifestChanges, ToolProfileAudit, ToolProfileAuditRow,
    ToolProfileAuditStatus, ToolProfileBuildChecks, ToolRolloutProfile, ToolRolloutReport,
};

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

pub enum ImportedGroupMember<'a> {
    Existing {
        name: &'a str,
        reviewed: &'a SshOptions,
    },
    New {
        name: &'a str,
        options: &'a SshOptions,
    },
}

const SHARE_TOKEN: &str = "folder_share_id";
const SHARE_EPOCH: &str = "folder_share_epoch";
const PINNED_PROFILES: &str = "pinned_profiles";
const STARTUP_PROFILE: &str = "startup_profile";
const SFTP_DOWNLOAD_FOLDER: &str = "sftp_download_folder";
const SSH_HOST_GROUPS: &str = "ssh_host_groups";
const SSH_RECIPIENT_SETS: &str = "ssh_recipient_sets";
const SSH_HOST_GROUP_DEFAULTS: &str = "ssh_host_group_defaults";

enum RosterDefaultsEdit {
    Set(String, SshRosterDefaults),
    Move(String, Option<String>),
    Copy(String, String),
    Replace(String, SshRosterDefaults),
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct SavedSshGroup {
    pub name: String,
    pub members: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_sets: Option<SshRecipientSets>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roster_defaults: Option<SshRosterDefaults>,
}

impl SavedSshGroup {
    pub fn capture(catalog: &Profiles, name: &str) -> Option<Self> {
        Some(Self {
            name: name.into(),
            members: catalog.ssh_host_groups.get(name)?.clone(),
            recipient_sets: catalog.ssh_recipient_sets.get(name).cloned(),
            roster_defaults: catalog.ssh_host_group_defaults.get(name).copied(),
        })
    }
}

#[derive(Default)]
struct CatalogEdit {
    order: Option<Vec<String>>,
    pinned_profiles: Option<BTreeSet<String>>,
    startup: Option<Option<String>>, // None leaves the setting untouched.
    ssh_host_groups: Option<BTreeMap<String, Vec<String>>>,
    ssh_recipient_sets: Option<BTreeMap<String, SshRecipientSets>>,
    roster_defaults: Option<RosterDefaultsEdit>,
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
    let pinned_profiles = old
        .filter(|name| Some(*name) != new && catalog.pinned_profiles.contains(*name))
        .map(|old| {
            let mut pins = catalog.pinned_profiles.clone();
            pins.remove(old);
            pins.extend(new.map(str::to_owned));
            pins
        });
    CatalogEdit {
        order: order_after_edit(catalog, old, new),
        pinned_profiles,
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
    if let (Some(old), Some(new)) = (old, new)
        && old != new
        && catalog.ssh_recipient_sets.values().any(|sets| {
            sets.values()
                .any(|members| members.iter().any(|name| name == old))
        })
    {
        let mut groups = catalog.ssh_recipient_sets.clone();
        for members in groups
            .values_mut()
            .flat_map(BTreeMap::values_mut)
            .filter(|members| members.iter().any(|name| name == old))
        {
            replace_profile_member(members, old, Some(new));
        }
        edit.ssh_recipient_sets = Some(groups);
    }
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
            replace_profile_member(members, old, new);
        }
        edit.ssh_host_groups = Some(groups);
    }
    edit
}

fn replace_profile_member(members: &mut Vec<String>, old: &str, new: Option<&str>) {
    let mut seen = std::collections::HashSet::new();
    members.retain_mut(|name| {
        if name == old {
            let Some(new) = new else { return false };
            *name = new.to_owned();
        }
        seen.insert(name.clone())
    });
}

fn preserve_forward_preset_fields(previous: &serde_json::Value, next: &mut serde_json::Value) {
    let (Some(previous), Some(next)) = (previous.as_array(), next.as_array_mut()) else {
        return;
    };
    let ports =
        |entry: &serde_json::Value| (entry["local_port"].as_u64(), entry["remote_port"].as_u64());
    let mut raw = BTreeMap::<_, VecDeque<_>>::new();
    for entry in previous {
        raw.entry(ports(entry)).or_default().push_back(entry);
    }
    for entry in next {
        if let Some(saved) = raw.get_mut(&ports(entry)).and_then(VecDeque::pop_front) {
            *entry = saved.clone();
        }
    }
}

fn validate_group_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name == name.trim() && !name.chars().any(char::is_control),
        "Enter a group name on one line"
    );
    Ok(())
}

fn validate_ssh_profile(name: &str, options: &SshOptions) -> Result<()> {
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
    options.validate_route()
}

fn replace_share_epoch(object: &mut serde_json::Map<String, serde_json::Value>) {
    object.remove(SHARE_TOKEN);
    // Distinguish a recreated profile even when its visible options match.
    object.insert(
        SHARE_EPOCH.into(),
        format!("{:032x}", rand::random::<u128>()).into(),
    );
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
        validate_group_name(name)?;
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
            let ssh_recipient_sets = original
                .filter(|(old, _)| *old != name)
                .and_then(|(old, _)| recipient_sets_after_group_edit(catalog, old, Some(name)));
            let roster_defaults = original
                .filter(|(old, _)| *old != name)
                .map(|(old, _)| RosterDefaultsEdit::Move(old.into(), Some(name.into())));
            Ok(CatalogEdit {
                ssh_host_groups: Some(groups),
                ssh_recipient_sets,
                roster_defaults,
                ..Default::default()
            })
        })
    }

    /// Duplicate the reviewed saved definition in one catalog replacement.
    pub fn duplicate_ssh_host_group(&self, name: &str, source: &SavedSshGroup) -> Result<Profiles> {
        validate_group_name(name)?;
        self.modify(|_, catalog| {
            ensure!(
                SavedSshGroup::capture(catalog, &source.name).as_ref() == Some(source),
                "The source SSH group changed on disk. Reopen the copy before saving."
            );
            catalog.resolve_ssh_host_group_members(&source.members)?;
            add_ssh_group(
                catalog,
                name,
                source,
                RosterDefaultsEdit::Copy(source.name.clone(), name.into()),
            )
        })
    }

    pub fn import_ssh_group_definition(
        &self,
        name: &str,
        group: &SavedSshGroup,
        reviewed: &[SshOptions],
    ) -> Result<Profiles> {
        validate_group_name(name)?;
        group_file::validate(group)?;
        self.modify(|_, catalog| {
            let resolved = catalog.resolve_ssh_host_group_members(&group.members)?;
            ensure!(
                resolved.into_iter().eq(reviewed),
                "The SSH profiles changed on disk. Review the group definition again."
            );
            add_ssh_group(
                catalog,
                name,
                group,
                match group.roster_defaults {
                    Some(defaults) => RosterDefaultsEdit::Replace(name.into(), defaults),
                    None => RosterDefaultsEdit::Move(name.into(), None),
                },
            )
        })
    }

    /// Save the group and any new SSH profiles in a single catalog replacement.
    pub fn save_imported_ssh_group(
        &self,
        name: &str,
        entries: &[ImportedGroupMember<'_>],
        verify_review: impl FnOnce(&Profiles) -> Result<()>,
    ) -> Result<Profiles> {
        validate_group_name(name)?;
        ensure!(!entries.is_empty(), "Check at least one SSH host");
        self.modify(|profiles, catalog| {
            verify_review(catalog)?;
            ensure!(
                !catalog.ssh_host_groups.contains_key(name),
                "An SSH group named {name} already exists"
            );
            let mut seen = std::collections::BTreeSet::new();
            let mut members = Vec::with_capacity(entries.len());
            let mut order = None;
            for entry in entries {
                let member = match *entry {
                    ImportedGroupMember::Existing { name, reviewed } => {
                        ensure!(
                            catalog.ssh_host_group_member(name) == Some(reviewed),
                            "SSH profile {name} changed. Preview the host list again."
                        );
                        name
                    }
                    ImportedGroupMember::New { name, options } => {
                        validate_ssh_profile(name, options)?;
                        ensure!(
                            !name.chars().any(char::is_control),
                            "Enter a profile name on one line"
                        );
                        ensure!(
                            !profiles.contains_key(name) && !catalog.local.contains_key(name),
                            "Profile {name} already exists"
                        );
                        ensure!(
                            !options.durable && !options.player.rustmush,
                            "Choose a direct SSH shell for the new profile"
                        );
                        let mut value = serde_json::to_value(options)?;
                        replace_share_epoch(
                            value
                                .as_object_mut()
                                .context("SSH profile must be an object")?,
                        );
                        profiles.insert(name.into(), value);
                        if catalog.has_explicit_order {
                            order
                                .get_or_insert_with(|| catalog.profile_order.clone())
                                .push(name.into());
                        }
                        name
                    }
                };
                ensure!(
                    seen.insert(member),
                    "SSH profile {member} appears more than once"
                );
                members.push(member.to_owned());
            }
            let mut groups = catalog.ssh_host_groups.clone();
            groups.insert(name.to_owned(), members);
            Ok(CatalogEdit {
                order,
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
                ssh_recipient_sets: recipient_sets_after_group_edit(catalog, name, None),
                roster_defaults: Some(RosterDefaultsEdit::Move(name.into(), None)),
                ..Default::default()
            })
        })
    }

    pub fn save_ssh_roster_defaults(
        &self,
        name: &str,
        expected_members: &[String],
        expected: SshRosterDefaults,
        defaults: SshRosterDefaults,
    ) -> Result<Profiles> {
        self.modify(|_, catalog| {
            ensure!(
                catalog.ssh_host_groups.get(name).map(Vec::as_slice) == Some(expected_members),
                "The SSH group changed on disk. Reopen it before saving roster defaults."
            );
            ensure!(
                catalog
                    .ssh_host_group_defaults
                    .get(name)
                    .copied()
                    .unwrap_or_default()
                    == expected,
                "Roster defaults changed on disk. Reopen the SSH group before saving."
            );
            Ok(CatalogEdit {
                roster_defaults: Some(RosterDefaultsEdit::Set(name.into(), defaults)),
                ..Default::default()
            })
        })
    }

    pub fn change_ssh_recipient_set(
        &self,
        group: &str,
        expected_group: &[String],
        name: &str,
        expected: Option<&[String]>,
        replacement: Option<&[String]>,
    ) -> Result<Profiles> {
        self.edit_ssh_recipient_set(group, expected_group, name, expected, replacement, |_| {
            Ok(())
        })
    }

    pub fn rename_ssh_recipient_set(
        &self,
        group: &str,
        expected_group: &[String],
        name: &str,
        expected: &[String],
        new_name: &str,
    ) -> Result<Profiles> {
        validate_recipient_set_name(new_name)?;
        self.modify(|_, catalog| {
            ensure!(
                catalog.ssh_host_groups.get(group).map(Vec::as_slice) == Some(expected_group),
                "The SSH group changed on disk. Reopen the group before editing recipient sets."
            );
            let mut groups = catalog.ssh_recipient_sets.clone();
            let sets = groups.entry(group.to_owned()).or_default();
            let available = !sets.contains_key(new_name);
            let members = sets
                .remove(name)
                .filter(|members| available && members.as_slice() == expected)
                .context(
                    "This name is already in use or the recipient set changed. Reopen recipient sets.",
                )?;
            sets.insert(new_name.to_owned(), members);
            Ok(CatalogEdit {
                ssh_recipient_sets: Some(groups),
                ..Default::default()
            })
        })
    }

    /// Save a recipient set and recheck the launch review under the catalog write lock.
    pub fn save_reviewed_ssh_recipient_set(
        &self,
        group: &str,
        expected_group: &[String],
        name: &str,
        expected: Option<&[String]>,
        members: &[String],
        verify: impl FnOnce(&Profiles) -> Result<()>,
    ) -> Result<Profiles> {
        self.edit_ssh_recipient_set(group, expected_group, name, expected, Some(members), verify)
    }

    fn edit_ssh_recipient_set(
        &self,
        group: &str,
        expected_group: &[String],
        name: &str,
        expected: Option<&[String]>,
        replacement: Option<&[String]>,
        verify: impl FnOnce(&Profiles) -> Result<()>,
    ) -> Result<Profiles> {
        validate_recipient_set_name(name)?;
        self.modify(|_, catalog| {
            ensure!(
                catalog.ssh_host_groups.get(group).map(Vec::as_slice) == Some(expected_group),
                "The SSH group changed on disk. Reopen the group before editing recipient sets."
            );
            verify(catalog)?;
            let mut groups = catalog.ssh_recipient_sets.clone();
            let sets = groups.entry(group.to_owned()).or_default();
            ensure!(
                sets.get(name).map(Vec::as_slice) == expected,
                "This name is already in use or the recipient set changed. Reopen recipient sets."
            );
            if let Some(members) = replacement {
                catalog.resolve_ssh_host_group_members(members)?;
                ensure!(
                    members.iter().all(|member| expected_group.contains(member)),
                    "Every recipient must belong to the saved SSH group"
                );
                sets.insert(name.to_owned(), members.to_vec());
            } else {
                sets.remove(name);
            }
            if sets.is_empty() {
                groups.remove(group);
            }
            Ok(CatalogEdit {
                ssh_recipient_sets: Some(groups),
                ..Default::default()
            })
        })
    }

    pub fn set_profile_pin(&self, name: &str, pinned: bool) -> Result<Profiles> {
        self.modify(|_, catalog| {
            ensure!(
                catalog.profiles.contains_key(name) || catalog.local.contains_key(name),
                "Profile {name} is no longer available"
            );
            let mut pins = catalog.pinned_profiles.clone();
            if pinned {
                pins.insert(name.into());
            } else {
                pins.remove(name);
            }
            Ok(CatalogEdit {
                pinned_profiles: Some(pins),
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
            if let Some(size) = shell.font_size {
                fields.insert("font_size".into(), serde_json::to_value(size)?);
            } else {
                fields.remove("font_size");
            }
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

    pub fn sftp_download_folder(&self, name: &str, peer: &BookmarkPeer) -> Result<Option<PathBuf>> {
        let (_, snapshot) = self.inspect(name)?;
        peer.check_profile(&snapshot.options)?;
        Ok(snapshot
            .raw
            .get(SFTP_DOWNLOAD_FOLDER)
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from))
    }

    pub fn set_sftp_download_folder(
        &self,
        name: &str,
        peer: &BookmarkPeer,
        folder: Option<&Path>,
    ) -> Result<()> {
        if let Some(folder) = folder {
            ensure!(folder.is_absolute(), "Choose an absolute Windows folder");
        }
        self.modify(|profiles, catalog| {
            let options = catalog
                .profiles
                .get(name)
                .context("The saved SSH profile is no longer available")?;
            peer.check_profile(options)?;
            let fields = profiles
                .get_mut(name)
                .and_then(serde_json::Value::as_object_mut)
                .context("The SSH profile must be an object")?;
            if let Some(folder) = folder {
                fields.insert(SFTP_DOWNLOAD_FOLDER.into(), serde_json::to_value(folder)?);
            } else {
                fields.remove(SFTP_DOWNLOAD_FOLDER);
            }
            Ok(CatalogEdit::default())
        })?;
        Ok(())
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
        validate_ssh_profile(name, options)?;
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
                object.remove(sftp_searches::FIELD);
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
                replace_share_epoch(object);
            }
            let old_authentication = expected.map(|previous| &previous.authentication);
            let identity = options.authentication.identity_path();
            let certificate = options.authentication.certificate_path();
            let keep_identity = identity.is_some()
                && old_authentication.and_then(crate::options::Authentication::identity_path)
                    == identity;
            let keep_certificate = certificate.is_some()
                && old_authentication.and_then(crate::options::Authentication::certificate_path)
                    == certificate;
            if !keep_identity {
                object.remove("identity");
            }
            if !keep_certificate {
                object.remove("certificate");
            }
            object.remove("agent_fingerprint");
            if options.agent_pipe.is_none() {
                object.remove("agent_pipe");
            }
            let keep_route =
                expected.is_some_and(|previous| previous.proxy_jump == options.proxy_jump);
            let keep_forwards =
                expected.is_some_and(|previous| previous.local_forwards == options.local_forwards);
            let keep_dotfiles =
                expected.is_some_and(|previous| previous.dotfiles == options.dotfiles);
            let keep_tools =
                expected.is_some_and(|previous| previous.tool_bundle == options.tool_bundle);
            if options.terminal_type.is_default() {
                object.remove("terminal_type");
            }
            if options.keepalive.is_default() {
                object.remove("keepalive");
            }
            if options.connect_timeout_seconds.is_none() {
                object.remove("connect_timeout_seconds");
            }
            if options.color_scheme.is_default() {
                object.remove("color_scheme");
            }
            if options.font_size.is_none() {
                object.remove("font_size");
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
            if options.group_prompt.is_none() {
                object.remove("group_prompt");
            }
            if options.dotfiles.is_none() {
                object.remove("dotfiles");
            }
            if options.tool_bundle.is_none() {
                object.remove("tool_bundle");
            }
            for (key, value) in serde_json::to_value(options)?.as_object().unwrap() {
                if (key == "identity" && keep_identity)
                    || (key == "certificate" && keep_certificate)
                    || (key == "proxy_jump" && keep_route)
                    || (key == "local_forwards" && keep_forwards)
                    || (key == "dotfiles" && keep_dotfiles)
                    || (key == "tool_bundle" && keep_tools)
                {
                    continue;
                }
                let mut value = value.clone();
                if key == "local_forwards"
                    && let Some(previous) = object.get(key)
                {
                    preserve_forward_preset_fields(previous, &mut value);
                }
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
        if let Some(pins) = edit.pinned_profiles {
            if pins.is_empty() {
                fields.remove(PINNED_PROFILES);
            } else {
                fields.insert(PINNED_PROFILES.into(), serde_json::to_value(pins)?);
            }
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
        if let Some(groups) = edit.ssh_recipient_sets {
            if groups.is_empty() {
                fields.remove(SSH_RECIPIENT_SETS);
            } else {
                fields.insert(SSH_RECIPIENT_SETS.into(), serde_json::to_value(groups)?);
            }
        }
        if let Some(edit) = edit.roster_defaults {
            patch_roster_defaults(fields, edit)?;
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

fn add_ssh_group(
    catalog: &Profiles,
    name: &str,
    group: &SavedSshGroup,
    roster_defaults: RosterDefaultsEdit,
) -> Result<CatalogEdit> {
    ensure!(
        !catalog.ssh_host_groups.contains_key(name),
        "An SSH group named {name} already exists"
    );
    let mut groups = catalog.ssh_host_groups.clone();
    groups.insert(name.into(), group.members.clone());
    let mut sets = catalog.ssh_recipient_sets.clone();
    if let Some(recipient_sets) = &group.recipient_sets {
        sets.insert(name.into(), recipient_sets.clone());
    } else {
        sets.remove(name);
    }
    Ok(CatalogEdit {
        ssh_host_groups: Some(groups),
        ssh_recipient_sets: Some(sets),
        roster_defaults: Some(roster_defaults),
        ..Default::default()
    })
}

fn patch_roster_defaults(
    fields: &mut serde_json::Map<String, serde_json::Value>,
    edit: RosterDefaultsEdit,
) -> Result<()> {
    match edit {
        RosterDefaultsEdit::Set(name, defaults) => {
            let groups = fields
                .entry(SSH_HOST_GROUP_DEFAULTS)
                .or_insert_with(|| serde_json::json!({}))
                .as_object_mut()
                .context("SSH group defaults must be an object")?;
            let entry = groups
                .entry(name)
                .or_insert_with(|| serde_json::json!({}))
                .as_object_mut()
                .context("Roster defaults must be an object")?;
            let changes = serde_json::to_value(defaults)?;
            let changes = changes
                .as_object()
                .context("Roster defaults must be an object")?;
            entry.extend(changes.clone());
        }
        RosterDefaultsEdit::Move(from, to) => {
            if let Some(groups) = fields
                .get_mut(SSH_HOST_GROUP_DEFAULTS)
                .and_then(serde_json::Value::as_object_mut)
            {
                let original = groups.remove(&from);
                if let Some(to) = to {
                    if let Some(original) = original {
                        groups.insert(to, original);
                    } else {
                        groups.remove(&to);
                    }
                }
                if groups.is_empty() {
                    fields.remove(SSH_HOST_GROUP_DEFAULTS);
                }
            }
        }
        RosterDefaultsEdit::Copy(from, to) => {
            if let Some(groups) = fields
                .get_mut(SSH_HOST_GROUP_DEFAULTS)
                .and_then(serde_json::Value::as_object_mut)
            {
                if let Some(original) = groups.get(&from).cloned() {
                    groups.insert(to, original);
                } else {
                    groups.remove(&to);
                }
                if groups.is_empty() {
                    fields.remove(SSH_HOST_GROUP_DEFAULTS);
                }
            }
        }
        RosterDefaultsEdit::Replace(name, defaults) => {
            let groups = fields
                .entry(SSH_HOST_GROUP_DEFAULTS)
                .or_insert_with(|| serde_json::json!({}))
                .as_object_mut()
                .context("SSH group defaults must be an object")?;
            groups.insert(name, serde_json::to_value(defaults)?);
        }
    }
    Ok(())
}

fn validate_recipient_set_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && name == name.trim()
            && !name.chars().any(char::is_control),
        "Enter a recipient set name on one line and within 128 bytes"
    );
    Ok(())
}

fn recipient_sets_after_group_edit(
    catalog: &Profiles,
    old: &str,
    new: Option<&str>,
) -> Option<BTreeMap<String, SshRecipientSets>> {
    let sets = catalog.ssh_recipient_sets.get(old)?;
    let mut groups = catalog.ssh_recipient_sets.clone();
    groups.remove(old);
    if let Some(new) = new {
        groups.insert(new.to_owned(), sets.clone());
    }
    Some(groups)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::Authentication;

    #[test]
    fn imported_group_and_new_profiles_commit_together_in_review_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        let original = br#"{"profiles":{"West":{"host":"west.example","user":"ops","auth":"agent","note":"keep"}},"profile_order":["West"],"other":7}"#;
        std::fs::write(&path, original).unwrap();
        let store = ProfileStore::new(path.clone()).unwrap();
        let catalog = store.catalog().unwrap();
        let new_host = SshOptions::from_destination("ops@east.example:2222").unwrap();
        let mut changed = catalog.profiles["West"].clone();
        changed.port = 2200;
        let entries = [
            ImportedGroupMember::New {
                name: "East",
                options: &new_host,
            },
            ImportedGroupMember::Existing {
                name: "West",
                reviewed: &changed,
            },
        ];
        assert!(
            store
                .save_imported_ssh_group("Fleet", &entries, |_| Ok(()))
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let entries = [
            ImportedGroupMember::New {
                name: "East",
                options: &new_host,
            },
            ImportedGroupMember::Existing {
                name: "West",
                reviewed: &catalog.profiles["West"],
            },
        ];
        assert!(
            store
                .save_imported_ssh_group("Fleet", &entries, |_| anyhow::bail!("stale review"))
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let saved = store
            .save_imported_ssh_group("Fleet", &entries, |_| Ok(()))
            .unwrap();
        assert_eq!(saved.ssh_host_groups["Fleet"], ["East", "West"]);
        assert_eq!(saved.profile_order.last().unwrap(), "East");
        assert_eq!(
            saved
                .resolve_ssh_host_group_members(&saved.ssh_host_groups["Fleet"])
                .unwrap(),
            [&new_host, &catalog.profiles["West"]]
        );
        let mut raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw["other"], 7);
        assert_eq!(raw["profiles"]["West"]["note"], "keep");
        assert!(raw["profiles"]["East"][SHARE_EPOCH].is_string());
        raw["profile_order"] = serde_json::json!(["West", "Old name", "East"]);
        std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();
        store
            .save_imported_ssh_group("Existing only", &entries[1..], |_| Ok(()))
            .unwrap();
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(written["profile_order"], raw["profile_order"]);
    }

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
            connect_timeout_seconds: None,
            keepalive: Default::default(),
            font_size: None,
            tab_color: Default::default(),
            terminal_type: Default::default(),
            user: "root".into(),
            port: 22,
            authentication: Authentication::identity("local-key", None),
            agent_pipe: None,
            host_key: None,
            host_key_alias: None,
            proxy_jump: Vec::new(),
            local_forwards: Vec::new(),
            durable: false,
            dotfiles: None,
            tool_bundle: None,
            group_prompt: None,
            player: Default::default(),
        };
        store.add("new", &options).unwrap();
        let saved = std::fs::read(&path).unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&saved).unwrap();
        assert_eq!(doc["theme"], "dark");
        assert_eq!(doc["profiles"]["existing"]["custom"], 42);
        assert_eq!(
            store.load().unwrap()["new"].authentication,
            Authentication::identity(dir.path().join("local-key"), None)
        );
        assert!(store.add("new", &options).is_err());
        assert_eq!(std::fs::read(path).unwrap(), saved);
    }

    #[test]
    fn editing_can_add_and_clear_a_group_prompt_regex() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        std::fs::write(
            &path,
            r#"{"profiles":{"Rack":{"host":"rack.test","user":"ops","auth":"none","custom":42}}}"#,
        )
        .unwrap();
        let store = ProfileStore::new(path.clone()).unwrap();
        let (_, previous) = store.inspect("Rack").unwrap();
        let mut next = previous.options.clone();
        next.group_prompt = Some("racadm>>".to_owned().try_into().unwrap());
        store
            .edit("Rack", "Rack", &next, &previous.options)
            .unwrap();

        let (_, saved) = store.inspect("Rack").unwrap();
        assert_eq!(saved.options.group_prompt, next.group_prompt);
        next.group_prompt = None;
        store.edit("Rack", "Rack", &next, &saved.options).unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert!(json["profiles"]["Rack"].get("group_prompt").is_none());
        assert_eq!(json["profiles"]["Rack"]["custom"], 42);
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

    #[test]
    fn recipient_sets_validate_group_members_and_preserve_identity_across_renames() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        std::fs::write(
            &path,
            br#"{"profiles":{
            "East":{"host":"east.test","user":"ops","auth":"none"},
            "West":{"host":"west.test","user":"ops","auth":"none"},
            "Other":{"host":"other.test","user":"ops","auth":"none"}
        },"ssh_host_groups":{"Fleet":["East","West"]}}"#,
        )
        .unwrap();
        let store = ProfileStore::new(path.clone()).unwrap();
        let other = ProfileStore::new(path).unwrap();
        let group = ["East".into(), "West".into()];
        let east = ["East".into()];
        let west = ["West".into()];
        assert!(store.catalog().unwrap().ssh_recipient_sets.is_empty());
        for invalid in [
            Vec::new(),
            vec!["Other".into()],
            vec!["East".into(), "East".into()],
        ] {
            assert!(
                store
                    .change_ssh_recipient_set("Fleet", &group, "Canary", None, Some(&invalid))
                    .is_err()
            );
        }
        store
            .change_ssh_recipient_set("Fleet", &group, "Canary", None, Some(&east))
            .unwrap();
        assert_eq!(
            other.catalog().unwrap().ssh_recipient_sets["Fleet"]["Canary"],
            east
        );
        other
            .change_ssh_recipient_set("Fleet", &group, "Canary", Some(&east), Some(&west))
            .unwrap();
        assert!(
            store
                .change_ssh_recipient_set("Fleet", &group, "Canary", Some(&east), None)
                .is_err()
        );
        let (_, profile) = store.inspect("West").unwrap();
        store
            .edit("West", "West Coast", &profile.options, &profile.options)
            .unwrap();
        let updated = ["East".into(), "West Coast".into()];
        assert!(
            other
                .change_ssh_recipient_set("Fleet", &group, "Old", None, Some(&east))
                .is_err()
        );
        store
            .save_ssh_host_group("Renamed", &updated, Some(("Fleet", &updated)))
            .unwrap();
        let catalog = other.catalog().unwrap();
        assert!(!catalog.ssh_recipient_sets.contains_key("Fleet"));
        assert_eq!(
            catalog.ssh_recipient_sets["Renamed"]["Canary"],
            ["West Coast"]
        );
        store.delete_ssh_host_group("Renamed", &updated).unwrap();
        assert!(other.catalog().unwrap().ssh_recipient_sets.is_empty());
    }

    #[test]
    fn rename_recipient_set_preserves_order_and_rejects_stale_membership_or_a_collision() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        std::fs::write(
            &path,
            br#"{"profiles":{
            "East":{"host":"east.test","user":"ops","auth":"none"},
            "West":{"host":"west.test","user":"ops","auth":"none"}
        },"ssh_host_groups":{"Fleet":["East","West"]},
          "ssh_recipient_sets":{"Fleet":{"Canary":["West","East"],"Reserved":["East"]}}}"#,
        )
        .unwrap();
        let store = ProfileStore::new(path).unwrap();
        let group = ["East".into(), "West".into()];
        let members = ["West".into(), "East".into()];
        for (expected_group, expected, new_name) in [
            (group.as_slice(), group.as_slice(), "Pilots"),
            (members.as_slice(), members.as_slice(), "Pilots"),
            (group.as_slice(), members.as_slice(), "Reserved"),
        ] {
            assert!(
                store
                    .rename_ssh_recipient_set("Fleet", expected_group, "Canary", expected, new_name)
                    .is_err()
            );
        }
        let catalog = store
            .rename_ssh_recipient_set("Fleet", &group, "Canary", &members, "Pilots")
            .unwrap();
        let sets = &catalog.ssh_recipient_sets["Fleet"];
        assert!(!sets.contains_key("Canary"));
        assert_eq!(sets["Pilots"], members);
        assert_eq!(sets["Reserved"], ["East"]);
        assert_eq!(catalog.ssh_host_groups["Fleet"], group);
    }

    #[test]
    fn roster_defaults_follow_group_rename_and_reject_stale_writes() {
        use crate::options::{
            SshGroupCommandInterval, SshGroupParallelLimit, SshGroupQueueExpiry, SshRosterOrder,
        };
        use serde_json::Value;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        std::fs::write(
            &path,
            br#"{"profiles":{
                "East":{"host":"east.test","user":"ops","auth":"none"},
                "West":{"host":"west.test","user":"ops","auth":"none"}
            },"ssh_host_groups":{"Fleet":["East"],"Other":["West"]},
            "ssh_host_group_defaults":{"Fleet":{"layout_hint":7},"Other":{"show_last_output":true}}}"#,
        )
        .unwrap();
        let store = ProfileStore::new(path.clone()).unwrap();
        let other = ProfileStore::new(path.clone()).unwrap();
        let members = ["East".to_owned()];
        let expected = store.catalog().unwrap().ssh_host_group_defaults["Fleet"];
        assert_eq!(expected, SshRosterDefaults::default());
        let defaults = SshRosterDefaults {
            order: SshRosterOrder::CheckedFirst,
            show_last_output: true,
            show_terminal_line: true,
            require_all_ready: true,
            login_limit: SshGroupParallelLimit::Four,
            command_limit: SshGroupParallelLimit::Two,
            command_interval: SshGroupCommandInterval::HalfSecond,
            command_queue_expiry: SshGroupQueueExpiry::ThirtySeconds,
            stop_command_queue_on_failure: true,
        };
        assert!(
            store
                .save_ssh_roster_defaults("Fleet", &["West".to_owned()], expected, defaults)
                .is_err()
        );
        store
            .save_ssh_roster_defaults("Fleet", &members, expected, defaults)
            .unwrap();
        assert!(
            other
                .save_ssh_roster_defaults("Fleet", &members, expected, expected)
                .is_err()
        );
        let renamed = other
            .save_ssh_host_group("Renamed", &members, Some(("Fleet", &members)))
            .unwrap();
        assert_eq!(renamed.ssh_host_group_defaults["Renamed"], defaults);
        assert!(!renamed.ssh_host_group_defaults.contains_key("Fleet"));
        let raw: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw["ssh_host_group_defaults"]["Renamed"]["layout_hint"], 7);
        assert_eq!(
            raw["ssh_host_group_defaults"]["Renamed"]["command_interval"],
            "500ms"
        );
        assert_eq!(
            raw["ssh_host_group_defaults"]["Renamed"]["command_queue_expiry"],
            "30s"
        );
        let remaining = other.delete_ssh_host_group("Renamed", &members).unwrap();
        assert_eq!(remaining.ssh_host_group_defaults.len(), 1);
        assert!(remaining.ssh_host_group_defaults["Other"].show_last_output);
    }

    #[test]
    fn duplicating_a_saved_group_copies_its_order_sets_and_defaults_independently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        std::fs::write(
            &path,
            br#"{"profiles":{
                "East":{"host":"east.test","user":"ops","auth":"none"},
                "West":{"host":"west.test","user":"ops","auth":"none"}
            },
            "ssh_host_groups":{"Fleet":["West","East"]},
            "ssh_recipient_sets":{"Fleet":{"Canary":["East"]}},
            "ssh_host_group_defaults":{"Fleet":{"show_terminal_line":true,"extra":7}}}"#,
        )
        .unwrap();
        let store = ProfileStore::new(path.clone()).unwrap();
        let catalog = store.catalog().unwrap();
        let source = SavedSshGroup::capture(&catalog, "Fleet").unwrap();

        let copied = store
            .duplicate_ssh_host_group("Fleet copy", &source)
            .unwrap();
        assert_eq!(
            SavedSshGroup::capture(&copied, "Fleet copy"),
            Some(SavedSshGroup {
                name: "Fleet copy".into(),
                ..source.clone()
            })
        );
        assert_eq!(
            SavedSshGroup::capture(&copied, "Fleet"),
            Some(source.clone())
        );
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw["ssh_host_group_defaults"]["Fleet copy"]["extra"], 7);

        let original_set = ["East".into()];
        let replacement_set = ["West".into()];
        store
            .change_ssh_recipient_set(
                "Fleet",
                &source.members,
                "Canary",
                Some(&original_set),
                Some(&replacement_set),
            )
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(store.duplicate_ssh_host_group("Stale", &source).is_err());
        let fresh = SavedSshGroup::capture(&store.catalog().unwrap(), "Fleet").unwrap();
        assert!(
            store
                .duplicate_ssh_host_group("Fleet copy", &fresh)
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            store.catalog().unwrap().ssh_recipient_sets["Fleet copy"]["Canary"],
            original_set
        );
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
    fn profile_pins_follow_renames_and_clear_when_removed() {
        let (_dir, store) = fixture();
        let original = store.catalog().unwrap();
        assert!(original.pinned_profiles.is_empty());
        store.set_profile_pin("Lima", true).unwrap();
        store.set_profile_pin("Alpha", true).unwrap();
        let pinned = disk(&store);
        assert_eq!(pinned[PINNED_PROFILES], json!(["Alpha", "Lima"]));
        assert_eq!(pinned["theme"]["tone"], "dark");
        assert!(store.set_profile_pin("missing", true).is_err());
        store.set_profile_pin("Alpha", true).unwrap();
        assert_eq!(disk(&store), pinned);

        let ssh = store.inspect("Alpha").unwrap().1;
        store.duplicate(&ssh, "Alpha copy", &ssh.options).unwrap();
        store
            .edit("Alpha", "Renamed SSH", &ssh.options, &ssh.options)
            .unwrap();
        let local = store.inspect_local("Lima").unwrap();
        store
            .save_local(Some(&local), "Renamed local", &local.options)
            .unwrap();
        assert_eq!(
            disk(&store)[PINNED_PROFILES],
            json!(["Renamed SSH", "Renamed local"])
        );
        store
            .delete(&store.inspect("Renamed SSH").unwrap().1)
            .unwrap();
        let catalog = store.set_profile_pin("Renamed local", false).unwrap();
        assert!(catalog.pinned_profiles.is_empty());
        assert!(disk(&store).get(PINNED_PROFILES).is_none());
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
    fn appearance_survives_profile_copies_and_allows_resets() {
        use crate::options::ColorScheme::{Default, OneHalfDark, OneHalfLight};
        use crate::options::TabColor;
        let (_dir, store) = fixture();
        let catalog = store.catalog().unwrap();
        assert_eq!(catalog.source("Alpha").unwrap().color_scheme(), Default);
        assert_eq!(catalog.source("Lima").unwrap().color_scheme(), Default);
        assert_eq!(catalog.source("Alpha").unwrap().tab_color(), TabColor::None);
        assert_eq!(catalog.source("Lima").unwrap().tab_color(), TabColor::None);
        assert_eq!(catalog.source("Alpha").unwrap().profile_font_size(), None);
        assert_eq!(catalog.source("Lima").unwrap().profile_font_size(), None);

        let mut ssh = catalog.profiles["Alpha"].clone();
        ssh.color_scheme = OneHalfLight;
        ssh.tab_color = TabColor::Red;
        ssh.font_size = Some(6.try_into().unwrap());
        store
            .edit("Alpha", "Alpha", &ssh, &catalog.profiles["Alpha"])
            .unwrap();
        let base = store.inspect("Alpha").unwrap().1;
        store.duplicate(&base, "SSH copy", &base.options).unwrap();
        let mut shell = catalog.local["Lima"].clone();
        shell.color_scheme = OneHalfDark;
        shell.tab_color = TabColor::Blue;
        shell.font_size = Some(72.try_into().unwrap());
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
        assert_eq!(
            catalog.source("SSH copy").unwrap().profile_font_size(),
            ssh.font_size
        );
        assert_eq!(
            catalog.source("Local copy").unwrap().profile_font_size(),
            shell.font_size
        );
        ssh.color_scheme = Default;
        ssh.tab_color = TabColor::None;
        ssh.font_size = None;
        store
            .edit("Alpha", "Alpha", &ssh, &catalog.profiles["Alpha"])
            .unwrap();
        shell.color_scheme = Default;
        shell.tab_color = TabColor::None;
        shell.font_size = None;
        store.save_local(Some(&base), "Lima", &shell).unwrap();
        let entries = &disk(&store)["profiles"];
        assert!(entries["Alpha"].get("color_scheme").is_none());
        assert!(entries["Lima"].get("color_scheme").is_none());
        assert!(entries["Alpha"].get("tab_color").is_none());
        assert!(entries["Lima"].get("tab_color").is_none());
        assert!(entries["Alpha"].get("font_size").is_none());
        assert!(entries["Lima"].get("font_size").is_none());
        assert_eq!(entries["SSH copy"]["tab_color"], "red");
        assert_eq!(entries["Local copy"]["tab_color"], "blue");
        assert_eq!(entries["SSH copy"]["color_scheme"], "one-half-light");
        assert_eq!(entries["Local copy"]["color_scheme"], "one-half-dark");
        assert_eq!(entries["SSH copy"]["font_size"], 6);
        assert_eq!(entries["Local copy"]["font_size"], 72);
        assert_eq!(entries["SSH copy"]["extra"], 7);
        assert_eq!(entries["Local copy"]["extra"], "local");
    }

    #[test]
    fn editing_keepalive_can_save_a_custom_policy_and_restore_the_default() {
        let (_dir, store) = fixture();
        let initial = store.inspect("Alpha").unwrap().1.options;
        assert_eq!(initial.keepalive, Default::default());
        let mut custom = initial.clone();
        custom.keepalive.interval_seconds = 0;
        custom.keepalive.max_unanswered = 7.try_into().unwrap();
        store.edit("Alpha", "Alpha", &custom, &initial).unwrap();
        let saved = store.inspect("Alpha").unwrap().1.options;
        assert_eq!(saved.keepalive, custom.keepalive);
        assert_eq!(
            disk(&store)["profiles"]["Alpha"]["keepalive"],
            json!({
                "interval_seconds": 0, "max_unanswered": 7
            })
        );
        store.edit("Alpha", "Alpha", &initial, &saved).unwrap();
        assert!(disk(&store)["profiles"]["Alpha"].get("keepalive").is_none());
    }

    #[test]
    fn editing_a_connect_timeout_can_restore_the_legacy_default() {
        let (_dir, store) = fixture();
        let initial = store.inspect("Alpha").unwrap().1.options;
        assert_eq!(initial.connect_timeout_seconds, None);
        let mut custom = initial.clone();
        custom.connect_timeout_seconds = Some(18.try_into().unwrap());
        store.edit("Alpha", "Alpha", &custom, &initial).unwrap();
        let saved = store.inspect("Alpha").unwrap().1.options;
        assert_eq!(
            saved.connect_timeout_seconds,
            custom.connect_timeout_seconds
        );
        assert_eq!(
            disk(&store)["profiles"]["Alpha"]["connect_timeout_seconds"],
            18
        );
        store.edit("Alpha", "Alpha", &initial, &saved).unwrap();
        assert!(
            disk(&store)["profiles"]["Alpha"]
                .get("connect_timeout_seconds")
                .is_none()
        );
    }

    #[test]
    fn reconnect_windows_round_trip_with_the_legacy_default_and_opt_out() {
        let (_dir, store) = fixture();
        let initial = store.inspect("Alpha").unwrap().1.options;
        assert!(initial.player.reconnect);
        assert_eq!(initial.player.reconnect_window_seconds.get(), 300);
        let mut custom = initial.clone();
        custom.player.reconnect = false;
        custom.player.reconnect_window_seconds = 45.try_into().unwrap();
        store.edit("Alpha", "Alpha", &custom, &initial).unwrap();
        let saved = store.inspect("Alpha").unwrap().1.options;
        assert_eq!(saved.player, custom.player);
        assert_eq!(
            disk(&store)["profiles"]["Alpha"]["player"]["reconnect_window_seconds"],
            45
        );
        store.edit("Alpha", "Alpha", &initial, &saved).unwrap();
        assert!(
            disk(&store)["profiles"]["Alpha"]["player"]
                .get("reconnect_window_seconds")
                .is_none()
        );
        assert!(
            serde_json::from_value::<crate::options::PlayerOptions>(
                json!({"reconnect_window_seconds": 0})
            )
            .is_err()
        );
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
    fn edit_preserves_each_relative_ssh_credential_independently_and_clears_removed_certificates() {
        use crate::options::Authentication::{Identity, Password};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        std::fs::write(
            &path,
            br#"{"profiles":{"pilot":{"host":"localhost","user":"pilot",
            "auth":"identity","identity":"keys/id","certificate":"keys/old-cert.pub"}}}"#,
        )
        .unwrap();
        let store = ProfileStore::new(path.clone()).unwrap();
        let read_document =
            || serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path).unwrap()).unwrap();

        let initial = store.load().unwrap().remove("pilot").unwrap();
        let mut updated = initial.clone();
        let Identity { certificate, .. } = &mut updated.authentication else {
            panic!("expected key authentication")
        };
        *certificate = Some(dir.path().join("keys/new-cert.pub").into());
        store.edit("pilot", "pilot", &updated, &initial).unwrap();
        assert_eq!(read_document()["profiles"]["pilot"]["identity"], "keys/id");

        updated.authentication = Password;
        let current = store.load().unwrap().remove("pilot").unwrap();
        store.edit("pilot", "pilot", &updated, &current).unwrap();
        let saved = read_document();
        let profile = &saved["profiles"]["pilot"];
        assert!(profile.get("identity").is_none());
        assert!(profile.get("certificate").is_none());
    }

    #[test]
    fn saving_an_agent_certificate_preserves_a_relative_path_and_clears_its_removed_pin() {
        const KEY: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"profiles":{"pilot":{
                "host":"localhost","user":"pilot","auth":"agent-certificate",
                "certificate":"keys/login-cert.pub","agent_fingerprint":KEY
            }}}))
            .unwrap(),
        )
        .unwrap();
        let store = ProfileStore::new(path.clone()).unwrap();
        let original = store.load().unwrap().remove("pilot").unwrap();
        let absolute = dir.path().join("keys/login-cert.pub");
        assert_eq!(
            original.authentication,
            Authentication::agent_certificate(&absolute, Some(KEY.parse().unwrap()))
        );
        let mut changed = original.clone();
        changed.authentication = Authentication::agent_certificate(absolute, None);
        store.edit("pilot", "pilot", &changed, &original).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let profile = &saved["profiles"]["pilot"];
        assert_eq!(profile["auth"], "agent-certificate");
        assert_eq!(profile["certificate"], "keys/login-cert.pub");
        assert!(profile.get("agent_fingerprint").is_none());
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
            Authentication::identity(dir.path().join("keys/jump"), None)
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
        edited.local_forwards.push(
            serde_json::from_value(serde_json::json!({"local_port":9090, "remote_port":9000}))
                .unwrap(),
        );
        store.edit("copy", "copy", &edited, &copy.options).unwrap();
        let (_, copy) = store.inspect("copy").unwrap();
        assert_eq!(copy.raw["local_forwards"][0], forwards[0]);
        assert_eq!(copy.raw["local_forwards"][1]["remote_port"], 9000);
        edited.local_forwards.clear();
        store.edit("copy", "copy", &edited, &copy.options).unwrap();
        let (_, copy) = store.inspect("copy").unwrap();
        assert!(copy.raw.get("local_forwards").is_none());
        assert!(copy.options.local_forwards.is_empty());
    }
}
