use super::{Manifest, ProfileStore, ToolRolloutReport, rollout::approval};
use crate::{
    options::SshOptions,
    sftp::{
        ToolArchitectureAudit, UtilityArchitecture, UtilityBundle, fetch_tool_bundle,
        fetch_tool_bundle_cancellable,
    },
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{collections::BTreeSet, path::PathBuf};

#[derive(Serialize)]
pub struct ToolProfilePreparation {
    profiles_path: PathBuf,
    architecture: Option<UtilityArchitecture>,
    profiles: Vec<ProfilePreparation>,
    pub failed_profiles: usize,
}

#[derive(Serialize)]
struct ProfilePreparation {
    profile: String,
    #[serde(flatten)]
    outcome: Outcome,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
enum Outcome {
    Prepared {
        manifest_sha256: String,
        prepared_builds: usize,
    },
    NoDownloads {
        manifest_sha256: String,
    },
    Failed {
        error: String,
    },
}

struct ApprovedDownloads {
    bundle: UtilityBundle,
    digest: String,
    count: usize,
}

impl ApprovedDownloads {
    fn load(
        options: &SshOptions,
        reviewed_digest: Option<&str>,
        architecture: Option<UtilityArchitecture>,
    ) -> Result<Self> {
        let settings = approval(options)?;
        let Manifest { bundle, digest, .. } = Manifest::load(&settings.bundle)?;
        ensure!(
            reviewed_digest.is_none_or(|review| review == digest)
                && settings.manifest_pin()?.is_none_or(|pin| pin == digest),
            if reviewed_digest.is_some() {
                "The tool manifest or its pin changed. Refresh the audit before preparing downloads."
            } else {
                "The tool manifest differs from its saved pin"
            }
        );
        settings.validate_manifest_destination(&bundle)?;
        let bundle = download_entries(bundle, architecture);
        let count = bundle.tools.iter().map(|tool| tool.builds.len()).sum();
        Ok(Self {
            bundle,
            digest,
            count,
        })
    }
}

fn download_entries(
    mut bundle: UtilityBundle,
    architecture: Option<UtilityArchitecture>,
) -> UtilityBundle {
    for tool in &mut bundle.tools {
        tool.builds.retain(|build| {
            build.download.is_some()
                && architecture.is_none_or(|architecture| build.architecture == architecture)
        });
    }
    bundle.tools.retain(|tool| !tool.builds.is_empty());
    bundle
}

impl ToolRolloutReport {
    fn reviewed_candidate(&self) -> Result<UtilityBundle> {
        let manifest = Manifest::load(&self.candidate_path)?;
        ensure!(
            manifest.digest == self.candidate_manifest_sha256,
            "The candidate manifest changed. Review the rollout again."
        );
        Ok(manifest.bundle)
    }

    /// Check the reviewed candidate's local files and cached downloads without fetching.
    pub fn verify_candidate_builds(
        &self,
        cancelled: impl Fn() -> bool,
    ) -> Result<Vec<ToolArchitectureAudit>> {
        ensure!(!cancelled(), "Candidate tool verification cancelled");
        self.reviewed_candidate()?
            .audit_executables(None, &cancelled)
    }

    /// Prepare one architecture from the reviewed candidate and refresh local readiness.
    pub fn prepare_candidate_downloads(
        &self,
        architecture: UtilityArchitecture,
        cancelled: impl Fn() -> bool,
    ) -> Result<Vec<ToolArchitectureAudit>> {
        ensure!(!cancelled(), "Candidate tool preparation cancelled");
        let downloads = download_entries(self.reviewed_candidate()?, Some(architecture));
        ensure!(
            !downloads.tools.is_empty(),
            "No pinned downloads match the selected architecture"
        );
        fetch_tool_bundle_cancellable(&downloads, None, &cancelled)?;
        self.verify_candidate_builds(cancelled)
    }
}

impl ProfileStore {
    /// Cache the downloads from the exact manifest shown in a saved profile audit.
    pub fn prepare_tool_profile(
        &self,
        profile: &str,
        manifest_sha256: &str,
        architecture: Option<UtilityArchitecture>,
        cancelled: impl Fn() -> bool,
    ) -> Result<usize> {
        ensure!(!cancelled(), "SSH tool preparation cancelled");
        let profiles = self.load()?;
        let options = profiles
            .get(profile)
            .with_context(|| format!("SSH profile {profile} was not found"))?;
        let downloads = ApprovedDownloads::load(options, Some(manifest_sha256), architecture)?;
        ensure!(
            downloads.count > 0,
            "No pinned downloads match the selected architecture"
        );
        fetch_tool_bundle_cancellable(&downloads.bundle, None, cancelled)?;
        Ok(downloads.count)
    }

    /// Prepare explicitly selected profiles using the saved approval for each one.
    pub fn prepare_tool_profiles(
        &self,
        names: &[String],
        architecture: Option<UtilityArchitecture>,
    ) -> Result<ToolProfilePreparation> {
        ensure!(
            !names.is_empty(),
            "Choose at least one SSH profile with --profile"
        );
        let unique: BTreeSet<_> = names.iter().collect();
        ensure!(
            unique.len() == names.len(),
            "Choose each SSH profile only once"
        );

        let saved = self.load()?;
        let mut profiles = Vec::with_capacity(names.len());
        let mut failed_profiles = 0;
        for name in names {
            let outcome = (|| -> Result<Outcome> {
                let options = saved
                    .get(name)
                    .with_context(|| format!("SSH profile {name} was not found"))?;
                let downloads = ApprovedDownloads::load(options, None, architecture)?;
                if downloads.count == 0 {
                    return Ok(Outcome::NoDownloads {
                        manifest_sha256: downloads.digest,
                    });
                }
                fetch_tool_bundle(&downloads.bundle, None)?;
                Ok(Outcome::Prepared {
                    manifest_sha256: downloads.digest,
                    prepared_builds: downloads.count,
                })
            })();
            let outcome = match outcome {
                Ok(outcome) => outcome,
                Err(error) => {
                    failed_profiles += 1;
                    Outcome::Failed {
                        error: format!("{error:#}"),
                    }
                }
            };
            profiles.push(ProfilePreparation {
                profile: name.clone(),
                outcome,
            });
        }
        Ok(ToolProfilePreparation {
            profiles_path: self.path().to_owned(),
            architecture,
            profiles,
            failed_profiles,
        })
    }
}
