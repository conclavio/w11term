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
    PacketConversationEndpoints, PacketConversationQuery, open_capture_in_wireshark,
    verify_packet_capture_report,
};

pub(super) const HELP: &str = concat!(
    "w11term-cli [--pid PID] list-packet-interfaces --pane ID\n",
    "w11term-cli [--pid PID] preview-packet-capture --pane ID (--settings PATH | --interface NAME [CAPTURE OPTIONS])\n",
    "w11term-cli [--pid PID] capture-packets --pane ID (--settings PATH | --interface NAME [CAPTURE OPTIONS]) ",
    "--connection-generation N --review-id ID --output-file PATH [--report-file PATH] [--job-id HEX] [--wireshark PATH]\n",
    "w11term-cli [--pid PID] stop-packet-capture --pane ID --connection-generation N --job-id HEX\n",
    "CAPTURE OPTIONS: [--filter EXPR] [--snap-length BYTES] [--seconds N] [--max-mib N] ",
    "[--packet-limit N] [--rotate-files 2..16] [--scratch-directory PATH] [--tmpfs]",
);

const CAPTURE_FLAGS: &[&str] = &[
    "--interface",
    "--filter",
    "--snap-length",
    "--seconds",
    "--max-mib",
    "--packet-limit",
    "--rotate-files",
    "--scratch-directory",
    "--tmpfs",
];

pub(super) const SAVE_SETTINGS_VERB: &str = "save-packet-capture-settings";
pub(super) const SAVE_SETTINGS_HELP: &str = concat!(
    "w11term-cli save-packet-capture-settings --interface NAME [--filter EXPR] ",
    "[--snap-length BYTES] [--seconds N] [--max-mib N] [--packet-limit N] ",
    "[--rotate-files 2..16] [--scratch-directory PATH] [--tmpfs] [--output-file PATH]",
);

pub(super) const INSPECT_VERB: &str = "inspect-packet-capture";
pub(super) const INSPECT_HELP: &str = concat!(
    "w11term-cli inspect-packet-capture FILE ",
    "[--all-conversations | --filter-conversations QUERY] [--output-file PATH]",
);
pub(super) const COMPARE_VERB: &str = "compare-packet-captures";
pub(super) const COMPARE_HELP: &str = concat!(
    "w11term-cli compare-packet-captures BEFORE AFTER [--elapsed-ns START:END] ",
    "[--format json|text] [--output-file PATH]",
);
pub(super) const CLIP_VERB: &str = "clip-packet-capture";
pub(super) const CLIP_HELP: &str = concat!(
    "w11term-cli clip-packet-capture FILE [--elapsed-ns START:END] ",
    "[--ip-pair A,B | --tcp-pair A:PORT,B:PORT | --udp-pair A:PORT,B:PORT] --output-file PATH",
);

pub(super) const VERIFY_REPORT_VERB: &str = "verify-packet-capture-report";
pub(super) const VERIFY_REPORT_HELP: &str =
    "w11term-cli verify-packet-capture-report CAPTURE REPORT";

pub(super) fn verify_report(args: &[String], stdout: &mut dyn Write) -> Result<()> {
    let [capture, report] = args else {
        bail!("provide the capture path and its report path");
    };
    let capture = path::absolute(capture)?;
    let report = path::absolute(report)?;
    let verified = verify_packet_capture_report(&capture, &report)?;
    let reply = Reply::from_result(Ok(serde_json::json!({
        "capture_path": capture,
        "report_path": report,
        "bytes": verified.bytes,
        "sha256": verified.sha256,
        "details": verified.details,
    })));
    serde_json::to_writer(&mut *stdout, &reply)?;
    writeln!(stdout)?;
    Ok(())
}

