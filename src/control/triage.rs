use super::*;
use crate::ssh::{
    CaptureStop, TriageReport, TriageTarget,
    triage::{cpu_profile::Recording, diagnostic_destination},
};
use std::num::NonZeroU64;

pub(super) struct PacketCapturePaths {
    pub capture: PathBuf,
    pub report: Option<PathBuf>,
}

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
            .capture_cpu_profile(options, process_start_ticks, perf_data_file, None)
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

    pub(super) async fn preview_packet_capture(
        &self,
        pane: u64,
        options: PacketCaptureOptions,
    ) -> Result<Value> {
        let mut target = self.diagnostic_target(pane, None).await?;
        Ok(json!({ "pane": pane, "plan": target.preview_packet_capture(options).await? }))
    }

    pub(super) async fn list_packet_interfaces(&self, pane: u64) -> Result<Value> {
        let mut target = self.diagnostic_target(pane, None).await?;
        let list = target.list_packet_interfaces().await?;
        Ok(json!({
            "pane": pane,
            "connection_generation": target.connection_generation(),
            "interfaces": list.interfaces,
            "truncated": list.truncated,
        }))
    }

    pub(super) async fn preview_syscall_capture(
        &self,
        pane: u64,
        options: SyscallOptions,
    ) -> Result<Value> {
        let mut target = self.diagnostic_target(pane, None).await?;
        Ok(json!({ "pane": pane, "plan": target.preview_syscall_capture(options).await? }))
    }

    pub(super) async fn capture_syscalls(
        &self,
        pane: u64,
        options: SyscallOptions,
        generation: NonZeroU64,
        review_id: &str,
        job_id: &str,
        path: PathBuf,
    ) -> Result<Value> {
        let target = self.diagnostic_target(pane, Some(generation)).await?;
        let capture = target
            .capture_syscalls(pane, options, review_id, job_id, path)
            .await?;
        Ok(
            json!({ "pane": pane, "connection_generation": generation, "job_id": job_id, "capture": capture }),
        )
    }

    pub(super) async fn stop_syscall_capture(
        &self,
        pane: u64,
        generation: NonZeroU64,
        job_id: &str,
    ) -> Result<Value> {
        let target = self.diagnostic_target(pane, Some(generation)).await?;
        target.stop_syscall_capture(pane, job_id)?;
        Ok(json!({ "pane": pane, "job_id": job_id, "stop_requested": true }))
    }

    pub(super) async fn capture_packets(
        &self,
        pane: u64,
        options: PacketCaptureOptions,
        connection_generation: NonZeroU64,
        review_id: &str,
        job_id: Option<String>,
        paths: PacketCapturePaths,
    ) -> Result<Value> {
        if let Some(report) = &paths.report {
            ensure!(
                report != &paths.capture,
                "Choose separate capture and report files"
            );
            diagnostic_destination(report, "packet capture report")?;
        }
        let target = self
            .diagnostic_target(pane, Some(connection_generation))
            .await?;
        let job_id = job_id.unwrap_or_else(|| format!("{:032x}", rand::random::<u128>()));
        let capture = target
            .capture_packets(
                pane,
                options,
                review_id,
                &job_id,
                paths.capture,
                CaptureStop::default(),
            )
            .await?;
        let mut result = json!({
            "pane": pane,
            "connection_generation": connection_generation,
            "job_id": job_id,
            "capture": &capture,
        });
        if let Some(path) = paths.report {
            let destination = path.clone();
            let saved = tokio::task::spawn_blocking(move || capture.save_report(&destination))
                .await
                .context("Writing the packet capture report")
                .and_then(|saved| saved);
            match saved {
                Ok(()) => result["report_path"] = json!(path),
                Err(error) => result["report_error"] = json!(format!("{error:#}")),
            }
        }
        Ok(result)
    }

    pub(super) async fn stop_packet_capture(
        &self,
        pane: u64,
        generation: NonZeroU64,
        job_id: &str,
    ) -> Result<Value> {
        let target = self.diagnostic_target(pane, Some(generation)).await?;
        target.stop_packet_capture(pane, job_id)?;
        Ok(json!({ "pane": pane, "job_id": job_id, "stop_requested": true }))
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
