use super::*;
use crate::events::{GroupTranscriptLine, GroupTranscriptOutput};
use anyhow::bail;
use regex::Regex;
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
};
use tokio::{
    sync::{broadcast::error::RecvError, watch},
    time::Instant,
};

#[derive(Debug)]
pub struct SshGroupOutput {
    pub output: GroupTranscriptOutput,
    pub hosts: BTreeMap<u64, (String, String)>,
    pub retained: Option<watch::Receiver<()>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SshGroupExpectedOutput {
    pub query: String,
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub match_case: bool,
}

#[derive(Debug)]
pub struct SshGroupCommandOptions {
    pub dry_run: bool,
    pub dispatch: SshGroupDispatchOverrides,
    pub expect_output: Option<(SshGroupExpectedOutput, Regex)>,
    pub record_output: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug)]
pub struct SshGroupDispatchOverrides {
    pub canary_pane_id: Option<u64>,
    pub parallel_limit: Option<NonZeroUsize>,
    pub interval_ms: Option<u64>,
}

impl SshGroupCommandOptions {
    pub(super) fn prepare(
        dry_run: bool,
        dispatch: SshGroupDispatchOverrides,
        expect_output: Option<SshGroupExpectedOutput>,
        record_output: Option<PathBuf>,
    ) -> Result<Self> {
        ensure!(
            record_output.as_ref().is_none_or(|path| path.is_absolute()),
            "group output recording path must be absolute"
        );
        let expect_output = expect_output
            .map(|rule| {
                let pattern = output_pattern(&rule.query, rule.regex, rule.match_case)?;
                Ok::<_, anyhow::Error>((rule, pattern))
            })
            .transpose()
            .context("invalid expected output")?;
        Ok(Self {
            dry_run,
            dispatch,
            expect_output,
            record_output,
        })
    }
}

