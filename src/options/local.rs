use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalOptions {
    pub executable: PathBuf,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub powershell_integration: bool,
}

impl LocalOptions {
    /// Only append a startup file when all existing arguments have an
    /// unambiguous, interactive meaning in both PowerShell 5.1 and 7.
    pub fn supports_powershell_integration(executable: &Path, args: &[String]) -> bool {
        let Some(name) = executable.file_name() else {
            return false;
        };
        let full_name =
            name.eq_ignore_ascii_case("powershell.exe") || name.eq_ignore_ascii_case("pwsh.exe");
        // CreateProcessW adds .exe to a bare command name, but not to a path.
        let bare_name = name == executable.as_os_str()
            && (name.eq_ignore_ascii_case("powershell") || name.eq_ignore_ascii_case("pwsh"));
        (full_name || bare_name)
            && args.iter().all(|arg| {
                ["-NoProfile", "-NoLogo", "-NoExit", "-STA", "-MTA"]
                    .iter()
                    .any(|flag| arg.eq_ignore_ascii_case(flag))
            })
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.executable.as_os_str().is_empty(),
            "Choose an executable for this local profile"
        );
        ensure!(
            !self.executable.to_string_lossy().contains('\0')
                && self.args.iter().all(|v| !v.contains('\0')),
            "Executable and arguments cannot contain a NUL character"
        );
        if let Some(cwd) = &self.cwd {
            ensure!(
                !cwd.as_os_str().is_empty() && !cwd.to_string_lossy().contains('\0'),
                "Invalid local working directory"
            );
        }
        Ok(())
    }

    pub fn resolve_paths(&mut self, profiles_path: &Path) -> Result<()> {
        self.validate()?;
        let config = std::path::absolute(profiles_path)?;
        let dir = config.parent().unwrap_or(Path::new("."));
        if self.executable.is_relative() && !self.is_command_name() {
            self.executable = dir.join(&self.executable);
        }
        if let Some(cwd) = &mut self.cwd
            && cwd.is_relative()
        {
            *cwd = dir.join(&*cwd);
        }
        self.validate_resolved_paths()
    }

    pub(crate) fn is_command_name(&self) -> bool {
        self.executable.file_name() == Some(self.executable.as_os_str())
    }

    pub(crate) fn validate_resolved_paths(&self) -> Result<()> {
        ensure!(
            self.executable.is_absolute() || self.is_command_name(),
            "Local executable must be a command name or an absolute path"
        );
        ensure!(
            self.cwd.as_ref().is_none_or(|cwd| cwd.is_absolute()),
            "Expected an absolute local working directory"
        );
        Ok(())
    }

    pub fn installed() -> BTreeMap<String, Self> {
        let mut shells = BTreeMap::new();
        let mut add = |label: &str, path: PathBuf| {
            if path.is_absolute() && path.is_file() {
                shells.entry(label.into()).or_insert(Self {
                    executable: path,
                    args: vec![],
                    cwd: None,
                    powershell_integration: false,
                });
            }
        };
        if let Some(windows) = std::env::var_os("SystemRoot") {
            add(
                "Windows PowerShell",
                PathBuf::from(windows).join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
            );
        }
        if let Some(programs) = std::env::var_os("ProgramFiles") {
            add(
                "PowerShell 7",
                PathBuf::from(programs).join(r"PowerShell\7\pwsh.exe"),
            );
        }
        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path).filter(|dir| dir.is_absolute()) {
                add("PowerShell 7", dir.join("pwsh.exe"));
            }
        }
        shells
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        options::{Options, Source},
        profiles::ProfileStore,
    };

    #[test]
    fn powershell_integration_requires_an_unambiguous_interactive_invocation() {
        let supports = |exe: &str, args: &[&str]| {
            LocalOptions::supports_powershell_integration(
                Path::new(exe),
                &args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>(),
            )
        };
        assert!(supports(r"C:\Program Files\PowerShell\7\PWSH.exe", &[]));
        assert!(supports(
            "POWERSHELL.EXE",
            &["-nOpROFILE", "-NoLOGO", "-noexit"]
        ));
        assert!(supports("pwsh.exe", &["-MTA"]));
        assert!(supports("pWsH", &["-NoProfile"]));
        assert!(supports("PowerShell", &[]));
        for exe in [
            "cmd.exe",
            "powershell.cmd",
            "pwsh.exe.cmd",
            "other-pwsh.exe",
            r".\pwsh",
            r"C:\shells\powershell",
            "pwsh.",
        ] {
            assert!(!supports(exe, &[]), "{exe}");
        }
        for args in [
            vec!["-NoExit", "-CoMManD", "Get-Date"],
            vec!["-File", "build.ps1", "-NoLogo"],
            vec!["-EncodedCommand", "AAAA"],
            vec!["-c", "-"],
            vec!["-NonInteractive"],
            vec!["tool.ps1"],
            vec!["-"],
            vec!["-SSHServerMode"],
            vec!["-OutputFormat", "XML"],
            vec!["-Help"],
            vec!["--"],
            vec![""],
        ] {
            assert!(!supports("pwsh.exe", &args), "{args:?}");
        }
    }

    #[test]
    fn powershell_integration_is_opt_in_for_saved_and_discovered_shells() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("profiles.json");
        std::fs::write(
            &path,
            r#"{"profiles":{"old":{"backend":"local","executable":"pwsh.exe","untouched":17}}}"#,
        )
        .unwrap();
        let store = ProfileStore::new(path.clone()).unwrap();
        let installed = LocalOptions::installed();
        assert!(
            installed
                .values()
                .all(|shell| !shell.powershell_integration)
        );
        for name in std::iter::once("old").chain(installed.keys().map(String::as_str)) {
            let original = store.inspect_local(name).unwrap();
            assert!(!original.options.powershell_integration);
            let mut edited = original.options.clone();
            edited.powershell_integration = true;
            store.save_local(Some(&original), name, &edited).unwrap();
            let enabled = store.inspect_local(name).unwrap();
            assert_eq!(enabled.options, edited);
            edited.powershell_integration = false;
            store.save_local(Some(&enabled), name, &edited).unwrap();
            assert_eq!(store.inspect_local(name).unwrap().options, original.options);
        }
        let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(doc["profiles"]["old"]["untouched"], 17);
        assert!(
            doc["profiles"]["old"]
                .get("powershell_integration")
                .is_none()
        );
    }
    #[test]
    fn local_profile_round_trip_preserves_arguments_and_refuses_stale_or_cross_backend_edits() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("profiles.json");
        std::fs::write(
            &path,
            r#"{"profiles":{"ssh":{"host":"remote","user":"admin","auth":"agent"}}}"#,
        )
        .unwrap();
        let store = ProfileStore::new(path.clone()).unwrap();
        let mut shell = LocalOptions {
            executable: r"bin\tool.exe".into(),
            args: vec!["".into(), "windows path with spaces".into()],
            cwd: Some("working".into()),
            powershell_integration: false,
        };
        assert!(store.save_local(None, "ssh", &shell).is_err());
        store.save_local(None, "build", &shell).unwrap();
        let launch = Options::parse_from(
            ["--profiles", path.to_str().unwrap(), "--profile", "build"].map(str::to_owned),
        )
        .unwrap()
        .unwrap();
        let Source::Local(selected) = launch.source else {
            panic!("expected local app");
        };
        assert_eq!(selected.args, shell.args);
        assert_eq!(selected.cwd, Some(temp.path().join("working")));
        assert_eq!(selected.executable, temp.path().join(r"bin\tool.exe"));
        let snapshot = store.inspect_local("build").unwrap();
        shell.args.push("-changed".into());
        store.save_local(Some(&snapshot), "build", &shell).unwrap();
        assert!(
            store
                .save_local(Some(&snapshot), "renamed", &shell)
                .is_err()
        );
        assert!(matches!(
            store.catalog().unwrap().source("ssh"),
            Some(Source::Ssh(_))
        ));
    }

    #[test]
    fn editing_local_paths_preserves_raw_metadata_and_rejects_stale_ssh_writes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("profiles.json");
        let mut doc = serde_json::json!({"theme":true,"profiles":{
            "build":{"backend":"local","executable":"bin/shell.exe","cwd":"project","extra":{"value":1}},
            "remote":{"host":"localhost","user":"pilot","auth":"none"}
        }});
        std::fs::write(&path, doc.to_string()).unwrap();
        let store = ProfileStore::new(path.clone()).unwrap();
        let local = store.inspect_local("build").unwrap();
        let mut edited = local.options.clone();
        edited.args.push("two words".into());
        store.save_local(Some(&local), "new", &edited).unwrap();
        doc = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(doc["profiles"]["new"]["cwd"], "project");
        assert_eq!(doc["profiles"]["new"]["executable"], "bin/shell.exe");
        assert_eq!(doc["profiles"]["new"]["extra"]["value"], 1);
        let current = store.inspect_local("new").unwrap();
        doc["profiles"]["new"]["extra"]["value"] = 2.into();
        std::fs::write(&path, doc.to_string()).unwrap();
        assert!(store.save_local(Some(&current), "new", &edited).is_err());
        let ssh = store.load().unwrap().remove("remote").unwrap();
        let unchanged = std::fs::read(&path).unwrap();
        assert!(store.update("new", &ssh).is_err());
        if let Some(name) = LocalOptions::installed().keys().next() {
            assert!(store.add(name, &ssh).is_err());
            assert!(store.save_local(None, name, &edited).is_err());
        }
        assert_eq!(std::fs::read(path).unwrap(), unchanged);
    }
}