pub(super) fn inspect_file(args: &[String], stdout: &mut dyn Write) -> Result<()> {
    let mut args = args.iter();
    let path = args
        .next()
        .context("provide the local PCAP or PCAPNG file")?;
    let options = report_options(INSPECT_VERB, args)?;
    ensure!(
        !options.all_conversations || options.conversation_filter.is_none(),
        "Choose --all-conversations or --filter-conversations"
    );
    let query = options
        .conversation_filter
        .or(options.all_conversations.then_some(""))
        .map(PacketConversationQuery::new);
    let capture = LocalPacketCapture::open(path::absolute(path)?)
        .with_context(|| format!("Inspecting packet capture {path}"))?;
    offline::write_report(stdout, options.output_file, |output| match &query {
        Some(query) => capture.write_json_with_conversations(output, query),
        None => capture.write_json(output),
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
    let bounds = options.elapsed_ns.map(parse_elapsed).transpose()?;
    let output = options.output_file.context("provide --output-file PATH")?;
    let source = path::absolute(input)?;
    let plan = if let Some((kind, pair)) = options.pair {
        PacketCaptureExcerptPlan::conversation(source, parse_clip_pair(kind, pair)?, bounds)?
    } else {
        PacketCaptureExcerptPlan::elapsed(
            source,
            bounds.context("provide --elapsed-ns START:END or a conversation pair")?,
        )?
    };
    let excerpt = plan.save(path::absolute(output)?)?;
    let reply = Reply::from_result(Ok(serde_json::to_value(excerpt)?));
    serde_json::to_writer(&mut *stdout, &reply)?;
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}

fn parse_clip_pair(kind: &str, value: &str) -> Result<PacketConversationEndpoints> {
    let (a, b) = value
        .split_once(',')
        .context("Separate the two endpoints with a comma")?;
    if kind == "--ip-pair" {
        return Ok(PacketConversationEndpoints::Ip {
            a: a.parse().context("Invalid first IP address")?,
            b: b.parse().context("Invalid second IP address")?,
        });
    }
    Ok(PacketConversationEndpoints::Transport {
        protocol: if kind == "--tcp-pair" { 6 } else { 17 },
        a: a.parse().context("Invalid first IP and port endpoint")?,
        b: b.parse().context("Invalid second IP and port endpoint")?,
    })
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
    pair: Option<(&'a str, &'a str)>,
    all_conversations: bool,
    conversation_filter: Option<&'a str>,
}

fn report_options<'a>(
    verb: &str,
    mut args: impl Iterator<Item = &'a String>,
) -> Result<ReportOptions<'a>> {
    let mut options = ReportOptions::default();
    while let Some(flag) = args.next() {
        if verb == INSPECT_VERB && flag == "--all-conversations" {
            ensure!(
                !options.all_conversations,
                "Specify --all-conversations only once"
            );
            options.all_conversations = true;
            continue;
        }
        if verb == CLIP_VERB && matches!(flag.as_str(), "--ip-pair" | "--tcp-pair" | "--udp-pair") {
            ensure!(options.pair.is_none(), "Specify only one conversation pair");
            let pair = args
                .next()
                .with_context(|| format!("{flag} requires two endpoints"))?;
            options.pair = Some((flag.as_str(), pair.as_str()));
            continue;
        }
        let target = match flag.as_str() {
            "--format" if verb == COMPARE_VERB => &mut options.format,
            "--filter-conversations" if verb == INSPECT_VERB => &mut options.conversation_filter,
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
    args: impl Iterator<Item = String>,
    pane: Option<u64>,
) -> Result<(Request, Option<PathBuf>)> {
    let listing = verb == "list-packet-interfaces";
    let capture = verb == "capture-packets";
    let stop = verb == "stop-packet-capture";
    let (mut flags, tmpfs) = read_flags(args, |flag| {
        flag == "--pane"
            || !listing
                && (!stop && (flag == "--settings" || CAPTURE_FLAGS.contains(&flag))
                    || (capture || stop) && matches!(flag, "--connection-generation" | "--job-id")
                    || capture
                        && matches!(
                            flag,
                            "--review-id" | "--output-file" | "--report-file" | "--wireshark"
                        ))
    })?;
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
    let options = if let Some(settings) = flags.remove("--settings") {
        ensure!(
            !tmpfs && !CAPTURE_FLAGS.iter().any(|flag| flags.contains_key(*flag)),
            "--settings cannot be combined with capture option flags"
        );
        PacketCaptureOptions::load_settings(&path::absolute(settings)?)?
    } else {
        capture_options(&mut flags, tmpfs)?
    };
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
        report_file: flags
            .remove("--report-file")
            .map(path::absolute)
            .transpose()?,
    };
    let wireshark = flags
        .remove("--wireshark")
        .map(path::absolute)
        .transpose()?;
    Ok((request, wireshark))
}

pub(super) fn save_settings(args: &[String], output: &mut dyn Write) -> Result<()> {
    let (mut flags, tmpfs) = read_flags(args.iter().cloned(), |flag| {
        CAPTURE_FLAGS.contains(&flag) || flag == "--output-file"
    })?;
    let json = capture_options(&mut flags, tmpfs)?.settings_json()?;
    offline::write_report(
        output,
        flags.get("--output-file").map(String::as_str),
        |writer| writer.write_all(json.as_bytes()),
    )
}

fn read_flags(
    mut args: impl Iterator<Item = String>,
    accepts: impl Fn(&str) -> bool,
) -> Result<(BTreeMap<String, String>, bool)> {
    let mut flags = BTreeMap::new();
    let mut tmpfs = false;
    while let Some(flag) = args.next() {
        ensure!(accepts(&flag), "unsupported packet capture option {flag}");
        if flag == "--tmpfs" {
            ensure!(!tmpfs, "specify --tmpfs only once");
            tmpfs = true;
            continue;
        }
        let value = args
            .next()
            .with_context(|| format!("{flag} requires a value"))?;
        ensure!(
            flags.insert(flag.clone(), value).is_none(),
            "specify {flag} only once"
        );
    }
    Ok((flags, tmpfs))
}

fn capture_options(
    flags: &mut BTreeMap<String, String>,
    tmpfs: bool,
) -> Result<PacketCaptureOptions> {
    PacketCaptureOptions {
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
    .validate()
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
    fn uses_saved_options_for_preview_and_capture_without_importing_execution_scope() {
        let temporary = tempfile::tempdir().unwrap();
        let settings = temporary.path().join("packet settings.json");
        let options = capture_options(
            &mut [
                ("--interface".into(), "lo".into()),
                ("--filter".into(), "udp and port 4096".into()),
                ("--rotate-files".into(), "3".into()),
                (
                    "--scratch-directory".into(),
                    "/dev/shm/capture files".into(),
                ),
            ]
            .into_iter()
            .collect(),
            true,
        )
        .unwrap();
        options.clone().save_settings(&settings).unwrap();
        let original = std::fs::read(&settings).unwrap();
        assert!(options.clone().save_settings(&settings).is_err());
        assert_eq!(std::fs::read(&settings).unwrap(), original);
        let expected = serde_json::to_value(options).unwrap();
        let file = settings.to_str().unwrap();
        let args = ["--settings", file, "--pane", "7"];
        let (preview, _) = parse(
            "preview-packet-capture",
            args.map(str::to_owned).into_iter(),
            None,
        )
        .unwrap();
        let preview = serde_json::to_value(preview).unwrap();
        assert_eq!(preview["pane"], 7);
        assert_eq!(preview["options"], expected);
        let review_id = "89".repeat(32);
        let (capture, _) = parse(
            "capture-packets",
            args.into_iter()
                .chain([
                    "--connection-generation",
                    "3",
                    "--review-id",
                    &review_id,
                    "--output-file",
                    "after.pcap",
                    "--report-file",
                    "after.pcap.json",
                ])
                .map(str::to_owned),
            None,
        )
        .unwrap();
        let capture = serde_json::to_value(capture).unwrap();
        assert_eq!(capture["options"], expected);
        assert_eq!(capture["connection_generation"], 3);
        assert_eq!(capture["review_id"], review_id);
        assert_eq!(
            capture["report_file"],
            serde_json::json!(path::absolute("after.pcap.json").unwrap())
        );
        for conflict in [&["--tmpfs"][..], &["--snap-length", "256"]] {
            assert!(
                parse(
                    "preview-packet-capture",
                    args.into_iter()
                        .chain(conflict.iter().copied())
                        .map(str::to_owned),
                    None
                )
                .unwrap_err()
                .to_string()
                .contains("cannot be combined")
            );
        }
    }

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
