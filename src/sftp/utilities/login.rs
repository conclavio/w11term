use super::*;
use crate::{
    events::{ToolBundleSyncStatus, ToolPathSyncStatus},
    options::ToolSync,
};

pub(crate) struct ToolSyncResult {
    pub status: ToolBundleSyncStatus,
    pub path: Option<ToolPathSyncStatus>,
    pub path_error: Option<anyhow::Error>,
}

impl ToolSyncResult {
    fn held() -> Self {
        Self {
            status: ToolBundleSyncStatus::Held,
            path: None,
            path_error: None,
        }
    }
}

pub(crate) async fn sync_login_tools(
    session: &russh::client::Handle<crate::ssh::Handler>,
    settings: &ToolSync,
) -> Result<ToolSyncResult> {
    ensure!(
        safe_component(&settings.name) && !settings.name.starts_with('.'),
        "Invalid approved tool bundle name"
    );
    ensure!(
        valid_bin_directory(&settings.bin_directory),
        "Invalid approved tool bin directory"
    );
    let engine = Engine::connect(session).await?;
    let home = engine.dotfile_home().await?;
    ensure!(
        home == settings.home,
        "SFTP home differs from the approved tool bundle home"
    );
    let relative = format!("{STORE}/{}", settings.name);
    let store = under_home(&home, &relative);
    let directories = engine
        .utility_directories(&home, &format!("{relative}/versions"))
        .await?;
    if engine
        .utility_selector_state(&store, &directories)
        .await?
        .manual
    {
        return Ok(ToolSyncResult::held());
    }
    ensure!(
        !settings.bundle.as_os_str().is_empty(),
        "Choose a Windows tool bundle manifest"
    );
    let path = settings.bundle.clone();
    let (bundle, digest) =
        tokio::task::spawn_blocking(move || UtilityBundle::load_with_manifest_sha256(&path))
            .await?
            .context("Reading the configured tool bundle")?;
    if let Some(pin) = &settings.manifest_sha256 {
        ensure!(
            digest == normalize_sha256(pin).context("Invalid tool manifest SHA-256 pin")?,
            "The tool manifest changed after approval. Review and approve it again."
        );
    }
    ensure!(
        bundle.name == settings.name && bundle.bin_directory == settings.bin_directory,
        "The manifest name or bin directory changed. Approve the tool bundle again."
    );
    let plan = engine
        .utility_plan(session, bundle, InstallMode::Sync)
        .await?;
    ensure!(
        plan.home == settings.home,
        "SFTP home differs from the approved tool bundle home"
    );
    if plan.selector.manual {
        return Ok(ToolSyncResult::held());
    }
    let mut stage = JobStage::Checking("Checking approved tool bundle");
    let result = engine
        .activate_utility_plan(session, &plan, &mut stage)
        .await
        .with_context(|| stage.interrupted())?;
    let updated =
        result["changed"] == true || result["launchers_created"].as_u64().unwrap_or_default() != 0;
    let mut outcome = ToolSyncResult {
        status: if updated {
            ToolBundleSyncStatus::Updated
        } else {
            ToolBundleSyncStatus::Current
        },
        path: None,
        path_error: None,
    };
    if let Some(path) = &settings.add_to_path {
        // Profiles on separate connections can share the same shell startup files.
        let result = install::with_lock(
            session,
            &home,
            engine.sync_tool_path(&home, &settings.bin_directory, path),
        )
        .await;
        match result {
            Ok(changed) => {
                outcome.path = Some(if changed {
                    ToolPathSyncStatus::Updated
                } else {
                    ToolPathSyncStatus::Current
                })
            }
            Err(error) => {
                outcome.path = Some(ToolPathSyncStatus::Skipped);
                outcome.path_error = Some(error);
            }
        }
    }
    Ok(outcome)
}