#[derive(Debug)]
pub enum SshGroupReadiness {
    CheckedShown,
    Panes(Vec<u64>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SshGroupQueueAction {
    Pause,
    Resume,
    Stop,
    ApproveCanary { pane_id: u64 },
    Skip { pane_id: u64 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SshGroupReportFormat {
    #[default]
    Json,
    Text,
}

type ReportWrite = Box<dyn FnOnce(&mut std::fs::File) -> std::io::Result<()> + Send>;

pub struct SshGroupReportWriter(ReportWrite);

impl SshGroupReportWriter {
    pub fn new(
        write: impl FnOnce(&mut std::fs::File) -> std::io::Result<()> + Send + 'static,
    ) -> Self {
        Self(Box::new(write))
    }
}

impl std::fmt::Debug for SshGroupReportWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshGroupReportWriter")
            .finish_non_exhaustive()
    }
}

pub(super) struct GroupOutputQuery {
    pub(super) pane: Option<u64>,
    pub(super) after_line: Option<u64>,
    pub(super) pattern: Regex,
    pub(super) timeout: Duration,
}

pub(super) struct GroupOutputFollow {
    pub(super) window: WindowId,
    pub(super) tab: u64,
    pub(super) run: u64,
    pub(super) pane: Option<u64>,
    pub(super) after_line: Option<u64>,
    pub(super) timeout_seconds: u64,
}

pub(super) fn output_pattern(query: &str, regex: bool, match_case: bool) -> Result<Regex> {
    ensure!(!query.is_empty(), "search text is empty");
    ensure!(
        query.len() <= crate::search::MAX_QUERY_BYTES,
        "search text exceeds {} UTF-8 bytes",
        crate::search::MAX_QUERY_BYTES
    );
    let pattern = if regex {
        Cow::Borrowed(query)
    } else {
        Cow::Owned(regex::escape(query))
    };
    crate::search::compile_regex(&pattern, match_case).map_err(anyhow::Error::msg)
}

fn check_cursor(after_line: Option<u64>, latest_line: u64) -> Result<()> {
    if let Some(cursor) = after_line {
        ensure!(
            cursor <= latest_line,
            "output cursor {cursor} is ahead of the latest line {latest_line}"
        );
    }
    Ok(())
}

fn describe_output_line(line: &GroupTranscriptLine, label: &str, address: &str) -> Value {
    json!({
        "line_id": line.id,
        "pane_id": line.pane,
        "label": label,
        "address": address,
        "text": &*line.text,
        "shortened": line.shortened,
        "elapsed_ms": line.elapsed_ms,
    })
}

pub fn validate_group_submission(
    pane_ids: &[u64],
    command: &str,
    canary_pane_id: Option<u64>,
) -> Result<()> {
    ensure!(
        crate::saved_commands::valid_command_text(command),
        "provide one nonempty shell command line of up to 4096 UTF-8 bytes"
    );
    validate_group_panes(pane_ids)?;
    if let Some(canary) = canary_pane_id {
        ensure!(
            pane_ids.contains(&canary),
            "canary pane {canary} must be one of the explicit recipient panes"
        );
        ensure!(
            pane_ids.len() > 1,
            "provide at least one other recipient pane with a canary"
        );
    }
    Ok(())
}

pub fn validate_group_panes(pane_ids: &[u64]) -> Result<()> {
    ensure!(
        !pane_ids.is_empty(),
        "provide at least one explicit SSH pane ID"
    );
    let mut seen = BTreeSet::new();
    for &pane in pane_ids {
        ensure!(pane != 0, "pane IDs must be positive");
        ensure!(
            seen.insert(pane),
            "pane ID {pane} was specified more than once"
        );
    }
    Ok(())
}

impl Registry {
    pub(super) async fn interrupt_ssh_group_command(
        &self,
        window: WindowId,
        tab: u64,
        run: u64,
        pane: u64,
    ) -> Result<Value> {
        let ui = self.window_ui(window)?;
        let (reply, response) = oneshot::channel();
        ui(UiRequest::InterruptSshGroupCommand {
            tab_id: tab,
            run_id: run,
            pane_id: pane,
            reply,
        });
        let Ok(Ok(run)) = tokio::time::timeout(TIMEOUT, response).await else {
            bail!("SSH group interrupt was not acknowledged. Check the run before retrying");
        };
        Ok(json!({ "window_id": window, "tab_id": tab, "run": run.map_err(anyhow::Error::msg)? }))
    }

    pub(super) async fn change_ssh_group_queue(
        &self,
        window: WindowId,
        tab: u64,
        run: u64,
        action: SshGroupQueueAction,
    ) -> Result<Value> {
        let ui = self.window_ui(window)?;
        let (reply, response) = oneshot::channel();
        ui(UiRequest::SshGroupQueue {
            tab_id: tab,
            run_id: run,
            action,
            reply,
        });
        let Ok(Ok(run)) = tokio::time::timeout(TIMEOUT, response).await else {
            bail!("SSH group queue action was not acknowledged. Check the run before retrying");
        };
        Ok(json!({ "window_id": window, "tab_id": tab, "run": run.map_err(anyhow::Error::msg)? }))
    }

    pub(super) async fn save_ssh_group_report(
        &self,
        window: WindowId,
        tab: u64,
        run: u64,
        path: PathBuf,
        format: SshGroupReportFormat,
        overwrite: bool,
    ) -> Result<Value> {
        ensure!(path.is_absolute(), "SSH group report path must be absolute");
        let deadline = Instant::now() + TIMEOUT;
        let ui = self.window_ui(window)?;
        let (reply, response) = oneshot::channel();
        ui(UiRequest::CaptureSshGroupReport {
            tab_id: tab,
            run_id: run,
            format,
            reply,
        });
        let writer = receive(window, response, deadline).await?;
        let path = tokio::task::spawn_blocking(move || {
            crate::transcript::save_with(&path, overwrite, writer.0)?;
            anyhow::Ok(path)
        })
        .await??;
        Ok(
            json!({ "window_id": window, "tab_id": tab, "run_id": run, "path": path, "format": format }),
        )
    }

    pub(super) async fn list_saved_ssh_groups(&self, window: WindowId) -> Result<Value> {
        let deadline = Instant::now() + TIMEOUT;
        let ui = self.window_ui(window)?;
        let (reply, response) = oneshot::channel();
        ui(UiRequest::ListSavedSshGroups { reply });
        receive(window, response, deadline).await
    }

    pub(super) async fn open_ssh_group(&self, window: WindowId, name: String) -> Result<Value> {
        ensure!(!name.trim().is_empty(), "provide a saved SSH group name");
        let ui = self.window_ui(window)?;
        let (reply, response) = oneshot::channel();
        ui(UiRequest::OpenSshGroup { name, reply });
        let Ok(Ok(group)) = tokio::time::timeout(TIMEOUT, response).await else {
            bail!("SSH group launch was not acknowledged. Check list-ssh-groups before retrying");
        };
        group.map_err(anyhow::Error::msg)
    }

    pub(super) async fn submit_ssh_group_command(
        &self,
        window: WindowId,
        tab: u64,
        pane_ids: Vec<u64>,
        command: String,
        options: SshGroupCommandOptions,
    ) -> Result<Value> {
        let ui = self.window_ui(window)?;
        let (reply, response) = oneshot::channel();
        let dry_run = options.dry_run;
        ui(UiRequest::SubmitSshGroupCommand {
            tab_id: tab,
            pane_ids,
            command,
            options: Box::new(options),
            reply,
        });
        let Ok(Ok(result)) = tokio::time::timeout(TIMEOUT, response).await else {
            if dry_run {
                bail!("SSH group preview was not acknowledged");
            }
            bail!("SSH group submission was not acknowledged. Check retained runs before retrying");
        };
        let result = result.map_err(anyhow::Error::msg)?;
        Ok(if dry_run {
            json!({ "window_id": window, "tab_id": tab, "plan": result })
        } else {
            json!({ "window_id": window, "tab_id": tab, "run_id": result })
        })
    }

    pub(super) async fn query_ssh_group_roster(
        &self,
        window: WindowId,
        tab: u64,
        wait_for: Option<(Duration, SshGroupReadiness)>,
    ) -> Result<Value> {
        let (timeout, readiness) =
            wait_for.map_or((TIMEOUT, None), |(timeout, scope)| (timeout, Some(scope)));
        let deadline = Instant::now() + timeout;
        let waiting = readiness.is_some();
        let panes = match &readiness {
            Some(SshGroupReadiness::Panes(panes)) => Some(panes.clone()),
            _ => None,
        };
        let (reply, response) = oneshot::channel();
        self.window_ui(window)?(UiRequest::GetSshGroupRoster {
            tab_id: tab,
            readiness,
            reply,
        });
        let roster = if waiting {
            let timeout_error = if panes.is_some() {
                "wait-ssh-group-ready timed out before all requested SSH panes were ready"
            } else {
                "wait-ssh-group-ready timed out before all checked, shown hosts were ready"
            };
            tokio::time::timeout_at(deadline, response)
                .await
                .context(timeout_error)?
                .with_context(|| {
                    format!(
                        "SSH group tab {tab} in window {window} closed during the readiness wait"
                    )
                })?
                .map_err(anyhow::Error::msg)?
        } else {
            receive(window, response, deadline).await?
        };
        let mut result = json!({ "window_id": window, "tab_id": tab, "roster": roster });
        if let Some(panes) = panes {
            result["ready_pane_ids"] = json!(panes);
        }
        Ok(result)
    }

    pub(super) async fn list_ssh_groups(&self) -> Result<Value> {
        let deadline = Instant::now() + TIMEOUT;
        let responses: Vec<_> = self
            .all_window_uis()?
            .into_iter()
            .map(|(window, ui)| {
                let (reply, response) = oneshot::channel();
                ui(UiRequest::ListSshGroups { reply });
                (window, response)
            })
            .collect();
        let mut windows = Vec::with_capacity(responses.len());
        for (window, response) in responses {
            let groups = receive(window, response, deadline).await?;
            windows.push(json!({ "window_id": window, "groups": groups }));
        }
        Ok(json!({ "pid": std::process::id(), "windows": windows }))
    }

    pub(super) async fn query_ssh_group_run(
        &self,
        window: WindowId,
        tab: u64,
        run: u64,
        wait_for: Option<Duration>,
    ) -> Result<Value> {
        let deadline = Instant::now() + wait_for.unwrap_or(TIMEOUT);
        let (reply, response) = oneshot::channel();
        self.window_ui(window)?(UiRequest::GetSshGroupRun {
            tab_id: tab,
            run_id: run,
            wait_for_settlement: wait_for.is_some(),
            reply,
        });
        let run = if wait_for.is_some() {
            tokio::time::timeout_at(deadline, response)
                .await
                .context("wait-ssh-group-run timed out before settlement")?
                .with_context(|| {
                    format!(
                        "SSH group run {run} is no longer retained in tab {tab} of window {window}"
                    )
                })?
                .map_err(anyhow::Error::msg)?
        } else {
            receive(window, response, deadline).await?
        };
        Ok(json!({ "window_id": window, "tab_id": tab, "run": run }))
    }

    async fn read_ssh_group_output(
        &self,
        window: WindowId,
        tab: u64,
        run: u64,
        pane: Option<u64>,
        follow: bool,
    ) -> Result<SshGroupOutput> {
        let (reply, response) = oneshot::channel();
        self.window_ui(window)?(UiRequest::ReadSshGroupOutput {
            tab_id: tab,
            run_id: run,
            pane,
            follow,
            reply,
        });
        response
            .await
            .with_context(|| format!("window {window} closed during SSH group output lookup"))?
            .map_err(anyhow::Error::msg)
    }

    pub(super) async fn query_ssh_group_output(
        &self,
        window: WindowId,
        tab: u64,
        run: u64,
        pane: Option<u64>,
        after_line: Option<u64>,
        limit: usize,
    ) -> Result<Value> {
        let SshGroupOutput { output, hosts, .. } = tokio::time::timeout(
            TIMEOUT,
            self.read_ssh_group_output(window, tab, run, pane, false),
        )
        .await
        .context("get-ssh-group-output timed out")??;
        check_cursor(after_line, output.latest_line)?;
        let cursor = after_line.unwrap_or(0);
        let mut available = output.history.lines.iter().filter_map(|line| {
            let (label, address) = hosts.get(&line.pane)?;
            (line.id > cursor).then_some((line, label, address))
        });
        let page: Vec<_> = available.by_ref().take(limit).collect();
        let has_more = available.next().is_some();
        let next_after_line = if has_more {
            page.last().map_or(cursor, |(line, _, _)| line.id)
        } else {
            output.latest_line
        };
        let lines: Vec<_> = page
            .into_iter()
            .map(|(line, label, address)| describe_output_line(line, label, address))
            .collect();
        Ok(json!({
            "window_id": window,
            "tab_id": tab,
            "run_id": run,
            "pane_id": pane,
            "settled": output.settled,
            "latest_line": output.latest_line,
            "evicted_through": output.evicted_through,
            "gap": cursor < output.evicted_through,
            "has_more": has_more,
            "next_after_line": next_after_line,
            "lines": lines,
        }))
    }

    pub(super) async fn wait_ssh_group_output(
        &self,
        window: WindowId,
        tab: u64,
        run: u64,
        query: GroupOutputQuery,
    ) -> Result<Value> {
        let GroupOutputQuery {
            pane,
            after_line,
            pattern,
            timeout,
        } = query;
        let deadline = Instant::now() + timeout;
        tokio::time::timeout_at(deadline, async {
            let SshGroupOutput { output, hosts, retained } = self.read_ssh_group_output(window, tab, run, pane, true).await?;
            check_cursor(after_line, output.latest_line)?;
            if let Some(cursor) = after_line {
                ensure!(cursor >= output.evicted_through, "SSH group output history after cursor {cursor} is incomplete through line {}", output.evicted_through);
            }
            let matched = |line: &GroupTranscriptLine| {
                let (label, address) = hosts.get(&line.pane)?;
                (after_line.is_none_or(|cursor| line.id > cursor)
                    && pane.is_none_or(|pane| pane == line.pane)
                    && pattern.is_match(&line.text)).then(|| json!({
                    "window_id": window,
                    "tab_id": tab,
                    "run_id": run,
                    "line": describe_output_line(line, label, address)
                }))
            };
            if let Some(result) = output.history.lines.iter().find_map(matched) {
                return Ok(result);
            }
            if let (Some(mut incoming), Some(mut retained)) = (output.incoming, retained) {
                loop {
                    tokio::select! {
                        _ = retained.changed() => bail!("SSH group run {run} is no longer retained in tab {tab} of window {window}"),
                        line = incoming.recv() => match line {
                            Ok(line) => {
                                if let Some(result) = matched(&line) {
                                    return Ok(result);
                                }
                            }
                            Err(RecvError::Closed) => break,
                            Err(RecvError::Lagged(missed)) => bail!("SSH group output wait fell behind and missed {missed} lines"),
                        }
                    }
                }
            }
            bail!("SSH group run settled without a matching output line")
        }).await.context("wait-ssh-group-output timed out before a match")?
    }

    pub(super) async fn follow_ssh_group_output(
        &self,
        pipe: &mut NamedPipeServer,
        follow: GroupOutputFollow,
    ) -> Result<()> {
        let GroupOutputFollow {
            window,
            tab,
            run,
            pane,
            after_line,
            timeout_seconds,
        } = follow;
        if !(1..=3600).contains(&timeout_seconds) {
            return write_reply(
                pipe,
                Err(anyhow::anyhow!(
                    "SSH group follow timeout must be 1..=3600 seconds"
                )),
            )
            .await;
        }
        let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
        let timeout = || anyhow::anyhow!("follow-ssh-group-output timed out before settlement");
        let captured = tokio::time::timeout_at(
            deadline,
            self.read_ssh_group_output(window, tab, run, pane, true),
        )
        .await
        .map_err(|_| timeout())
        .and_then(|captured| captured);
        let SshGroupOutput {
            output,
            hosts,
            retained,
        } = match captured {
            Ok(captured) => captured,
            Err(error) => return write_reply(pipe, Err(error)).await,
        };
        if let Err(error) = check_cursor(after_line, output.latest_line) {
            return write_reply(pipe, Err(error)).await;
        }
        let cursor = after_line.unwrap_or(0);
        if cursor < output.evicted_through {
            if after_line.is_some() {
                return write_reply(pipe, Err(anyhow::anyhow!(
                    "SSH group output history after cursor {cursor} is incomplete through line {}", output.evicted_through
                ))).await;
            }
            write_reply(
                pipe,
                Ok(json!({
                    "kind": "gap", "window_id": window, "tab_id": tab, "run_id": run,
                    "pane_id": pane, "evicted_through": output.evicted_through,
                })),
            )
            .await?;
        }
        let event = |line: &GroupTranscriptLine| {
            let (label, address) = hosts.get(&line.pane)?;
            (line.id > cursor).then(|| {
                json!({
                    "kind": "line", "window_id": window, "tab_id": tab, "run_id": run,
                    "line": describe_output_line(line, label, address),
                })
            })
        };
        for line in output.history.lines.iter().filter_map(event) {
            if Instant::now() >= deadline {
                return write_reply(pipe, Err(timeout())).await;
            }
            write_reply(pipe, Ok(line)).await?;
        }
        let mut next_after_line = output.latest_line;
        if let (Some(mut incoming), Some(mut retained)) = (output.incoming, retained) {
            let mut disconnect = [0];
            loop {
                let received = tokio::select! {
                    _ = pipe.read(&mut disconnect) => return Ok(()),
                    _ = retained.changed() => {
                        return write_reply(pipe, Err(anyhow::anyhow!(
                            "SSH group run {run} is no longer retained in tab {tab} of window {window}"
                        ))).await;
                    }
                    received = tokio::time::timeout_at(deadline, incoming.recv()) => received,
                };
                match received {
                    Ok(Ok(line)) => {
                        next_after_line = line.id;
                        if let Some(line) = event(&line) {
                            write_reply(pipe, Ok(line)).await?;
                        }
                    }
                    Ok(Err(RecvError::Closed)) => break,
                    Ok(Err(RecvError::Lagged(missed))) => {
                        return write_reply(
                            pipe,
                            Err(anyhow::anyhow!(
                                "SSH group output follow fell behind and missed {missed} lines"
                            )),
                        )
                        .await;
                    }
                    Err(_) => return write_reply(pipe, Err(timeout())).await,
                }
            }
        }
        write_reply(
            pipe,
            Ok(json!({
                "kind": "settled", "window_id": window, "tab_id": tab, "run_id": run,
                "pane_id": pane, "next_after_line": next_after_line,
            })),
        )
        .await
    }
}

async fn receive<T>(
    window: WindowId,
    response: oneshot::Receiver<Result<T, String>>,
    deadline: Instant,
) -> Result<T> {
    tokio::time::timeout_at(deadline, response)
        .await
        .with_context(|| format!("SSH group query timed out for window {window}"))?
        .with_context(|| format!("window {window} closed during SSH group query"))?
        .map_err(anyhow::Error::msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn report_export_captures_from_the_explicit_window_before_writing() {
        use std::io::Write as _;

        let owner = Registry::window(3, None);
        let foreground = owner.register_window(5, None).unwrap();
        foreground.activate();
        owner.set_ui(|request| {
            let UiRequest::CaptureSshGroupReport {
                tab_id,
                run_id,
                format,
                reply,
            } = request
            else {
                panic!("unexpected control request");
            };
            assert_eq!((tab_id, run_id, format), (7, 2, SshGroupReportFormat::Text));
            assert!(
                reply
                    .send(Ok(SshGroupReportWriter::new(
                        |file| file.write_all(b"captured output")
                    )))
                    .is_ok()
            );
        });
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("group report.txt");
        let saved = foreground
            .save_ssh_group_report(3, 7, 2, path.clone(), SshGroupReportFormat::Text, false)
            .await
            .unwrap();
        assert_eq!(
            saved,
            json!({ "window_id":3, "tab_id":7, "run_id":2, "path":path, "format":"text" })
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"captured output");
        assert!(
            foreground
                .save_ssh_group_report(
                    3,
                    7,
                    2,
                    "relative.txt".into(),
                    SshGroupReportFormat::Text,
                    false
                )
                .await
                .unwrap_err()
                .to_string()
                .contains("must be absolute")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn output_wait_times_out_and_reports_retirement() {
        use crate::events::GroupTranscript;

        let registry = Registry::window(3, None);
        let (lifetime, retained) = watch::channel(());
        let transcript = GroupTranscript::default();
        registry.set_ui(move |request| {
            let UiRequest::ReadSshGroupOutput {
                tab_id,
                run_id,
                pane,
                follow,
                reply,
            } = request
            else {
                panic!("unexpected output request");
            };
            assert_eq!((tab_id, run_id, pane), (9, 2, Some(7)));
            assert!(follow);
            reply
                .send(Ok(SshGroupOutput {
                    output: transcript.read_output(pane, follow),
                    hosts: Default::default(),
                    retained: Some(retained.clone()),
                }))
                .unwrap();
        });
        let wait = || {
            registry.wait_ssh_group_output(
                3,
                9,
                2,
                GroupOutputQuery {
                    pane: Some(7),
                    after_line: None,
                    pattern: output_pattern("ready", false, false).unwrap(),
                    timeout: Duration::from_secs(1),
                },
            )
        };
        assert!(
            wait()
                .await
                .unwrap_err()
                .to_string()
                .contains("timed out before a match")
        );
        drop(lifetime);
        assert!(
            wait()
                .await
                .unwrap_err()
                .to_string()
                .contains("no longer retained")
        );
        assert!(output_pattern("(", true, false).is_err());
        assert!(
            output_pattern("[READY]", false, false)
                .unwrap()
                .is_match("[ready]")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_timed_out_wait_drops_its_reply_and_retirement_is_reported() {
        let registry = Registry::window(3, None);
        let (send, receive) = std::sync::mpsc::channel();
        registry.set_ui(move |request| send.send(request).unwrap());
        let error = registry
            .query_ssh_group_run(3, 9, 2, Some(Duration::from_secs(1)))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("timed out before settlement"));
        let UiRequest::GetSshGroupRun {
            wait_for_settlement,
            reply,
            ..
        } = receive.try_recv().unwrap()
        else {
            panic!("wrong request");
        };
        assert!(wait_for_settlement && reply.is_closed());

        registry.set_ui(drop);
        let error = registry
            .query_ssh_group_run(3, 9, 2, Some(Duration::from_secs(1)))
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("no longer retained in tab 9 of window 3")
        );
    }

    #[tokio::test]
    async fn group_requests_list_all_windows_and_get_from_the_explicit_owner() {
        let first = Registry::window(3, None);
        let second = first.register_window(5, None).unwrap();
        second.activate();
        first.set_ui(|request| match request {
            UiRequest::ListSshGroups { reply } => {
                reply
                    .send(Ok(json!([{ "tab_id": 9, "runs": [{ "run_id": 2 }] }])))
                    .unwrap();
            }
            UiRequest::OpenSshGroup { name, reply } => {
                assert_eq!(name, "East Fleet");
                reply
                    .send(Ok(json!({ "window_id": 3, "tab_id": 10 })))
                    .unwrap();
            }
            UiRequest::ListSavedSshGroups { reply } => {
                reply
                    .send(Ok(json!({
                        "window_id": 3,
                        "groups": [{ "name": "East Fleet" }],
                    })))
                    .unwrap();
            }
            UiRequest::GetSshGroupRun {
                tab_id,
                run_id,
                wait_for_settlement,
                reply,
            } => {
                assert_eq!((tab_id, run_id), (9, 2));
                assert!(!wait_for_settlement);
                reply
                    .send(Ok(json!({ "summary": { "run_id": run_id } })))
                    .unwrap();
            }
            UiRequest::SshGroupQueue {
                tab_id,
                run_id,
                action,
                reply,
            } => {
                assert_eq!((tab_id, run_id, action), (9, 2, SshGroupQueueAction::Pause));
                reply
                    .send(Ok(json!({ "queue": {"state":"paused", "unsent":1} })))
                    .unwrap();
            }
            UiRequest::InterruptSshGroupCommand {
                tab_id,
                run_id,
                pane_id,
                reply,
            } => {
                assert_eq!((tab_id, run_id, pane_id), (9, 2, 7));
                reply
                    .send(Ok(
                        json!({"recipients":[{"pane_id":7,"interrupt":{"state":"pending"}}]}),
                    ))
                    .unwrap();
            }
            UiRequest::SubmitSshGroupCommand {
                tab_id,
                pane_ids,
                command,
                options,
                reply,
            } => {
                assert_eq!(tab_id, 9);
                assert_eq!(pane_ids, [8, 7]);
                assert_eq!(command, "show  status");
                assert_eq!(options.dispatch.canary_pane_id, Some(7));
                assert_eq!(
                    options.dispatch.parallel_limit.map(NonZeroUsize::get),
                    Some(3)
                );
                assert_eq!(options.dispatch.interval_ms, Some(725));
                assert_eq!(
                    options.record_output,
                    Some(PathBuf::from(r"C:\fixture\capture.txt"))
                );
                let (rule, pattern) = options.expect_output.unwrap();
                assert_eq!(rule.query, "^READY [0-9]+$");
                assert!(pattern.is_match("READY 2"));
                assert!(!pattern.is_match("ready 2"));
                let result = if options.dry_run {
                    json!({"command": command, "recipients": [{"pane_id": 7}, {"pane_id": 8}]})
                } else {
                    json!(3)
                };
                reply.send(Ok(result)).unwrap();
            }
            UiRequest::GetSshGroupRoster {
                tab_id,
                readiness,
                reply,
            } => {
                assert_eq!(tab_id, 9);
                if let Some(scope) = readiness {
                    assert!(matches!(scope, SshGroupReadiness::Panes(panes) if panes == [8, 7]));
                }
                reply
                    .send(Ok(json!({ "hosts": [{ "pane_id": 7 }] })))
                    .unwrap();
            }
            _ => panic!("unexpected first-window request"),
        });
        second.set_ui(|request| match request {
            UiRequest::ListSshGroups { reply } => reply.send(Ok(json!([]))).unwrap(),
            _ => panic!("the active window must not receive a lookup for another window"),
        });
        let listed = second.list_ssh_groups().await.unwrap();
        assert_eq!(
            listed["windows"],
            json!([
                { "window_id": 3, "groups": [{ "tab_id": 9, "runs": [{ "run_id": 2 }] }] },
                { "window_id": 5, "groups": [] }
            ])
        );
        let lookup = second.query_ssh_group_run(3, 9, 2, None).await.unwrap();
        let paused = second
            .change_ssh_group_queue(3, 9, 2, SshGroupQueueAction::Pause)
            .await
            .unwrap();
        assert_eq!(
            paused,
            json!({"window_id":3, "tab_id":9, "run":{"queue":{"state":"paused","unsent":1}}})
        );
        let interrupted = second
            .interrupt_ssh_group_command(3, 9, 2, 7)
            .await
            .unwrap();
        assert_eq!(
            interrupted,
            json!({"window_id":3,"tab_id":9,"run":{"recipients":[{"pane_id":7,"interrupt":{"state":"pending"}}]}})
        );
        let opened = second.open_ssh_group(3, "East Fleet".into()).await.unwrap();
        assert_eq!(opened, json!({ "window_id": 3, "tab_id": 10 }));
        let saved = second.list_saved_ssh_groups(3).await.unwrap();
        assert_eq!(
            saved,
            json!({ "window_id": 3, "groups": [{ "name": "East Fleet" }] })
        );
        assert!(
            second
                .open_ssh_group(99, "East Fleet".into())
                .await
                .unwrap_err()
                .to_string()
                .contains("unknown window 99")
        );
        let submit = |mut wire: Value| {
            wire["verb"] = json!("submit-ssh-group-command");
            wire["tab_id"] = json!(9);
            second.execute(
                serde_json::from_value(wire).unwrap(),
                Arc::new(|_, _, _| unreachable!()),
            )
        };
        let submission = json!({
            "window_id": 3,
            "pane_ids": [8, 7],
            "command": "show  status",
            "canary_pane_id": 7,
            "parallel_limit": 3,
            "interval_ms": 725,
            "expect_output": { "query": "^READY [0-9]+$", "regex": true, "match_case": true },
            "record_output": r"C:\fixture\capture.txt",
        });
        let mut preview = submission.clone();
        preview["dry_run"] = json!(true);
        assert_eq!(
            submit(preview).await.unwrap(),
            json!({
                "window_id": 3, "tab_id": 9,
                "plan": {"command":"show  status","recipients":[{"pane_id":7},{"pane_id":8}]}
            })
        );
        assert_eq!(
            submit(submission).await.unwrap(),
            json!({ "window_id": 3, "tab_id": 9, "run_id": 3 })
        );
        for (mut wire, message) in [
            (json!({"pane_ids": [7, 7]}), "specified more than once"),
            (
                json!({"pane_ids": [7], "expect_output": {"query": "[", "regex": true}}),
                "invalid expected output",
            ),
            (
                json!({"pane_ids": [7], "record_output": "relative.txt"}),
                "recording path must be absolute",
            ),
            (
                json!({"pane_ids": [7, 8], "canary_pane_id": 6}),
                "must be one of the explicit recipient panes",
            ),
            (
                json!({"pane_ids": [7], "canary_pane_id": 7}),
                "at least one other recipient pane",
            ),
        ] {
            wire["window_id"] = json!(5);
            wire["command"] = json!("show");
            let error = submit(wire).await.unwrap_err().to_string();
            assert!(error.contains(message), "{error}");
        }
        assert_eq!(
            lookup,
            json!({ "window_id": 3, "tab_id": 9, "run": { "summary": { "run_id": 2 } } })
        );
        assert_eq!(
            second.query_ssh_group_roster(3, 9, None).await.unwrap(),
            json!({ "window_id": 3, "tab_id": 9, "roster": { "hosts": [{ "pane_id": 7 }] } })
        );
        let explicit = second
            .query_ssh_group_roster(
                3,
                9,
                Some((Duration::from_secs(1), SshGroupReadiness::Panes(vec![8, 7]))),
            )
            .await
            .unwrap();
        assert_eq!(explicit["ready_pane_ids"], json!([8, 7]));
        first.unregister().unwrap();
        assert_eq!(
            second
                .query_ssh_group_run(3, 9, 2, None)
                .await
                .unwrap_err()
                .to_string(),
            "unknown window 3"
        );
    }
}
