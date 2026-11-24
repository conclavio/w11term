use super::{SavedSshGroup, validate_group_name, validate_recipient_set_name};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::Path,
};

const FORMAT: &str = "w11term-ssh-group";
const VERSION: u32 = 1;
const MAX_BYTES: usize = 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document<G> {
    format: String,
    version: u32,
    group: G,
}

pub fn encode_group_file(group: &SavedSshGroup) -> Result<String> {
    validate(group)?;
    let mut encoded = serde_json::to_string_pretty(&Document {
        format: FORMAT.into(),
        version: VERSION,
        group,
    })?;
    encoded.push('\n');
    check_size(encoded.len())?;
    Ok(encoded)
}

pub fn decode_group_file(bytes: &[u8]) -> Result<SavedSshGroup> {
    check_size(bytes.len())?;
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let doc: Document<SavedSshGroup> =
        serde_json::from_slice(bytes).context("Reading the SSH group definition")?;
    ensure!(
        doc.format == FORMAT,
        "Choose a w11term SSH group definition"
    );
    ensure!(
        doc.version == VERSION,
        "Unsupported SSH group definition version {}",
        doc.version
    );
    validate(&doc.group)?;
    Ok(doc.group)
}

pub fn read_group_file(path: &Path) -> Result<SavedSshGroup> {
    let mut bytes = Vec::new();
    File::open(path)
        .context("Opening the SSH group definition")?
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .context("Reading the SSH group definition")?;
    decode_group_file(&bytes)
}

/// Resolve the reviewed local names in file order, including every recipient set.
pub fn remap_group_profiles(group: &SavedSshGroup, local: &[String]) -> Result<SavedSshGroup> {
    ensure!(
        group.members.len() == local.len(),
        "Choose one local profile for each SSH group member"
    );
    validate(group)?;
    members(local)?;
    let mapping: BTreeMap<_, _> = group.members.iter().zip(local).collect();
    let mut mapped = group.clone();
    mapped.members.clone_from_slice(local);
    if let Some(sets) = &mut mapped.recipient_sets {
        for profile in sets.values_mut().flatten() {
            let local = mapping
                .get(profile)
                .context("A recipient has no local profile")?;
            profile.clone_from(local);
        }
    }
    Ok(mapped)
}

fn check_size(bytes: usize) -> Result<()> {
    ensure!(
        bytes <= MAX_BYTES,
        "SSH group definitions are limited to 1 MiB"
    );
    Ok(())
}

fn members(names: &[String]) -> Result<BTreeSet<&str>> {
    ensure!(!names.is_empty(), "Choose at least one SSH profile");
    let mut unique = BTreeSet::new();
    for name in names {
        ensure!(
            !name.is_empty() && name == name.trim() && !name.chars().any(char::is_control),
            "Enter each SSH profile name on one line"
        );
        ensure!(
            unique.insert(name.as_str()),
            "SSH profile {name} appears more than once"
        );
    }
    Ok(unique)
}

