use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::Path;
use w11term::sftp::{
    ToolBundlePreset, UtilityArchitecture, UtilityBundle, export_builtin_tool_bundle,
    fetch_tool_bundle,
};

pub const HELP: &str = "w11term-cli fetch-tool-bundle BUNDLE.json [--architecture x86_64|aarch64]\n\
w11term-cli fetch-tool-bundle --example NEW_BUNDLE.json [--preset essentials|admin | --tools rg,jq,mdd]";

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
            let architecture = match raw.as_str() {
                "x86_64" => UtilityArchitecture::X86_64,
                "aarch64" => UtilityArchitecture::Aarch64,
                _ => bail!("--architecture requires x86_64 or aarch64"),
            };
            fetch(file, Some(architecture))
        }
        _ => bail!("{HELP}"),
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
