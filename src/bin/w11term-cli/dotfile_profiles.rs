use anyhow::{Context, Result, bail, ensure};
use russh::keys::ssh_key::Fingerprint;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::Write,
    path::{self, Path, PathBuf},
};
use w11term::{
    control::Reply,
    options::{DotfileSync, SshOptions, default_profiles_path},
    profiles::ProfileStore,
    sftp::{DotfileBundle, DotfileShell, select_startup_directory, validate_dotfiles_home},
};

pub(super) const VERB: &str = "audit-dotfiles-profiles";
pub(super) const HELP: &str = "w11term-cli audit-dotfiles-profiles [--profiles CATALOG] [--profile NAME] [--candidate BUNDLE]";

type Versions = Vec<(DotfileShell, String)>;
type BundleCache = HashMap<PathBuf, Result<Versions, String>>;

pub(super) fn audit(args: &[String], output: &mut dyn Write) -> Result<()> {
    let mut catalog = None;
    let mut profile = None;
    let mut candidate = None;
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        let target = match flag.as_str() {
            "--profiles" => &mut catalog,
            "--profile" => &mut profile,
            "--candidate" => &mut candidate,
            _ => bail!("unsupported dotfiles audit option {flag}"),
        };
        let value = args
            .next()
            .with_context(|| format!("{flag} requires a value"))?;
        ensure!(
            target.replace(value.as_str()).is_none(),
            "specify {flag} only once"
        );
    }

    let catalog = match catalog {
        Some(path) => path::absolute(path)?,
        None => default_profiles_path()?,
    };
    ensure!(
        catalog
            .metadata()
            .with_context(|| format!("Opening profile catalog {}", catalog.display()))?
            .is_file(),
        "The profile catalog must be a regular file"
    );
    let profiles = ProfileStore::new(catalog.clone())?.load()?;
    if let Some(profile) = profile {
        ensure!(
            profiles.contains_key(profile),
            "SSH profile {profile} was not found"
        );
    }
    let candidate_path = candidate.map(path::absolute).transpose()?;
    let candidate = candidate_path
        .as_deref()
        .map(|path| {
            versions(path)
                .with_context(|| format!("Reading candidate dotfiles bundle {}", path.display()))
        })
        .transpose()?;
    let mut bundles = BundleCache::new();
    if let (Some(path), Some(versions)) = (&candidate_path, &candidate) {
        bundles.insert(path.clone(), Ok(versions.clone()));
    }
    let profiles: Vec<_> = profiles
        .iter()
        .filter(|(name, _)| profile.is_none_or(|profile| profile == name.as_str()))
        .map(|(name, profile)| profile_report(name, profile, candidate.as_deref(), &mut bundles))
        .collect();
    let reply = Reply::from_result(Ok(json!({
        "profiles_path": catalog,
        "candidate_path": candidate_path,
        "profiles": profiles,
    })));
    serde_json::to_writer(&mut *output, &reply)?;
    writeln!(output)?;
    Ok(())
}

fn versions(path: &Path) -> Result<Versions> {
    DotfileBundle::load(path)?.content_versions()
}

fn digest_for(versions: &[(DotfileShell, String)], shell: DotfileShell) -> Option<&str> {
    versions
        .iter()
        .find_map(|(available, digest)| (*available == shell).then_some(digest.as_str()))
}

fn validate_settings(profile: &SshOptions, settings: &DotfileSync) -> Result<()> {
    ensure!(
        (
            settings.host.as_str(),
            settings.user.as_str(),
            settings.port
        ) == (profile.host.as_str(), profile.user.as_str(), profile.port),
        "The dotfiles approval names a different SSH account"
    );
    ensure!(
        matches!(settings.fingerprint.parse::<Fingerprint>(), Ok(fingerprint @ Fingerprint::Sha256(_)) if fingerprint.to_string() == settings.fingerprint),
        "The dotfiles approval needs a SHA256 host-key fingerprint"
    );
    ensure!(
        profile
            .host_key
            .as_deref()
            .is_none_or(|pin| pin == settings.fingerprint),
        "The dotfiles approval differs from the profile's host-key pin"
    );
    validate_dotfiles_home(&settings.home)?;
    select_startup_directory(
        settings.shell,
        settings.zdotdir.as_deref(),
        settings.fish_config_dir.as_deref(),
    )?;
    Ok(())
}

fn profile_report(
    name: &str,
    profile: &SshOptions,
    candidate: Option<&[(DotfileShell, String)]>,
    bundles: &mut BundleCache,
) -> Value {
    let mut row =
        json!({"profile": name, "host": profile.host, "user": profile.user, "port": profile.port});
    let Some(settings) = &profile.dotfiles else {
        row["status"] = json!("disabled");
        return row;
    };
    row["shell"] = json!(settings.shell);
    row["bundle"] = json!(settings.bundle);
    row["pinned_bundle_sha256"] = json!(settings.bundle_sha256);
    row["bundle_sha256"] = Value::Null;
    if profile.durable || profile.player.rustmush {
        row["status"] = json!("not-run");
        row["detail"] = json!("This profile uses its own shell startup.");
        return row;
    }
    let pin = settings.bundle_sha256.as_deref();
    let current = match validate_settings(profile, settings) {
        Err(error) => Err(("invalid-settings", format!("{error:#}"))),
        Ok(()) => bundles
            .entry(settings.bundle.clone())
            .or_insert_with(|| versions(&settings.bundle).map_err(|error| format!("{error:#}")))
            .as_ref()
            .map_err(|error| ("bundle-error", error.clone()))
            .and_then(|versions| {
                digest_for(versions, settings.shell).ok_or_else(|| {
                    (
                        "bundle-error",
                        format!("The bundle has no fragments for {}", settings.shell.label()),
                    )
                })
            }),
    };
    let digest = match current {
        Ok(digest) => {
            row["bundle_sha256"] = json!(digest);
            row["status"] = json!(match pin {
                Some(pin) if pin == digest => "matches-pin",
                Some(_) => "pin-mismatch",
                None => "follows-file",
            });
            Some(digest)
        }
        Err((status, detail)) => {
            row["status"] = json!(status);
            row["detail"] = json!(detail);
            None
        }
    };
    if let Some(candidate) = candidate {
        let proposed = digest_for(candidate, settings.shell);
        let comparison = match (digest, proposed) {
            (_, None) => "shell-unavailable",
            (None, Some(_)) => "no-current-content",
            (Some(current), Some(proposed)) if current == proposed => "same-content",
            _ => "different-content",
        };
        row["candidate"] = json!({
            "comparison": comparison,
            "bundle_sha256": proposed,
            "matches_pin": pin.zip(proposed).map(|(pin, proposed)| pin == proposed),
        });
    }
    row
}
