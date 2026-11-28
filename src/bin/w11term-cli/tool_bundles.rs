use super::offline::profile_store;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{io::Write, path::Path};
use w11term::control::Reply;
use w11term::profiles::ToolProfileBuildChecks;
use w11term::sftp::{
    ToolBundlePreset, UtilityArchitecture, UtilityBundle, export_builtin_tool_bundle,
    fetch_tool_bundle,
};

pub const HELP: &str = "w11term-cli fetch-tool-bundle BUNDLE.json [--architecture x86_64|aarch64]\n\
w11term-cli fetch-tool-bundle --example NEW_BUNDLE.json [--preset essentials|admin | --tools rg,jq,mdd]";
pub(super) const AUDIT_VERB: &str = "audit-tool-profiles";
pub(super) const AUDIT_HELP: &str = "w11term-cli audit-tool-profiles [--profiles CATALOG] [--profile NAME] [--verify-builds [--architecture x86_64|aarch64]]";

pub(super) fn audit(args: &[String], output: &mut dyn Write) -> Result<()> {
    let mut catalog = None;
    let mut profile = None;
    let mut verify = false;
    let mut architecture = None;
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        if flag == "--verify-builds" {
            ensure!(!verify, "specify --verify-builds only once");
            verify = true;
            continue;
        }
        let target = match flag.as_str() {
            "--profiles" => &mut catalog,
            "--profile" => &mut profile,
            "--architecture" => &mut architecture,
            _ => bail!("unsupported tool profile audit option {flag}"),
        };
        let value = args
            .next()
            .with_context(|| format!("{flag} requires a value"))?;
        ensure!(
            target.replace(value.as_str()).is_none(),
            "specify {flag} only once"
        );
    }
    ensure!(
        verify || architecture.is_none(),
        "--architecture requires --verify-builds"
    );
    let checks = if verify {
        ToolProfileBuildChecks::Verify(architecture.map(parse_architecture).transpose()?)
    } else {
        ToolProfileBuildChecks::Skip
    };
    let audit = profile_store(catalog)?.audit_tool_profiles(profile, checks)?;
    let reply = Reply::from_result(Ok(serde_json::to_value(audit)?));
    serde_json::to_writer(&mut *output, &reply)?;
    writeln!(output)?;
    Ok(())
}

pub fn run(args: &[String]) -> Result<Value> {
    match args {
        [flag, file] if flag == "--example" => {
            write_example(Path::new(file), ToolBundlePreset::Essentials)
        }
        [example, file, flag, raw] if example == "--example" => {
            let preset = match (flag.as_str(), raw.as_str()) {
                ("--preset", "essentials") => ToolBundlePreset::Essentials,
                ("--preset", "admin") => ToolBundlePreset::Admin,
                ("--preset", _) => bail!("--preset requires essentials or admin"),
                ("--tools", names) => ToolBundlePreset::Custom(
                    names
                        .split(',')
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned)
                        .collect(),
                ),
                _ => bail!("{HELP}"),
            };
            write_example(Path::new(file), preset)
        }
        [file] if !file.starts_with("--") => fetch(file, None),
        [file, flag, raw] if !file.starts_with("--") && flag == "--architecture" => {
            fetch(file, Some(parse_architecture(raw)?))
        }
        _ => bail!("{HELP}"),
    }
}

fn parse_architecture(value: &str) -> Result<UtilityArchitecture> {
    match value {
        "x86_64" => Ok(UtilityArchitecture::X86_64),
        "aarch64" => Ok(UtilityArchitecture::Aarch64),
        _ => bail!("--architecture requires x86_64 or aarch64"),
    }
}

fn fetch(file: &str, architecture: Option<UtilityArchitecture>) -> Result<Value> {
    let bundle =
        UtilityBundle::load(Path::new(file)).context("Reading the tool bundle manifest")?;
    fetch_tool_bundle(&bundle, architecture)
}

fn write_example(path: &Path, preset: ToolBundlePreset) -> Result<Value> {
    Ok(json!({"manifest": export_builtin_tool_bundle(path, &preset)?}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_writes_a_valid_manifest_without_overwriting() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Starter Tools.json");
        write_example(&path, ToolBundlePreset::Essentials).unwrap();
        assert!(write_example(&path, ToolBundlePreset::Admin).is_err());
        let bundle = UtilityBundle::load(&path).unwrap();
        assert_eq!(
            bundle
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["rg", "jq", "dust", "hyperfine"]
        );
        assert!(bundle.tools.iter().all(|tool| tool.builds.len() == 2));
        assert!(
            run(&[
                "tools.json".into(),
                "--architecture".into(),
                "powerpc".into()
            ])
            .is_err()
        );
    }

    #[test]
    fn admin_export_extends_the_same_managed_bundle() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Admin tools.json");
        run(&[
            "--example".into(),
            path.to_str().unwrap().into(),
            "--preset".into(),
            "admin".into(),
        ])
        .unwrap();
        let bundle = UtilityBundle::load(&path).unwrap();
        assert_eq!(bundle.name, "linux-essentials");
        assert_eq!(bundle.bin_directory, ".local/bin");
        assert_eq!(
            bundle
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            [
                "rg",
                "jq",
                "dust",
                "hyperfine",
                "bandwhich",
                "diskwatch",
                "mdd",
                "ncdu",
                "netwatch"
            ]
        );
        assert!(bundle.tools.iter().all(|tool| tool.builds.len() == 2));
    }

    #[test]
    fn custom_export_keeps_only_selected_tools_in_catalog_order() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Selected tools.json");
        let args = |names: &str| {
            [
                "--example".into(),
                path.to_str().unwrap().into(),
                "--tools".into(),
                names.into(),
            ]
        };
        assert!(run(&args("")).is_err());
        assert!(run(&args("rg,unknown")).is_err());
        assert!(!path.exists());
        run(&args("mdd, rg,jq,rg")).unwrap();
        let bundle = UtilityBundle::load(&path).unwrap();
        assert_eq!(bundle.name, "linux-essentials");
        assert_eq!(bundle.bin_directory, ".local/bin");
        assert_eq!(
            bundle
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["rg", "jq", "mdd"]
        );
        assert!(bundle.tools.iter().all(|tool| tool.builds.len() == 2));
    }
}
