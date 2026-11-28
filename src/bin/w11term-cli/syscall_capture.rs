use super::offline;
use anyhow::{Context, Result, bail, ensure};
use std::{collections::BTreeMap, io::Write, path, sync::Arc};
use w11term::{
    control::{Request, SyscallFilter, SyscallOptions},
    syscall_trace::{Comparison, SummarySort, Trace},
};

pub(super) const HELP: &str = concat!(
    "w11term-cli [--pid PID] preview-syscall-capture --pane ID --program PROGRAM [TRACE OPTIONS]\n",
    "w11term-cli [--pid PID] capture-syscalls --pane ID --program PROGRAM [TRACE OPTIONS] ",
    "--connection-generation N --review-id ID --job-id HEX --output-file PATH\n",
    "w11term-cli [--pid PID] stop-syscall-capture --pane ID --connection-generation N --job-id HEX\n",
    "TRACE OPTIONS: [--arg ARG ...] [--directory PATH] [--seconds N] [--max-mib N] ",
    "[--string-size N] [--filter all|files|processes|network]",
);

pub(super) const INSPECT_VERB: &str = "inspect-syscall-trace";
pub(super) const INSPECT_HELP: &str = concat!(
    "w11term-cli inspect-syscall-trace --trace PATH [--trace PATH ...] ",
    "[--pid PID] [--sort total|slowest|calls|errors] [--output-file PATH]",
);

pub(super) fn inspect_files(args: &[String], stdout: &mut dyn Write) -> Result<()> {
    let mut args = args.iter();
    let mut traces = Vec::new();
    let mut pid = None;
    let mut sort = None;
    let mut output_file = None;
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .with_context(|| format!("{flag} requires a value"))
        };
        match flag.as_str() {
            "--trace" => traces.push(path::absolute(value()?)?),
            "--pid" => {
                ensure!(pid.is_none(), "specify {flag} only once");
                pid = Some(
                    value()?
                        .parse::<u32>()
                        .ok()
                        .filter(|&pid| pid > 0)
                        .context("--pid requires a positive process ID")?,
                );
            }
            "--sort" => {
                ensure!(sort.is_none(), "specify {flag} only once");
                let choice = value()?;
                sort = Some(
                    SummarySort::CHOICES
                        .iter()
                        .find_map(|(_, key, sort)| (*key == choice).then_some(*sort))
                        .context("--sort requires total, slowest, calls or errors")?,
                );
            }
            "--output-file" => {
                ensure!(output_file.is_none(), "specify {flag} only once");
                output_file = Some(value()?.as_str());
            }
            _ => bail!("unknown option {flag} for {INSPECT_VERB}"),
        }
    }
    ensure!(!traces.is_empty(), "provide at least one --trace file");
    let trace = Trace::open(traces).context("Reading the syscall trace")?;
    let summary = trace.summarize(pid);
    offline::write_report(stdout, output_file, |dest| {
        summary.write_json(&trace, sort.unwrap_or_default(), dest)
    })
}

pub(super) const COMPARE_VERB: &str = "compare-syscall-traces";
pub(super) const COMPARE_HELP: &str = concat!(
    "w11term-cli compare-syscall-traces --working PATH [--working PATH ...] ",
    "--failing PATH [--failing PATH ...] [--filter TEXT] [--include-timing] ",
    "[--anchor WORKING_FILE:LINE,FAILING_FILE:LINE] [--output-file PATH]",
);

pub(super) fn compare_files(args: &[String], stdout: &mut dyn Write) -> Result<()> {
    let mut args = args.iter();
    let mut working = Vec::new();
    let mut failing = Vec::new();
    let mut filter = None;
    let mut output_file = None;
    let mut include_timing = false;
    let mut anchor = None;
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .with_context(|| format!("{flag} requires a value"))
        };
        match flag.as_str() {
            "--working" => working.push(path::absolute(value()?)?),
            "--failing" => failing.push(path::absolute(value()?)?),
            "--include-timing" => {
                ensure!(!include_timing, "specify {flag} only once");
                include_timing = true;
            }
            "--anchor" => {
                ensure!(anchor.is_none(), "specify {flag} only once");
                anchor = Some(parse_anchor(value()?)?);
            }
            "--filter" | "--output-file" => {
                let target = if flag == "--filter" {
                    &mut filter
                } else {
                    &mut output_file
                };
                ensure!(target.is_none(), "specify {flag} only once");
                *target = Some(value()?.as_str());
            }
            _ => bail!("unknown option {flag} for {COMPARE_VERB}"),
        }
    }
    ensure!(
        !working.is_empty(),
        "provide at least one --working trace file"
    );
    ensure!(
        !failing.is_empty(),
        "provide at least one --failing trace file"
    );
    let working = Arc::new(Trace::open(working).context("Reading the working trace")?);
    let failing = Arc::new(Trace::open(failing).context("Reading the failing trace")?);
    let comparison = Comparison::build(working, failing);
    let anchor = anchor
        .map(|sources| {
            comparison.timing_anchor_for_sources(sources).context(
                "--anchor must identify a reliably aligned call with a usable time in each trace",
            )
        })
        .transpose()?;
    offline::write_report(stdout, output_file, |dest| {
        comparison.write_json(filter.unwrap_or_default(), include_timing, anchor, dest)
    })
}

