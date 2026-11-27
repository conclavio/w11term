use super::offline;
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeMap,
    io::Write,
    ops::Range,
    path::{self, Path, PathBuf},
};
use w11term::control::{PacketCaptureOptions, Reply, Request};
use w11term::ssh::{
    LocalPacketCapture, PacketCaptureComparison, PacketCaptureExcerptPlan,
    open_capture_in_wireshark,
};

pub(super) const HELP: &str = concat!(
    "w11term-cli [--pid PID] list-packet-interfaces --pane ID\n",
    "w11term-cli [--pid PID] preview-packet-capture --pane ID --interface NAME [CAPTURE OPTIONS]\n",
    "w11term-cli [--pid PID] capture-packets --pane ID --interface NAME [CAPTURE OPTIONS] ",
    "--connection-generation N --review-id ID --output-file PATH [--job-id HEX] [--wireshark PATH]\n",
    "w11term-cli [--pid PID] stop-packet-capture --pane ID --connection-generation N --job-id HEX\n",
    "CAPTURE OPTIONS: [--filter EXPR] [--snap-length BYTES] [--seconds N] [--max-mib N] ",
    "[--packet-limit N] [--rotate-files 2..16] [--scratch-directory PATH] [--tmpfs]",
);

pub(super) const INSPECT_VERB: &str = "inspect-packet-capture";
pub(super) const INSPECT_HELP: &str =
    "w11term-cli inspect-packet-capture FILE [--output-file PATH]";
pub(super) const COMPARE_VERB: &str = "compare-packet-captures";
pub(super) const COMPARE_HELP: &str = concat!(
    "w11term-cli compare-packet-captures BEFORE AFTER [--elapsed-ns START:END] ",
    "[--format json|text] [--output-file PATH]",
);
pub(super) const CLIP_VERB: &str = "clip-packet-capture";
pub(super) const CLIP_HELP: &str =
    "w11term-cli clip-packet-capture FILE --elapsed-ns START:END --output-file PATH";

pub(super) fn inspect_file(args: &[String], stdout: &mut dyn Write) -> Result<()> {
    let mut args = args.iter();
    let path = args
        .next()
        .context("provide the local PCAP or PCAPNG file")?;
    let options = report_options(INSPECT_VERB, args)?;
    let capture = LocalPacketCapture::open(path::absolute(path)?)
        .with_context(|| format!("Inspecting packet capture {path}"))?;
    offline::write_report(stdout, options.output_file, |output| {
        capture.write_json(output)
    })
}

pub(super) fn compare_files(args: &[String], stdout: &mut dyn Write) -> Result<()> {
    let mut args = args.iter();
    let before = args.next().context("provide the before capture file")?;
    let after = args.next().context("provide the after capture file")?;
    let options = report_options(COMPARE_VERB, args)?;
    let text = match options.format {
        None | Some("json") => false,
        Some("text") => true,
        Some(_) => bail!("--format requires json or text"),
    };
    let paths = [path::absolute(before)?, path::absolute(after)?];
    let comparison = if let Some(elapsed) = options.elapsed_ns {
        PacketCaptureComparison::compare_elapsed(paths, parse_elapsed(elapsed)?)?
    } else {
        let [before_path, after_path] = paths;
        let before = LocalPacketCapture::open(before_path)
            .with_context(|| format!("Opening before capture {before}"))?;
        let after = LocalPacketCapture::open(after_path)
            .with_context(|| format!("Opening after capture {after}"))?;
        PacketCaptureComparison::new(&before, &after)
    };
    offline::write_report(stdout, options.output_file, |output| {
        if text {
            comparison.write_report(output)
        } else {
            comparison.write_json(output)
        }
    })
}

pub(super) fn clip_file(args: &[String], stdout: &mut dyn Write) -> Result<()> {
    let mut args = args.iter();
    let input = args.next().context("provide the source capture file")?;
    let options = report_options(CLIP_VERB, args)?;
    let bounds = parse_elapsed(
        options
            .elapsed_ns
            .context("provide --elapsed-ns START:END")?,
    )?;
    let output = options.output_file.context("provide --output-file PATH")?;
    let excerpt = PacketCaptureExcerptPlan::elapsed(path::absolute(input)?, bounds)?
        .save(path::absolute(output)?)?;
    let reply = Reply::from_result(Ok(serde_json::to_value(excerpt)?));
    serde_json::to_writer(&mut *stdout, &reply)?;
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}

