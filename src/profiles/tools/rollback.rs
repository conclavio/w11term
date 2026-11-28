use super::{
    Manifest,
    rollout::{approval, write_binding},
};
use crate::{
    options::{SshOptions, ToolSync},
    profiles::{CatalogEdit, ProfileStore, receipt},
    sftp::{digest_hex, normalize_sha256},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
struct Receipt {
    profiles_path: PathBuf,
    candidate_path: PathBuf,
    candidate_manifest_sha256: String,
    applied: bool,
    changed_profiles: usize,
    profiles: Vec<ReceiptProfile>,
}

#[derive(Deserialize)]
struct ReceiptProfile {
    profile: String,
    previous: ToolSync,
    next_pin: Option<String>,
    changed: bool,
}

impl Receipt {
    fn load(path: &Path) -> Result<Self> {
        receipt::read::<Self>(path, "SSH tool")?.checked()
    }

    fn checked(mut self) -> Result<Self> {
        ensure!(
            self.applied,
            "Choose the reply from an applied SSH tool rollout"
        );
        ensure!(
            self.profiles_path.is_absolute() && self.candidate_path.is_absolute(),
            "The rollout receipt must identify the original catalog and candidate manifest paths"
        );
        ensure!(
            normalize_sha256(&self.candidate_manifest_sha256)? == self.candidate_manifest_sha256,
            "Invalid candidate digest in the rollout receipt"
        );
        self.profiles
            .sort_unstable_by(|a, b| a.profile.cmp(&b.profile));
        ensure!(
            self.profiles
                .windows(2)
                .all(|pair| pair[0].profile != pair[1].profile),
            "A profile appears more than once in the receipt"
        );
        self.profiles.retain(|entry| entry.changed);
        ensure!(
            !self.profiles.is_empty(),
            "The rollout did not change any profiles"
        );
        ensure!(
            self.changed_profiles == self.profiles.len(),
            "The rollout receipt's changed profile count does not match its entries"
        );
        for entry in &self.profiles {
            let previous = &entry.previous;
            let expected = previous
                .manifest_pin()?
                .map(|_| self.candidate_manifest_sha256.as_str());
            let next = entry
                .next_pin
                .as_deref()
                .map(normalize_sha256)
                .transpose()?;
            ensure!(
                next.as_deref() == expected,
                "The receipt changes the pin policy or candidate pin for {}",
                entry.profile
            );
            ensure!(
                previous.bundle.is_absolute() || previous.bundle.as_os_str().is_empty(),
                "Invalid previous manifest path for {}",
                entry.profile
            );
            ensure!(
                previous.bundle != self.candidate_path
                    || previous.manifest_sha256 != entry.next_pin,
                "The receipt marks an unchanged profile as updated"
            );
        }
        Ok(self)
    }
}

#[derive(Serialize)]
pub struct ToolRollbackProfile {
    pub profile: String,
    pub current: ToolSync,
    pub restore: ToolSync,
    pub current_manifest_sha256: Option<String>,
    pub restored_manifest_sha256: Option<String>,
    pub restore_status: ToolRestoreStatus,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolRestoreStatus {
    ManifestUnavailable,
    FollowsFile,
    MatchesPin,
    PinMismatch,
    ScopeMismatch,
}

#[derive(Serialize)]
pub struct ToolRollbackReport {
    pub profiles_path: PathBuf,
    pub profiles: Vec<ToolRollbackProfile>,
    pub changed_profiles: usize,
    pub applied: bool,
    pub review: String,
}

fn observe<'a>(
    settings: &ToolSync,
    manifests: &'a mut HashMap<PathBuf, Option<Manifest>>,
) -> Option<&'a Manifest> {
    manifests
        .entry(settings.bundle.clone())
        .or_insert_with(|| Manifest::load(&settings.bundle).ok())
        .as_ref()
}

fn plan(
    receipt: &Receipt,
    profiles: &BTreeMap<String, SshOptions>,
) -> Result<Vec<ToolRollbackProfile>> {
    use ToolRestoreStatus::*;
    let mut manifests = HashMap::new();
    receipt
        .profiles
        .iter()
        .map(|entry| {
            let options = profiles
                .get(&entry.profile)
                .with_context(|| format!("SSH profile {} was not found", entry.profile))?;
            let current =
                approval(options).with_context(|| format!("SSH profile {}", entry.profile))?;
            let mut expected = entry.previous.clone();
            expected.bundle.clone_from(&receipt.candidate_path);
            expected.manifest_sha256.clone_from(&entry.next_pin);
            ensure!(
                current == &expected,
                "The SSH tool approval for {} no longer matches the applied rollout",
                entry.profile
            );
            let current_manifest_sha256 =
                observe(current, &mut manifests).map(|manifest| manifest.digest.clone());
            let restore = observe(&entry.previous, &mut manifests);
            let pin = entry.previous.manifest_pin()?;
            let restore_status = match restore {
                None => ManifestUnavailable,
                Some(manifest) if pin.as_deref().is_some_and(|pin| pin != manifest.digest) => {
                    PinMismatch
                }
                Some(manifest)
                    if entry
                        .previous
                        .validate_manifest_destination(&manifest.bundle)
                        .is_err() =>
                {
                    ScopeMismatch
                }
                Some(_) if pin.is_some() => MatchesPin,
                Some(_) => FollowsFile,
            };
            Ok(ToolRollbackProfile {
                profile: entry.profile.clone(),
                current: current.clone(),
                restore: entry.previous.clone(),
                current_manifest_sha256,
                restored_manifest_sha256: restore.map(|manifest| manifest.digest.clone()),
                restore_status,
            })
        })
        .collect()
}

fn review_token(receipt: &Receipt, profiles: &[ToolRollbackProfile]) -> Result<String> {
    let mut digest = Sha256::new_with_prefix(b"w11term-tool-rollout-rollback-v1\0");
    digest.update(serde_json::to_vec(&(
        &receipt.profiles_path,
        &receipt.candidate_manifest_sha256,
        profiles,
    ))?);
    Ok(digest_hex(digest))
}

pub fn rollback_tool_rollout(path: &Path, review: Option<&str>) -> Result<ToolRollbackReport> {
    const STALE: &str = "The rollback manifests or approvals changed. Preview the rollback again.";
    let receipt = Receipt::load(path)?;
    let store = ProfileStore::new(receipt.profiles_path.clone())?;
    let rows = plan(&receipt, &store.load()?)?;
    let token = review_token(&receipt, &rows)?;
    if let Some(review) = review {
        let review = normalize_sha256(review)?;
        ensure!(token == review, STALE);
        store.modify(|entries, catalog| {
            let current = plan(&receipt, &catalog.profiles)?;
            ensure!(review_token(&receipt, &current)? == review, STALE);
            for entry in &current {
                write_binding(
                    entries,
                    &entry.profile,
                    &entry.restore.bundle,
                    entry.restore.manifest_sha256.as_deref(),
                )?;
            }
            Ok(CatalogEdit::default())
        })?;
    }
    Ok(ToolRollbackReport {
        profiles_path: store.path().to_owned(),
        changed_profiles: rows.len(),
        profiles: rows,
        applied: review.is_some(),
        review: token,
    })
}
