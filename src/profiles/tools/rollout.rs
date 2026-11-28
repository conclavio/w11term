use super::Manifest;
use crate::{
    options::{SshOptions, ToolSync},
    profiles::{CatalogEdit, ProfileStore},
    sftp::{
        ToolArchitectureAudit, UtilityArchitecture, UtilityBundle, digest_hex, normalize_sha256,
    },
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
};

#[derive(Serialize)]
pub struct ToolRolloutReport {
    pub profiles_path: PathBuf,
    pub candidate_path: PathBuf,
    pub candidate_manifest_sha256: String,
    pub candidate_commands: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_builds: Option<ToolRolloutBuildReport>,
    pub profiles: Vec<ToolRolloutProfile>,
    pub changed_profiles: usize,
    pub applied: bool,
    pub review: String,
}

#[derive(Clone, Copy, Serialize)]
pub struct ToolRolloutBuildPolicy {
    pub architecture: Option<UtilityArchitecture>,
    pub require_ready: bool,
}

#[derive(Serialize)]
pub struct ToolRolloutBuildReport {
    #[serde(flatten)]
    policy: ToolRolloutBuildPolicy,
    pub ready: bool,
    pub checks: Vec<ToolArchitectureAudit>,
}

#[derive(Serialize)]
pub struct ToolRolloutProfile {
    pub profile: String,
    pub previous: ToolSync,
    pub baseline: ToolManifestBaseline,
    pub changes: Option<ToolManifestChanges>,
    pub next_pin: Option<String>,
    pub changed: bool,
}

#[derive(Serialize)]
#[serde(tag = "kind", content = "manifest_sha256", rename_all = "kebab-case")]
pub enum ToolManifestBaseline {
    Pinned(String),
    FollowsFile(String),
    PinMismatch(String),
    ScopeMismatch(String),
    Unavailable,
}

#[derive(Serialize)]
pub struct ToolManifestChanges {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
}

#[derive(Serialize)]
struct Plan<'a> {
    catalog: &'a Path,
    candidate: &'a Path,
    candidate_manifest_sha256: String,
    candidate_commands: Vec<String>,
    #[serde(skip)]
    candidate_bundle: UtilityBundle,
    profiles: Vec<ToolRolloutProfile>,
}

pub(super) fn approval(options: &SshOptions) -> Result<&ToolSync> {
    ensure!(
        !options.durable && !options.player.rustmush,
        "This profile uses its own shell startup"
    );
    let settings = options
        .tool_bundle
        .as_deref()
        .context("This profile has no SSH tool approval")?;
    settings.validate_profile(options)?;
    Ok(settings)
}

fn load_candidate(candidate: &Path) -> Result<Manifest> {
    Manifest::load(candidate)
        .with_context(|| format!("Reading candidate tool manifest {}", candidate.display()))
}

impl<'a> Plan<'a> {
    fn build(
        catalog: &'a Path,
        candidate: &'a Path,
        selected: &BTreeSet<&str>,
        profiles: &BTreeMap<String, SshOptions>,
    ) -> Result<Self> {
        let proposed = load_candidate(candidate)?;
        let mut baselines = HashMap::new();
        let mut revisions = Vec::with_capacity(selected.len());
        for &name in selected {
            let options = profiles
                .get(name)
                .with_context(|| format!("SSH profile {name} was not found"))?;
            let settings = approval(options).with_context(|| format!("SSH profile {name}"))?;
            settings
                .validate_manifest_destination(&proposed.bundle)
                .with_context(|| {
                    format!("Candidate does not match the approved destination for {name}")
                })?;
            let pin = settings
                .manifest_pin()
                .with_context(|| format!("SSH profile {name}"))?;
            let before = if settings.bundle == candidate {
                Some(&proposed)
            } else {
                baselines
                    .entry(settings.bundle.clone())
                    .or_insert_with(|| Manifest::load(&settings.bundle).ok())
                    .as_ref()
            };
            let (baseline, changes) = compare(before, &proposed, settings, pin.as_deref());
            let next_pin = settings.manifest_sha256.as_ref().map(|original| {
                if pin.as_deref() == Some(proposed.digest.as_str()) {
                    original.clone()
                } else {
                    proposed.digest.clone()
                }
            });
            revisions.push(ToolRolloutProfile {
                profile: name.to_owned(),
                previous: settings.clone(),
                baseline,
                changes,
                changed: settings.bundle != candidate || settings.manifest_sha256 != next_pin,
                next_pin,
            });
        }
        Ok(Self {
            catalog,
            candidate,
            candidate_manifest_sha256: proposed.digest,
            candidate_commands: proposed
                .bundle
                .tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect(),
            candidate_bundle: proposed.bundle,
            profiles: revisions,
        })
    }