fn parse_elapsed(value: &str) -> Result<Range<i128>> {
    let (start, end) = value
        .split_once(':')
        .context("--elapsed-ns requires START:END")?;
    Ok(start
        .parse()
        .context("Invalid elapsed start in nanoseconds")?
        ..end.parse().context("Invalid elapsed end in nanoseconds")?)
}

#[derive(Default)]
struct ReportOptions<'a> {
    format: Option<&'a str>,
    output_file: Option<&'a str>,
    elapsed_ns: Option<&'a str>,
}

fn report_options<'a>(
    verb: &str,
    mut args: impl Iterator<Item = &'a String>,
) -> Result<ReportOptions<'a>> {
    let mut options = ReportOptions::default();
    while let Some(flag) = args.next() {
        let target = match flag.as_str() {
            "--format" if verb == COMPARE_VERB => &mut options.format,
            "--elapsed-ns" if matches!(verb, COMPARE_VERB | CLIP_VERB) => &mut options.elapsed_ns,
            "--output-file" => &mut options.output_file,
            _ => bail!("unknown option {flag} for {verb}"),
        };
        ensure!(target.is_none(), "specify {flag} only once");
        *target = Some(
            args.next()
                .with_context(|| format!("{flag} requires a value"))?
                .as_str(),
        );
    }
    Ok(options)
}

