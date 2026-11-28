use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::Write,
    path::{self, Path, PathBuf},
};
use w11term::{
    control::Reply,
    options::{SshOptions, default_profiles_path},
    profiles::{ProfileStore, rollback_dotfile_rollout},
    sftp::{DotfileBundle, DotfileShell},
};

pub(super) const VERB: &str = "audit-dotfiles-profiles";
pub(super) const HELP: &str = "w11term-cli audit-dotfiles-profiles [--profiles CATALOG] [--profile NAME] [--candidate BUNDLE]";
pub(super) const ROLLOUT_VERB: &str = "rollout-dotfiles-profiles";
pub(super) const ROLLOUT_HELP: &str = "w11term-cli rollout-dotfiles-profiles --candidate BUNDLE --profile NAME [--profile NAME ...] [--profiles CATALOG] [--review TOKEN]";
pub(super) const ROLLBACK_VERB: &str = "rollback-dotfiles-profiles";
pub(super) const ROLLBACK_HELP: &str =
    "w11term-cli rollback-dotfiles-profiles ROLLOUT-REPLY.json [--review TOKEN]";

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

    let store = profile_store(catalog)?;
    let profiles = store.load()?;
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
        "profiles_path": store.path(),
        "candidate_path": candidate_path,
        "profiles": profiles,
    })));
    serde_json::to_writer(&mut *output, &reply)?;
    writeln!(output)?;
    Ok(())
}

pub(super) fn rollout(args: &[String], output: &mut dyn Write) -> Result<()> {
    let mut catalog = None;
    let mut candidate = None;
    let mut review = None;
    let mut profiles = Vec::new();
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .with_context(|| format!("{flag} requires a value"))?;
        let target = match flag.as_str() {
            "--profiles" => &mut catalog,
            "--candidate" => &mut candidate,
            "--review" => &mut review,
            "--profile" => {
                profiles.push(value.clone());
                continue;
            }
            _ => bail!("unsupported dotfiles rollout option {flag}"),
        };
        ensure!(
            target.replace(value.as_str()).is_none(),
            "specify {flag} only once"
        );
    }
    let candidate =
        path::absolute(candidate.context("Choose a dotfiles bundle with --candidate")?)?;
    let result = profile_store(catalog)?.rollout_dotfiles(&profiles, &candidate, review)?;
    serde_json::to_writer(
        &mut *output,
        &Reply::from_result(Ok(serde_json::to_value(result)?)),
    )?;
    writeln!(output)?;
    Ok(())
}

pub(super) fn rollback(args: &[String], output: &mut dyn Write) -> Result<()> {
    let (path, review) = match args {
        [path] => (path, None),
        [path, flag, token] if flag == "--review" => (path, Some(token.as_str())),
        _ => bail!("Provide the saved rollout reply and optionally --review TOKEN"),
    };
    let report = rollback_dotfile_rollout(Path::new(path), review)?;
    let reply = Reply::from_result(Ok(serde_json::to_value(report)?));
    serde_json::to_writer(&mut *output, &reply)?;
    writeln!(output)?;
    Ok(())
}

fn profile_store(catalog: Option<&str>) -> Result<ProfileStore> {
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
    ProfileStore::new(catalog)
}

fn versions(path: &Path) -> Result<Versions> {
    DotfileBundle::load(path)?.content_versions()
}

fn digest_for(versions: &[(DotfileShell, String)], shell: DotfileShell) -> Option<&str> {
    versions
        .iter()
        .find_map(|(available, digest)| (*available == shell).then_some(digest.as_str()))
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
    let current = match settings.validate_profile(profile) {
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
