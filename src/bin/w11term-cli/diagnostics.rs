use anyhow::{Context, Result, bail, ensure};
use std::{collections::BTreeMap, path};
use w11term::{
    control::Request,
    diagnostic_history::{Artifact, Kind},
};

pub(super) const HELP: &str = concat!(
    "w11term-cli [--pid PID] open-diagnostic --window ID ",
    "--kind cpu-profile|triage-report|syscall-trace|packet-capture ",
    "--file PATH [--file PATH ...] [--compare-with PATH ...]",
);

pub(super) struct CaptureFlags {
    pub flags: BTreeMap<String, String>,
    pub arguments: Vec<String>,
}

pub(super) fn read_capture_flags(
    verb: &str,
    mut args: impl Iterator<Item = String>,
    accepts: impl Fn(&str) -> bool,
) -> Result<CaptureFlags> {
    let mut flags = BTreeMap::new();
    let mut arguments = Vec::new();
    while let Some(flag) = args.next() {
        ensure!(accepts(&flag), "Unsupported option {flag} for {verb}");
        let value = args
            .next()
            .with_context(|| format!("{flag} requires a value"))?;
        if flag == "--arg" {
            arguments.push(value);
        } else {
            ensure!(
                flags.insert(flag.clone(), value).is_none(),
                "Specify {flag} only once"
            );
        }
    }
    Ok(CaptureFlags { flags, arguments })
}

pub(super) fn parse(mut args: impl Iterator<Item = String>) -> Result<Request> {
    let mut window = None;
    let mut kind = None;
    let mut paths = Vec::new();
    let mut compare_with = Vec::new();
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .with_context(|| format!("{flag} requires a value"))
        };
        match flag.as_str() {
            "--window" => {
                ensure!(window.is_none(), "Specify --window only once");
                let id: u64 = value()?.parse().context("Provide a positive --window ID")?;
                ensure!(id > 0, "Provide a positive --window ID");
                window = Some(id);
            }
            "--kind" => {
                ensure!(kind.is_none(), "Specify --kind only once");
                kind = Some(match value()?.as_str() {
                    "cpu-profile" => Kind::CpuProfile,
                    "triage-report" => Kind::TriageReport,
                    "syscall-trace" => Kind::SyscallTrace,
                    "packet-capture" => Kind::PacketCapture,
                    _ => bail!(
                        "Choose --kind cpu-profile, triage-report, syscall-trace or packet-capture"
                    ),
                });
            }
            "--file" => paths.push(path::absolute(value()?)?),
            "--compare-with" => compare_with.push(path::absolute(value()?)?),
            _ => bail!("Unknown open-diagnostic option {flag}"),
        }
    }
    ensure!(!paths.is_empty(), "Provide at least one --file PATH");
    Ok(Request::OpenDiagnostic {
        window_id: window.context("Provide --window ID from list-windows")?,
        artifact: Artifact {
            kind: kind.context("Provide --kind")?,
            paths,
        },
        compare_with,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_split_trace_paths_and_targets_a_specific_window() {
        let invocation = super::super::parse_invocation(
            [
                "--pid",
                "4",
                "open-diagnostic",
                "--kind",
                "syscall-trace",
                "--window",
                "7",
                "--file",
                "on host.12",
                "--file",
                "on host.13",
                "--compare-with",
                "after.15",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .unwrap();
        let Request::OpenDiagnostic {
            window_id,
            artifact,
            compare_with,
        } = invocation.request
        else {
            panic!("Expected an open-diagnostic request");
        };
        assert_eq!(invocation.pid, Some(4));
        assert_eq!(window_id, 7);
        assert_eq!(artifact.kind, Kind::SyscallTrace);
        assert_eq!(
            artifact.paths,
            ["on host.12", "on host.13"].map(|name| path::absolute(name).unwrap())
        );
        assert_eq!(compare_with, [path::absolute("after.15").unwrap()]);
    }
}
