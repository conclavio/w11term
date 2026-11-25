use super::{Registry, TIMEOUT, UiRequest, WindowId};
use crate::sftp::{
    TransferBatchProgress, TransferId, TransferOperation, TransferPhase, TransferProgress,
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use tokio::{sync::oneshot, time::Instant};

#[derive(Debug, Serialize)]
pub struct SftpTransferStatus {
    pub transfer_id: TransferId,
    pub pane: u64,
    pub window_id: WindowId,
    pub tab_id: u64,
    pub label: String,
    pub operation: TransferOperation,
    pub state: TransferPhase,
    pub remote: Option<String>,
    pub progress: Option<TransferProgress>,
    pub batch: Option<TransferBatchProgress>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SftpCancelState {
    Cancelling,
    AlreadyCancelling,
    CancelledBeforeStart,
}

impl Registry {
    pub(super) async fn wait_sftp_transfer(
        &self,
        id: TransferId,
        timeout_seconds: u64,
    ) -> Result<Value> {
        ensure!(
            (1..=3600).contains(&timeout_seconds),
            "wait timeout must be 1..=3600 seconds"
        );
        let deadline = Instant::now() + std::time::Duration::from_secs(timeout_seconds);
        let pane = self.pane(id.pane)?;
        let events = tokio::time::timeout_at(deadline, Self::pane_events(&pane))
            .await
            .context("Timed out while opening the pane's transfer journal")??;
        let event = tokio::select! {
            biased;
            result = events.wait_sftp_transfer(id, deadline) => result?,
            _ = pane.input.closed() => events.sftp_transfer_result(id)
                .context("Pane closed before a final SFTP transfer result was recorded")?,
        };
        Ok(json!({ "pane": id.pane, "sequence": event.sequence,
            "timestamp_ms": event.timestamp_ms, "transfer": event.data }))
    }

    pub(super) async fn list_sftp_transfers(&self, pane: Option<u64>) -> Result<Value> {
        if let Some(pane) = pane {
            self.pane(pane)?;
        }
        let deadline = Instant::now() + TIMEOUT;
        let responses: Vec<_> = self
            .all_window_uis()?
            .into_iter()
            .map(|(window, ui)| {
                let (reply, response) = oneshot::channel();
                ui(UiRequest::ListSftpTransfers { pane, reply });
                (window, response)
            })
            .collect();

        // If a pane moved between UI snapshots, keep the row from its current
        // owner. The transfer token is identical in both windows.
        let mut samples = Vec::new();
        for (window, response) in responses {
            let rows = tokio::time::timeout_at(deadline, response)
                .await
                .with_context(|| format!("SFTP transfer listing timed out for window {window}"))?
                .with_context(|| format!("GUI for window {window} is closed"))?
                .map_err(anyhow::Error::msg)?;
            samples.extend(rows);
        }
        let owners: BTreeMap<_, _> = self
            .listing()
            .into_iter()
            .map(|(window, pane)| (pane.id, window))
            .collect();
        let mut transfers = BTreeMap::new();
        for row in samples {
            if owners.get(&row.pane) == Some(&row.window_id) {
                transfers.insert(row.transfer_id, row);
            }
        }
        Ok(
            json!({ "pid": std::process::id(), "transfers": transfers.into_values().collect::<Vec<_>>() }),
        )
    }
}
