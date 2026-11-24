use super::*;
use tokio::time::Instant;

impl Registry {
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
