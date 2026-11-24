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

    pub(super) async fn get_ssh_group_run(
        &self,
        window: WindowId,
        tab: u64,
        run: u64,
    ) -> Result<Value> {
        let deadline = Instant::now() + TIMEOUT;
        let (reply, response) = oneshot::channel();
        self.window_ui(window)?(UiRequest::GetSshGroupRun {
            tab_id: tab,
            run_id: run,
            reply,
        });
        let run = receive(window, response, deadline).await?;
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
                reply,
            } => {
                assert_eq!((tab_id, run_id), (9, 2));
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
        let lookup = second.get_ssh_group_run(3, 9, 2).await.unwrap();
        assert_eq!(
            lookup,
            json!({ "window_id": 3, "tab_id": 9, "run": { "summary": { "run_id": 2 } } })
        );
        first.unregister().unwrap();
        assert_eq!(
            second
                .get_ssh_group_run(3, 9, 2)
                .await
                .unwrap_err()
                .to_string(),
            "unknown window 3"
        );
    }
}
