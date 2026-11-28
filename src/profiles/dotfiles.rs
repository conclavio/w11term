use super::{CatalogEdit, ProfileStore};
use crate::{
    options::{DotfileSync, SshOptions},
    sftp::{DotfileBundle, digest_hex, normalize_sha256},
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

#[derive(Serialize)]
struct Rollout<'a> {
    catalog: &'a Path,
    candidate: &'a Path,
    profiles: Vec<Revision>,
}

#[derive(Serialize)]
struct Revision {
    profile: String,
    previous: DotfileSync,
    candidate_bundle_sha256: String,
    next_pin: Option<String>,
    changed: bool,
}

impl<'a> Rollout<'a> {
    fn plan(
        catalog: &'a Path,
        candidate: &'a Path,
        selected: &BTreeSet<&str>,
        profiles: &BTreeMap<String, SshOptions>,
    ) -> Result<Self> {
        let versions = DotfileBundle::load(candidate)
            .and_then(|bundle| bundle.content_versions())
            .with_context(|| {
                format!("Reading candidate dotfiles bundle {}", candidate.display())
            })?;
        let mut revisions = Vec::with_capacity(selected.len());
        for &name in selected {
            let options = profiles
                .get(name)
                .with_context(|| format!("SSH profile {name} was not found"))?;
            ensure!(
                !options.durable && !options.player.rustmush,
                "SSH profile {name} uses its own shell startup"
            );
            let previous = options
                .dotfiles
                .as_deref()
                .with_context(|| format!("SSH profile {name} has no dotfiles approval"))?;
            previous
                .validate_profile(options)
                .with_context(|| format!("SSH profile {name}"))?;
            let digest = versions
                .iter()
                .find_map(|(shell, digest)| (*shell == previous.shell).then_some(digest))
                .with_context(|| {
                    format!(
                        "The candidate has no fragments for {} in SSH profile {name}",
                        previous.shell.label()
                    )
                })?;
            let next_pin = previous.bundle_sha256.as_ref().map(|_| digest.clone());
            revisions.push(Revision {
                profile: name.to_owned(),
                previous: previous.clone(),
                candidate_bundle_sha256: digest.clone(),
                changed: previous.bundle.as_path() != candidate
                    || previous.bundle_sha256 != next_pin,
                next_pin,
            });
        }
        Ok(Self {
            catalog,
            candidate,
            profiles: revisions,
        })
    }

    fn review(&self) -> Result<String> {
        let mut digest = Sha256::new_with_prefix(b"w11term-dotfiles-rollout-v1\0");
        digest.update(serde_json::to_vec(self)?);
        Ok(digest_hex(digest))
    }

    fn apply(&self, profiles: &mut Map<String, Value>) -> Result<()> {
        for entry in self.profiles.iter().filter(|entry| entry.changed) {
            let dotfiles = profiles
                .get_mut(&entry.profile)
                .and_then(|profile| profile.get_mut("dotfiles"))
                .and_then(Value::as_object_mut)
                .context("The reviewed dotfiles approval is missing")?;
            dotfiles.insert("bundle".into(), serde_json::to_value(self.candidate)?);
            if let Some(pin) = &entry.next_pin {
                dotfiles.insert("bundle_sha256".into(), json!(pin));
            }
        }
        Ok(())
    }
}

impl ProfileStore {
    pub fn rollout_dotfiles(
        &self,
        profiles: &[String],
        candidate: &Path,
        review: Option<&str>,
    ) -> Result<Value> {
        ensure!(
            candidate.is_absolute(),
            "Choose an absolute candidate bundle path"
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
        let plan = Rollout::plan(self.path(), candidate, &selected, &self.load()?)?;
        let token = plan.review()?;
        let changed = plan.profiles.iter().filter(|entry| entry.changed).count();
        if let Some(review) = review {
            const STALE: &str =
                "The candidate or profile approvals changed. Preview the rollout again.";
            let review = normalize_sha256(review)?;
            ensure!(token == review, STALE);
            if changed > 0 {
                self.modify(|entries, catalog| {
                    let current =
                        Rollout::plan(self.path(), candidate, &selected, &catalog.profiles)?;
                    ensure!(current.review()? == review, STALE);
                    current.apply(entries)?;
                    Ok(CatalogEdit::default())
                })?;
            }
        }
        Ok(json!({
            "profiles_path": plan.catalog,
            "candidate_path": plan.candidate,
            "profiles": plan.profiles,
            "changed_profiles": changed,
            "applied": review.is_some(),
            "review": token,
        }))
    }
}