    fn review(&self, policy: Option<ToolRolloutBuildPolicy>) -> Result<String> {
        let mut digest = Sha256::new_with_prefix(b"w11term-tools-rollout-v1\0");
        digest.update(serde_json::to_vec(self)?);
        if let Some(policy) = policy {
            digest.update(b"\0candidate-build-policy-v1\0");
            digest.update(serde_json::to_vec(&policy)?);
        }
        Ok(digest_hex(digest))
    }

    fn check_builds(
        &self,
        policy: Option<ToolRolloutBuildPolicy>,
        applying: bool,
    ) -> Result<Option<ToolRolloutBuildReport>> {
        let Some(policy) = policy else {
            return Ok(None);
        };
        let checks = self
            .candidate_bundle
            .audit_executables(policy.architecture, &|| false)?;
        let ready = checks.iter().all(|check| check.locally_available);
        if applying && policy.require_ready && !ready {
            let missing: Vec<_> = checks
                .iter()
                .flat_map(|check| {
                    check
                        .commands
                        .iter()
                        .filter(|command| !command.locally_available)
                        .map(|command| format!("{} ({:?})", command.name, check.architecture))
                })
                .collect();
            anyhow::bail!(
                "Candidate builds are missing or invalid for {}. Prepare the builds and try again.",
                missing.join(", ")
            );
        }
        Ok(Some(ToolRolloutBuildReport {
            policy,
            ready,
            checks,
        }))
    }

    fn apply(&self, entries: &mut Map<String, Value>) -> Result<()> {
        for entry in self.profiles.iter().filter(|entry| entry.changed) {
            write_binding(
                entries,
                &entry.profile,
                self.candidate,
                entry.next_pin.as_deref(),
            )?;
        }
        Ok(())
    }
}

pub(super) fn write_binding(
    entries: &mut Map<String, Value>,
    profile: &str,
    path: &Path,
    pin: Option<&str>,
) -> Result<()> {
    let approval = entries
        .get_mut(profile)
        .and_then(|profile| profile.get_mut("tool_bundle"))
        .and_then(Value::as_object_mut)
        .context("The reviewed SSH tool approval is missing")?;
    approval.insert("bundle".into(), serde_json::to_value(path)?);
    if let Some(pin) = pin {
        approval.insert("manifest_sha256".into(), pin.into());
    }
    Ok(())
}

fn compare(
    before: Option<&Manifest>,
    proposed: &Manifest,
    settings: &ToolSync,
    pin: Option<&str>,
) -> (ToolManifestBaseline, Option<ToolManifestChanges>) {
    use ToolManifestBaseline::*;
    let Some(before) = before else {
        return (Unavailable, None);
    };
    if pin.is_some_and(|pin| pin != before.digest) {
        return (PinMismatch(before.digest.clone()), None);
    }
    if settings
        .validate_manifest_destination(&before.bundle)
        .is_err()
    {
        return (ScopeMismatch(before.digest.clone()), None);
    }
    let baseline = if pin.is_some() {
        Pinned(before.digest.clone())
    } else {
        FollowsFile(before.digest.clone())
    };
    (baseline, Some(changes(&before.bundle, &proposed.bundle)))
}

