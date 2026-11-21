use super::*;
use tokio::{task::JoinSet, time::Instant};

impl Registry {
    pub(super) async fn find_text(
        &self,
        pattern: String,
        pane: Option<u64>,
        all_panes: bool,
    ) -> Result<Value> {
        ensure!(
            all_panes != pane.is_some(),
            "select either one pane or all panes"
        );
        ensure!(!pattern.is_empty(), "search text is empty");
        ensure!(
            pattern.len() <= crate::search::MAX_QUERY_BYTES,
            "search text exceeds {} UTF-8 bytes",
            crate::search::MAX_QUERY_BYTES
        );
        // A move between windows keeps the session sender and its pane ID.
        // Report the window and tab at the instant the search was requested.
        let mut targets = self.listing();
        if let Some(id) = pane {
            targets.retain(|(_, target)| target.id == id);
            ensure!(!targets.is_empty(), "unknown pane {id}");
        }
        let mut workers = JoinSet::new();
        let mut rows = BTreeMap::new();
        let deadline = Instant::now() + TIMEOUT;
        let mut targets = targets.into_iter();
        loop {
            // Long-running panes may complete in any order. Limit concurrent
            // scans while allowing ready siblings to make progress.
            for (window_id, target) in targets.by_ref().take(4 - workers.len()) {
                let query = pattern.clone();
                workers.spawn(async move {
                    let result = search_pane(&target, query, deadline).await;
                    (window_id, target, result)
                });
            }
            let Some(worker) = workers.join_next().await else {
                break;
            };
            let (window_id, target, result) = worker.context("scrollback search task failed")?;
            let mut row = match result {
                Ok(row) => row,
                Err(error) if pane.is_some() => return Err(error),
                Err(error) => json!({"error": error.to_string()}),
            };
            row["pane"] = json!(target.id);
            row["tab_id"] = json!(target.tab_id);
            row["window_id"] = json!(window_id);
            rows.insert(target.id, row);
        }
        Ok(json!({"panes": rows.into_values().collect::<Vec<_>>()}))
    }
}

async fn search_pane(target: &Pane, query: String, deadline: Instant) -> Result<Value> {
    ensure!(Instant::now() < deadline, "scrollback search timed out");
    let (reply, response) = oneshot::channel();
    target
        .input
        .send(Command::SearchPreview { query, reply })
        .context("pane is closed")?;
    let preview = tokio::time::timeout_at(deadline, response)
        .await
        .context("scrollback search timed out")?
        .context("pane closed during scrollback search")?;
    ensure!(
        !preview.updating,
        "terminal is updating a synchronized frame; search again after it finishes"
    );
    let first = preview.first.map(|(excerpt, anchor)| {
        let start = anchor.start();
        json!({"row": start.row, "column": start.col, "excerpt": excerpt})
    });
    Ok(json!({"count": preview.total, "truncated": preview.truncated, "first": first}))
}
