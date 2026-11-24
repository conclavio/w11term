//! Console companion: stdout always contains exactly one JSON reply (including errors).
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{io::Read, path::PathBuf};
use w11term::control::{self, Reply, Request};
#[path = "w11term-cli/automation.rs"]
mod automation;
#[path = "w11term-cli/sftp.rs"]
mod sftp_args;

const HELP: &str = "w11term-cli [--pid PID] list\nw11term-cli [--pid PID] send-text --pane ID --no-paste [TEXT | --base64 BASE64 | --file PATH | --stdin]\nw11term-cli [--pid PID] get-text --pane ID\nw11term-cli [--pid PID] screenshot --pane ID PATH\nw11term-cli instances\n\nTEXT is literal UTF-8 (no escape interpretation). Use --base64/--file/--stdin for arbitrary bytes.\nWithout --pid, exactly one live instance is required. No command starts a GUI or changes focus.";

fn parse(args: impl IntoIterator<Item = String>) -> Result<(Option<u32>, Request)> {
    let mut args = args.into_iter();
    let mut pid = None;
    let mut verb = None;
    let mut pane = None;
    let mut no_paste = false;
    let mut payload = None;
    let mut path = None;
    while let Some(arg) = args.next() {
        let value = |args: &mut dyn Iterator<Item = String>| -> Result<String> {
            args.next()
                .with_context(|| format!("{arg} requires a value"))
        };
        match arg.as_str() {
            "find-text" | "wait-for-text" | "events" | "open" | "resize" | "record"
            | "save-text" | "restart" | "list-commands" | "get-command" | "inspect"
            | "playback" | "list-ssh-groups" | "get-ssh-group-run" | "wait-ssh-group-run"
                if verb.is_none() =>
            {
                ensure!(
                    payload.is_none() && path.is_none() && !no_paste,
                    "invalid options before automation command"
                );
                return automation::parse(&arg, args, pid, pane);
            }
            "sftp" if verb.is_none() => {
                ensure!(
                    payload.is_none() && path.is_none() && !no_paste,
                    "sftp accepts only --pid and --pane before the command"
                );
                return sftp_args::parse(args, pid, pane);
            }
            "--pid" => pid = Some(value(&mut args)?.parse()?),
            "--pane" => pane = Some(value(&mut args)?.parse()?),
            "--no-paste" => no_paste = true,
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
        "list" => {
            ensure!(
                pane.is_none() && payload.is_none() && path.is_none() && !no_paste,
                "list takes no pane/input options"
            );
            Request::List
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
    Ok((pid, request))
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
async fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--help"] || args == ["-h"] {
        println!("{HELP}\n\n{}\n\n{}", sftp_args::HELP, automation::HELP);
        return;
    }
    let result = if args == ["instances"] {
        control::discover().await.map(|instances| {
            Reply::from_result(serde_json::to_value(instances).map_err(Into::into))
        })
    } else {
        match parse(args) {
            Ok((pid, request)) => control::call(pid, &request).await,
            Err(e) => Err(e),
        }
    };
    let reply = result.unwrap_or_else(|e| Reply::from_result(Err(e)));
    println!("{}", serde_json::to_string(&reply).unwrap());
    if !reply.ok {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ssh_group_discovery_and_scoped_run_ids() {
        let parse_line = |line: &str| parse(line.split_ascii_whitespace().map(str::to_owned));
        assert!(matches!(
            parse_line("list-ssh-groups --pid 7").unwrap(),
            (Some(7), Request::ListSshGroups)
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
        for invalid in [
            "--pane 7 list-ssh-groups",
            "list-ssh-groups --window 2",
            "get-ssh-group-run --window 2 --tab 3",
            "get-ssh-group-run --window 2 --tab 0 --run 4",
            "get-ssh-group-run --window 2 --tab 3 --run 4 --run 5",
            "get-ssh-group-run --window 2 --tab 3 --run 4 --pane 7",
            "get-ssh-group-run --window 2 --tab 3 --run 4 --timeout 1",
            "wait-ssh-group-run --window 2 --tab 3 --run 4 --timeout 0",
            "wait-ssh-group-run --window 2 --tab 3 --run 4 --timeout 3601",
        ] {
            assert!(parse_line(invalid).is_err(), "{invalid}");
        }
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
                    connection_generation: 0
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
    fn raw_bytes_and_required_options() {
        let p = |a: &[&str]| parse(a.iter().map(|s| s.to_string()));
        let (_, Request::SendText { data_base64, .. }) = p(&[
            "send-text",
            "--pane",
            "3",
            "--no-paste",
            "--base64",
            "AAcbW0P/",
        ])
        .unwrap() else {
            panic!()
        };
        assert_eq!(STANDARD.decode(data_base64).unwrap(), b"\0\x07\x1b[C\xff");
        assert!(p(&["send-text", "--pane", "3", "hello"]).is_err());
        assert!(p(&["get-text"]).is_err());
        assert!(p(&["list", "--file", "missing"]).is_err());
    }
}
