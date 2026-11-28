//! Console companion for JSON control replies and JSON Lines output streams.
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    io::{Read, Write},
    path::PathBuf,
    process::ExitCode,
};
use w11term::control::{self, Reply, Request};
#[path = "w11term-cli/automation.rs"]
mod automation;
#[path = "w11term-cli/command_reference.rs"]
mod command_reference;
#[path = "w11term-cli/cpu_profile.rs"]
mod cpu_profile;
#[path = "w11term-cli/diagnostics.rs"]
mod diagnostics;
#[path = "w11term-cli/dotfile_profiles.rs"]
mod dotfile_profiles;
#[path = "w11term-cli/dotfiles.rs"]
mod dotfiles;
#[path = "w11term-cli/offline.rs"]
mod offline;
#[path = "w11term-cli/packet_capture.rs"]
mod packet_capture;
#[path = "w11term-cli/sftp.rs"]
mod sftp_args;
#[path = "w11term-cli/syscall_capture.rs"]
mod syscall_capture;
#[path = "w11term-cli/tool_bundles.rs"]
mod tool_bundles;
#[path = "w11term-cli/triage.rs"]
mod triage;

const HELP: &str = "w11term-cli [--pid PID] list\nw11term-cli [--pid PID] list-windows\nw11term-cli [--pid PID] list-tabs --window ID\nw11term-cli [--pid PID] reorder-tabs --window ID --tab ID [--tab ID ...]\nw11term-cli [--pid PID] rename-tab --window ID --tab ID (--name NAME | --reset)\nw11term-cli [--pid PID] rename-pane --pane ID (--name NAME | --reset)\nw11term-cli [--pid PID] pin-tab --window ID --tab ID (--on | --off)\nw11term-cli [--pid PID] set-tab-color --window ID --tab ID (--color NAME | --reset)\nw11term-cli [--pid PID] list-profiles\nw11term-cli [--pid PID] list-workspaces\nw11term-cli [--pid PID] get-workspace --name NAME [--window ID]\nw11term-cli [--pid PID] rename-workspace --name OLD --to NEW\nw11term-cli [--pid PID] repair-workspace --window ID --name NAME --replace OLD NEW [--replace OLD NEW ...]\nw11term-cli [--pid PID] save-workspace --window ID --name NAME\nw11term-cli [--pid PID] open-workspace (--window ID [--activate] | --new-window) --name NAME\nw11term-cli [--pid PID] send-text --pane ID --no-paste [--connection-generation N] [TEXT | --base64 BASE64 | --file PATH | --stdin]\nw11term-cli [--pid PID] get-text --pane ID\nw11term-cli [--pid PID] screenshot --pane ID PATH\nw11term-cli instances\n\nTEXT is literal UTF-8 (no escape interpretation). Use --base64/--file/--stdin for arbitrary bytes.\nControl commands without --pid require exactly one live instance. No command starts a GUI or changes focus.";

