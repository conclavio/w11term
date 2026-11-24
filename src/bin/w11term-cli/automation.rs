use anyhow::{Context, Result, bail, ensure};
use w11term::control::{CommandStatus, Request};
use w11term::session::ReplayAction;
pub const HELP: &str = "w11term-cli [--pid PID] inspect --pane ID\nw11term-cli [--pid PID] playback --pane ID [--pause | --resume | --step]\nw11term-cli [--pid PID] find-text (--pane ID | --all-panes) [--regex] [--match-case] [--] TEXT\nw11term-cli [--pid PID] wait-for-text --pane ID [--regex] [--timeout SECONDS] TEXT\nw11term-cli [--pid PID] events --pane ID [--since CURSOR] [--timeout SECONDS]\nw11term-cli [--pid PID] resize --pane ID --cols N --rows N\nw11term-cli [--pid PID] open --profile NAME [--cols N --rows N] [--activate]\nw11term-cli [--pid PID] record --pane ID ([--timed] PATH | --stop)\nw11term-cli [--pid PID] save-text --pane ID PATH [--overwrite]\nw11term-cli [--pid PID] list-commands --pane ID [--status all|failed|succeeded|unknown]\nw11term-cli [--pid PID] get-command --pane ID --id ID --connection-generation N\nw11term-cli [--pid PID] list-ssh-groups\nw11term-cli [--pid PID] get-ssh-group-run --window ID --tab ID --run ID\nw11term-cli [--pid PID] restart --pane ID --exit-token TOKEN";
pub fn parse(
    verb: &str,
    args: impl Iterator<Item = String>,
    mut pid: Option<u32>,
    mut pane: Option<u64>,
) -> Result<(Option<u32>, Request)> {
    let mut args = args.peekable();
    let mut cols = None;
    let mut rows = None;
    let mut profile = None;
    let mut activate = false;
    let mut all_panes = false;
    let mut regex = false;
    let mut match_case = false;
    let mut timeout = None;
    let mut since = None;
    let mut text = None;
    let mut stop = false;
    let mut timed = false;
    let mut playback = None;
    let mut overwrite = false;
    let mut exit_token = None;
    let mut command_id = None;
    let mut status = None;
    let mut connection_generation = None;
    let mut window_id = None;
    let mut tab_id = None;
    let mut run_id = None;
    while let Some(arg) = args.next() {
        let value = |args: &mut dyn Iterator<Item = String>| {
            args.next()
                .with_context(|| format!("{arg} requires a value"))
        };
        match arg.as_str() {
            "--pid" => pid = Some(value(&mut args)?.parse()?),
            "--pane" => pane = Some(value(&mut args)?.parse()?),
            "--all-panes" if verb == "find-text" => all_panes = true,
            "--cols" if matches!(verb, "open" | "resize") => {
                cols = Some(value(&mut args)?.parse()?)
            }
            "--rows" if matches!(verb, "open" | "resize") => {
                rows = Some(value(&mut args)?.parse()?)
            }
            "--profile" if verb == "open" => profile = Some(value(&mut args)?),
            "--activate" if verb == "open" => activate = true,
            "--regex" if matches!(verb, "find-text" | "wait-for-text") => regex = true,
            "--match-case" if verb == "find-text" => match_case = true,
            "--timeout" if matches!(verb, "wait-for-text" | "events") => {
                timeout = Some(value(&mut args)?.parse()?)
            }
            "--since" if verb == "events" => since = Some(value(&mut args)?.parse()?),
            "--stop" if verb == "record" => stop = true,
            "--timed" if verb == "record" => timed = true,
            "--pause" | "--resume" | "--step" if verb == "playback" => {
                ensure!(playback.is_none(), "choose one playback action");
                playback = Some(match arg.as_str() {
                    "--pause" => ReplayAction::Pause,
                    "--resume" => ReplayAction::Resume,
                    _ => ReplayAction::Step,
                });
            }
            "--overwrite" if verb == "save-text" => overwrite = true,
            "--exit-token" if verb == "restart" => exit_token = Some(value(&mut args)?.parse()?),
            "--id" if verb == "get-command" => command_id = Some(value(&mut args)?.parse()?),
            "--status" if verb == "list-commands" => {
                ensure!(status.is_none(), "specify --status only once");
                status = Some(
                    serde_json::from_value::<CommandStatus>(value(&mut args)?.into())
                        .context("--status expects all, failed, succeeded or unknown")?,
                );
            }
            "--connection-generation" if verb == "get-command" => {
                connection_generation = Some(value(&mut args)?.parse()?)
            }
            "--window" | "--tab" | "--run" if verb == "get-ssh-group-run" => {
                let id: u64 = value(&mut args)?
                    .parse()
                    .with_context(|| format!("{arg} requires a positive integer"))?;
                ensure!(id != 0, "{arg} requires a positive integer");
                let slot = match arg.as_str() {
                    "--window" => &mut window_id,
                    "--tab" => &mut tab_id,
                    _ => &mut run_id,
                };
                ensure!(slot.replace(id).is_none(), "specify {arg} only once");
            }
            "--" if matches!(verb, "find-text" | "wait-for-text") => {
                ensure!(text.is_none(), "only one pattern is accepted");
                text = Some(value(&mut args)?);
                ensure!(args.next().is_none(), "unexpected argument after pattern");
            }
            _ if !arg.starts_with("--")
                && matches!(verb, "find-text" | "wait-for-text" | "record" | "save-text") =>
            {
                ensure!(text.is_none(), "only one pattern/path is accepted");
                text = Some(arg);
            }
            _ => bail!("unsupported {verb} option {arg}"),
        }
    }
    let request = match verb {
        "open" => {
            ensure!(pane.is_none(), "open does not accept --pane");
            Request::Open {
                profile: profile.context("--profile is required")?,
                cols,
                rows,
                activate,
            }
        }
        "resize" => Request::Resize {
            pane: pane.context("--pane is required")?,
            cols: cols.context("--cols is required")?,
            rows: rows.context("--rows is required")?,
        },
        "events" => Request::Events {
            pane: pane.context("--pane is required")?,
            since: since.unwrap_or(0),
            timeout_seconds: timeout.unwrap_or(0),
        },
        "find-text" => {
            ensure!(
                all_panes != pane.is_some(),
                "specify --pane ID or --all-panes"
            );
            let pattern = text.context("search text or regular expression is required")?;
            ensure!(!pattern.is_empty(), "search text is empty");
            ensure!(
                pattern.len() <= w11term::search::MAX_QUERY_BYTES,
                "search text exceeds {} UTF-8 bytes",
                w11term::search::MAX_QUERY_BYTES
            );
            Request::FindText {
                pattern,
                pane,
                all_panes,
                regex,
                match_case,
            }
        }
        "wait-for-text" => Request::WaitForText {
            pane: pane.context("--pane is required")?,
            pattern: text.context("a literal pattern or regular expression is required")?,
            regex,
            timeout_seconds: timeout.unwrap_or(30),
        },
        "inspect" => Request::Inspect {
            pane: pane.context("--pane is required")?,
        },
        "playback" => Request::Playback {
            pane: pane.context("--pane is required")?,
            action: playback.unwrap_or_default(),
        },
        "list-commands" => Request::ListCommands {
            pane: pane.context("--pane is required")?,
            status: status.unwrap_or_default(),
        },
        "get-command" => Request::GetCommand {
            pane: pane.context("--pane is required")?,
            id: command_id.context("--id is required (from list-commands)")?,
            connection_generation: connection_generation
                .context("--connection-generation is required (from list-commands)")?,
        },
        "list-ssh-groups" => {
            ensure!(pane.is_none(), "list-ssh-groups does not accept --pane");
            Request::ListSshGroups
        }
        "get-ssh-group-run" => {
            ensure!(pane.is_none(), "get-ssh-group-run does not accept --pane");
            Request::GetSshGroupRun {
                window_id: window_id.context("--window is required (from list-ssh-groups)")?,
                tab_id: tab_id.context("--tab is required (from list-ssh-groups)")?,
                run_id: run_id.context("--run is required (from list-ssh-groups)")?,
            }
        }
        "restart" => Request::Restart {
            pane: pane.context("--pane is required")?,
            exit_token: exit_token.context("--exit-token is required (from events)")?,
        },
        "save-text" => Request::SaveText {
            pane: pane.context("--pane is required")?,
            path: std::path::absolute(text.context("save-text requires a destination path")?)?,
            overwrite,
        },
        "record" => {
            ensure!(stop != text.is_some(), "provide PATH or --stop");
            ensure!(!timed || !stop, "--timed requires a recording path");
            Request::Record {
                pane: pane.context("--pane is required")?,
                path: text.map(std::path::absolute).transpose()?,
                timed,
            }
        }
        _ => bail!("unknown automation command"),
    };
    Ok((pid, request))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timed_recording_is_opt_in_and_old_record_requests_round_trip() {
        let record =
            |args: &[&str]| parse("record", args.iter().map(|s| s.to_string()), None, Some(7));
        let (_, timed) = record(&["--timed", "my capture.vt"]).unwrap();
        assert!(
            matches!(timed, Request::Record { pane: 7, timed: true, path: Some(path) } if path.is_absolute())
        );
        assert!(record(&["--timed", "--stop"]).is_err());
        assert!(record(&["--timed"]).is_err());
        let stop = serde_json::json!({"verb":"record","pane":7,"path":null});
        let decoded: Request = serde_json::from_value(stop.clone()).unwrap();
        assert!(matches!(decoded, Request::Record { timed: false, .. }));
        assert_eq!(serde_json::to_value(decoded).unwrap(), stop);
        assert_eq!(
            serde_json::to_value(record(&["--stop"]).unwrap().1).unwrap(),
            stop
        );
    }

    #[test]
    fn rejects_cross_command_flags_and_missing_values() {
        let p = |v, a: &[&str]| parse(v, a.iter().map(|s| s.to_string()), None, None);
        assert!(matches!(
            p("wait-for-text", &["--pane", "1", "--regex", "fps: [0-9]+"])
                .unwrap()
                .1,
            Request::WaitForText { regex: true, .. }
        ));
        assert!(p("events", &["--pane", "1", "--regex"]).is_err());
        assert!(p("resize", &["--pane", "1", "--cols", "120"]).is_err());
        assert!(p("record", &["--pane", "1", "--stop", "file.vt"]).is_err());
        assert!(p("open", &["--profile", "play", "--pane", "1"]).is_err());
        assert!(
            matches!(p("save-text", &["--pane", "2", "shell log.txt"]).unwrap().1,
            Request::SaveText { pane: 2, path, overwrite: false } if path.is_absolute())
        );
        assert!(matches!(
            p("save-text", &["--overwrite", "--pane", "2", "log.txt"])
                .unwrap()
                .1,
            Request::SaveText {
                overwrite: true,
                ..
            }
        ));
        assert!(p("save-text", &["--pane", "2"]).is_err());
        assert!(p("record", &["--pane", "2", "--overwrite", "log.txt"]).is_err());
        assert!(matches!(
            p("restart", &["--pane", "2", "--exit-token", "13"])
                .unwrap()
                .1,
            Request::Restart {
                pane: 2,
                exit_token: 13
            }
        ));
        assert!(p("restart", &["--pane", "2"]).is_err());
        assert!(p("restart", &["--exit-token", "2"]).is_err());
        assert!(p("events", &["--pane", "2", "--exit-token", "1"]).is_err());
    }
}