pub(super) fn parse(
    verb: &str,
    mut args: impl Iterator<Item = String>,
    pane: Option<u64>,
) -> Result<(Request, Option<PathBuf>)> {
    let listing = verb == "list-packet-interfaces";
    let capture = verb == "capture-packets";
    let stop = verb == "stop-packet-capture";
    let mut flags = BTreeMap::new();
    let mut tmpfs = false;
    while let Some(flag) = args.next() {
        ensure!(
            !listing || flag == "--pane",
            "unsupported packet capture option {flag}"
        );
        if flag == "--tmpfs" {
            ensure!(!stop && !tmpfs, "invalid --tmpfs for {verb}");
            tmpfs = true;
            continue;
        }
        let accepted = flag == "--pane"
            || !stop
                && matches!(
                    flag.as_str(),
                    "--interface"
                        | "--filter"
                        | "--snap-length"
                        | "--seconds"
                        | "--max-mib"
                        | "--packet-limit"
                        | "--rotate-files"
                        | "--scratch-directory"
                )
            || (capture || stop) && matches!(flag.as_str(), "--connection-generation" | "--job-id")
            || capture
                && matches!(
                    flag.as_str(),
                    "--review-id" | "--output-file" | "--wireshark"
                );
        ensure!(accepted, "unsupported packet capture option {flag}");
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
        .map(|value| value.parse())
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
    if listing {
        return Ok((Request::ListPacketInterfaces { pane }, None));
    }
    if stop {
        return Ok((
            Request::StopPacketCapture {
                pane,
                connection_generation: flags
                    .remove("--connection-generation")
                    .context("provide --connection-generation from the reviewed plan")?
                    .parse()?,
                job_id: flags
                    .remove("--job-id")
                    .context("provide --job-id from the capture command")?,
            },
            None,
        ));
    }
    let options = PacketCaptureOptions {
        interface: flags
            .remove("--interface")
            .context("choose --interface NAME")?,
        filter: flags.remove("--filter").unwrap_or_default(),
        snap_length: flags
            .remove("--snap-length")
            .map(|value| value.parse())
            .transpose()
            .context("invalid snap length")?
            .unwrap_or(PacketCaptureOptions::DEFAULT_SNAP_LENGTH),
        seconds: flags
            .remove("--seconds")
            .map(|value| value.parse())
            .transpose()
            .context("invalid capture duration")?
            .unwrap_or(PacketCaptureOptions::DEFAULT_SECONDS),
        max_mib: flags
            .remove("--max-mib")
            .map(|value| value.parse())
            .transpose()
            .context("invalid capture budget")?
            .unwrap_or(PacketCaptureOptions::DEFAULT_MAX_MIB),
        packet_limit: flags
            .remove("--packet-limit")
            .map(|value| value.parse())
            .transpose()
            .context("invalid packet limit")?,
        rotate_files: flags
            .remove("--rotate-files")
            .map(|value| value.parse())
            .transpose()
            .context("invalid rolling capture file count")?,
        scratch_directory: flags
            .remove("--scratch-directory")
            .unwrap_or_else(|| PacketCaptureOptions::default_scratch_directory(tmpfs).into()),
        require_tmpfs: tmpfs,
    }
    .validate()?;
    if !capture {
        return Ok((Request::PreviewPacketCapture { pane, options }, None));
    }
    let request = Request::CapturePackets {
        pane,
        options,
        connection_generation: flags
            .remove("--connection-generation")
            .context("provide --connection-generation from the reviewed plan")?
            .parse()?,
        review_id: flags
            .remove("--review-id")
            .context("provide --review-id from the reviewed plan")?,
        job_id: flags.remove("--job-id"),
        output_file: path::absolute(
            flags
                .remove("--output-file")
                .context("choose --output-file PATH")?,
        )?,
    };
    let wireshark = flags
        .remove("--wireshark")
        .map(path::absolute)
        .transpose()?;
    Ok((request, wireshark))
}

pub(super) fn open_in_wireshark(reply: &mut Reply, executable: PathBuf) {
    let Some(result) = reply.result.as_mut().filter(|_| reply.ok) else {
        return;
    };
    let opened = (|| -> Result<_> {
        let capture = result
            .pointer("/capture/path")
            .and_then(serde_json::Value::as_str)
            .context("No packet capture path was returned")?;
        open_capture_in_wireshark(Path::new(capture), &executable)
    })();
    match opened {
        Ok(pid) => result["wireshark_pid"] = pid.into(),
        Err(error) => result["wireshark_error"] = format!("{error:#}").into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_matches_the_reviewed_pane_generation_and_job() {
        let job = "0123456789abcdef0123456789abcdef";
        let args = [
            "--pane",
            "7",
            "--connection-generation",
            "3",
            "--job-id",
            job,
        ];
        let (request, viewer) = parse(
            "stop-packet-capture",
            args.into_iter().map(str::to_owned),
            None,
        )
        .unwrap();
        let Request::StopPacketCapture {
            pane,
            connection_generation,
            job_id,
        } = request
        else {
            panic!("expected a stop request");
        };
        assert_eq!(
            (pane, connection_generation.get(), job_id.as_str()),
            (7, 3, job)
        );
        assert!(viewer.is_none());
        assert!(
            parse(
                "stop-packet-capture",
                args.into_iter().chain(["--tmpfs"]).map(str::to_owned),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn missing_wireshark_keeps_the_saved_capture_in_the_reply() {
        let temporary = tempfile::tempdir().unwrap();
        let capture = temporary.path().join("my capture.pcap");
        std::fs::write(&capture, b"saved").unwrap();
        let mut reply = Reply::from_result(Ok(serde_json::json!({"capture": {"path": capture}})));
        open_in_wireshark(&mut reply, temporary.path().join("Wireshark.exe"));
        assert!(reply.ok);
        let result = reply.result.unwrap();
        assert_eq!(result["capture"]["path"], serde_json::json!(capture));
        assert!(
            result["wireshark_error"]
                .as_str()
                .unwrap()
                .contains("not found")
        );
        assert_eq!(std::fs::read(capture).unwrap(), b"saved");
    }

    #[test]
    fn tmpfs_review_defaults_to_shm_and_keeps_filter_literal() {
        let (request, open) = parse(
            "preview-packet-capture",
            [
                "--interface",
                "lo",
                "--tmpfs",
                "--filter",
                "udp and port 4096",
            ]
            .into_iter()
            .map(str::to_string),
            Some(1),
        )
        .unwrap();
        let Request::PreviewPacketCapture { options, .. } = request else {
            panic!("expected preview")
        };
        assert_eq!(options.scratch_directory, "/dev/shm");
        assert_eq!(options.filter, "udp and port 4096");
        assert!(open.is_none());
    }
}
