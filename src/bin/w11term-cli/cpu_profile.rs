use super::offline;
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{self, PathBuf},
    sync::Arc,
};
use w11term::{
    control::{CpuCallGraph, CpuProfileOptions, Request},
    cpu_profile::{Comparison, Profile},
};

pub(super) const HELP: &str = concat!(
    "w11term-cli [--pid PID] preview-cpu-profile --pane ID --target-pid PID ",
    "[--seconds N] [--frequency HZ] [--call-graph fp|dwarf]\n",
    "w11term-cli [--pid PID] capture-cpu-profile --pane ID --target-pid PID ",
    "[--seconds N] [--frequency HZ] [--call-graph fp|dwarf] ",
    "--connection-generation N --process-start-ticks N --output-file PATH [--perf-data PATH]",
);

pub(super) const COMPARE_VERB: &str = "compare-cpu-profiles";
pub(super) const COMPARE_HELP: &str =
    "w11term-cli compare-cpu-profiles BEFORE AFTER [--filter TEXT] [--output-file PATH]";

pub(super) fn compare_files(args: &[String], stdout: &mut dyn Write) -> Result<()> {
    let mut args = args.iter();
    let before = args.next().context("provide the BEFORE folded profile")?;
    let after = args.next().context("provide the AFTER folded profile")?;
    let mut filter = None;
    let mut output_file = None;
    while let Some(flag) = args.next() {
        let target = match flag.as_str() {
            "--filter" => &mut filter,
            "--output-file" => &mut output_file,
            _ => bail!("unknown option {flag} for {COMPARE_VERB}"),
        };
        ensure!(target.is_none(), "specify {flag} only once");
        *target = Some(
            args.next()
                .with_context(|| format!("{flag} requires a value"))?,
        );
    }
    let paths = [PathBuf::from(before), PathBuf::from(after)];
    let open = |side: usize, name: &str| {
        Profile::open(&paths[side])
            .with_context(|| format!("Reading the {name} CPU profile {}", paths[side].display()))
            .map(Arc::new)
    };
    let profiles = [open(0, "before")?, open(1, "after")?];
    let comparison = Arc::new(Comparison::new(profiles, paths));
    let report = comparison.filter(filter.map_or("", String::as_str));
    offline::write_report(stdout, output_file.map(String::as_str), |dest| {
        report.write_json(dest)
    })
}