pub(super) fn validate(group: &SavedSshGroup) -> Result<()> {
    validate_group_name(&group.name)?;
    let available = members(&group.members)?;
    if let Some(sets) = &group.recipient_sets {
        for (name, recipients) in sets {
            validate_recipient_set_name(name)?;
            ensure!(
                members(recipients)?.is_subset(&available),
                "Every recipient in {name} must belong to the SSH group"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::SshRosterDefaults;
    use crate::profiles::ProfileStore;
    use serde_json::json;

    #[test]
    fn group_file_round_trips_names_order_sets_and_defaults() {
        let group = SavedSshGroup {
            name: "Builds 雪".into(),
            members: vec!["West".into(), "East".into()],
            recipient_sets: Some([("Canary".into(), vec!["East".into()])].into()),
            roster_defaults: Some(SshRosterDefaults {
                show_terminal_line: true,
                ..Default::default()
            }),
        };
        let encoded = encode_group_file(&group).unwrap();
        assert_eq!(decode_group_file(encoded.as_bytes()).unwrap(), group);
        assert_eq!(
            decode_group_file(format!("\u{feff}{encoded}").as_bytes()).unwrap(),
            group
        );
        let parsed: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(parsed["format"], FORMAT);
        assert_eq!(parsed["version"], VERSION);
        assert_eq!(parsed["group"]["members"], json!(["West", "East"]));
        assert!(parsed.get("profiles").is_none());
    }

    #[test]
    fn group_file_rejects_secrets_wrong_versions_and_invalid_recipient_sets() {
        let valid = json!({
            "format": FORMAT,
            "version": VERSION,
            "group": {"name": "Builds", "members": ["West", "East"]}
        });
        assert!(decode_group_file(&serde_json::to_vec(&valid).unwrap()).is_ok());
        let mut changed = valid.clone();
        changed["version"] = json!(VERSION + 1);
        assert!(decode_group_file(&serde_json::to_vec(&changed).unwrap()).is_err());
        changed = valid.clone();
        changed["group"]["password"] = json!("unsupported");
        assert!(decode_group_file(&serde_json::to_vec(&changed).unwrap()).is_err());
        changed = valid.clone();
        changed["group"]["members"] = json!(["West", "West"]);
        assert!(decode_group_file(&serde_json::to_vec(&changed).unwrap()).is_err());
        changed = valid;
        changed["group"]["recipient_sets"] = json!({"Canary":["Outside"]});
        assert!(decode_group_file(&serde_json::to_vec(&changed).unwrap()).is_err());
        assert!(decode_group_file(&vec![b' '; MAX_BYTES + 1]).is_err());
    }

    #[test]
    fn importing_a_group_rechecks_the_reviewed_local_profile_and_preserves_other_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        let catalog = |host: &str| {
            serde_json::to_vec(&json!({
                "profiles": {"West":{"host":host, "user":"ops", "auth":"none"}},
                "other": 17
            }))
            .unwrap()
        };
        std::fs::write(&path, catalog("old.test")).unwrap();
        let store = ProfileStore::new(path.clone()).unwrap();
        let group = SavedSshGroup {
            name: "Builds".into(),
            members: vec!["West".into()],
            recipient_sets: Some([("Canary".into(), vec!["West".into()])].into()),
            roster_defaults: Some(SshRosterDefaults {
                require_all_ready: true,
                ..Default::default()
            }),
        };
        let reviewed = [store.catalog().unwrap().profiles["West"].clone()];
        let replacement = catalog("new.test");
        std::fs::write(&path, &replacement).unwrap();
        assert!(
            store
                .import_ssh_group_definition("Local builds", &group, &reviewed)
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), replacement);
        let new_review = [store.catalog().unwrap().profiles["West"].clone()];
        let saved = store
            .import_ssh_group_definition("Local builds", &group, &new_review)
            .unwrap();
        assert_eq!(
            SavedSshGroup::capture(&saved, "Local builds"),
            Some(SavedSshGroup {
                name: "Local builds".into(),
                ..group
            })
        );
        assert_eq!(saved.profiles["West"].host, "new.test");
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(written["other"], 17);
    }

    #[test]
    fn profile_mapping_keeps_member_and_recipient_order_and_rejects_duplicate_targets() {
        let definition = SavedSshGroup {
            name: "Builds".into(),
            members: vec!["West".into(), "East".into(), "Lab".into()],
            recipient_sets: Some(
                [
                    ("Canary".into(), vec!["East".into(), "West".into()]),
                    ("Testing".into(), vec!["Lab".into(), "East".into()]),
                ]
                .into(),
            ),
            roster_defaults: Some(SshRosterDefaults::default()),
        };
        let local = ["Dev West".into(), "East".into(), "Test Cluster".into()];
        let mapped = remap_group_profiles(&definition, &local).unwrap();
        assert_eq!(mapped.members, local);
        assert_eq!(mapped.name, definition.name);
        assert_eq!(mapped.roster_defaults, definition.roster_defaults);
        let sets = mapped.recipient_sets.unwrap();
        assert_eq!(sets["Canary"], ["East", "Dev West"]);
        assert_eq!(sets["Testing"], ["Test Cluster", "East"]);
        assert!(remap_group_profiles(&definition, &local[..2]).is_err());
        assert!(
            remap_group_profiles(&definition, &["East".into(), "East".into(), "Lab".into()])
                .is_err()
        );
    }
}
