use super::*;
use crate::ssh::TriageReport;
use std::num::NonZeroU64;

impl Registry {
    pub(super) async fn linux_triage(
        &self,
        pane: u64,
        generation: Option<NonZeroU64>,
        output_file: Option<PathBuf>,
    ) -> Result<Value> {
        if output_file.is_some() {
            ensure!(
                generation.is_some(),
                "Preview the selected SSH pane and provide its connection generation"
            );
        }
        let (reply, response) = oneshot::channel();
        self.pane(pane)?
            .input
            .send(Command::TriageTarget { generation, reply })
            .context("The selected pane is closed")?;
        let target = tokio::time::timeout(TIMEOUT, response)
            .await
            .context("The pane did not respond to the triage request")?
            .context("The selected pane is closed")?
            .map_err(anyhow::Error::msg)?;
        let Some(path) = output_file else {
            return Ok(json!({ "pane": pane, "plan": target.preview() }));
        };
        TriageReport::check_destination(&path)?;
        let report = target.collect().await?;
        let path = tokio::task::spawn_blocking(move || -> Result<PathBuf> {
            report.save(&path)?;
            Ok(path)
        })
        .await??;
        Ok(json!({ "pane": pane, "report_path": path }))
    }
}