#[derive(Debug)]
struct Invocation {
    pid: Option<u32>,
    request: Request,
    success_check: Option<SuccessCheck>,
    wireshark: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum SuccessCheck {
    Command,
    SshGroup,
}

impl Invocation {
    fn new(pid: Option<u32>, request: Request) -> Self {
        Self {
            pid,
            request,
            success_check: None,
            wireshark: None,
        }
    }
}

fn parse_invocation(args: impl IntoIterator<Item = String>) -> Result<Invocation> {
    let mut args = args.into_iter();
    let mut pid = None;
    let mut verb = None;
    let mut pane = None;
    let mut no_paste = false;
    let mut connection_generation = None;
    let mut payload = None;
    let mut path = None;
    while let Some(arg) = args.next() {
        let value = |args: &mut dyn Iterator<Item = String>| -> Result<String> {
            args.next()
                .with_context(|| format!("{arg} requires a value"))
        };
        match arg.as_str() {
            "find-text"
            | "wait-for-text"
            | "events"
            | "open"
            | "resize"
            | "record"
            | "save-text"
            | "restart"
            | "stop-ssh-attempt"
            | "retry-ssh-now"
            | "disconnect-ssh-shell"
            | "wait-ssh-connected"
            | "wait-pane-exit"
            | "close-pane"
            | "split-pane"
            | "move-pane"
            | "move-tab"
            | "list-tabs"
            | "reorder-tabs"
            | "rename-tab"
            | "rename-pane"
            | "pin-tab"
            | "set-tab-color"
            | "focus-pane"
            | "zoom-pane"
            | "get-tab-layout"
            | "resize-split"
            | "list-commands"
            | "get-command"
            | "get-running-output"
            | "wait-running-output"
            | "interrupt-command"
            | "wait-command"
            | "inspect"
            | "preview-linux-triage"
            | "linux-triage"
            | "playback"
            | "list-ssh-groups"
            | "list-saved-ssh-groups"
            | "open-ssh-group"
            | "open-workspace"
            | "get-workspace"
            | "rename-workspace"
            | "repair-workspace"
            | "save-workspace"
            | "get-ssh-group-roster"
            | "get-ssh-group-run"
            | "get-ssh-group-retry-plan"
            | "retry-ssh-group-run"
            | "interrupt-ssh-group-command"
            | "ssh-group-queue"
            | "save-ssh-group-report"
            | "submit-ssh-group-command"
            | "wait-ssh-group-run"
            | "wait-ssh-group-approval"
            | "wait-ssh-group-output"
            | "get-ssh-group-output"
            | "follow-ssh-group-output"
            | "wait-ssh-group-ready"
                if verb.is_none() =>
            {
                ensure!(
                    payload.is_none() && path.is_none() && !no_paste,
                    "invalid options before automation command"
                );
                return automation::parse(&arg, args, pid, pane);
            }
            "open-diagnostic" if verb.is_none() => {
                ensure!(
                    pane.is_none()
                        && payload.is_none()
                        && path.is_none()
                        && !no_paste
                        && connection_generation.is_none(),
                    "open-diagnostic accepts --pid before the command"
                );
                return Ok(Invocation::new(pid, diagnostics::parse(args)?));
            }
            command_reference::OPEN_VERB if verb.is_none() => {
                ensure!(
                    pane.is_none()
                        && payload.is_none()
                        && path.is_none()
                        && !no_paste
                        && connection_generation.is_none(),
                    "open-command-reference accepts --pid before the command"
                );
                return Ok(Invocation::new(pid, command_reference::parse_open(args)?));
            }
            "preview-cpu-profile" | "capture-cpu-profile" if verb.is_none() => {
                ensure!(
                    payload.is_none() && path.is_none() && !no_paste,
                    "invalid options before CPU profile command"
                );
                let request = cpu_profile::parse(&arg, args, pane)?;
                return Ok(Invocation::new(pid, request));
            }
            "list-packet-interfaces"
            | "preview-packet-capture"
            | "capture-packets"
            | "stop-packet-capture"
                if verb.is_none() =>
            {
                ensure!(
                    payload.is_none() && path.is_none() && !no_paste,
                    "invalid options before packet capture command"
                );
                let (request, wireshark) = packet_capture::parse(&arg, args, pane)?;
                return Ok(Invocation {
                    wireshark,
                    ..Invocation::new(pid, request)
                });
            }
            "preview-syscall-capture" | "capture-syscalls" | "stop-syscall-capture"
                if verb.is_none() =>
            {
                ensure!(
                    payload.is_none() && path.is_none() && !no_paste,
                    "invalid options before syscall capture command"
                );
                return syscall_capture::parse(&arg, args, pane)
                    .map(|request| Invocation::new(pid, request));
            }
            "sftp" if verb.is_none() => {
                ensure!(
                    payload.is_none() && path.is_none() && !no_paste,
                    "sftp accepts only --pid and --pane before the command"
                );
                return sftp_args::parse(args, pid, pane)
                    .map(|(pid, request)| Invocation::new(pid, request));
            }
            "--pid" => pid = Some(value(&mut args)?.parse()?),
            "--pane" => {
                ensure!(pane.is_none(), "specify --pane only once");
                pane = Some(value(&mut args)?.parse()?);
            }
            "--no-paste" => no_paste = true,
            "--connection-generation" if verb.as_deref() == Some("send-text") => {
                ensure!(
                    connection_generation.is_none(),
                    "specify --connection-generation only once"
                );
                connection_generation = Some(
                    value(&mut args)?
                        .parse()
                        .context("--connection-generation requires a positive integer")?,
                );
            }
            "--base64" | "--file" | "--stdin" => {
                ensure!(payload.is_none(), "choose one input source");
                let bytes = match arg.as_str() {
                    "--base64" => STANDARD
                        .decode(value(&mut args)?)
                        .context("invalid base64")?,
                    "--file" => read_limited(std::fs::File::open(value(&mut args)?)?)?,
                    _ => read_limited(std::io::stdin().lock())?,
                };
                payload = Some(bytes);
            }
            "--" => {
                ensure!(payload.is_none(), "choose one input source");
                payload = Some(
                    args.next()
                        .context("-- requires literal text")?
                        .into_bytes(),
                );
                ensure!(
                    args.next().is_none(),
                    "only one literal text argument is accepted"
                );
            }
            _ if arg.starts_with("--") => {
                bail!("unknown option {arg}; use -- before literal text beginning with --")
            }
            _ if verb.is_none() => verb = Some(arg),
            _ if verb.as_deref() == Some("send-text") => {
                ensure!(payload.is_none(), "choose one input source");
                payload = Some(arg.into_bytes());
            }
            _ if verb.as_deref() == Some("screenshot") => {
                ensure!(path.is_none(), "only one screenshot path is accepted");
                path = Some(PathBuf::from(arg));
            }
            _ => bail!("unknown argument {arg}"),
        }
    }
    let verb = verb.context(HELP)?;
    let request = match verb.as_str() {
        "list" | "list-windows" | "list-workspaces" | "list-profiles" => {
            ensure!(
                pane.is_none() && payload.is_none() && path.is_none() && !no_paste,
                "{verb} takes no pane/input options"
            );
            match verb.as_str() {
                "list-windows" => Request::ListWindows,
                "list-workspaces" => Request::ListWorkspaces,
                "list-profiles" => Request::ListProfiles,
                _ => Request::List,
            }
        }
        "get-text" => {
            ensure!(
                payload.is_none() && path.is_none() && !no_paste,
                "get-text takes only --pane"
            );
            Request::GetText {
                pane: pane.context("--pane is required")?,
            }
        }
        "send-text" => {
            ensure!(no_paste, "--no-paste is required (raw SSH input)");
            let bytes = payload.context("provide TEXT, --base64, --file, or --stdin")?;
            ensure!(bytes.len() <= control::MAX_BYTES, "input exceeds 1 MiB");
            Request::SendText {
                pane: pane.context("--pane is required")?,
                data_base64: STANDARD.encode(bytes),
                no_paste,
                connection_generation,
            }
        }
        "screenshot" => {
            ensure!(
                payload.is_none() && !no_paste,
                "screenshot takes only --pane and PATH"
            );
            Request::Screenshot {
                pane: pane.context("--pane is required")?,
                path: std::path::absolute(path.context("screenshot path is required")?)?,
            }
        }
        _ => bail!("unknown command {verb}\n{HELP}"),
    };
    Ok(Invocation::new(pid, request))
}
fn read_limited(reader: impl Read) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((control::MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= control::MAX_BYTES, "input exceeds 1 MiB");
    Ok(bytes)
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--version"] {
        use w11term::windows_release::{VERSION, build_commit};
        match build_commit() {
            Some(commit) => println!("w11term-cli {VERSION} ({commit})"),
            None => println!("w11term-cli {VERSION}"),
        }
        return ExitCode::SUCCESS;
    }
    if args == ["--help"] || args == ["-h"] {
        println!(
            "{HELP}\n\n{}\n\n{}\n\n{}",
            sftp_args::HELP,
            automation::HELP,
            tool_bundles::HELP
        );
        println!(
            "\nw11term-cli [--pid PID] preview-linux-triage --pane ID\nw11term-cli [--pid PID] linux-triage --pane ID --connection-generation N --output-file PATH"
        );
        println!("{}", triage::COMPARE_HELP);
        println!(
            "\n{}\n{}\n{}",
            cpu_profile::HELP,
            cpu_profile::COMPARE_HELP,
            cpu_profile::VERIFY_REPORT_HELP,
        );
        println!(
            "\n{}\n{}\n{}\n{}\n{}\n{}",
            packet_capture::HELP,
            packet_capture::INSPECT_HELP,
            packet_capture::COMPARE_HELP,
            packet_capture::CLIP_HELP,
            packet_capture::SAVE_SETTINGS_HELP,
            packet_capture::VERIFY_REPORT_HELP
        );
        println!(
            "\n{}\n{}\n{}\n{}\n{}",
            syscall_capture::HELP,
            syscall_capture::INSPECT_HELP,
            syscall_capture::COMPARE_HELP,
            syscall_capture::SAVE_SETTINGS_HELP,
            syscall_capture::VERIFY_REPORT_HELP,
        );
        println!("\n{}", diagnostics::HELP);
        println!("\n{}", command_reference::HELP);
        println!(
            "\n{}\n{}\n{}",
            dotfiles::HELP,
            dotfiles::INSPECT_HELP,
            dotfiles::COMPARE_HELP
        );
        println!("{}", dotfile_profiles::HELP);
        println!("{}", dotfile_profiles::ROLLOUT_HELP);
        println!("{}", dotfile_profiles::ROLLBACK_HELP);
        println!("{}", tool_bundles::AUDIT_HELP);
        println!("{}", tool_bundles::PREPARE_HELP);
        println!("{}", tool_bundles::ROLLOUT_HELP);
        println!("{}", tool_bundles::ROLLBACK_HELP);
        println!("{}", command_reference::OPEN_HELP);
        return ExitCode::SUCCESS;
    }
    if args == ["fetch-tool-bundle", "--help"] || args == ["fetch-tool-bundle", "-h"] {
        println!("{}", tool_bundles::HELP);
        return ExitCode::SUCCESS;
    }
    if args == ["open-diagnostic", "--help"] || args == ["open-diagnostic", "-h"] {
        println!("{}", diagnostics::HELP);
        return ExitCode::SUCCESS;
    }
    if args == [command_reference::OPEN_VERB, "--help"]
        || args == [command_reference::OPEN_VERB, "-h"]
    {
        println!("{}", command_reference::OPEN_HELP);
        return ExitCode::SUCCESS;
    }
    if let Some(exit) = offline::run(&args) {
        return exit;
    }
    let mut success_check = None;
    let result = if args
        .first()
        .is_some_and(|command| command == "fetch-tool-bundle")
    {
        tool_bundles::run(&args[1..]).map(|result| Reply::from_result(Ok(result)))
    } else if args == ["instances"] {
        control::discover().await.map(|instances| {
            Reply::from_result(serde_json::to_value(instances).map_err(Into::into))
        })
    } else {
        match parse_invocation(args) {
            Ok(Invocation {
                pid,
                request: request @ Request::FollowSshGroupOutput { .. },
                ..
            }) => {
                match control::follow(pid, &request, |reply| {
                    let mut stdout = std::io::stdout().lock();
                    let bytes = serde_json::to_vec(reply)?;
                    stdout.write_all(&bytes)?;
                    stdout.write_all(b"\n")?;
                    stdout.flush()?;
                    Ok(())
                })
                .await
                {
                    Ok(true) => return ExitCode::SUCCESS,
                    Ok(false) => return ExitCode::FAILURE,
                    Err(error)
                        if error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                            error.kind() == std::io::ErrorKind::BrokenPipe
                        }) =>
                    {
                        return ExitCode::SUCCESS;
                    }
                    Err(error) => Err(error),
                }
            }
            Ok(invocation) => {
                success_check = invocation.success_check;
                control::call(invocation.pid, &invocation.request)
                    .await
                    .map(|mut reply| {
                        if let Some(executable) = invocation.wireshark {
                            packet_capture::open_in_wireshark(&mut reply, executable);
                        }
                        reply
                    })
            }
            Err(e) => Err(e),
        }
    };
    let reply = result.unwrap_or_else(|e| Reply::from_result(Err(e)));
    println!("{}", serde_json::to_string(&reply).unwrap());
    cli_exit_code(&reply, success_check)
}