fn parse_anchor(value: &str) -> Result<[(usize, u32); 2]> {
    fn source(value: &str) -> Option<(usize, u32)> {
        let (file, line) = value.split_once(':')?;
        let file = file.parse().ok()?;
        let line = line.parse().ok()?;
        (file > 0 && line > 0).then_some((file, line))
    }

    let pair = value
        .split_once(',')
        .and_then(|(working, failing)| Some([source(working)?, source(failing)?]));
    pair.context("--anchor requires two one-based file and line references in WORKING_FILE:LINE,FAILING_FILE:LINE form")
}

pub(super) fn parse(
    verb: &str,
    mut args: impl Iterator<Item = String>,
    pane: Option<u64>,
) -> Result<Request> {
    let stop = verb == "stop-syscall-capture";
    let preview = verb == "preview-syscall-capture";
    let mut flags = BTreeMap::new();
    let mut arguments = Vec::new();
    while let Some(flag) = args.next() {
        ensure!(
            flag == "--pane"
                || !preview && matches!(flag.as_str(), "--connection-generation" | "--job-id")
                || !preview && !stop && matches!(flag.as_str(), "--output-file" | "--review-id")
                || !stop
                    && matches!(
                        flag.as_str(),
                        "--program"
                            | "--arg"
                            | "--directory"
                            | "--seconds"
                            | "--max-mib"
                            | "--string-size"
                            | "--filter"
                    ),
            "Unsupported option {flag} for {verb}"
        );
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
    let explicit = flags
        .remove("--pane")
        .map(|value| value.parse())
        .transpose()
        .context("--pane requires an integer")?;
    ensure!(
        pane.is_none() || explicit.is_none(),
        "Specify --pane only once"
    );
    let pane = pane
        .or(explicit)
        .filter(|&value| value > 0)
        .context("Provide a positive --pane ID")?;
    if stop {
        return Ok(Request::StopSyscallCapture {
            pane,
            connection_generation: flags
                .remove("--connection-generation")
                .context("Provide --connection-generation")?
                .parse()?,
            job_id: flags.remove("--job-id").context("Provide --job-id")?,
        });
    }
    let options = SyscallOptions {
        program: flags.remove("--program").context("Provide --program")?,
        arguments,
        directory: flags.remove("--directory").unwrap_or_default(),
        seconds: flags
            .remove("--seconds")
            .map(|value| value.parse())
            .transpose()
            .context("Invalid trace duration")?
            .unwrap_or(SyscallOptions::DEFAULT_SECONDS),
        max_mib: flags
            .remove("--max-mib")
            .map(|value| value.parse())
            .transpose()
            .context("Invalid trace budget")?
            .unwrap_or(SyscallOptions::DEFAULT_MAX_MIB),
        string_size: flags
            .remove("--string-size")
            .map(|value| value.parse())
            .transpose()
            .context("Invalid trace string size")?
            .unwrap_or(SyscallOptions::DEFAULT_STRING_SIZE),
        filter: match flags.remove("--filter").as_deref() {
            None | Some("all") => SyscallFilter::All,
            Some("files") => SyscallFilter::Files,
            Some("processes") => SyscallFilter::Processes,
            Some("network") => SyscallFilter::Network,
            Some(_) => bail!("Choose --filter all, files, processes or network"),
        },
    }
    .validate()?;
    if preview {
        return Ok(Request::PreviewSyscallCapture { pane, options });
    }
    Ok(Request::CaptureSyscalls {
        pane,
        options,
        connection_generation: flags
            .remove("--connection-generation")
            .context("Provide --connection-generation from the review")?
            .parse()?,
        review_id: flags
            .remove("--review-id")
            .context("Provide --review-id from the review")?,
        job_id: flags
            .remove("--job-id")
            .context("Provide a unique --job-id")?,
        output_file: path::absolute(
            flags
                .remove("--output-file")
                .context("Provide --output-file")?,
        )?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_program_arguments_remain_literal() {
        let request = parse(
            "preview-syscall-capture",
            [
                "--program",
                "/bin/cat",
                "--arg",
                "--",
                "--arg",
                "a 'file'\n$",
                "--filter",
                "files",
            ]
            .into_iter()
            .map(str::to_owned),
            Some(7),
        )
        .unwrap();
        let Request::PreviewSyscallCapture { pane: 7, options } = request else {
            panic!()
        };
        assert_eq!(options.arguments, ["--", "a 'file'\n$"]);
        assert!(matches!(options.filter, SyscallFilter::Files));
    }
}
