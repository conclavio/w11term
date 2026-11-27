use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::Path;
use w11term::sftp::{
    UtilityArchitecture, UtilityBundle, export_starter_tool_bundle, fetch_tool_bundle,
};

pub const HELP: &str = "w11term-cli fetch-tool-bundle BUNDLE.json [--architecture x86_64|aarch64]\n\
w11term-cli fetch-tool-bundle --example NEW_BUNDLE.json";

pub fn run(args: &[String]) -> Result<Value> {
    match args {
        [flag, file] if flag == "--example" => write_example(Path::new(file)),
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

fn write_example(path: &Path) -> Result<Value> {
    Ok(json!({"manifest": export_starter_tool_bundle(path)?}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_writes_a_valid_manifest_without_overwriting() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Starter Tools.json");
        write_example(&path).unwrap();
        assert!(write_example(&path).is_err());
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
}
