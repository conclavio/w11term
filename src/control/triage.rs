use super::*;
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
        ensure!(
            path.is_absolute() && path.file_name().is_some(),
            "Choose an absolute path for a new triage report"
        );
        let parent = path
            .parent()
            .context("The triage report has no parent directory")?;
        ensure!(
            parent.is_dir(),
            "The triage report directory does not exist"
        );
        ensure!(
            !path.try_exists()?,
            "The triage report destination already exists"
        );
        let report = target.collect().await?;
        let path = tokio::task::spawn_blocking(move || -> Result<PathBuf> {
            let directory = path
                .parent()
                .context("The triage report has no parent directory")?;
            let mut staged = tempfile::NamedTempFile::new_in(directory)?;
            serde_json::to_writer_pretty(staged.as_file_mut(), &report)?;
            staged.as_file().sync_all()?;
            staged
                .persist_noclobber(&path)
                .map_err(|error| error.error)
                .context("Saving the Linux triage report")?;
            Ok(path)
        })
        .await??;
        Ok(json!({ "pane": pane, "report_path": path }))
    }
}
