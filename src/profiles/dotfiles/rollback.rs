use super::*;
use serde::Deserialize;
use serde_json::json;
use std::{fs::File, io::Read};

const MAX_RECEIPT_BYTES: usize = 1024 * 1024;

#[derive(Deserialize)]
struct Envelope {
    version: u32,
    ok: bool,
    result: Option<Receipt>,
}

#[derive(Deserialize)]
struct Receipt {
    profiles_path: PathBuf,
    candidate_path: PathBuf,
    applied: bool,
    profiles: Vec<ReceiptProfile>,
}

#[derive(Deserialize)]
struct ReceiptProfile {
    profile: String,
    previous: DotfileSync,
    candidate_bundle_sha256: String,
    next_pin: Option<String>,
    changed: bool,
}

impl Receipt {
    fn load(path: &Path) -> Result<Self> {
        ensure!(
            path.metadata()
                .with_context(|| format!("Opening rollout receipt {}", path.display()))?
                .is_file(),
            "Choose a regular rollout receipt file"
        );
        let mut bytes = Vec::new();
        File::open(path)
            .with_context(|| format!("Opening rollout receipt {}", path.display()))?
            .take((MAX_RECEIPT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= MAX_RECEIPT_BYTES,
            "Rollout receipt exceeds 1 MiB"
        );
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
        let receipt: Envelope =
            serde_json::from_slice(bytes).context("Reading the dotfile rollout receipt")?;
        ensure!(
            receipt.version == crate::control::VERSION && receipt.ok,
            "Choose a successful dotfile rollout reply"
        );
        let mut receipt = receipt
            .result
            .filter(|result| result.applied)
            .context("Choose the reply from an applied dotfile rollout")?;
        ensure!(
            receipt.profiles_path.is_absolute() && receipt.candidate_path.is_absolute(),
            "The rollout receipt must identify the original catalog and bundle paths"
        );
        receipt.profiles.retain(|entry| entry.changed);
        ensure!(
            !receipt.profiles.is_empty(),
            "The rollout did not change any profiles"
        );
        receipt
            .profiles
            .sort_unstable_by(|a, b| a.profile.cmp(&b.profile));
        ensure!(
            receipt
                .profiles
                .windows(2)
                .all(|pair| pair[0].profile != pair[1].profile),
            "A profile appears more than once in the receipt"
        );
        for entry in &receipt.profiles {
            let previous = &entry.previous;
            let candidate = &entry.candidate_bundle_sha256;
            ensure!(
                normalize_sha256(candidate)? == *candidate,
                "Invalid candidate digest in the rollout receipt"
            );
            ensure!(
                entry.next_pin.as_deref()
                    == previous.bundle_sha256.as_ref().map(|_| candidate.as_str()),
                "The receipt changes the pin policy for {}",
                entry.profile
            );
            ensure!(
                previous.bundle.is_absolute() || previous.bundle.as_os_str().is_empty(),
                "Invalid previous bundle path for {}",
                entry.profile
            );
            ensure!(
                previous.bundle != receipt.candidate_path
                    || previous.bundle_sha256 != entry.next_pin,
                "The receipt marks an unchanged profile as updated"
            );
        }
        Ok(receipt)
    }
}

#[derive(Serialize)]
struct RestoreProfile<'a> {
    profile: &'a str,
    current: DotfileSync,
    restore: &'a DotfileSync,
    current_bundle_sha256: Option<String>,
    restored_bundle_sha256: Option<String>,
    restore_status: &'static str,
}

type Versions = Vec<(DotfileShell, String)>;

fn observed_digest(
    settings: &DotfileSync,
    files: &mut BTreeMap<PathBuf, Versions>,
) -> Option<String> {
    files
        .entry(settings.bundle.clone())
        .or_insert_with(|| {
            DotfileBundle::load(&settings.bundle)
                .and_then(|bundle| bundle.content_versions())
                .unwrap_or_default()
        })
        .iter()
        .find_map(|(shell, digest)| (*shell == settings.shell).then(|| digest.clone()))
}

fn plan<'a>(
    receipt: &'a Receipt,
    profiles: &BTreeMap<String, SshOptions>,
) -> Result<Vec<RestoreProfile<'a>>> {
    let mut files = BTreeMap::new();
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
            expected.bundle = receipt.candidate_path.clone();
            expected.bundle_sha256 = entry.next_pin.clone();
            ensure!(
                current == &expected,
                "The dotfiles approval for {} no longer matches the applied rollout",
                entry.profile
            );
            let restored_bundle_sha256 = observed_digest(&entry.previous, &mut files);
            let restore_status = match (
                restored_bundle_sha256.as_deref(),
                entry.previous.bundle_sha256.as_deref(),
            ) {
                (None, _) => "bundle-unavailable",
                (Some(_), None) => "follows-file",
                (Some(digest), Some(pin)) if digest == pin => "matches-pin",
                (Some(_), Some(_)) => "pin-mismatch",
            };
            Ok(RestoreProfile {
                profile: &entry.profile,
                current: current.clone(),
                restore: &entry.previous,
                current_bundle_sha256: observed_digest(current, &mut files),
                restored_bundle_sha256,
                restore_status,
            })
        })
        .collect()
}

fn review_token(catalog: &Path, profiles: &[RestoreProfile<'_>]) -> Result<String> {
    let mut digest = Sha256::new_with_prefix(b"w11term-dotfiles-rollout-rollback-v1\0");
    digest.update(serde_json::to_vec(&(catalog, profiles))?);
    Ok(digest_hex(digest))
}

pub fn rollback_dotfile_rollout(receipt: &Path, review: Option<&str>) -> Result<Value> {
    const STALE: &str = "The rollback files or approvals changed. Preview the rollback again.";
    let receipt = Receipt::load(receipt)?;
    let store = ProfileStore::new(receipt.profiles_path.clone())?;
    let rows = plan(&receipt, &store.load()?)?;
    let token = review_token(store.path(), &rows)?;
    if let Some(review) = review {
        let review = normalize_sha256(review)?;
        ensure!(token == review, STALE);
        store.modify(|values, catalog| {
            let current = plan(&receipt, &catalog.profiles)?;
            ensure!(review_token(store.path(), &current)? == review, STALE);
            for entry in &current {
                write_binding(
                    values,
                    entry.profile,
                    &entry.restore.bundle,
                    entry.restore.bundle_sha256.as_deref(),
                )?;
            }
            Ok(CatalogEdit::default())
        })?;
    }
    Ok(json!({
        "profiles_path": store.path(), "profiles": rows,
        "changed_profiles": rows.len(), "applied": review.is_some(), "review": token,
    }))
}
