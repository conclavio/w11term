use super::ProfileStore;
use crate::{options::SshOptions, sftp::UtilityBundle};
use anyhow::{Result, ensure};
use serde::Serialize;
use std::{collections::HashMap, path::PathBuf};

#[derive(Serialize)]
pub struct ToolProfileAudit {
    pub profiles_path: PathBuf,
    pub profiles: Vec<ToolProfileAuditRow>,
}

#[derive(Serialize)]
pub struct ToolProfileAuditRow {
    pub profile: String,
    pub host: String,
    pub user: String,
    pub port: u16,
    pub status: ToolProfileAuditStatus,
    pub bundle: Option<PathBuf>,
    pub bundle_name: Option<String>,
    pub bin_directory: Option<String>,
    pub pinned_manifest_sha256: Option<String>,
    pub manifest_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolProfileAuditStatus {
    Disabled,
    NotRun,
    InvalidSettings,
    ManifestError,
    ManifestScopeMismatch,
    MatchesPin,
    PinMismatch,
    FollowsFile,
}

type ManifestCache = HashMap<PathBuf, Result<(UtilityBundle, String), String>>;

impl ProfileStore {
    pub fn audit_tool_profiles(&self, selected: Option<&str>) -> Result<ToolProfileAudit> {
        let profiles = self.load()?;
        if let Some(profile) = selected {
            ensure!(
                profiles.contains_key(profile),
                "SSH profile {profile} was not found"
            );
        }
        let mut manifests = ManifestCache::new();
        let profiles = profiles
            .iter()
            .filter(|(name, _)| selected.is_none_or(|selected| selected == name.as_str()))
            .map(|(name, profile)| audit_profile(name, profile, &mut manifests))
            .collect();
        Ok(ToolProfileAudit {
            profiles_path: self.path().to_owned(),
            profiles,
        })
    }
}

fn audit_profile(
    name: &str,
    profile: &SshOptions,
    manifests: &mut ManifestCache,
) -> ToolProfileAuditRow {
    use ToolProfileAuditStatus::*;

    let settings = profile.tool_bundle.as_deref();
    let mut row = ToolProfileAuditRow {
        profile: name.into(),
        host: profile.host.clone(),
        user: profile.user.clone(),
        port: profile.port,
        status: Disabled,
        bundle: settings.map(|settings| settings.bundle.clone()),
        bundle_name: settings.map(|settings| settings.name.clone()),
        bin_directory: settings.map(|settings| settings.bin_directory.clone()),
        pinned_manifest_sha256: settings.and_then(|settings| settings.manifest_sha256.clone()),
        manifest_sha256: None,
        detail: None,
        path_error: None,
    };
    let Some(settings) = settings else {
        return row;
    };
    if profile.durable || profile.player.rustmush {
        row.status = NotRun;
        row.detail = Some("This profile uses its own shell startup.".into());
        return row;
    }
    let pin = match settings
        .validate_profile(profile)
        .and_then(|()| settings.manifest_pin())
    {
        Ok(pin) => pin,
        Err(error) => {
            row.status = InvalidSettings;
            row.detail = Some(format!("{error:#}"));
            return row;
        }
    };
    row.path_error = settings.add_to_path.as_ref().and_then(|path| {
        path.startup_directory(&settings.home)
            .err()
            .map(|error| format!("{error:#}"))
    });
    let (bundle, digest) = match manifests.entry(settings.bundle.clone()).or_insert_with(|| {
        UtilityBundle::load_with_manifest_sha256(&settings.bundle)
            .map_err(|error| format!("{error:#}"))
    }) {
        Ok(manifest) => manifest,
        Err(error) => {
            row.status = ManifestError;
            row.detail = Some(error.clone());
            return row;
        }
    };
    row.manifest_sha256 = Some(digest.clone());
    row.status = if pin.as_deref().is_some_and(|pin| pin != digest.as_str()) {
        PinMismatch
    } else if let Err(error) = settings.validate_manifest_destination(bundle) {
        row.detail = Some(format!("{error:#}"));
        ManifestScopeMismatch
    } else if pin.is_some() {
        MatchesPin
    } else {
        FollowsFile
    };
    row
}
