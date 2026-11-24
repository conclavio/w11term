use super::*;
use crate::events::{GroupTranscriptLine, GroupTranscriptOutput};
use anyhow::bail;
use regex::Regex;
use std::{borrow::Cow, collections::BTreeMap};
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

pub(super) struct GroupOutputQuery {
    pub(super) pane: Option<u64>,
    pub(super) after_line: Option<u64>,
    pub(super) pattern: Regex,
    pub(super) timeout: Duration,
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

impl Registry {
    pub(super) async fn query_ssh_group_roster(
        &self,
        window: WindowId,
        tab: u64,
        wait_for: Option<Duration>,
    ) -> Result<Value> {
        let deadline = Instant::now() + wait_for.unwrap_or(TIMEOUT);
        let (reply, response) = oneshot::channel();
        self.window_ui(window)?(UiRequest::GetSshGroupRoster {
            tab_id: tab,
            wait_for_readiness: wait_for.is_some(),
            reply,
        });
        let roster = if wait_for.is_some() {
            tokio::time::timeout_at(deadline, response)
                .await
                .context(
                    "wait-ssh-group-ready timed out before all checked, shown hosts were ready",
                )?
                .with_context(|| {
                    format!(
                        "SSH group tab {tab} in window {window} closed during the readiness wait"
                    )
                })?
                .map_err(anyhow::Error::msg)?
        } else {
            receive(window, response, deadline).await?
        };
        Ok(json!({ "window_id": window, "tab_id": tab, "roster": roster }))
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
}

async fn receive(
    window: WindowId,
    response: oneshot::Receiver<Result<Value, String>>,
    deadline: Instant,
) -> Result<Value> {
    tokio::time::timeout_at(deadline, response)
        .await
        .with_context(|| format!("SSH group query timed out for window {window}"))?
        .with_context(|| format!("window {window} closed during SSH group query"))?
        .map_err(anyhow::Error::msg)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            UiRequest::GetSshGroupRoster {
                tab_id,
                wait_for_readiness,
                reply,
            } => {
                assert_eq!(tab_id, 9);
                assert!(!wait_for_readiness);
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
        assert_eq!(
            lookup,
            json!({ "window_id": 3, "tab_id": 9, "run": { "summary": { "run_id": 2 } } })
        );
        assert_eq!(
            second.query_ssh_group_roster(3, 9, None).await.unwrap(),
            json!({ "window_id": 3, "tab_id": 9, "roster": { "hosts": [{ "pane_id": 7 }] } })
        );
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
