use super::Invocation;
use anyhow::{Context, Result, bail, ensure};
use w11term::control::{
    CommandStatus, Request, SshGroupExpectedOutput, SshGroupQueueAction, SshGroupReportFormat,
    validate_group_panes, validate_group_submission,
};
use w11term::session::ReplayAction;
pub const HELP: &str = "w11term-cli [--pid PID] inspect --pane ID\nw11term-cli [--pid PID] playback --pane ID [--pause | --resume | --step]\nw11term-cli [--pid PID] find-text (--pane ID | --all-panes) [--regex] [--match-case] [--] TEXT\nw11term-cli [--pid PID] wait-for-text --pane ID [--regex] [--timeout SECONDS] TEXT\nw11term-cli [--pid PID] events --pane ID [--since CURSOR] [--timeout SECONDS]\nw11term-cli [--pid PID] resize --pane ID --cols N --rows N\nw11term-cli [--pid PID] open --profile NAME [--cols N --rows N] [--activate]\nw11term-cli [--pid PID] record --pane ID ([--timed] PATH | --stop)\nw11term-cli [--pid PID] save-text --pane ID PATH [--overwrite]\nw11term-cli [--pid PID] list-commands --pane ID [--status all|failed|succeeded|unknown]\nw11term-cli [--pid PID] get-command --pane ID --id ID --connection-generation N\nw11term-cli [--pid PID] list-ssh-groups\nw11term-cli [--pid PID] list-saved-ssh-groups --window ID\nw11term-cli [--pid PID] open-ssh-group --window ID --name NAME\nw11term-cli [--pid PID] get-ssh-group-roster --window ID --tab ID\nw11term-cli [--pid PID] wait-ssh-group-ready --window ID --tab ID [--pane ID ...] [--timeout SECONDS]\nw11term-cli [--pid PID] submit-ssh-group-command --window ID --tab ID --pane ID [--pane ID ...] [--dry-run] [--canary PANE] [--parallel N] [--interval-ms MS] [--queue-expiry-ms MS] [--on-failure stop|continue] [--expect-output TEXT [--regex] [--match-case]] [--record-output PATH] [--] COMMAND\nw11term-cli [--pid PID] get-ssh-group-run --window ID --tab ID --run ID\nw11term-cli [--pid PID] get-ssh-group-retry-plan --window ID --tab ID --run ID\nw11term-cli [--pid PID] retry-ssh-group-run --window ID --tab ID --run ID --pane ID [--pane ID ...]\nw11term-cli [--pid PID] interrupt-ssh-group-command --window ID --tab ID --run ID --pane ID\nw11term-cli [--pid PID] ssh-group-queue --window ID --tab ID --run ID pause|resume|stop\nw11term-cli [--pid PID] ssh-group-queue --window ID --tab ID --run ID approve-canary --pane ID\nw11term-cli [--pid PID] ssh-group-queue --window ID --tab ID --run ID skip --pane ID\nw11term-cli [--pid PID] save-ssh-group-report --window ID --tab ID --run ID [--format json|text] [--overwrite] [--] PATH\nw11term-cli [--pid PID] wait-ssh-group-run --window ID --tab ID --run ID [--timeout SECONDS] [--require-success]\nw11term-cli [--pid PID] wait-ssh-group-approval --window ID --tab ID --run ID [--timeout SECONDS]\nw11term-cli [--pid PID] get-ssh-group-output --window ID --tab ID --run ID [--pane ID] [--after-line ID] [--limit LINES]\nw11term-cli [--pid PID] wait-ssh-group-output --window ID --tab ID --run ID [--pane ID] [--after-line ID] [--regex] [--match-case] [--timeout SECONDS] [--] TEXT\nw11term-cli [--pid PID] follow-ssh-group-output --window ID --tab ID --run ID [--pane ID] [--after-line ID] [--timeout SECONDS]\nw11term-cli [--pid PID] restart --pane ID --exit-token TOKEN\nw11term-cli [--pid PID] stop-ssh-attempt --pane ID --attempt-token TOKEN\nw11term-cli [--pid PID] disconnect-ssh-shell --pane ID --connection-generation N";
pub(super) fn parse(
    verb: &str,
    args: impl Iterator<Item = String>,
    mut pid: Option<u32>,
    mut pane: Option<u64>,
) -> Result<Invocation> {
    let mut args = args.peekable();
    let mut cols = None;
    let mut rows = None;
    let mut profile = None;
    let mut activate = false;
    let mut all_panes = false;
    let mut regex = false;
    let mut match_case = false;
    let mut timeout = None;
    let mut require_success = false;
    let mut since = None;
    let mut after_line = None;
    let mut limit = None;
    let mut text = None;
    let mut stop = false;
    let mut timed = false;
    let mut playback = None;
    let mut overwrite = false;
    let mut exit_token = None;
    let mut attempt_token = None;
    let mut command_id = None;
    let mut status = None;
    let mut connection_generation = None;
    let mut window_id = None;
    let mut tab_id = None;
    let mut run_id = None;
    let mut pane_ids = Vec::new();
    let mut dry_run = false;
    let mut canary_pane_id = None;
    let mut parallel_limit = None;
    let mut interval_ms = None;
    let mut queue_expiry_ms = None;
    let mut stop_on_failure = None;
    let mut expected_query = None;
    let mut record_output = None;
    let mut group_name = None;
    let mut report_format = None;
    while let Some(arg) = args.next() {
        let value = |args: &mut dyn Iterator<Item = String>| {
            args.next()
                .with_context(|| format!("{arg} requires a value"))
        };
        match arg.as_str() {
            "--pid" => pid = Some(value(&mut args)?.parse()?),
            "--pane"
                if matches!(
                    verb,
                    "submit-ssh-group-command" | "wait-ssh-group-ready" | "retry-ssh-group-run"
                ) =>
            {
                pane_ids.push(value(&mut args)?.parse()?);
            }
            "--pane" if matches!(verb, "ssh-group-queue" | "interrupt-ssh-group-command") => {
                ensure!(pane.is_none(), "specify --pane only once");
                pane = Some(value(&mut args)?.parse()?);
            }
            "--pane" => pane = Some(value(&mut args)?.parse()?),
            "--dry-run" if verb == "submit-ssh-group-command" => dry_run = true,
            "--canary" if verb == "submit-ssh-group-command" => {
                ensure!(canary_pane_id.is_none(), "specify --canary only once");
                canary_pane_id = Some(value(&mut args)?.parse()?);
            }
            "--parallel" if verb == "submit-ssh-group-command" => {
                ensure!(parallel_limit.is_none(), "specify --parallel only once");
                parallel_limit = Some(
                    value(&mut args)?
                        .parse()
                        .context("--parallel requires a positive integer")?,
                );
            }
            "--interval-ms" if verb == "submit-ssh-group-command" => {
                ensure!(interval_ms.is_none(), "specify --interval-ms only once");
                interval_ms = Some(
                    value(&mut args)?
                        .parse()
                        .context("--interval-ms requires nonnegative milliseconds")?,
                );
            }
            "--queue-expiry-ms" if verb == "submit-ssh-group-command" => {
                ensure!(
                    queue_expiry_ms.is_none(),
                    "specify --queue-expiry-ms only once"
                );
                queue_expiry_ms = Some(
                    value(&mut args)?
                        .parse()
                        .context("--queue-expiry-ms requires nonnegative milliseconds")?,
                );
            }
            "--on-failure" if verb == "submit-ssh-group-command" => {
                ensure!(stop_on_failure.is_none(), "specify --on-failure only once");
                stop_on_failure = Some(match value(&mut args)?.as_str() {
                    "stop" => true,
                    "continue" => false,
                    _ => bail!("--on-failure requires stop or continue"),
                });
            }
            "--expect-output" if verb == "submit-ssh-group-command" => {
                ensure!(
                    expected_query.is_none(),
                    "specify --expect-output only once"
                );
                expected_query = Some(value(&mut args)?);
            }
            "--record-output" if verb == "submit-ssh-group-command" => {
                ensure!(record_output.is_none(), "specify --record-output only once");
                let path = value(&mut args)?;
                ensure!(
                    !path.is_empty(),
                    "--record-output requires a destination path"
                );
                record_output = Some(std::path::absolute(path)?);
            }
            "--all-panes" if verb == "find-text" => all_panes = true,
            "--cols" if matches!(verb, "open" | "resize") => {
                cols = Some(value(&mut args)?.parse()?)
            }
            "--rows" if matches!(verb, "open" | "resize") => {
                rows = Some(value(&mut args)?.parse()?)
            }
            "--profile" if verb == "open" => profile = Some(value(&mut args)?),
            "--name" if verb == "open-ssh-group" => {
                ensure!(group_name.is_none(), "specify --name only once");
                group_name = Some(value(&mut args)?);
            }
            "--activate" if verb == "open" => activate = true,
            "--regex"
                if matches!(
                    verb,
                    "find-text"
                        | "wait-for-text"
                        | "wait-ssh-group-output"
                        | "submit-ssh-group-command"
                ) =>
            {
                regex = true
            }
            "--match-case"
                if matches!(
                    verb,
                    "find-text" | "wait-ssh-group-output" | "submit-ssh-group-command"
                ) =>
            {
                match_case = true
            }
            "--require-success" if verb == "wait-ssh-group-run" => require_success = true,
            "--timeout"
                if matches!(
                    verb,
                    "wait-for-text"
                        | "events"
                        | "wait-ssh-group-run"
                        | "wait-ssh-group-approval"
                        | "wait-ssh-group-ready"
                        | "wait-ssh-group-output"
                        | "follow-ssh-group-output"
                ) =>
            {
                timeout = Some(value(&mut args)?.parse()?)
            }
            "--since" if verb == "events" => since = Some(value(&mut args)?.parse()?),
            "--after-line"
                if matches!(
                    verb,
                    "wait-ssh-group-output" | "get-ssh-group-output" | "follow-ssh-group-output"
                ) =>
            {
                let id = value(&mut args)?
                    .parse()
                    .context("--after-line requires a line ID")?;
                ensure!(
                    after_line.replace(id).is_none(),
                    "specify --after-line only once"
                );
            }
            "--limit" if verb == "get-ssh-group-output" => {
                let count = value(&mut args)?
                    .parse()
                    .context("--limit requires a line count")?;
                ensure!(limit.replace(count).is_none(), "specify --limit only once");
            }
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
            "--overwrite" if matches!(verb, "save-text" | "save-ssh-group-report") => {
                overwrite = true
            }
            "--format" if verb == "save-ssh-group-report" => {
                ensure!(report_format.is_none(), "specify --format only once");
                report_format = Some(match value(&mut args)?.as_str() {
                    "json" => SshGroupReportFormat::Json,
                    "text" => SshGroupReportFormat::Text,
                    _ => bail!("--format expects json or text"),
                });
            }
            "--exit-token" if verb == "restart" => exit_token = Some(value(&mut args)?.parse()?),
            "--attempt-token" if verb == "stop-ssh-attempt" => {
                ensure!(attempt_token.is_none(), "specify --attempt-token only once");
                attempt_token = Some(value(&mut args)?.parse::<std::num::NonZeroU64>()?.into());
            }
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
            "--connection-generation" if verb == "disconnect-ssh-shell" => {
                ensure!(
                    connection_generation.is_none(),
                    "specify --connection-generation only once"
                );
                connection_generation = Some(value(&mut args)?.parse()?);
            }
            "--window" | "--tab" | "--run"
                if (matches!(verb, "open-ssh-group" | "list-saved-ssh-groups")
                    && arg == "--window")
                    || matches!(
                        verb,
                        "get-ssh-group-roster"
                            | "get-ssh-group-run"
                            | "get-ssh-group-retry-plan"
                            | "retry-ssh-group-run"
                            | "interrupt-ssh-group-command"
                            | "get-ssh-group-output"
                            | "follow-ssh-group-output"
                            | "wait-ssh-group-run"
                            | "wait-ssh-group-approval"
                            | "wait-ssh-group-ready"
                            | "wait-ssh-group-output"
                            | "submit-ssh-group-command"
                            | "save-ssh-group-report"
                            | "ssh-group-queue"
                    ) =>
            {
                ensure!(
                    !matches!(
                        verb,
                        "get-ssh-group-roster"
                            | "wait-ssh-group-ready"
                            | "submit-ssh-group-command"
                    ) || arg != "--run",
                    "{verb} does not accept --run"
                );
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
            "--" if verb == "submit-ssh-group-command" => {
                ensure!(text.is_none(), "provide one quoted shell command line");
                text = Some(args.next().context("missing shell command after --")?);
                ensure!(
                    args.next().is_none(),
                    "provide one quoted shell command line"
                );
            }
            "--" if verb == "save-ssh-group-report" => {
                ensure!(text.is_none(), "provide one report destination");
                text = Some(args.next().context("missing report destination after --")?);
                ensure!(args.next().is_none(), "provide one report destination");
            }
            "--" if matches!(
                verb,
                "find-text" | "wait-for-text" | "wait-ssh-group-output"
            ) =>
            {
                ensure!(text.is_none(), "only one pattern is accepted");
                text = Some(value(&mut args)?);
                ensure!(args.next().is_none(), "unexpected argument after pattern");
            }
            _ if !arg.starts_with("--")
                && matches!(
                    verb,
                    "find-text"
                        | "wait-for-text"
                        | "wait-ssh-group-output"
                        | "record"
                        | "save-text"
                        | "save-ssh-group-report"
                        | "ssh-group-queue"
                        | "submit-ssh-group-command"
                ) =>
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
        "list-saved-ssh-groups" => {
            ensure!(
                pane.is_none(),
                "list-saved-ssh-groups does not accept --pane"
            );
            Request::ListSavedSshGroups {
                window_id: window_id.context("--window is required (from list-ssh-groups)")?,
            }
        }
        "open-ssh-group" => {
            ensure!(pane.is_none(), "open-ssh-group does not accept --pane");
            let name = group_name.context("--name is required")?;
            ensure!(!name.trim().is_empty(), "provide a saved SSH group name");
            Request::OpenSshGroup {
                window_id: window_id.context("--window is required (from list-ssh-groups)")?,
                name,
            }
        }
        "submit-ssh-group-command" => {
            ensure!(
                pane.is_none(),
                "list recipient panes after submit-ssh-group-command"
            );
            let command = text.context("provide one quoted shell command line")?;
            validate_group_submission(&pane_ids, &command, canary_pane_id)?;
            ensure!(
                expected_query.is_some() || (!regex && !match_case),
                "--regex and --match-case require --expect-output for a group submission"
            );
            Request::SubmitSshGroupCommand {
                window_id: window_id.context("--window is required (from list-ssh-groups)")?,
                tab_id: tab_id.context("--tab is required (from list-ssh-groups)")?,
                pane_ids,
                command,
                dry_run,
                canary_pane_id,
                parallel_limit,
                interval_ms,
                queue_expiry_ms,
                stop_on_failure,
                expect_output: expected_query.map(|query| SshGroupExpectedOutput {
                    query,
                    regex,
                    match_case,
                }),
                record_output,
            }
        }
        "save-ssh-group-report" => {
            ensure!(
                pane.is_none(),
                "save-ssh-group-report does not accept --pane"
            );
            let path = text.context("save-ssh-group-report requires a destination path")?;
            Request::SaveSshGroupReport {
                window_id: window_id.context("--window is required (from list-ssh-groups)")?,
                tab_id: tab_id.context("--tab is required (from list-ssh-groups)")?,
                run_id: run_id.context("--run is required (from list-ssh-groups)")?,
                path: std::path::absolute(path)?,
                format: report_format.unwrap_or_default(),
                overwrite,
            }
        }
        "retry-ssh-group-run" => {
            ensure!(pane.is_none(), "put --pane after retry-ssh-group-run");
            validate_group_panes(&pane_ids)?;
            Request::RetrySshGroupRun {
                window_id: window_id.context("--window is required (from list-ssh-groups)")?,
                tab_id: tab_id.context("--tab is required (from list-ssh-groups)")?,
                run_id: run_id.context("--run is required (from list-ssh-groups)")?,
                pane_ids,
            }
        }
        "interrupt-ssh-group-command" => Request::InterruptSshGroupCommand {
            window_id: window_id.context("--window is required (from list-ssh-groups)")?,
            tab_id: tab_id.context("--tab is required (from list-ssh-groups)")?,
            run_id: run_id.context("--run is required (from list-ssh-groups)")?,
            pane_id: pane
                .filter(|pane| *pane != 0)
                .context("--pane is required (from get-ssh-group-run)")?,
        },
        "ssh-group-queue" => {
            let action = match (text.as_deref(), pane) {
                (Some("pause"), None) => SshGroupQueueAction::Pause,
                (Some("resume"), None) => SshGroupQueueAction::Resume,
                (Some("stop"), None) => SshGroupQueueAction::Stop,
                (Some("approve-canary"), Some(pane_id @ 1..)) => {
                    SshGroupQueueAction::ApproveCanary { pane_id }
                }
                (Some("skip"), Some(pane_id @ 1..)) => SshGroupQueueAction::Skip { pane_id },
                (Some(action @ ("approve-canary" | "skip")), _) => {
                    bail!("{action} requires --pane with a positive pane ID")
                }
                (Some("pause" | "resume" | "stop"), Some(_)) => {
                    bail!("--pane requires approve-canary or skip")
                }
                _ => bail!(
                    "ssh-group-queue requires an action of pause, resume, stop, approve-canary, or skip"
                ),
            };
            Request::SshGroupQueue {
                window_id: window_id.context("--window is required (from list-ssh-groups)")?,
                tab_id: tab_id.context("--tab is required (from list-ssh-groups)")?,
                run_id: run_id.context("--run is required (from list-ssh-groups)")?,
                action,
            }
        }
        "get-ssh-group-roster"
        | "get-ssh-group-run"
        | "get-ssh-group-retry-plan"
        | "get-ssh-group-output"
        | "follow-ssh-group-output"
        | "wait-ssh-group-run"
        | "wait-ssh-group-approval"
        | "wait-ssh-group-output"
        | "wait-ssh-group-ready" => {
            ensure!(
                pane.is_none()
                    || matches!(
                        verb,
                        "wait-ssh-group-output"
                            | "get-ssh-group-output"
                            | "follow-ssh-group-output"
                    ),
                "{verb} does not accept --pane"
            );
            let window_id = window_id.context("--window is required (from list-ssh-groups)")?;
            let tab_id = tab_id.context("--tab is required (from list-ssh-groups)")?;
            if verb == "get-ssh-group-roster" {
                return Ok(Invocation::new(
                    pid,
                    Request::GetSshGroupRoster { window_id, tab_id },
                ));
            }
            let timeout_seconds = timeout.unwrap_or(30);
            ensure!(
                (1..=3600).contains(&timeout_seconds),
                "SSH group wait timeout must be 1..=3600 seconds"
            );
            if verb == "wait-ssh-group-ready" {
                let pane_ids = if pane_ids.is_empty() {
                    None
                } else {
                    validate_group_panes(&pane_ids)?;
                    Some(pane_ids)
                };
                return Ok(Invocation::new(
                    pid,
                    Request::WaitSshGroupReady {
                        window_id,
                        tab_id,
                        pane_ids,
                        timeout_seconds,
                    },
                ));
            }
            let run_id = run_id.context("--run is required (from list-ssh-groups)")?;
            if verb == "follow-ssh-group-output" {
                ensure!(pane != Some(0), "--pane requires a positive integer");
                Request::FollowSshGroupOutput {
                    window_id,
                    tab_id,
                    run_id,
                    pane,
                    after_line,
                    timeout_seconds,
                }
            } else if verb == "get-ssh-group-output" {
                let max = w11term::control::MAX_SSH_GROUP_OUTPUT_LINES;
                let limit = limit.unwrap_or(max);
                ensure!(
                    (1..=max).contains(&limit),
                    "SSH group output limit must be 1..={max} lines"
                );
                ensure!(pane != Some(0), "--pane requires a positive integer");
                Request::GetSshGroupOutput {
                    window_id,
                    tab_id,
                    run_id,
                    pane,
                    after_line,
                    limit,
                }
            } else if verb == "wait-ssh-group-output" {
                let pattern = text.context("search text or regular expression is required")?;
                ensure!(!pattern.is_empty(), "search text is empty");
                ensure!(
                    pattern.len() <= w11term::search::MAX_QUERY_BYTES,
                    "search text exceeds {} UTF-8 bytes",
                    w11term::search::MAX_QUERY_BYTES
                );
                ensure!(pane != Some(0), "--pane requires a positive integer");
                Request::WaitSshGroupOutput {
                    window_id,
                    tab_id,
                    run_id,
                    pane,
                    after_line,
                    pattern,
                    regex,
                    match_case,
                    timeout_seconds,
                }
            } else if verb == "wait-ssh-group-run" {
                Request::WaitSshGroupRun {
                    window_id,
                    tab_id,
                    run_id,
                    timeout_seconds,
                }
            } else if verb == "wait-ssh-group-approval" {
                Request::WaitSshGroupApproval {
                    window_id,
                    tab_id,
                    run_id,
                    timeout_seconds,
                }
            } else if verb == "get-ssh-group-retry-plan" {
                Request::GetSshGroupRetryPlan {
                    window_id,
                    tab_id,
                    run_id,
                }
            } else {
                Request::GetSshGroupRun {
                    window_id,
                    tab_id,
                    run_id,
                }
            }
        }
        "restart" => Request::Restart {
            pane: pane.context("--pane is required")?,
            exit_token: exit_token.context("--exit-token is required (from events)")?,
        },
        "stop-ssh-attempt" => Request::StopSshAttempt {
            pane: pane.context("--pane is required")?,
            attempt_token: attempt_token
                .context("--attempt-token is required (from list or events)")?,
        },
        "disconnect-ssh-shell" => Request::DisconnectSshShell {
            pane: pane.context("--pane is required")?,
            connection_generation: connection_generation
                .and_then(std::num::NonZeroU64::new)
                .context("--connection-generation must be positive (from list)")?,
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
    Ok(Invocation {
        pid,
        request,
        require_success,
    })
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnect_requires_a_pane_and_a_positive_connection_generation() {
        let disconnect = |args: &[&str]| {
            parse(
                "disconnect-ssh-shell",
                args.iter().map(|s| s.to_string()),
                None,
                None,
            )
            .map(|invocation| invocation.request)
        };
        let request = disconnect(&["--pane", "2", "--connection-generation", "7"]).unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({"verb":"disconnect-ssh-shell","pane":2,"connection_generation":7})
        );
        assert!(disconnect(&["--pane", "2"]).is_err());
        assert!(disconnect(&["--connection-generation", "7"]).is_err());
        assert!(disconnect(&["--pane", "2", "--connection-generation", "0"]).is_err());
    }
    #[test]
    fn timed_recording_is_opt_in_and_old_record_requests_round_trip() {
        let record =
            |args: &[&str]| parse("record", args.iter().map(|s| s.to_string()), None, Some(7));
        let timed = record(&["--timed", "my capture.vt"]).unwrap().request;
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
            serde_json::to_value(record(&["--stop"]).unwrap().request).unwrap(),
            stop
        );
    }

    #[test]
    fn rejects_cross_command_flags_and_missing_values() {
        let p = |v, a: &[&str]| {
            parse(v, a.iter().map(|s| s.to_string()), None, None)
                .map(|invocation| (invocation.pid, invocation.request))
        };
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
        let stop = p(
            "stop-ssh-attempt",
            &["--pane", "2", "--attempt-token", "13"],
        )
        .unwrap()
        .1;
        assert_eq!(
            serde_json::to_value(stop).unwrap(),
            serde_json::json!({"verb":"stop-ssh-attempt","pane":2,"attempt_token":13})
        );
        assert!(p("stop-ssh-attempt", &["--pane", "2"]).is_err());
        assert!(p("stop-ssh-attempt", &["--attempt-token", "13"]).is_err());
        assert!(p("stop-ssh-attempt", &["--pane", "2", "--attempt-token", "0"]).is_err());
        assert!(
            p(
                "stop-ssh-attempt",
                &[
                    "--pane",
                    "2",
                    "--attempt-token",
                    "1",
                    "--attempt-token",
                    "2"
                ]
            )
            .is_err()
        );
    }
}