fn changes(before: &UtilityBundle, proposed: &UtilityBundle) -> ToolManifestChanges {
    let old: BTreeMap<_, _> = before
        .tools
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect();
    let next: BTreeMap<_, _> = proposed
        .tools
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect();
    let mut result = ToolManifestChanges {
        added: Vec::new(),
        removed: Vec::new(),
        changed: Vec::new(),
    };
    for (&name, &tool) in &next {
        match old.get(name) {
            None => result.added.push(name.to_owned()),
            Some(&old_tool) if old_tool != tool => result.changed.push(name.to_owned()),
            Some(_) => {}
        }
    }
    result.removed.extend(
        old.into_keys()
            .filter(|name| !next.contains_key(name))
            .map(str::to_owned),
    );
    result
}

impl ProfileStore {
    pub fn tool_rollout_targets(&self, candidate: &Path) -> Result<Vec<(String, ToolSync)>> {
        let candidate = load_candidate(candidate)?;
        Ok(self
            .load()?
            .into_iter()
            .filter_map(|(name, options)| {
                let settings = approval(&options).ok()?;
                (settings.manifest_pin().is_ok()
                    && settings
                        .validate_manifest_destination(&candidate.bundle)
                        .is_ok())
                .then(|| (name, settings.clone()))
            })
            .collect())
    }

    pub fn rollout_tools(
        &self,
        profiles: &[String],
        candidate: &Path,
        review: Option<&str>,
    ) -> Result<ToolRolloutReport> {
        self.rollout_tools_with_builds(profiles, candidate, review, None)
    }

    pub fn rollout_tools_with_builds(
        &self,
        profiles: &[String],
        candidate: &Path,
        review: Option<&str>,
        policy: Option<ToolRolloutBuildPolicy>,
    ) -> Result<ToolRolloutReport> {
        ensure!(
            candidate.is_absolute(),
            "Choose an absolute candidate tool manifest path"
        );
        ensure!(
            !profiles.is_empty(),
            "Choose at least one SSH profile with --profile"
        );
        let selected: BTreeSet<_> = profiles.iter().map(String::as_str).collect();
        ensure!(
            selected.len() == profiles.len(),
            "Choose each SSH profile only once"
        );
        let plan = Plan::build(self.path(), candidate, &selected, &self.load()?)?;
        let token = plan.review(policy)?;
        let changed = plan.profiles.iter().filter(|entry| entry.changed).count();
        let candidate_builds = if let Some(review) = review {
            let stale = if policy.is_some() {
                "The candidate, baseline, profile approvals, or build policy changed. Preview the rollout again."
            } else {
                "The candidate, baseline, or profile approvals changed. Preview the rollout again."
            };
            let review = normalize_sha256(review)?;
            ensure!(token == review, stale);
            if changed > 0 {
                let mut checks = None;
                self.modify(|entries, catalog| {
                    let current =
                        Plan::build(self.path(), candidate, &selected, &catalog.profiles)?;
                    ensure!(current.review(policy)? == review, stale);
                    checks = current.check_builds(policy, true)?;
                    current.apply(entries)?;
                    Ok(CatalogEdit::default())
                })?;
                checks
            } else {
                plan.check_builds(policy, true)?
            }
        } else {
            plan.check_builds(policy, false)?
        };
        Ok(ToolRolloutReport {
            profiles_path: plan.catalog.to_owned(),
            candidate_path: plan.candidate.to_owned(),
            candidate_manifest_sha256: plan.candidate_manifest_sha256,
            candidate_commands: plan.candidate_commands,
            candidate_builds,
            profiles: plan.profiles,
            changed_profiles: changed,
            applied: review.is_some(),
            review: token,
        })
    }
}