fn cli_exit_code(reply: &Reply, success_check: Option<SuccessCheck>) -> ExitCode {
    if !reply.ok {
        return ExitCode::FAILURE;
    }
    let Some(success_check) = success_check else {
        return ExitCode::SUCCESS;
    };
    let Some(result) = reply.result.as_ref() else {
        return ExitCode::FAILURE;
    };
    if success_check == SuccessCheck::Command {
        return if result["exit_code"].as_i64() == Some(0) {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(2)
        };
    }
    let Some(run) = result.get("run") else {
        return ExitCode::FAILURE;
    };
    let Some(recipients) = run.get("recipients").and_then(serde_json::Value::as_array) else {
        return ExitCode::FAILURE;
    };
    let has_expected_output = run.get("output_expectation").is_some();
    let all_succeeded = !recipients.is_empty()
        && recipients.iter().all(|recipient| {
            recipient["delivery"]["state"] == "sent"
                && recipient["execution"]["state"] == "completed"
                && recipient["execution"]["exit_code"].as_i64() == Some(0)
                && (!has_expected_output || recipient["output_expectation"] == "matched")
        });
    if all_succeeded {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: impl IntoIterator<Item = String>) -> Result<(Option<u32>, Request)> {
        let invocation = parse_invocation(args)?;
        Ok((invocation.pid, invocation.request))
    }

    #[test]
    fn lists_windows_and_targets_a_specific_gui_window() {
        let parse_line = |command: &str| parse(command.split_whitespace().map(str::to_owned));
        let (pid, request) = parse_line("--pid 7 list-windows").unwrap();
        assert_eq!(pid, Some(7));
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({"verb":"list-windows"})
        );
        let (_, request) = parse_line("open --profile desk --window 12 --activate").unwrap();
        assert!(matches!(
            request,
            Request::Open {
                window_id: Some(12),
                activate: true,
                ..
            }
        ));
        let (_, request) = parse_line("open --profile desk").unwrap();
        assert!(matches!(
            request,
            Request::Open {
                window_id: None,
                ..
            }
        ));
        let legacy: Request = serde_json::from_value(serde_json::json!({
            "verb": "open",
            "profile": "desk",
            "rows": 30
        }))
        .unwrap();
        assert!(matches!(
            legacy,
            Request::Open {
                window_id: None,
                rows: Some(30),
                ..
            }
        ));
        for invalid in [
            "list-windows --pane 1",
            "open --profile desk --window 0",
            "open --profile desk --window 1 --window 2",
            "open --profile desk --tab 1",
        ] {
            assert!(parse_line(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn sets_a_percentage_for_two_distinct_panes() {
        let parse_line = |command: &str| parse(command.split_whitespace().map(str::to_owned));
        let (pid, request) =
            parse_line("--pid 3 resize-split --pane 42 --target-pane 57 --percent 62.5").unwrap();
        assert_eq!(pid, Some(3));
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({"verb":"resize-split","pane":42,"target_pane":57,"percent":62.5})
        );
        for invalid in [
            "resize-split --pane 1 --target-pane 2",
            "resize-split --pane 1 --percent 50",
            "resize-split --pane 1 --target-pane 1 --percent 50",
            "resize-split --pane 1 --target-pane 0 --percent 50",
            "resize-split --pane 1 --target-pane 2 --percent 50 --percent 60",
            "resize-split --pane 1 --target-pane 2 --percent 0",
            "resize-split --pane 1 --target-pane 2 --percent 100",
            "resize-split --pane 1 --target-pane 2 --percent NaN",
            "resize-split --pane 1 --target-pane 2 --percent inf",
        ] {
            assert!(parse_line(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn reads_the_layout_of_one_exact_pane() {
        let parse_line = |command: &str| parse(command.split_whitespace().map(str::to_owned));
        let (pid, request) = parse_line("--pid 3 get-tab-layout --pane 42").unwrap();
        assert_eq!(pid, Some(3));
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({"verb":"get-tab-layout","pane":42})
        );
        for invalid in [
            "get-tab-layout",
            "get-tab-layout --pane 0",
            "get-tab-layout --pane 1 --pane 2",
            "get-tab-layout --pane 1 --activate",
            "get-tab-layout --window 1",
            "get-tab-layout --pane 1 trailing",
        ] {
            assert!(parse_line(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn selects_an_exact_pane_and_requires_an_explicit_zoom_state() {
        let parse_line = |command: &str| parse(command.split_whitespace().map(str::to_owned));
        assert!(matches!(
            parse_line("--pid 3 focus-pane --pane 42").unwrap(),
            (Some(3), Request::FocusPane { pane: 42 })
        ));
        let (_, request) = parse_line("zoom-pane --pane 42 --on").unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({"verb":"zoom-pane","pane":42,"zoomed":true})
        );
        assert!(matches!(
            parse_line("zoom-pane --off --pane 42").unwrap(),
            (
                _,
                Request::ZoomPane {
                    pane: 42,
                    zoomed: false
                }
            )
        ));
        for invalid in [
            "focus-pane --pane 0",
            "focus-pane --pane 1 --pane 2",
            "focus-pane --pane 1 --on",
            "focus-pane --pane 1 --window 2",
            "zoom-pane --pane 1",
            "zoom-pane --on",
            "zoom-pane --pane 0 --on",
            "zoom-pane --pane 1 --pane 2 --on",
            "zoom-pane --pane 1 --on --off",
            "zoom-pane --pane 1 --off --off",
        ] {
            assert!(parse_line(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn pins_tabs_and_keeps_hidden_colors_distinct_from_profile_defaults() {
        let parse_line = |command: &str| parse(command.split_whitespace().map(str::to_owned));
        let (_, pin) = parse_line("pin-tab --window 12 --tab 14 --off").unwrap();
        assert_eq!(
            serde_json::to_value(pin).unwrap(),
            serde_json::json!({"verb":"pin-tab","window_id":12,"tab_id":14,"pinned":false})
        );
        for (option, expected) in [
            ("--color BLUE", serde_json::json!("blue")),
            ("--color none", serde_json::json!("none")),
            ("--reset", serde_json::Value::Null),
        ] {
            let (_, command) =
                parse_line(&format!("set-tab-color --window 12 --tab 14 {option}")).unwrap();
            let value = serde_json::to_value(command).unwrap();
            assert_eq!(
                value,
                serde_json::json!({"verb":"set-tab-color","window_id":12,"tab_id":14,"color":expected})
            );
            assert!(matches!(
                serde_json::from_value::<Request>(value),
                Ok(Request::SetTabColor { .. })
            ));
        }
        for invalid in [
            "pin-tab --window 12 --tab 14",
            "pin-tab --tab 14 --on",
            "pin-tab --window 12 --on",
            "pin-tab --window 12 --tab 14 --on --off",
            "set-tab-color --window 12 --tab 14",
            "set-tab-color --window 12 --tab 14 --color chartreuse",
            "set-tab-color --window 12 --tab 14 --color blue --reset",
            "set-tab-color --window 12 --tab 14 --color blue --color red",
        ] {
            assert!(parse_line(invalid).is_err(), "{invalid}");
        }
        assert!(
            serde_json::from_value::<Request>(
                serde_json::json!({"verb":"set-tab-color","window_id":12,"tab_id":14})
            )
            .is_err()
        );
    }

    #[test]
    fn names_terminal_tabs_and_panes_with_an_explicit_reset() {
        let (_, request) = parse(
            [
                "rename-tab",
                "--window",
                "12",
                "--tab",
                "14",
                "--name",
                "Fleet Ops",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({"verb":"rename-tab","window_id":12,"tab_id":14,"name":"Fleet Ops"})
        );
        let parse_line = |command: &str| parse(command.split_whitespace().map(str::to_owned));
        let (_, request) = parse_line("rename-pane --pane 42 --reset").unwrap();
        let reset = serde_json::json!({"verb":"rename-pane","pane":42,"name":null});
        assert_eq!(serde_json::to_value(request).unwrap(), reset);
        assert!(matches!(
            serde_json::from_value::<Request>(reset).unwrap(),
            Request::RenamePane {
                pane: 42,
                name: None
            }
        ));
        for invalid in [
            "rename-tab --tab 14 --reset",
            "rename-tab --window 12 --reset",
            "rename-tab --window 12 --tab 0 --reset",
            "rename-pane --pane 42",
            "rename-pane --pane 42 --name Fleet --reset",
            "rename-pane --pane 42 --reset --reset",
            "rename-pane --pane 42 --window 12 --name Fleet",
        ] {
            assert!(parse_line(invalid).is_err(), "{invalid}");
        }
        for invalid in [
            serde_json::json!({"verb":"rename-pane","pane":42}),
            serde_json::json!({"verb":"rename-tab","window_id":12,"tab_id":14}),
            serde_json::json!({"verb":"rename-tab","window_id":12,"tab_id":14,"name":" Fleet "}),
            serde_json::json!({"verb":"rename-pane","pane":42,"name":"invalid\nname"}),
        ] {
            assert!(serde_json::from_value::<Request>(invalid).is_err());
        }
    }

    #[test]
    fn lists_tabs_and_accepts_the_requested_order_in_one_window() {
        let parse_line = |command: &str| parse(command.split_whitespace().map(str::to_owned));
        assert!(matches!(
            parse_line("--pid 7 list-tabs --window 12").unwrap(),
            (Some(7), Request::ListTabs { window_id: 12 })
        ));
        let (_, request) = parse_line("reorder-tabs --window 12 --tab 14 --tab 2 --tab 6").unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({"verb":"reorder-tabs","window_id":12,"tab_ids":[14,2,6]})
        );
        for invalid in [
            "list-tabs",
            "list-tabs --window 0",
            "list-tabs --window 12 --tab 14",
            "reorder-tabs --tab 14",
            "reorder-tabs --window 12",
            "reorder-tabs --window 12 --tab 0",
            "reorder-tabs --window 12 --tab 14 --window 13",
            "reorder-tabs --window 12 --tab 14 --activate",
        ] {
            assert!(parse_line(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn moves_a_tab_with_an_explicit_window_choice() {
        let parse_line = |command: &str| parse(command.split_whitespace().map(str::to_owned));
        let (pid, request) =
            parse_line("--pid 7 move-tab --pane 42 --window 9 --activate").unwrap();
        assert_eq!(pid, Some(7));
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({"verb":"move-tab","pane":42,"window_id":9,"activate":true})
        );
        let (_, request) = parse_line("move-tab --pane 42 --new-window").unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({"verb":"move-tab","pane":42,"new_window":true})
        );
        for invalid in [
            "move-tab --pane 1",
            "move-tab --new-window",
            "move-tab --pane 0 --new-window",
            "move-tab --pane 1 --window 0",
            "move-tab --pane 1 --window 2 --new-window",
            "move-tab --pane 1 --new-window --activate",
            "move-tab --pane 1 --pane 2 --new-window",
            "move-tab --pane 1 --target-pane 2 --new-window",
        ] {
            assert!(parse_line(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn moves_an_exact_pane_beside_another_pane() {
        let parse_line = |command: &str| parse(command.split_whitespace().map(str::to_owned));
        let (_, request) =
            parse_line("move-pane --pane 42 --target-pane 9 --direction down --activate").unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "verb": "move-pane", "pane": 42, "target_pane": 9, "direction": "down", "activate": true,
            })
        );
        for invalid in [
            "move-pane --pane 42 --target-pane 9",
            "move-pane --pane 42 --direction left",
            "move-pane --target-pane 9 --direction left",
            "move-pane --pane 0 --target-pane 9 --direction left",
            "move-pane --pane 42 --target-pane 0 --direction left",
            "move-pane --pane 42 --target-pane 42 --direction left",
            "move-pane --pane 42 --pane 43 --target-pane 9 --direction left",
            "move-pane --pane 42 --target-pane 9 --target-pane 10 --direction left",
            "move-pane --pane 42 --target-pane 9 --direction left --direction right",
            "move-pane --pane 42 --target-pane 9 --direction diagonal",
            "move-pane --pane 42 --target-pane 9 --direction left --window 1",
        ] {
            assert!(parse_line(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn splits_an_explicit_pane_with_a_saved_profile() {
        let parse_line = |command: &str| parse(command.split_whitespace().map(str::to_owned));
        let (pid, request) = parse(
            [
                "--pid",
                "7",
                "split-pane",
                "--pane",
                "42",
                "--profile",
                "Fleet Ops",
                "--axis",
                "columns",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(pid, Some(7));
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "verb": "split-pane", "pane": 42, "profile": "Fleet Ops", "axis": "columns",
                "activate": false,
            })
        );
        assert!(matches!(
            parse_line("split-pane --pane 42 --profile local --axis rows --activate")
                .unwrap()
                .1,
            Request::SplitPane {
                axis: w11term::pane_layout::Axis::Rows,
                activate: true,
                ..
            }
        ));
        for invalid in [
            "split-pane --pane 42 --profile local",
            "split-pane --pane 42 --axis rows",
            "split-pane --profile local --axis rows",
            "split-pane --pane 0 --profile local --axis rows",
            "split-pane --pane 42 --pane 43 --profile local --axis rows",
            "split-pane --pane 42 --profile local --profile remote --axis rows",
            "split-pane --pane 42 --profile local --axis diagonal",
            "split-pane --pane 42 --profile local --axis rows --axis columns",
            "split-pane --pane 42 --profile local --axis rows --window 1",
        ] {
            assert!(parse_line(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn lists_profiles_and_opens_one_in_a_new_window() {
        let parse_line = |command: &str| parse(command.split_whitespace().map(str::to_owned));
        assert!(matches!(
            parse_line("list-profiles --pid 7").unwrap(),
            (Some(7), Request::ListProfiles)
        ));
        let (_, request) = parse(
            [
                "open",
                "--profile",
                "Fleet Ops",
                "--new-window",
                "--cols",
                "88",
                "--rows",
                "27",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({"verb":"open-profile-window","profile":"Fleet Ops","cols":88,"rows":27})
        );
        for invalid in [
            "list-profiles --pane 1",
            "list-profiles --window 1",
            "open --new-window",
            "open --profile fleet --new-window --window 1",
            "open --profile fleet --new-window --activate",
            "open --profile fleet --new-window --new-window",
        ] {
            assert!(parse_line(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn repairs_explicit_missing_ssh_profile_names_in_one_workspace() {
        let (_, request) = parse(
            [
                "repair-workspace",
                "--window",
                "12",
                "--name",
                "Fleet Ops",
                "--replace",
                "Old DB",
                "New DB",
                "--replace",
                "Old App",
                "New App",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "verb":"repair-workspace","window_id":12,"name":"Fleet Ops",
                "replacements":[
                    {"missing":"Old DB","replacement":"New DB"},
                    {"missing":"Old App","replacement":"New App"}
                ]
            })
        );
        for invalid in [
            "repair-workspace --window 12 --name Fleet",
            "repair-workspace --name Fleet --replace old new",
            "repair-workspace --window 12 --replace old new",
            "repair-workspace --window 0 --name Fleet --replace old new",
            "repair-workspace --window 12 --name Fleet --replace old",
            "repair-workspace --window 12 --name Fleet --replace old old",
            "repair-workspace --window 12 --name Fleet --replace old one --replace old two",
        ] {
            assert!(
                parse(invalid.split_whitespace().map(str::to_owned)).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn previews_a_saved_workspace_with_an_optional_window() {
        let (_, request) =
            parse(["get-workspace", "--name", "Fleet Ops", "--window", "12"].map(str::to_owned))
                .unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({"verb":"get-workspace","name":"Fleet Ops","window_id":12})
        );
        let parse_line = |command: &str| parse(command.split_whitespace().map(str::to_owned));
        assert!(matches!(
            parse_line("get-workspace --name Fleet").unwrap(),
            (
                _,
                Request::GetWorkspace {
                    window_id: None,
                    ..
                }
            )
        ));
        for invalid in [
            "get-workspace",
            "get-workspace --window 12",
            "get-workspace --name Fleet --window 0",
            "get-workspace --name Fleet --window 12 --window 13",
            "get-workspace --name Fleet --name Other",
            "get-workspace --name Fleet --activate",
            "get-workspace --name Fleet --new-window",
        ] {
            assert!(parse_line(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn renames_an_existing_workspace_in_the_process_catalog() {
        let (pid, request) = parse(
            [
                "--pid",
                "7",
                "rename-workspace",
                "--name",
                "Fleet Ops",
                "--to",
                "Production Ops",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(pid, Some(7));
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "verb":"rename-workspace","name":"Fleet Ops","new_name":"Production Ops"
            })
        );
        for invalid in [
            "rename-workspace --name fleet",
            "rename-workspace --to ops",
            "rename-workspace --name fleet --to ops --to other",
            "rename-workspace --name fleet --name lab --to ops",
            "rename-workspace --name fleet --to ops --window 1",
            "rename-workspace --name fleet --to ops --pane 1",
            "rename-workspace --name fleet --to ops --overwrite",
        ] {
            assert!(
                parse(invalid.split_whitespace().map(str::to_owned)).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn saves_a_live_workspace_to_a_new_name_in_an_explicit_window() {
        let (pid, request) = parse(
            [
                "--pid",
                "7",
                "save-workspace",
                "--window",
                "12",
                "--name",
                "Fleet Ops",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(pid, Some(7));
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({"verb":"save-workspace","window_id":12,"name":"Fleet Ops"})
        );
        for invalid in [
            "save-workspace --name fleet",
            "save-workspace --window 1",
            "save-workspace --window 0 --name fleet",
            "save-workspace --window 1 --window 2 --name fleet",
            "save-workspace --window 1 --name fleet --name twice",
            "save-workspace --window 1 --name fleet --overwrite",
            "save-workspace --window 1 --name fleet --activate",
        ] {
            assert!(
                parse(invalid.split_whitespace().map(str::to_owned)).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn lists_and_opens_saved_workspaces() {
        let parse_line = |command: &str| parse(command.split_whitespace().map(str::to_owned));
        assert!(matches!(
            parse_line("list-workspaces --pid 7").unwrap(),
            (Some(7), Request::ListWorkspaces)
        ));
        let (_, request) = parse(
            [
                "open-workspace",
                "--window",
                "12",
                "--name",
                "Fleet Ops",
                "--activate",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "verb": "open-workspace", "window_id": 12, "name": "Fleet Ops", "activate": true,
            })
        );
        let (_, request) =
            parse(["open-workspace", "--new-window", "--name", "Fleet Ops"].map(str::to_owned))
                .unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({"verb": "open-workspace-window", "name": "Fleet Ops"})
        );
        for invalid in [
            "list-workspaces --window 1",
            "open-workspace --name fleet",
            "open-workspace --window 1",
            "open-workspace --window 0 --name fleet",
            "open-workspace --window 1 --window 2 --name fleet",
            "open-workspace --window 1 --name fleet --name another",
            "open-workspace --window 1 --name fleet --pane 2",
            "open-workspace --new-window",
            "open-workspace --new-window --window 1 --name fleet",
            "open-workspace --new-window --activate --name fleet",
            "open-workspace --new-window --new-window --name fleet",
        ] {
            assert!(parse_line(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn require_success_is_a_cli_option_for_the_group_wait() {
        let invocation = parse_invocation(
            "wait-ssh-group-run --window 2 --tab 3 --run 4 --require-success"
                .split_ascii_whitespace()
                .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(invocation.success_check, Some(SuccessCheck::SshGroup));
        assert_eq!(
            serde_json::to_value(invocation.request).unwrap(),
            serde_json::json!({
                "verb":"wait-ssh-group-run","window_id":2,"tab_id":3,"run_id":4,"timeout_seconds":30,
            })
        );
        assert!(
            parse_invocation(
                "get-ssh-group-run --window 2 --tab 3 --run 4 --require-success"
                    .split_ascii_whitespace()
                    .map(str::to_owned)
            )
            .is_err()
        );
    }

    #[test]
    fn strict_wait_requires_verified_zero_exits_and_checked_output() {
        use serde_json::{Value, json};

        let success = json!({
            "delivery":{"state":"sent"},
            "execution":{"state":"completed","exit_code":0},
            "output_expectation":"matched",
        });
        let outcome = |recipient: Value| {
            Reply::from_result(Ok(json!({
                "run":{"recipients":[success.clone(), recipient],"output_expectation":{"query":"ready"}},
            })))
        };
        assert_eq!(
            cli_exit_code(&outcome(success.clone()), Some(SuccessCheck::SshGroup)),
            ExitCode::SUCCESS
        );
        for (field, bad) in [
            ("delivery", json!({"state":"skipped"})),
            ("execution", json!({"state":"completed","exit_code":17})),
            ("execution", json!({"state":"completed","exit_code":null})),
            ("output_expectation", json!("missing")),
            ("output_expectation", json!("unverified")),
        ] {
            let mut recipient = success.clone();
            recipient[field] = bad;
            let reply = outcome(recipient);
            assert!(reply.ok);
            assert_eq!(
                cli_exit_code(&reply, Some(SuccessCheck::SshGroup)),
                ExitCode::from(2)
            );
            assert_eq!(cli_exit_code(&reply, None), ExitCode::SUCCESS);
        }
        assert_eq!(
            cli_exit_code(
                &Reply::from_result(Err(anyhow::anyhow!("timeout"))),
                Some(SuccessCheck::SshGroup)
            ),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn parses_ssh_group_submission_without_rewriting_the_command() {
        let base = [
            "--pid",
            "7",
            "submit-ssh-group-command",
            "--window",
            "2",
            "--tab",
            "3",
        ];
        let submit = |extra: &[&str]| {
            parse(
                base.into_iter()
                    .chain(extra.iter().copied())
                    .map(str::to_owned),
            )
        };
        let (pid, request) =
            submit(&["--pane", "9", "--pane", "5", "--", "show  status; pwd"]).unwrap();
        assert_eq!(pid, Some(7));
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "verb":"submit-ssh-group-command", "window_id":2, "tab_id":3,
                "pane_ids":[9,5], "command":"show  status; pwd",
            }),
        );
        let (_, checked) = submit(&[
            "--pane",
            "9",
            "--pane",
            "5",
            "--canary",
            "5",
            "--parallel",
            "3",
            "--interval-ms",
            "0",
            "--queue-expiry-ms",
            "1750",
            "--on-failure",
            "continue",
            "--dry-run",
            "--expect-output",
            "^READY [0-9]+$",
            "--regex",
            "--match-case",
            "--record-output",
            "capture output.txt",
            "--",
            "show  status",
        ])
        .unwrap();
        let checked = serde_json::to_value(checked).unwrap();
        assert_eq!(checked["dry_run"], true);
        assert_eq!(checked["canary_pane_id"], 5);
        assert_eq!(checked["parallel_limit"], 3);
        assert_eq!(checked["interval_ms"], 0);
        assert_eq!(checked["queue_expiry_ms"], 1750);
        assert_eq!(checked["stop_on_failure"], false);
        let (_, stop) = submit(&["--pane", "5", "--on-failure", "stop", "show"]).unwrap();
        assert_eq!(serde_json::to_value(stop).unwrap()["stop_on_failure"], true);
        assert_eq!(
            checked["expect_output"],
            serde_json::json!({ "query":"^READY [0-9]+$", "regex":true, "match_case":true }),
        );
        assert_eq!(
            checked["record_output"],
            serde_json::to_value(std::path::absolute("capture output.txt").unwrap()).unwrap()
        );
        for invalid in [
            &["show"][..],
            &["--pane", "0", "show"],
            &["--pane", "5", "--pane", "5", "show"],
            &["--pane", "5", "show\rstatus"],
            &["--pane", "5", "--run", "1", "show"],
            &["--pane", "5", "show", "status"],
            &["--pane", "5", "--regex", "show"],
            &["--pane", "5", "--match-case", "show"],
            &["--pane", "9", "--pane", "5", "--canary", "8", "show"],
            &["--pane", "5", "--canary", "5", "show"],
            &["--pane", "5", "--parallel", "0", "show"],
            &["--pane", "5", "--parallel", "-1", "show"],
            &["--pane", "5", "--parallel", "1", "--parallel", "4", "show"],
            &["--pane", "5", "--interval-ms", "-1", "show"],
            &["--pane", "5", "--queue-expiry-ms", "-1", "show"],
            &["--pane", "5", "--on-failure", "ignore", "show"],
            &[
                "--pane",
                "5",
                "--on-failure",
                "stop",
                "--on-failure",
                "continue",
                "show",
            ],
            &[
                "--pane",
                "5",
                "--queue-expiry-ms",
                "0",
                "--queue-expiry-ms",
                "4",
                "show",
            ],
            &[
                "--pane",
                "5",
                "--interval-ms",
                "1",
                "--interval-ms",
                "4",
                "show",
            ],
            &[
                "--pane", "5", "--pane", "9", "--canary", "5", "--canary", "9", "show",
            ],
            &["--pane", "5", "--expect-output"],
            &["--pane", "5", "--record-output"],
            &[
                "--pane",
                "5",
                "--record-output",
                "a",
                "--record-output",
                "b",
                "show",
            ],
            &[
                "--pane",
                "5",
                "--expect-output",
                "a",
                "--expect-output",
                "b",
                "show",
            ],
        ] {
            assert!(submit(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn parses_explicit_ssh_group_queue_actions() {
        let parse_queue = |action: &[&str]| {
            parse(
                [
                    "ssh-group-queue",
                    "--window",
                    "2",
                    "--tab",
                    "3",
                    "--run",
                    "4",
                ]
                .into_iter()
                .chain(action.iter().copied())
                .map(str::to_owned),
            )
        };
        for action in ["pause", "resume", "stop"] {
            let (_, request) = parse_queue(&[action]).unwrap();
            assert_eq!(
                serde_json::to_value(request).unwrap(),
                serde_json::json!({"verb":"ssh-group-queue","window_id":2,"tab_id":3,"run_id":4,"action":action})
            );
        }
        for (command, wire) in [("approve-canary", "approve_canary"), ("skip", "skip")] {
            let (_, targeted) = parse_queue(&[command, "--pane", "5"]).unwrap();
            assert_eq!(
                serde_json::to_value(targeted).unwrap(),
                serde_json::json!({
                    "verb":"ssh-group-queue","window_id":2,"tab_id":3,"run_id":4,
                    "action":{(wire):{"pane_id":5}}
                })
            );
            assert!(parse_queue(&[command]).is_err());
            assert!(parse_queue(&[command, "--pane", "0"]).is_err());
            assert!(parse_queue(&[command, "--pane", "5", "--pane", "6"]).is_err());
        }
        assert!(parse_queue(&["cancel"]).is_err());
        assert!(parse_queue(&["pause", "--pane", "5"]).is_err());
        assert!(
            parse(["ssh-group-queue", "--window", "2", "--tab", "3", "pause"].map(str::to_owned))
                .is_err()
        );
    }

    #[test]
    fn parses_report_exports_and_defaults_to_json_without_overwriting() {
        use w11term::control::SshGroupReportFormat;

        let base = [
            "save-ssh-group-report",
            "--window",
            "2",
            "--tab",
            "3",
            "--run",
            "4",
        ];
        let save = |extra: &[&str]| {
            parse(
                base.into_iter()
                    .chain(extra.iter().copied())
                    .map(str::to_owned),
            )
        };
        assert!(matches!(
            save(&["report file.json"]).unwrap().1,
            Request::SaveSshGroupReport {
                window_id: 2, tab_id: 3, run_id: 4, path, format: SshGroupReportFormat::Json, overwrite: false,
            } if path == std::path::absolute("report file.json").unwrap()
        ));
        assert!(matches!(
            save(&["--format", "text", "--overwrite", "--", "--report.txt"])
                .unwrap()
                .1,
            Request::SaveSshGroupReport {
                format: SshGroupReportFormat::Text,
                overwrite: true,
                ..
            }
        ));
        for invalid in [
            &[][..],
            &["--format", "yaml", "report.txt"],
            &["--format", "json", "--format", "text", "report.txt"],
            &["--pane", "1", "report.txt"],
            &["one.json", "two.json"],
        ] {
            assert!(save(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn parses_saved_ssh_group_launch_with_an_explicit_window() {
        let (pid, catalog) =
            parse(["--pid", "7", "list-saved-ssh-groups", "--window", "3"].map(str::to_owned))
                .unwrap();
        assert_eq!(pid, Some(7));
        assert_eq!(
            serde_json::to_value(catalog).unwrap(),
            serde_json::json!({"verb":"list-saved-ssh-groups", "window_id":3}),
        );
        let (pid, request) = parse(
            [
                "--pid",
                "7",
                "open-ssh-group",
                "--name",
                "Fleet Ops",
                "--window",
                "3",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(pid, Some(7));
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({"verb":"open-ssh-group", "window_id":3, "name":"Fleet Ops"}),
        );
        for invalid in [
            "list-saved-ssh-groups",
            "list-saved-ssh-groups --window 0",
            "list-saved-ssh-groups --window 3 --name Fleet",
            "list-saved-ssh-groups --window 3 --pane 4",
            "open-ssh-group --name Fleet",
            "open-ssh-group --window 3",
            "open-ssh-group --window 0 --name Fleet",
            "open-ssh-group --window 3 --name Fleet --name Other",
            "open-ssh-group --window 3 --window 4 --name Fleet",
            "open-ssh-group --window 3 --name Fleet --tab 4",
            "open-ssh-group --window 3 --name Fleet --pane 4",
        ] {
            assert!(
                parse(invalid.split_whitespace().map(str::to_owned)).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn parses_ssh_group_discovery_and_scoped_run_ids() {
        let parse_line = |line: &str| parse(line.split_ascii_whitespace().map(str::to_owned));
        assert!(matches!(
            parse_line("list-ssh-groups --pid 7").unwrap(),
            (Some(7), Request::ListSshGroups)
        ));
        assert!(matches!(
            parse_line("get-ssh-group-roster --window 11 --tab 12 --pid 7").unwrap(),
            (
                Some(7),
                Request::GetSshGroupRoster {
                    window_id: 11,
                    tab_id: 12
                }
            )
        ));
        assert!(matches!(
            parse_line("wait-ssh-group-ready --window 11 --tab 12 --timeout 4").unwrap(),
            (
                None,
                Request::WaitSshGroupReady {
                    window_id: 11,
                    tab_id: 12,
                    pane_ids: None,
                    timeout_seconds: 4
                }
            )
        ));
        assert!(matches!(
            parse_line("wait-ssh-group-ready --window 11 --tab 12").unwrap(),
            (
                None,
                Request::WaitSshGroupReady {
                    timeout_seconds: 30,
                    ..
                }
            )
        ));
        assert!(matches!(
            parse_line("wait-ssh-group-ready --window 11 --tab 12 --pane 8 --pane 3 --timeout 4").unwrap(),
            (None, Request::WaitSshGroupReady { window_id: 11, tab_id: 12, pane_ids: Some(panes), timeout_seconds: 4 })
                if panes == [8, 3]
        ));
        assert!(matches!(
            parse_line("wait-ssh-group-run --window 11 --tab 12 --run 4 --timeout 7").unwrap(),
            (
                None,
                Request::WaitSshGroupRun {
                    window_id: 11,
                    tab_id: 12,
                    run_id: 4,
                    timeout_seconds: 7
                }
            )
        ));
        assert!(matches!(
            parse_line("wait-ssh-group-run --window 11 --tab 12 --run 4").unwrap(),
            (
                None,
                Request::WaitSshGroupRun {
                    timeout_seconds: 30,
                    ..
                }
            )
        ));
        assert!(matches!(
            parse_line("wait-ssh-group-output --window 11 --tab 12 --run 4 --pane 5 --after-line 0 --regex --match-case -- --ready").unwrap(),
            (
                None,
                Request::WaitSshGroupOutput {
                    window_id: 11,
                    tab_id: 12,
                    run_id: 4,
                    pane: Some(5),
                    after_line: Some(0),
                    pattern,
                    regex: true,
                    match_case: true,
                    timeout_seconds: 30,
                }
            ) if pattern == "--ready"
        ));
        assert!(matches!(
            parse_line("get-ssh-group-retry-plan --window 11 --tab 12 --run 4").unwrap(),
            (
                None,
                Request::GetSshGroupRetryPlan {
                    window_id: 11,
                    tab_id: 12,
                    run_id: 4
                }
            )
        ));
        assert!(matches!(
            parse_line("retry-ssh-group-run --window 11 --tab 12 --run 4 --pane 8 --pane 5").unwrap(),
            (None, Request::RetrySshGroupRun { window_id: 11, tab_id: 12, run_id: 4, pane_ids })
                if pane_ids == [8, 5]
        ));
        assert!(matches!(
            parse_line("wait-ssh-group-approval --window 11 --tab 12 --run 4 --timeout 8").unwrap(),
            (
                None,
                Request::WaitSshGroupApproval {
                    window_id: 11,
                    tab_id: 12,
                    run_id: 4,
                    timeout_seconds: 8
                }
            )
        ));
        assert!(matches!(
            parse_line("--pid 7 get-ssh-group-run --tab 12 --run 4 --window 11").unwrap(),
            (
                Some(7),
                Request::GetSshGroupRun {
                    window_id: 11,
                    tab_id: 12,
                    run_id: 4
                }
            )
        ));
        assert!(matches!(
            parse_line("interrupt-ssh-group-command --window 11 --tab 12 --run 4 --pane 8")
                .unwrap(),
            (
                None,
                Request::InterruptSshGroupCommand {
                    window_id: 11,
                    tab_id: 12,
                    run_id: 4,
                    pane_id: 8,
                }
            )
        ));
        for invalid in [
            "--pane 7 list-ssh-groups",
            "list-ssh-groups --window 2",
            "get-ssh-group-roster --tab 2",
            "get-ssh-group-roster --window 2 --tab 3 --run 1",
            "get-ssh-group-roster --window 2 --tab 3 --pane 1",
            "wait-ssh-group-ready --window 2 --tab 3 --run 1",
            "wait-ssh-group-ready --window 2 --tab 3 --pane 0",
            "wait-ssh-group-ready --window 2 --tab 3 --pane 8 --pane 8",
            "wait-ssh-group-ready --window 2 --tab 3 --timeout 0",
            "wait-ssh-group-ready --window 2 --tab 3 --timeout 3601",
            "get-ssh-group-run --window 2 --tab 3",
            "get-ssh-group-run --window 2 --tab 0 --run 4",
            "get-ssh-group-run --window 2 --tab 3 --run 4 --run 5",
            "get-ssh-group-run --window 2 --tab 3 --run 4 --pane 7",
            "get-ssh-group-run --window 2 --tab 3 --run 4 --timeout 1",
            "get-ssh-group-retry-plan --window 2 --tab 3",
            "get-ssh-group-retry-plan --window 2 --tab 3 --run 4 --pane 7",
            "retry-ssh-group-run --window 2 --tab 3 --run 4",
            "retry-ssh-group-run --window 2 --tab 3 --run 4 --pane 7 --pane 7",
            "interrupt-ssh-group-command --window 2 --tab 3 --run 4",
            "interrupt-ssh-group-command --window 2 --tab 3 --run 4 --pane 0",
            "interrupt-ssh-group-command --window 2 --tab 3 --pane 7",
            "interrupt-ssh-group-command --window 2 --tab 3 --run 4 --pane 7 --pane 8",
            "wait-ssh-group-run --window 2 --tab 3 --run 4 --timeout 0",
            "wait-ssh-group-run --window 2 --tab 3 --run 4 --timeout 3601",
            "wait-ssh-group-approval --window 2 --tab 3 --run 4 --timeout 3601",
            "wait-ssh-group-approval --window 2 --tab 3 --run 4 --require-success",
            "wait-ssh-group-output --window 2 --tab 3 --run 4",
            "wait-ssh-group-output --window 2 --tab 3 --run 4 --pane 0 ready",
            "wait-ssh-group-output --window 2 --tab 3 --run 4 --timeout 3601 ready",
            "wait-ssh-group-output --window 2 --tab 3 --run 4 --after-line -1 ready",
            "wait-ssh-group-output --window 2 --tab 3 --run 4 --after-line 0 --after-line 3 ready",
            "get-ssh-group-output --window 2 --tab 3 --run 4 --limit 0",
            "get-ssh-group-output --window 2 --tab 3 --run 4 --limit 129",
            "get-ssh-group-output --window 2 --tab 3 --run 4 --limit 1 --limit 2",
            "get-ssh-group-output --window 2 --tab 3 --run 4 unexpected",
            "follow-ssh-group-output --window 2 --tab 3 --run 4 --timeout 0",
            "follow-ssh-group-output --window 2 --tab 3 --run 4 --limit 1",
        ] {
            assert!(parse_line(invalid).is_err(), "{invalid}");
        }
        assert!(matches!(
            parse_line("get-ssh-group-output --window 11 --tab 12 --run 4 --pane 8 --after-line 5 --limit 16").unwrap(),
            (None, Request::GetSshGroupOutput {
                window_id: 11, tab_id: 12, run_id: 4, pane: Some(8), after_line: Some(5), limit: 16
            })
        ));
        assert!(matches!(
            parse_line("follow-ssh-group-output --window 11 --tab 12 --run 4 --pane 8 --after-line 5 --timeout 60").unwrap(),
            (None, Request::FollowSshGroupOutput {
                window_id: 11, tab_id: 12, run_id: 4, pane: Some(8), after_line: Some(5), timeout_seconds: 60
            })
        ));
    }

    #[test]
    fn playback_requires_one_pane_and_at_most_one_action() {
        use w11term::session::ReplayAction;
        let cli = |line: &str| parse(line.split_ascii_whitespace().map(str::to_owned));
        for (tail, expected) in [
            ("", ReplayAction::Status),
            ("--pause", ReplayAction::Pause),
            ("--resume", ReplayAction::Resume),
            ("--step", ReplayAction::Step),
        ] {
            assert!(
                matches!(cli(&format!("--pid 42 playback --pane 7 {tail}")).unwrap(),
                (Some(42), Request::Playback { pane: 7, action }) if action == expected)
            );
        }
        for args in [
            "playback",
            "playback --pane 1 --step --step",
            "playback --pane 1 --step --resume",
            "playback --pane 1 --all-panes",
            "playback --pane 1 32",
            "inspect --pane 1 --pause",
        ] {
            assert!(cli(args).is_err(), "{args}");
        }
        let request: Request = serde_json::from_str(r#"{"verb":"playback","pane":7}"#).unwrap();
        assert!(matches!(
            request,
            Request::Playback {
                pane: 7,
                action: ReplayAction::Status
            }
        ));
    }

    #[test]
    fn inspect_requires_a_pane_and_accepts_the_optional_process_id() {
        let parse_line = |line: &str| parse(line.split_ascii_whitespace().map(str::to_owned));
        assert!(matches!(
            parse_line("--pid 42 inspect --pane 7").unwrap(),
            (Some(42), Request::Inspect { pane: 7 })
        ));
        assert!(matches!(
            parse_line("--pane 7 inspect").unwrap(),
            (None, Request::Inspect { pane: 7 })
        ));
        for line in [
            "inspect",
            "inspect --pane a",
            "inspect --pane 1 --all-panes",
            "inspect --pane 1 extra",
            "inspect --pane 2 --stdin",
        ] {
            assert!(parse_line(line).is_err(), "{line}");
        }
    }

    #[test]
    fn running_output_requires_an_exact_positive_start_token_and_pane() {
        let parse_line = |line: &str| parse(line.split_ascii_whitespace().map(str::to_owned));
        let (pid, request) =
            parse_line("--pid 42 get-running-output --pane 8 --start-token 27").unwrap();
        let json = serde_json::to_value(request).unwrap();
        assert_eq!(pid, Some(42));
        assert_eq!(
            json,
            serde_json::json!({"verb":"get-running-output","pane":8,"start_token":27})
        );
        assert!(matches!(
            serde_json::from_value(json).unwrap(),
            Request::GetRunningOutput { pane: 8, start_token } if start_token.get() == 27
        ));
        for line in [
            "get-running-output --pane 8",
            "get-running-output --start-token 27",
            "get-running-output --pane 0 --start-token 27",
            "get-running-output --pane 8 --start-token 0",
            "get-running-output --pane 8 --start-token -1",
            "get-running-output --pane 8 --start-token 27 --start-token 28",
            "get-running-output --pane 8 --pane 9 --start-token 27",
        ] {
            assert!(parse_line(line).is_err(), "{line}");
        }
    }

    #[test]
    fn running_output_wait_accepts_match_options_and_valid_timeout() {
        let parse_line = |line: &str| parse(line.split_ascii_whitespace().map(str::to_owned));
        let (_, request) = parse_line("wait-running-output --pane 8 --start-token 27 --regex --match-case --timeout 7 -- ^ready$").unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({"verb":"wait-running-output","pane":8,"start_token":27,"pattern":"^ready$","regex":true,"match_case":true,"timeout_seconds":7})
        );
        for line in [
            "wait-running-output --pane 8 --start-token 0 ready",
            "wait-running-output --pane 8 --start-token 27",
            "wait-running-output --pane 8 --start-token 27 --timeout 0 ready",
            "wait-running-output --pane 8 --start-token 27 --timeout 3601 ready",
            "wait-running-output --pane 8 --start-token 27 --timeout 1 --timeout 2 ready",
            "get-running-output --pane 8 --start-token 27 --regex ready",
        ] {
            assert!(parse_line(line).is_err(), "{line}");
        }
    }

    #[test]
    fn interrupt_command_requires_a_positive_pane_and_start_token() {
        let parse_line = |line: &str| parse(line.split_ascii_whitespace().map(str::to_owned));
        let (pid, request) =
            parse_line("--pid 42 interrupt-command --pane 8 --start-token 27").unwrap();
        assert_eq!(pid, Some(42));
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({"verb":"interrupt-command","pane":8,"start_token":27})
        );
        for line in [
            "interrupt-command --pane 8",
            "interrupt-command --start-token 27",
            "interrupt-command --pane 0 --start-token 27",
            "interrupt-command --pane 8 --start-token 0",
            "interrupt-command --pane 8 --start-token 27 --start-token 28",
            "interrupt-command --pane 8 --pane 9 --start-token 27",
            "interrupt-command --pane 8 --start-token 27 --connection-generation 1",
        ] {
            assert!(parse_line(line).is_err(), "{line}");
        }
    }

    #[test]
    fn wait_command_start_token_is_exclusive_and_preserves_success_checks() {
        let parse_line = |line: &str| parse(line.split_ascii_whitespace().map(str::to_owned));
        let invocation = parse_invocation(
            "--pid 9 wait-command --pane 8 --start-token 27 --timeout 7 --require-success"
                .split_ascii_whitespace()
                .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(invocation.pid, Some(9));
        assert_eq!(invocation.success_check, Some(SuccessCheck::Command));
        assert_eq!(
            serde_json::to_value(invocation.request).unwrap(),
            serde_json::json!({"verb":"wait-command-capture","pane":8,"start_token":27,"timeout_seconds":7})
        );
        for line in [
            "wait-command --pane 8 --start-token 0",
            "wait-command --pane 0 --start-token 27",
            "wait-command --pane 8 --start-token 27 --after-id 4",
            "wait-command --pane 8 --start-token 27 --connection-generation 1",
            "wait-command --pane 8 --start-token 27 --command build",
            "wait-command --pane 8 --start-token 27 --start-token 28",
            "wait-command --pane 8 --start-token 27 --timeout 0",
        ] {
            assert!(parse_line(line).is_err(), "{line}");
        }
    }

    #[test]
    fn single_pane_ssh_wait_accepts_a_generation_floor_and_valid_timeout() {
        let parse_line = |line: &str| parse(line.split_ascii_whitespace().map(str::to_owned));
        assert!(matches!(
            parse_line("--pid 9 wait-ssh-connected --pane 7").unwrap(),
            (
                Some(9),
                Request::WaitSshConnected {
                    pane: 7,
                    after_generation: 0,
                    timeout_seconds: 30
                }
            )
        ));
        assert!(matches!(
            parse_line("wait-ssh-connected --pane 7 --after-generation 4 --timeout 15").unwrap(),
            (
                None,
                Request::WaitSshConnected {
                    pane: 7,
                    after_generation: 4,
                    timeout_seconds: 15
                }
            )
        ));
        for line in [
            "wait-ssh-connected",
            "wait-ssh-connected --pane 0",
            "wait-ssh-connected --pane 7 --pane 8",
            "wait-ssh-connected --pane 7 --after-generation -1",
            "wait-ssh-connected --pane 7 --after-generation 1 --after-generation 2",
            "wait-ssh-connected --pane 7 --timeout 0",
            "wait-ssh-connected --pane 7 --timeout 3601",
            "wait-ssh-connected --pane 7 --timeout 4 --timeout 5",
        ] {
            assert!(parse_line(line).is_err(), "{line}");
        }
    }

    #[test]
    fn pane_exit_wait_accepts_only_its_scoped_options() {
        let parse_line = |line: &str| parse(line.split_ascii_whitespace().map(str::to_owned));
        assert!(matches!(
            parse_line("--pid 9 wait-pane-exit --pane 7").unwrap(),
            (
                Some(9),
                Request::WaitPaneExit {
                    pane: 7,
                    after_exit_token: 0,
                    timeout_seconds: 30
                }
            )
        ));
        assert!(matches!(
            parse_line("wait-pane-exit --pane 7 --after-exit-token 4 --timeout 60").unwrap(),
            (
                None,
                Request::WaitPaneExit {
                    pane: 7,
                    after_exit_token: 4,
                    timeout_seconds: 60
                }
            )
        ));
        for line in [
            "wait-pane-exit",
            "wait-pane-exit --pane 7 --after-exit-token -1",
            "wait-pane-exit --pane 7 --after-exit-token 1 --after-exit-token 2",
            "wait-pane-exit --pane 7 --after-generation 1",
            "wait-pane-exit --pane 7 --timeout 3601",
            "wait-ssh-connected --pane 7 --after-exit-token 1",
        ] {
            assert!(parse_line(line).is_err(), "{line}");
        }
    }

    #[test]
    fn command_history_requires_the_listed_pane_id_and_generation_for_lookup() {
        use w11term::control::CommandStatus;
        let parse_line = |line: &str| parse(line.split_ascii_whitespace().map(str::to_owned));
        assert!(matches!(
            parse_line("--pane 2 list-commands --pid 7").unwrap(),
            (
                Some(7),
                Request::ListCommands {
                    pane: 2,
                    status: CommandStatus::All
                }
            )
        ));
        for (value, expected) in [
            ("all", CommandStatus::All),
            ("failed", CommandStatus::Failed),
            ("succeeded", CommandStatus::Succeeded),
            ("unknown", CommandStatus::Unknown),
        ] {
            assert!(matches!(
                parse_line(&format!("list-commands --status {value} --pane 2")).unwrap(),
                (None, Request::ListCommands { pane: 2, status }) if status == expected
            ));
        }
        assert!(matches!(
            parse_line("--pid 7 get-command --id 53 --connection-generation 0 --pane 2").unwrap(),
            (
                Some(7),
                Request::GetCommand {
                    pane: 2,
                    id: 53,
                    connection_generation: 0,
                    output_file: None
                }
            )
        ));
        for line in [
            "list-commands",
            "list-commands --pane 2 --id 53",
            "list-commands --pane 2 --connection-generation 1",
            "list-commands --pane 2 --status",
            "list-commands --pane 2 --status running",
            "list-commands --pane 2 --status failed --status succeeded",
            "get-command --pane 2 --id 53 --connection-generation 0 --status failed",
            "get-command --pane 2 --id 53",
            "get-command --pane 2 --connection-generation 0",
            "get-command --id 53 --connection-generation 1",
            "get-command --pane 2 --id 53 --connection-generation -1",
            "get-command --pane 2 --id foo --connection-generation 1",
        ] {
            assert!(parse_line(line).is_err(), "{line}");
        }
    }

    #[test]
    fn wait_command_requires_a_pane_and_explicit_cursor() {
        let parse_line = |line: &str| parse(line.split_ascii_whitespace().map(str::to_owned));
        assert!(matches!(
            parse_line("wait-command --pane 7 --after-id 0 --connection-generation 0 --timeout 2")
                .unwrap(),
            (
                None,
                Request::WaitCommand {
                    pane: 7,
                    after_id: 0,
                    connection_generation: 0,
                    timeout_seconds: 2,
                    command: None,
                    output_file: None
                }
            )
        ));
        for line in [
            "wait-command --pane 7 --connection-generation 0",
            "wait-command --pane 7 --after-id 0",
            "wait-command --after-id 0 --connection-generation 0",
            "wait-command --pane 7 --after-id -1 --connection-generation 0",
            "wait-command --pane 7 --after-id 0 --after-id 1 --connection-generation 0",
            "wait-command --pane 7 --after-id 0 --connection-generation 0 --connection-generation 1",
            "wait-command --pane 7 --after-id 0 --connection-generation 0 --timeout 0",
        ] {
            assert!(parse_line(line).is_err(), "{line}");
        }
    }

    #[test]
    fn exact_command_wait_preserves_text_and_can_check_shell_success() {
        let command = "Build  Ω";
        let invocation = parse_invocation(
            [
                "wait-command",
                "--pane",
                "7",
                "--after-id",
                "5",
                "--connection-generation",
                "1",
                "--command",
                command,
                "--require-success",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(invocation.success_check, Some(SuccessCheck::Command));
        let request = serde_json::to_value(invocation.request).unwrap();
        assert_eq!(request["command"], command);
        assert!(request.get("require_success").is_none());

        for (code, expected) in [
            (Some(0), ExitCode::SUCCESS),
            (Some(17), ExitCode::from(2)),
            (None, ExitCode::from(2)),
        ] {
            let reply = Reply::from_result(Ok(serde_json::json!({"exit_code":code})));
            assert_eq!(cli_exit_code(&reply, Some(SuccessCheck::Command)), expected);
            assert_eq!(cli_exit_code(&reply, None), ExitCode::SUCCESS);
        }
        let timeout = Reply::from_result(Err(anyhow::anyhow!("timeout")));
        assert_eq!(
            cli_exit_code(&timeout, Some(SuccessCheck::Command)),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn command_capture_file_flags_keep_paths_and_require_an_explicit_destination() {
        let destination = "test folder/output.txt";
        for options in [
            ["get-command", "--id", "9"],
            ["wait-command", "--after-id", "8"],
        ] {
            let parse_options = |extra: &[&str]| {
                parse(
                    options
                        .into_iter()
                        .chain(["--pane", "7", "--connection-generation", "1"])
                        .chain(extra.iter().copied())
                        .map(str::to_owned),
                )
            };
            let (_, request) =
                parse_options(&["--overwrite", "--output-file", destination]).unwrap();
            assert_eq!(
                serde_json::to_value(request).unwrap()["output_file"],
                serde_json::json!({"path": std::path::absolute(destination).unwrap(), "overwrite": true})
            );
            for extra in [
                vec!["--overwrite"],
                vec!["--output-file"],
                vec!["--output-file", "--overwrite"],
                vec!["--output-file", ""],
                vec!["--output-file", destination, "--output-file", "duplicate"],
            ] {
                assert!(parse_options(&extra).is_err(), "{options:?} {extra:?}");
            }
        }
    }

    #[test]
    fn find_text_requires_an_explicit_scope_and_keeps_literal_input() {
        let parse_args = |args: &[&str]| parse(args.iter().map(|arg| arg.to_string()));
        let (pid, request) = parse_args(&[
            "--pid",
            "42",
            "--pane",
            "7",
            "find-text",
            "--",
            "--Some Text",
        ])
        .unwrap();
        assert_eq!(pid, Some(42));
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({"verb":"find-text","pane":7,"all_panes":false,"pattern":"--Some Text"})
        );
        assert!(
            matches!(request, Request::FindText { pattern, pane: Some(7), all_panes: false, regex: false, match_case: false } if pattern == "--Some Text")
        );
        assert!(matches!(
            parse_args(&["find-text", "--all-panes", "  x "]).unwrap().1,
            Request::FindText { pattern, pane: None, all_panes: true, .. } if pattern == "  x "
        ));
        assert!(matches!(
            parse_args(&["find-text", "--all-panes", "--match-case", "--regex", "--", "--Err[0-9]+"]).unwrap().1,
            Request::FindText { pattern, pane: None, all_panes: true, regex: true, match_case: true } if pattern == "--Err[0-9]+"
        ));
        for args in [
            &["find-text", "x"][..],
            &["find-text", "--pane", "7", "--all-panes", "x"],
            &["find-text", "--all-panes", ""],
            &["find-text", "--all-panes", "--regex"],
            &["wait-for-text", "--pane", "7", "--match-case", "x"],
        ] {
            assert!(parse_args(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn wait_for_text_accepts_an_optional_positive_connection_generation() {
        let parse_line = |line: &str| parse(line.split_ascii_whitespace().map(str::to_owned));
        let (_, request) =
            parse_line("wait-for-text --pane 7 --connection-generation 4 --regex ready").unwrap();
        assert!(matches!(
            request,
            Request::WaitForText { pane: 7, connection_generation: Some(generation), regex: true, .. }
                if generation.get() == 4
        ));
        let (_, unscoped) = parse_line("wait-for-text --pane 7 ready").unwrap();
        assert!(
            serde_json::to_value(unscoped)
                .unwrap()
                .get("connection_generation")
                .is_none()
        );
        for line in [
            "wait-for-text --pane 7 --connection-generation 0 ready",
            "wait-for-text --pane 7 --connection-generation 1 --connection-generation 2 ready",
            "find-text --pane 7 --connection-generation 4 ready",
        ] {
            assert!(parse_line(line).is_err(), "{line}");
        }
    }

    #[test]
    fn wait_for_text_parses_a_frame_cursor_alongside_connection_scope() {
        let parse_line = |line: &str| parse(line.split_ascii_whitespace().map(str::to_owned));
        let (_, request) =
            parse_line("wait-for-text --pane 7 --after-frame 15 --connection-generation 4 ready")
                .unwrap();
        assert!(matches!(
            request,
            Request::WaitForText {
                pane: 7,
                after_frame: Some(15),
                connection_generation: Some(generation),
                ..
            } if generation.get() == 4
        ));
        for line in [
            "wait-for-text --pane 7 --after-frame -1 ready",
            "wait-for-text --pane 7 --after-frame 1 --after-frame 2 ready",
            "events --pane 7 --after-frame 15",
        ] {
            assert!(parse_line(line).is_err(), "{line}");
        }
    }

    #[test]
    fn close_pane_requires_one_explicit_positive_id() {
        let parse_args = |args: &[&str]| parse(args.iter().map(|arg| arg.to_string()));
        assert!(matches!(
            parse_args(&["--pid", "5", "close-pane", "--pane", "7"]).unwrap(),
            (Some(5), Request::ClosePane { pane: 7 })
        ));
        for args in [
            &["close-pane"][..],
            &["close-pane", "--pane", "0"],
            &["close-pane", "--pane", "3", "--pane", "7"],
            &["--pane", "3", "--pane", "7", "close-pane"],
            &["--pane", "3", "close-pane", "--pane", "7"],
            &["close-pane", "--pane", "7", "--activate"],
        ] {
            assert!(parse_args(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn events_accepts_one_exact_optional_kind() {
        let parse_args = |args: &[&str]| parse(args.iter().map(|arg| arg.to_string()));
        let (_, request) = parse_args(&[
            "events",
            "--pane",
            "7",
            "--since",
            "42",
            "--kind",
            "bell",
            "--timeout",
            "5",
        ])
        .unwrap();
        assert!(matches!(
            request,
            Request::Events { pane: 7, since: 42, kind: Some(kind), timeout_seconds: 5 }
                if kind == "bell"
        ));
        let (_, unfiltered) = parse_args(&["events", "--pane", "7"]).unwrap();
        assert!(
            serde_json::to_value(unfiltered)
                .unwrap()
                .get("kind")
                .is_none()
        );
        for args in [
            &["events", "--pane", "7", "--kind"][..],
            &["events", "--pane", "7", "--kind", ""],
            &["events", "--pane", "7", "--kind", "bell", "--kind", "sound"],
            &["wait-for-text", "--pane", "7", "--kind", "bell", "ready"],
        ] {
            assert!(parse_args(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn wait_for_text_resolves_its_optional_screenshot_path() {
        let parse_args = |args: &[&str]| parse(args.iter().map(|arg| arg.to_string()));
        let (_, request) = parse_args(&[
            "wait-for-text",
            "--pane",
            "7",
            "--screenshot",
            "match.png",
            "--",
            "ready",
        ])
        .unwrap();
        assert!(matches!(
            request,
            Request::WaitForText { screenshot_path: Some(path), .. }
                if path == std::path::absolute("match.png").unwrap()
        ));
        let (_, text_only) = parse_args(&["wait-for-text", "--pane", "7", "ready"]).unwrap();
        assert!(
            serde_json::to_value(text_only)
                .unwrap()
                .get("screenshot_path")
                .is_none()
        );
        for args in [
            &["wait-for-text", "--pane", "7", "ready", "--screenshot"][..],
            &[
                "wait-for-text",
                "--pane",
                "7",
                "--screenshot",
                "a",
                "--screenshot",
                "b",
                "ready",
            ],
            &[
                "find-text",
                "--pane",
                "7",
                "--screenshot",
                "match.png",
                "ready",
            ],
        ] {
            assert!(parse_args(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn raw_bytes_and_required_options() {
        let p = |a: &[&str]| parse(a.iter().map(|s| s.to_string()));
        let (
            _,
            Request::SendText {
                data_base64,
                connection_generation,
                ..
            },
        ) = p(&[
            "send-text",
            "--pane",
            "3",
            "--no-paste",
            "--connection-generation",
            "4",
            "--base64",
            "AAcbW0P/",
        ])
        .unwrap()
        else {
            panic!()
        };
        assert_eq!(STANDARD.decode(data_base64).unwrap(), b"\0\x07\x1b[C\xff");
        assert_eq!(connection_generation.unwrap().get(), 4);
        let (_, unscoped) = p(&["send-text", "--pane", "3", "--no-paste", "hello"]).unwrap();
        let json = serde_json::to_value(unscoped).unwrap();
        assert!(json.get("connection_generation").is_none());
        assert!(p(&["send-text", "--pane", "3", "hello"]).is_err());
        for args in [
            &[
                "send-text",
                "--pane",
                "3",
                "--no-paste",
                "--connection-generation",
                "0",
                "hello",
            ][..],
            &[
                "send-text",
                "--pane",
                "3",
                "--no-paste",
                "--connection-generation",
                "4",
                "--connection-generation",
                "5",
                "hello",
            ],
            &["get-text", "--pane", "3", "--connection-generation", "4"],
        ] {
            assert!(p(args).is_err(), "{args:?}");
        }
        assert!(p(&["get-text"]).is_err());
        assert!(p(&["list", "--file", "missing"]).is_err());
    }
}