pub(super) fn parse(
    verb: &str,
    mut args: impl Iterator<Item = String>,
    pane: Option<u64>,
) -> Result<Request> {
    let mut flags = BTreeMap::new();
    while let Some(flag) = args.next() {
        let allowed = matches!(
            flag.as_str(),
            "--pane" | "--target-pid" | "--seconds" | "--frequency" | "--call-graph"
        ) || (verb == "capture-cpu-profile"
            && matches!(
                flag.as_str(),
                "--connection-generation"
                    | "--process-start-ticks"
                    | "--output-file"
                    | "--perf-data"
            ));
        ensure!(allowed, "unknown option {flag} for {verb}");
        let value = args
            .next()
            .with_context(|| format!("{flag} requires a value"))?;
        ensure!(
            flags.insert(flag.clone(), value).is_none(),
            "specify {flag} only once"
        );
    }
    let pane_arg = flags
        .remove("--pane")
        .map(|arg| arg.parse())
        .transpose()
        .context("--pane requires an integer")?;
    ensure!(
        pane.is_none() || pane_arg.is_none(),
        "specify --pane only once"
    );
    let pane = pane
        .or(pane_arg)
        .filter(|pane| *pane > 0)
        .context("provide a positive --pane ID")?;
    let options = CpuProfileOptions {
        target_pid: flags
            .remove("--target-pid")
            .context("provide --target-pid from the remote Linux host")?
            .parse()
            .context("--target-pid requires a positive Linux PID")?,
        seconds: flags
            .remove("--seconds")
            .map(|arg| arg.parse())
            .transpose()
            .context("--seconds requires an integer from 1 to 30")?
            .unwrap_or(CpuProfileOptions::DEFAULT_SECONDS),
        frequency: flags
            .remove("--frequency")
            .map(|arg| arg.parse())
            .transpose()
            .context("--frequency requires an integer from 1 to 199")?
            .unwrap_or(CpuProfileOptions::DEFAULT_FREQUENCY),
        call_graph: match flags.remove("--call-graph").as_deref() {
            None | Some("fp") => CpuCallGraph::Fp,
            Some("dwarf") => CpuCallGraph::Dwarf,
            Some(_) => bail!("--call-graph requires fp or dwarf"),
        },
    }
    .validate()?;
    if verb == "preview-cpu-profile" {
        return Ok(Request::PreviewCpuProfile { pane, options });
    }
    Ok(Request::CaptureCpuProfile {
        pane,
        options,
        connection_generation: flags
            .remove("--connection-generation")
            .context("provide --connection-generation from preview-cpu-profile")?
            .parse()
            .context("--connection-generation requires a positive integer")?,
        process_start_ticks: flags
            .remove("--process-start-ticks")
            .context("provide --process-start-ticks from preview-cpu-profile")?
            .parse()
            .context("--process-start-ticks requires the Linux process start time in ticks")?,
        output_file: path::absolute(
            flags
                .remove("--output-file")
                .context("provide --output-file for the folded CPU profile")?,
        )?,
        perf_data_file: flags
            .remove("--perf-data")
            .map(path::absolute)
            .transpose()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invoke(verb: &str, args: &[&str]) -> Result<Request> {
        parse(verb, args.iter().map(|arg| arg.to_string()), Some(7))
    }

    #[test]
    fn perf_data_is_opt_in_and_the_capture_path_is_absolute() {
        let args = [
            "--target-pid",
            "54",
            "--connection-generation",
            "9",
            "--process-start-ticks",
            "800",
            "--output-file",
            "stacks.folded",
        ];
        let original = invoke("capture-cpu-profile", &args).unwrap();
        let json = serde_json::to_value(original).unwrap();
        assert!(json.get("perf_data_file").is_none());
        let raw = [args.as_slice(), &["--perf-data", "raw.perf.data"]].concat();
        assert!(matches!(invoke("capture-cpu-profile", &raw).unwrap(),
            Request::CaptureCpuProfile { perf_data_file: Some(path), .. } if path.is_absolute()));
        assert!(
            invoke(
                "preview-cpu-profile",
                &["--target-pid", "54", "--perf-data", "raw.perf.data"]
            )
            .is_err()
        );
    }

    #[test]
    fn reviews_only_supported_settings() {
        let request = invoke(
            "preview-cpu-profile",
            &["--target-pid", "54", "--call-graph", "dwarf"],
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "verb": "preview-cpu-profile",
                "pane": 7,
                "options": {"target_pid": 54, "seconds": 10, "frequency": 99, "call_graph": "dwarf"}
            })
        );
        for (flag, value) in [
            ("--target-pid", "2147483648"),
            ("--target-pid", "0"),
            ("--seconds", "31"),
            ("--frequency", "200"),
            ("--output-file", "stacks.folded"),
        ] {
            let mut args = vec![flag, value];
            if flag != "--target-pid" {
                args.extend(["--target-pid", "54"]);
            }
            assert!(
                invoke("preview-cpu-profile", &args).is_err(),
                "{flag} {value}"
            );
        }
    }

    #[test]
    fn capture_requires_reviewed_identity_and_absolute_destination() {
        let args = [
            "--target-pid",
            "54",
            "--connection-generation",
            "9",
            "--process-start-ticks",
            "800",
            "--output-file",
            "stacks.folded",
        ];
        assert!(matches!(
            invoke("capture-cpu-profile", &args).unwrap(),
            Request::CaptureCpuProfile { pane: 7, process_start_ticks: 800, output_file, .. }
                if output_file.is_absolute()
        ));
        assert!(invoke("capture-cpu-profile", &args[..6]).is_err());
        assert!(invoke("capture-cpu-profile", &[&args[..4], &args[6..]].concat()).is_err());
        assert!(invoke("capture-cpu-profile", &[&args[..2], &args[4..]].concat()).is_err());
    }
}
