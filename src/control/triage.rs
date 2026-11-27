use super::*;
use crate::ssh::{TriageReport, TriageTarget, triage::cpu_profile::Recording};
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
        let target = self.diagnostic_target(pane, generation).await?;
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

    pub(super) async fn preview_cpu_profile(
        &self,
        pane: u64,
        options: CpuProfileOptions,
    ) -> Result<Value> {
        let target = self.diagnostic_target(pane, None).await?;
        let plan = target.preview_cpu_profile(options).await?;
        Ok(json!({ "pane": pane, "plan": plan }))
    }

    pub(super) async fn capture_cpu_profile(
        &self,
        pane: u64,
        options: CpuProfileOptions,
        generation: NonZeroU64,
        process_start_ticks: u64,
        path: PathBuf,
        perf_data_file: Option<PathBuf>,
    ) -> Result<Value> {
        Recording::check_destinations(&path, perf_data_file.as_deref())?;
        let target = self.diagnostic_target(pane, Some(generation)).await?;
        let profile = target
            .capture_cpu_profile(options, process_start_ticks, perf_data_file)
            .await?;
        let (path, summary) = tokio::task::spawn_blocking(move || -> Result<_> {
            let summary = profile.save(&path)?;
            Ok((path, summary))
        })
        .await??;
        Ok(json!({
            "pane": pane,
            "connection_generation": generation,
            "target_pid": options.target_pid,
            "profile_path": path,
            "summary": summary,
        }))
    }

    async fn diagnostic_target(
        &self,
        pane: u64,
        generation: Option<NonZeroU64>,
    ) -> Result<TriageTarget> {
        let (reply, response) = oneshot::channel();
        self.pane(pane)?
            .input
            .send(Command::TriageTarget { generation, reply })
            .context("The selected pane is closed")?;
        tokio::time::timeout(TIMEOUT, response)
            .await
            .context("The pane did not respond to the performance diagnostic request")?
            .context("The selected pane is closed")?
            .map_err(anyhow::Error::msg)
    }
}
