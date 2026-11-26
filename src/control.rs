//! Local, current-user-only automation. Control requests never focus a desktop window.
use crate::{
    session::{Command, PaneDescription},
    terminal::Snapshot,
};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::Future,
    num::NonZeroUsize,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer},
    sync::{mpsc, oneshot},
};
use wezterm_term::TerminalSize;
use windows::Win32::{
    Foundation::{ERROR_INVALID_PARAMETER, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
};
mod command_history;
mod find;
mod registry;
pub(crate) mod security;
mod sftp_transfers;
mod ssh_groups;
pub use command_history::{CommandOutputFile, CommandStatus};
pub use registry::{PaneTransfer, Registry, WindowId, native_window_matches};
use security::PipeSecurity;
pub use sftp_transfers::{SftpCancelState, SftpTransferStatus};

pub const VERSION: u32 = 1;
pub const MAX_BYTES: usize = 1024 * 1024;
pub const MAX_SSH_GROUP_OUTPUT_LINES: usize = 128;
const MAX_LINE: usize = MAX_BYTES * 2;
const TIMEOUT: Duration = Duration::from_secs(10);
pub type CapturePng =
    Arc<dyn Fn(&Snapshot, TerminalSize, &Path) -> Result<(u32, u32)> + Send + Sync>;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "verb", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Request {
    Record {
        pane: u64,
        path: Option<PathBuf>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        timed: bool,
    },
    Restart {
        pane: u64,
        exit_token: u64,
    },
    StopSshAttempt {
        pane: u64,
        attempt_token: crate::events::PendingSshAttempt,
    },
    RetrySshNow {
        pane: u64,
        retry_token: crate::events::PendingSshRetry,
    },
    DisconnectSshShell {
        pane: u64,
        connection_generation: std::num::NonZeroU64,
    },
    WaitSshConnected {
        pane: u64,
        #[serde(default)]
        after_generation: u64,
        #[serde(default = "default_wait_timeout")]
        timeout_seconds: u64,
    },
    WaitPaneExit {
        pane: u64,
        #[serde(default)]
        after_exit_token: u64,
        #[serde(default = "default_wait_timeout")]
        timeout_seconds: u64,
    },
    WaitForText {
        pane: u64,
        pattern: String,
        #[serde(default)]
        regex: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        connection_generation: Option<std::num::NonZeroU64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after_frame: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        screenshot_path: Option<PathBuf>,
        #[serde(default = "default_wait_timeout")]
        timeout_seconds: u64,
    },
    Events {
        pane: u64,
        #[serde(default)]
        since: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        #[serde(default)]
        timeout_seconds: u64,
    },
    Resize {
        pane: u64,
        cols: usize,
        rows: usize,
    },
    Open {
        profile: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window_id: Option<WindowId>,
        cols: Option<usize>,
        rows: Option<usize>,
        #[serde(default)]
        activate: bool,
    },
    ClosePane {
        pane: u64,
    },
    Sftp {
        pane: u64,
        #[serde(default = "sftp_default_timeout")]
        timeout_seconds: u64,
        operation: crate::sftp::Operation,
    },
    ListSftpTransfers {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pane: Option<u64>,
    },
    CancelSftpTransfer {
        transfer_id: crate::sftp::TransferId,
    },
    WaitSftpTransfer {
        transfer_id: crate::sftp::TransferId,
        #[serde(default = "default_wait_timeout")]
        timeout_seconds: u64,
    },
    List,
    ListWindows,
    ListSshGroups,
    ListSavedSshGroups {
        window_id: WindowId,
    },
    OpenSshGroup {
        window_id: WindowId,
        name: String,
    },
    GetSshGroupRoster {
        window_id: WindowId,
        tab_id: u64,
    },
    WaitSshGroupReady {
        window_id: WindowId,
        tab_id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pane_ids: Option<Vec<u64>>,
        #[serde(default = "default_wait_timeout")]
        timeout_seconds: u64,
    },
    SubmitSshGroupCommand {
        window_id: WindowId,
        tab_id: u64,
        pane_ids: Vec<u64>,
        command: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        dry_run: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        canary_pane_id: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parallel_limit: Option<NonZeroUsize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interval_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queue_expiry_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop_on_failure: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect_output: Option<SshGroupExpectedOutput>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        record_output: Option<PathBuf>,
    },
    GetSshGroupRun {
        window_id: WindowId,
        tab_id: u64,
        run_id: u64,
    },
    GetSshGroupRetryPlan {
        window_id: WindowId,
        tab_id: u64,
        run_id: u64,
    },
    RetrySshGroupRun {
        window_id: WindowId,
        tab_id: u64,
        run_id: u64,
        pane_ids: Vec<u64>,
    },
    SshGroupQueue {
        window_id: WindowId,
        tab_id: u64,
        run_id: u64,
        action: SshGroupQueueAction,
    },
    InterruptSshGroupCommand {
        window_id: WindowId,
        tab_id: u64,
        run_id: u64,
        pane_id: u64,
    },
    SaveSshGroupReport {
        window_id: WindowId,
        tab_id: u64,
        run_id: u64,
        path: PathBuf,
        #[serde(default)]
        format: SshGroupReportFormat,
        #[serde(default)]
        overwrite: bool,
    },
    WaitSshGroupRun {
        window_id: WindowId,
        tab_id: u64,
        run_id: u64,
        #[serde(default = "default_wait_timeout")]
        timeout_seconds: u64,
    },
    WaitSshGroupApproval {
        window_id: WindowId,
        tab_id: u64,
        run_id: u64,
        #[serde(default = "default_wait_timeout")]
        timeout_seconds: u64,
    },
    WaitSshGroupOutput {
        window_id: WindowId,
        tab_id: u64,
        run_id: u64,
        pane: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after_line: Option<u64>,
        pattern: String,
        #[serde(default)]
        regex: bool,
        #[serde(default)]
        match_case: bool,
        #[serde(default = "default_wait_timeout")]
        timeout_seconds: u64,
    },
    GetSshGroupOutput {
        window_id: WindowId,
        tab_id: u64,
        run_id: u64,
        pane: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after_line: Option<u64>,
        #[serde(default = "default_group_output_limit")]
        limit: usize,
    },
    FollowSshGroupOutput {
        window_id: WindowId,
        tab_id: u64,
        run_id: u64,
        pane: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after_line: Option<u64>,
        #[serde(default = "default_wait_timeout")]
        timeout_seconds: u64,
    },
    WorkspaceIdentity,
    WorkspaceWindows {
        key: String,
    },
    SendText {
        pane: u64,
        data_base64: String,
        no_paste: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        connection_generation: Option<std::num::NonZeroU64>,
    },
    GetText {
        pane: u64,
    },
    Inspect {
        pane: u64,
    },
    GetRunningOutput {
        pane: u64,
        start_token: std::num::NonZeroU64,
    },
    WaitRunningOutput {
        pane: u64,
        start_token: std::num::NonZeroU64,
        pattern: String,
        #[serde(default)]
        regex: bool,
        #[serde(default)]
        match_case: bool,
        #[serde(default = "default_wait_timeout")]
        timeout_seconds: u64,
    },
    Playback {
        pane: u64,
        #[serde(default)]
        action: crate::session::ReplayAction,
    },
    ListCommands {
        pane: u64,
        #[serde(default, skip_serializing_if = "CommandStatus::is_all")]
        status: CommandStatus,
    },
    GetCommand {
        pane: u64,
        id: u64,
        connection_generation: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_file: Option<CommandOutputFile>,
    },
    WaitCommand {
        pane: u64,
        after_id: u64,
        connection_generation: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_file: Option<CommandOutputFile>,
        #[serde(default = "default_wait_timeout")]
        timeout_seconds: u64,
    },
    WaitCommandCapture {
        pane: u64,
        start_token: std::num::NonZeroU64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_file: Option<CommandOutputFile>,
        #[serde(default = "default_wait_timeout")]
        timeout_seconds: u64,
    },
    InterruptCommand {
        pane: u64,
        start_token: std::num::NonZeroU64,
    },
    FindText {
        pattern: String,
        pane: Option<u64>,
        #[serde(default)]
        all_panes: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        regex: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        match_case: bool,
    },
    SaveText {
        pane: u64,
        path: PathBuf,
        #[serde(default)]
        overwrite: bool,
    },
    Screenshot {
        pane: u64,
        path: PathBuf,
    },
}
fn default_wait_timeout() -> u64 {
    30
}
fn default_group_output_limit() -> usize {
    MAX_SSH_GROUP_OUTPUT_LINES
}
#[derive(Debug)]
pub enum UiRequest {
    ListSftpTransfers {
        pane: Option<u64>,
        reply: oneshot::Sender<Result<Vec<SftpTransferStatus>, String>>,
    },
    CancelSftpTransfer {
        transfer_id: crate::sftp::TransferId,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    ListSshGroups {
        reply: oneshot::Sender<Result<Value, String>>,
    },
    ListSavedSshGroups {
        reply: oneshot::Sender<Result<Value, String>>,
    },
    OpenSshGroup {
        name: String,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    GetSshGroupRoster {
        tab_id: u64,
        readiness: Option<SshGroupReadiness>,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    SubmitSshGroupCommand {
        tab_id: u64,
        pane_ids: Vec<u64>,
        command: String,
        options: Box<SshGroupCommandOptions>,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    GetSshGroupRun {
        tab_id: u64,
        run_id: u64,
        wait_for: Option<SshGroupRunWait>,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    GetSshGroupRetryPlan {
        tab_id: u64,
        run_id: u64,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    RetrySshGroupRun {
        tab_id: u64,
        run_id: u64,
        pane_ids: Vec<u64>,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    SshGroupQueue {
        tab_id: u64,
        run_id: u64,
        action: SshGroupQueueAction,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    InterruptSshGroupCommand {
        tab_id: u64,
        run_id: u64,
        pane_id: u64,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    CaptureSshGroupReport {
        tab_id: u64,
        run_id: u64,
        format: SshGroupReportFormat,
        reply: oneshot::Sender<Result<SshGroupReportWriter, String>>,
    },
    ReadSshGroupOutput {
        tab_id: u64,
        run_id: u64,
        pane: Option<u64>,
        follow: bool,
        reply: oneshot::Sender<Result<SshGroupOutput, String>>,
    },
    Open {
        profile: String,
        cols: Option<usize>,
        rows: Option<usize>,
        activate: bool,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    ClosePane {
        pane: u64,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    Resize {
        pane: u64,
        cols: usize,
        rows: usize,
        reply: oneshot::Sender<Result<Value, String>>,
    },
}
impl UiRequest {
    pub fn fail(self, message: &str) {
        let reply = match self {
            Self::Open { reply, .. }
            | Self::ClosePane { reply, .. }
            | Self::Resize { reply, .. }
            | Self::CancelSftpTransfer { reply, .. }
            | Self::ListSshGroups { reply }
            | Self::ListSavedSshGroups { reply }
            | Self::OpenSshGroup { reply, .. }
            | Self::GetSshGroupRoster { reply, .. }
            | Self::SubmitSshGroupCommand { reply, .. }
            | Self::GetSshGroupRun { reply, .. }
            | Self::GetSshGroupRetryPlan { reply, .. }
            | Self::SshGroupQueue { reply, .. }
            | Self::InterruptSshGroupCommand { reply, .. } => reply,
            Self::ListSftpTransfers { reply, .. } => {
                let _ = reply.send(Err(message.into()));
                return;
            }
            Self::ReadSshGroupOutput { reply, .. } => {
                let _ = reply.send(Err(message.into()));
                return;
            }
            Self::RetrySshGroupRun { reply, .. } => {
                let _ = reply.send(Err(message.into()));
                return;
            }
            Self::CaptureSshGroupReport { reply, .. } => {
                let _ = reply.send(Err(message.into()));
                return;
            }
        };
        let _ = reply.send(Err(message.into()));
    }

    fn routed_pane(&self) -> Option<u64> {
        match self {
            Self::Resize { pane, .. } | Self::ClosePane { pane, .. } => Some(*pane),
            Self::CancelSftpTransfer { transfer_id, .. } => Some(transfer_id.pane),
            _ => None,
        }
    }

    fn pane_reply_closed(&self) -> bool {
        matches!(self,
            Self::Resize { reply, .. }
            | Self::ClosePane { reply, .. }
            | Self::CancelSftpTransfer { reply, .. }
            if reply.is_closed()
        )
    }
}

pub use ssh_groups::{
    SshGroupCommandOptions, SshGroupDispatchOverrides, SshGroupExpectedOutput, SshGroupOutput,
    SshGroupQueueAction, SshGroupReadiness, SshGroupReportFormat, SshGroupReportWriter,
    SshGroupRunWait,
};
pub use ssh_groups::{validate_group_panes, validate_group_submission};
type UiHandler = Arc<dyn Fn(UiRequest) + Send + Sync>;
fn sftp_default_timeout() -> u64 {
    crate::sftp::DEFAULT_TIMEOUT
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Reply {
    pub version: u32,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
impl Reply {
    pub fn from_result(result: Result<Value>) -> Self {
        match result {
            Ok(value) => Self {
                version: VERSION,
                ok: true,
                result: Some(value),
                error: None,
            },
            Err(error) => Self {
                version: VERSION,
                ok: false,
                result: None,
                error: Some(format!("{error:#}")),
            },
        }
    }
}

#[derive(Clone)]
pub struct Pane {
    pub read_only: bool,
    pub id: u64,
    pub tab_id: u64,
    pub tab_title: Option<String>,
    pub pane_name: Option<String>,
    pub label: String,
    pub active: bool,
    pub visible: bool,
    pub input: mpsc::UnboundedSender<Command>,
    pub capture: Option<CapturePng>,
}
impl Registry {
    async fn pane_events(pane: &Pane) -> Result<crate::events::Events> {
        let (reply, response) = oneshot::channel();
        pane.input
            .send(Command::Events(reply))
            .context("pane is closed")?;
        tokio::time::timeout(TIMEOUT, response)
            .await
            .context("pane did not respond")?
            .context("pane is closed")
    }

    async fn live_frames(pane: &Pane) -> Result<crate::terminal::FrameStore> {
        let (reply, response) = oneshot::channel();
        pane.input
            .send(Command::Frames(reply))
            .context("pane is closed")?;
        tokio::time::timeout(TIMEOUT, response)
            .await
            .context("pane frame request timed out")?
            .context("pane is closed")
    }

    async fn capture(pane: &Pane) -> Result<(Snapshot, TerminalSize)> {
        let (tx, rx) = oneshot::channel();
        pane.input
            .send(Command::Capture(tx))
            .context("pane is closed")?;
        tokio::time::timeout(TIMEOUT, rx)
            .await
            .context("pane capture timed out")?
            .context("pane is closed")
    }

    async fn describe_pane(pane: &Pane) -> Result<PaneDescription> {
        let (reply, response) = oneshot::channel();
        pane.input
            .send(Command::DescribePane(reply))
            .context("pane is closed")?;
        tokio::time::timeout(TIMEOUT, response)
            .await
            .context("pane description timed out")?
            .context("pane is closed")
    }

    pub async fn execute(&self, request: Request, capture_png: CapturePng) -> Result<Value> {
        match request {
            Request::Record { pane, path, timed } => {
                ensure!(
                    !timed || path.is_some(),
                    "timed recording requires a destination path"
                );
                ensure!(
                    path.as_ref().is_none_or(|p| p.is_absolute()),
                    "recording path must be absolute"
                );
                let (reply, response) = oneshot::channel();
                self.pane(pane)?
                    .input
                    .send(Command::Record { path, timed, reply })
                    .context("pane is closed")?;
                tokio::time::timeout(TIMEOUT, response)
                    .await??
                    .map_err(anyhow::Error::msg)
            }
            Request::Open {
                profile,
                window_id,
                cols,
                rows,
                activate,
            } => {
                validate_grid(cols.unwrap_or(120), rows.unwrap_or(32))?;
                let ui = match window_id {
                    Some(id) => self.window_ui(id)?,
                    None => self.active_ui()?,
                };
                let (reply, response) = oneshot::channel();
                ui(UiRequest::Open {
                    profile,
                    cols,
                    rows,
                    activate,
                    reply,
                });
                tokio::time::timeout(TIMEOUT, response)
                    .await
                    .context("open timed out; tab may have been created")?
                    .context("GUI is closed")?
                    .map_err(anyhow::Error::msg)
            }
            Request::ClosePane { pane } => {
                let (reply, response) = oneshot::channel();
                self.post_pane_request(pane, UiRequest::ClosePane { pane, reply });
                tokio::time::timeout(TIMEOUT, response)
                    .await
                    .context("close-pane timed out. Use list to check whether the pane closed")?
                    .context("GUI is closed")?
                    .map_err(anyhow::Error::msg)
            }
            Request::Resize { pane, cols, rows } => {
                validate_grid(cols, rows)?;
                let (reply, response) = oneshot::channel();
                self.post_pane_request(
                    pane,
                    UiRequest::Resize {
                        pane,
                        cols,
                        rows,
                        reply,
                    },
                );
                tokio::time::timeout(TIMEOUT, response)
                    .await
                    .context("resize timed out; geometry may have changed")?
                    .context("GUI is closed")?
                    .map_err(anyhow::Error::msg)
            }
            Request::Restart { pane, exit_token } => {
                let target = self.pane(pane)?;
                let events = Self::pane_events(&target).await?;
                events.restart_exited(&target.input, exit_token)?;
                Ok(json!({"pane":pane,"exit_token":exit_token,"accepted":true}))
            }
            Request::StopSshAttempt {
                pane,
                attempt_token,
            } => {
                let events = Self::pane_events(&self.pane(pane)?).await?;
                ensure!(
                    events.cancel_ssh_attempt(attempt_token),
                    "SSH attempt is no longer pending in this pane"
                );
                Ok(json!({"pane":pane,"attempt_token":attempt_token,"accepted":true}))
            }
            Request::RetrySshNow { pane, retry_token } => {
                let events = Self::pane_events(&self.pane(pane)?).await?;
                ensure!(
                    events.retry_ssh_now(retry_token),
                    "SSH retry is no longer waiting in this pane"
                );
                Ok(json!({"pane":pane,"retry_token":retry_token,"accepted":true}))
            }
            Request::DisconnectSshShell {
                pane,
                connection_generation,
            } => {
                let (reply, response) = oneshot::channel();
                self.pane(pane)?
                    .input
                    .send(Command::DisconnectSshShell {
                        connection: connection_generation.get(),
                        reply,
                    })
                    .context("pane is closed")?;
                tokio::time::timeout(TIMEOUT, response)
                    .await
                    .context("disconnect acknowledgement timed out")?
                    .context("pane is closed")?
                    .map_err(anyhow::Error::msg)?;
                Ok(
                    json!({"pane":pane,"connection_generation":connection_generation,"accepted":true}),
                )
            }
            Request::WaitSshConnected {
                pane,
                after_generation,
                timeout_seconds,
            } => {
                ensure!(
                    (1..=3600).contains(&timeout_seconds),
                    "wait timeout must be 1..=3600 seconds"
                );
                let target = self.pane(pane)?;
                let events = Self::pane_events(&target).await?;
                let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_seconds);
                let generation = tokio::select! {
                    result = events.wait_ssh_connected(after_generation, deadline) => result?,
                    _ = target.input.closed() => anyhow::bail!("pane is closed"),
                };
                Ok(json!({"pane":pane,"connection_generation":generation}))
            }
            Request::WaitPaneExit {
                pane,
                after_exit_token,
                timeout_seconds,
            } => {
                ensure!(
                    (1..=3600).contains(&timeout_seconds),
                    "wait timeout must be 1..=3600 seconds"
                );
                let target = self.pane(pane)?;
                let events = Self::pane_events(&target).await?;
                let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_seconds);
                let exit = tokio::select! {
                    result = events.wait_pane_exit(after_exit_token, deadline) => result?,
                    _ = target.input.closed() => anyhow::bail!("pane is closed"),
                };
                let mut result = json!({
                    "pane": pane,
                    "exit_token": exit.exit_token,
                    "restart_token": exit.restart_token,
                });
                if let Some(phase) = exit.ssh_connection {
                    result["ssh_connection"] = phase.as_json();
                }
                Ok(result)
            }
            Request::Events {
                pane,
                since,
                kind,
                timeout_seconds,
            } => {
                ensure!(
                    kind.as_ref().is_none_or(|kind| !kind.is_empty()),
                    "event kind is empty"
                );
                ensure!(
                    timeout_seconds <= 3600,
                    "events timeout must be 0..=3600 seconds"
                );
                let target = self.pane(pane)?;
                let events = Self::pane_events(&target).await?;
                ensure!(
                    since <= events.cursor(),
                    "event cursor is ahead of this pane; check the instance/pane identity"
                );
                let wait =
                    events.wait_kind(since, Duration::from_secs(timeout_seconds), kind.as_deref());
                let journal = tokio::select! {
                    biased;
                    journal = wait => journal,
                    _ = target.input.closed() => anyhow::bail!("pane is closed"),
                };
                let connection = events.connection_snapshot();
                let mut result =
                    json!({"pane":pane,"journal":journal,"restart_token":connection.restart_token});
                if let Some(phase) = connection.phase {
                    result["ssh_connection"] = phase.as_json();
                }
                if let Some(attempt) = connection.attempt {
                    result["ssh_attempt_token"] = json!(attempt);
                }
                if let Some(retry) = connection.retry {
                    result["ssh_retry_token"] = json!(retry);
                }
                Ok(result)
            }
            Request::WaitForText {
                pane,
                pattern,
                regex,
                timeout_seconds,
                connection_generation,
                after_frame,
                screenshot_path,
            } => {
                ensure!(
                    screenshot_path
                        .as_ref()
                        .is_none_or(|path| path.is_absolute()),
                    "screenshot path must be absolute"
                );
                ensure!(
                    (1..=3600).contains(&timeout_seconds),
                    "wait timeout must be 1..=3600 seconds"
                );
                ensure!(pattern.len() <= 4096, "pattern exceeds 4096 bytes");
                let matcher = if regex {
                    Some(
                        regex::RegexBuilder::new(&pattern)
                            .size_limit(1024 * 1024)
                            .dfa_size_limit(8 * 1024 * 1024)
                            .build()
                            .context("invalid regular expression")?,
                    )
                } else {
                    None
                };
                let target = self.pane(pane)?;
                let expected = connection_generation.map(std::num::NonZeroU64::get);
                let scope = if let Some(generation) = expected {
                    Some((generation, Self::pane_events(&target).await?))
                } else {
                    None
                };
                let frames = Self::live_frames(&target).await?;
                ensure!(
                    after_frame.is_none_or(|baseline| baseline <= frames.load().generation),
                    "frame generation is ahead of this pane; check the instance and pane ID"
                );
                let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_seconds);
                let wait = async {
                    loop {
                        let frame = frames.load();
                        if after_frame.is_none_or(|baseline| frame.generation > baseline)
                            && expected
                                .is_none_or(|generation| frame.connection_generation == generation)
                        {
                            let text = frame
                                .rows
                                .iter()
                                .map(|line| line.as_str().trim_end_matches(' ').to_owned())
                                .collect::<Vec<_>>()
                                .join("\n");
                            if matcher
                                .as_ref()
                                .map_or_else(|| text.contains(&pattern), |r| r.is_match(&text))
                            {
                                return Ok((frame, text));
                            }
                        }
                        ensure!(
                            frames.changed(frame.generation, deadline).await,
                            "wait-for-text timed out without a match"
                        );
                    }
                };
                const STALE: &str =
                    "The pane connection changed or is no longer active; no match was accepted";
                let (frame, text) = if let Some((generation, events)) = scope {
                    let result = tokio::select! {
                        biased;
                        _ = events.terminal_connection_changed(generation) => anyhow::bail!(STALE),
                        _ = target.input.closed() => anyhow::bail!("pane is closed"),
                        result = wait => result?,
                    };
                    ensure!(events.terminal_connection_is(generation), STALE);
                    result
                } else {
                    tokio::select! {
                        biased;
                        result = wait => result?,
                        _ = target.input.closed() => anyhow::bail!("pane is closed"),
                    }
                };
                let mut result = json!({
                    "pane": pane,
                    "matched": true,
                    "generation": frame.generation,
                    "cols": frame.cols,
                    "rows": frame.rows.len(),
                    "text": text,
                });
                if let Some(generation) = expected {
                    result["connection_generation"] = generation.into();
                }
                if let Some(path) = screenshot_path {
                    let (_, capture_png) = self
                        .capture_for(pane, capture_png)
                        .context("matched text but the pane is no longer available for capture")?;
                    let size = frame.size;
                    result["screenshot"] = save_screenshot(capture_png, frame, size, path)
                        .await
                        .context("matched text but failed to save its screenshot")?;
                }
                Ok(result)
            }
            Request::Sftp {
                pane,
                timeout_seconds,
                operation,
            } => {
                ensure!(
                    (1..=3600).contains(&timeout_seconds),
                    "SFTP timeout must be 1..=3600 seconds"
                );
                let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_seconds);
                let (reply, response) = oneshot::channel();
                self.pane(pane)?
                    .input
                    .send(Command::Sftp(crate::sftp::Job {
                        expected_generation: None,
                        operation,
                        deadline,
                        reply,
                        progress: None,
                        cancel: None,
                    }))
                    .context("pane is closed")?;
                let result = tokio::time::timeout_at(deadline + Duration::from_secs(1), response)
                    .await
                    .context("SFTP timed out; outcome may be unknown, do not blindly retry")?
                    .context("SSH connection closed during SFTP operation")?
                    .map_err(anyhow::Error::msg)?;
                Ok(json!({"pane":pane,"sftp":result}))
            }
            Request::ListSftpTransfers { pane } => self.list_sftp_transfers(pane).await,
            Request::CancelSftpTransfer { transfer_id } => {
                let (reply, response) = oneshot::channel();
                self.post_pane_request(
                    transfer_id.pane,
                    UiRequest::CancelSftpTransfer { transfer_id, reply },
                );
                tokio::time::timeout(TIMEOUT, response)
                    .await
                    .context("SFTP cancellation timed out. Check the transfer's current state.")?
                    .context("GUI is closed")?
                    .map_err(anyhow::Error::msg)
            }
            Request::WaitSftpTransfer {
                transfer_id,
                timeout_seconds,
            } => self.wait_sftp_transfer(transfer_id, timeout_seconds).await,
            Request::WorkspaceIdentity => Ok(json!(self.legacy_workspace())),
            Request::WorkspaceWindows { key } => Ok(json!(self.workspace_windows(&key))),
            Request::ListSshGroups => self.list_ssh_groups().await,
            Request::ListSavedSshGroups { window_id } => {
                self.list_saved_ssh_groups(window_id).await
            }
            Request::OpenSshGroup { window_id, name } => self.open_ssh_group(window_id, name).await,
            Request::GetSshGroupRoster { window_id, tab_id } => {
                self.query_ssh_group_roster(window_id, tab_id, None).await
            }
            Request::WaitSshGroupReady {
                window_id,
                tab_id,
                pane_ids,
                timeout_seconds,
            } => {
                ensure!(
                    (1..=3600).contains(&timeout_seconds),
                    "SSH group wait timeout must be 1..=3600 seconds"
                );
                let readiness = match pane_ids {
                    Some(panes) => {
                        validate_group_panes(&panes)?;
                        SshGroupReadiness::Panes(panes)
                    }
                    None => SshGroupReadiness::CheckedShown,
                };
                self.query_ssh_group_roster(
                    window_id,
                    tab_id,
                    Some((Duration::from_secs(timeout_seconds), readiness)),
                )
                .await
            }
            Request::GetSshGroupRun {
                window_id,
                tab_id,
                run_id,
            } => {
                self.query_ssh_group_run(window_id, tab_id, run_id, None)
                    .await
            }
            Request::GetSshGroupRetryPlan {
                window_id,
                tab_id,
                run_id,
            } => self.ssh_group_retry_plan(window_id, tab_id, run_id).await,
            Request::RetrySshGroupRun {
                window_id,
                tab_id,
                run_id,
                pane_ids,
            } => {
                self.retry_ssh_group_run(window_id, tab_id, run_id, pane_ids)
                    .await
            }
            Request::SshGroupQueue {
                window_id,
                tab_id,
                run_id,
                action,
            } => {
                self.change_ssh_group_queue(window_id, tab_id, run_id, action)
                    .await
            }
            Request::InterruptSshGroupCommand {
                window_id,
                tab_id,
                run_id,
                pane_id,
            } => {
                ensure!(pane_id != 0, "pane IDs must be positive");
                self.interrupt_ssh_group_command(window_id, tab_id, run_id, pane_id)
                    .await
            }
            Request::SaveSshGroupReport {
                window_id,
                tab_id,
                run_id,
                path,
                format,
                overwrite,
            } => {
                self.save_ssh_group_report(window_id, tab_id, run_id, path, format, overwrite)
                    .await
            }
            Request::SubmitSshGroupCommand {
                window_id,
                tab_id,
                pane_ids,
                command,
                dry_run,
                canary_pane_id,
                parallel_limit,
                interval_ms,
                queue_expiry_ms,
                stop_on_failure,
                expect_output,
                record_output,
            } => {
                validate_group_submission(&pane_ids, &command, canary_pane_id)?;
                let options = SshGroupCommandOptions::prepare(
                    dry_run,
                    SshGroupDispatchOverrides {
                        canary_pane_id,
                        parallel_limit,
                        interval_ms,
                        queue_expiry_ms,
                        stop_on_failure,
                    },
                    expect_output,
                    record_output,
                )?;
                self.submit_ssh_group_command(window_id, tab_id, pane_ids, command, options)
                    .await
            }
            Request::WaitSshGroupRun {
                window_id,
                tab_id,
                run_id,
                timeout_seconds,
            } => {
                self.query_ssh_group_run(
                    window_id,
                    tab_id,
                    run_id,
                    Some((SshGroupRunWait::Settlement, timeout_seconds)),
                )
                .await
            }
            Request::WaitSshGroupApproval {
                window_id,
                tab_id,
                run_id,
                timeout_seconds,
            } => {
                self.query_ssh_group_run(
                    window_id,
                    tab_id,
                    run_id,
                    Some((SshGroupRunWait::CanaryApproval, timeout_seconds)),
                )
                .await
            }
            Request::WaitSshGroupOutput {
                window_id,
                tab_id,
                run_id,
                pane,
                after_line,
                pattern,
                regex,
                match_case,
                timeout_seconds,
            } => {
                ensure!(
                    (1..=3600).contains(&timeout_seconds),
                    "SSH group wait timeout must be 1..=3600 seconds"
                );
                let pattern = ssh_groups::output_pattern(&pattern, regex, match_case)?;
                self.wait_ssh_group_output(
                    window_id,
                    tab_id,
                    run_id,
                    ssh_groups::GroupOutputQuery {
                        pane,
                        after_line,
                        pattern,
                        timeout: Duration::from_secs(timeout_seconds),
                    },
                )
                .await
            }
            Request::GetSshGroupOutput {
                window_id,
                tab_id,
                run_id,
                pane,
                after_line,
                limit,
            } => {
                ensure!(
                    (1..=MAX_SSH_GROUP_OUTPUT_LINES).contains(&limit),
                    "SSH group output limit must be 1..={MAX_SSH_GROUP_OUTPUT_LINES} lines"
                );
                self.query_ssh_group_output(window_id, tab_id, run_id, pane, after_line, limit)
                    .await
            }
            Request::FollowSshGroupOutput { .. } => {
                anyhow::bail!("follow-ssh-group-output requires a streaming reply")
            }
            Request::ListWindows => Ok(self.list_windows()),
            Request::List => {
                let mut sessions = Vec::new();
                for (window_id, pane) in self.listing() {
                    let mut session = match Self::describe_pane(&pane).await {
                        Ok(PaneDescription {
                            frame,
                            connection,
                            ssh_transport_id,
                        }) => {
                            let mut session = json!({
                                "title": frame.title,
                                "status": frame.status,
                                "cols": frame.size.cols,
                                "rows": frame.size.rows,
                                "prompt": frame.prompt,
                                "connection_generation": frame.connection_generation,
                            });
                            if let Some(phase) = connection.phase {
                                session["ssh_connection"] = phase.as_json();
                                session["restart_token"] = json!(connection.restart_token);
                            }
                            if let Some(attempt) = connection.attempt {
                                session["ssh_attempt_token"] = json!(attempt);
                            }
                            if let Some(retry) = connection.retry {
                                session["ssh_retry_token"] = json!(retry);
                            }
                            if let Some(id) = ssh_transport_id {
                                session["ssh_transport_id"] = id.into();
                            }
                            session
                        }
                        Err(error) => {
                            json!({"status": "unavailable", "error": format!("{error:#}")})
                        }
                    };
                    session["id"] = pane.id.into();
                    session["window_id"] = window_id.into();
                    session["tab_id"] = pane.tab_id.into();
                    if let Some(title) = pane.tab_title {
                        session["tab_title"] = title.into();
                    }
                    if let Some(name) = pane.pane_name {
                        session["pane_name"] = name.into();
                    }
                    session["label"] = pane.label.into();
                    session["active"] = pane.active.into();
                    session["read_only"] = pane.read_only.into();
                    session["visible"] = pane.visible.into();
                    sessions.push(session);
                }
                Ok(json!({"pid":std::process::id(),"sessions":sessions}))
            }
            Request::SendText {
                pane,
                data_base64,
                no_paste,
                connection_generation,
            } => {
                ensure!(
                    no_paste,
                    "only raw input is supported; specify --no-paste / no_paste:true"
                );
                ensure!(
                    data_base64.len() <= MAX_BYTES.div_ceil(3) * 4,
                    "input exceeds 1 MiB"
                );
                let bytes = STANDARD
                    .decode(data_base64)
                    .context("invalid data_base64")?;
                ensure!(bytes.len() <= MAX_BYTES, "input exceeds 1 MiB");
                let count = bytes.len();
                let (tx, rx) = oneshot::channel();
                let command = match connection_generation {
                    Some(generation) => Command::ScopedRaw {
                        connection: generation.get(),
                        bytes,
                        reply: tx,
                    },
                    None => Command::Raw(bytes, tx),
                };
                self.pane(pane)?
                    .input
                    .send(command)
                    .context("pane is closed")?;
                tokio::time::timeout(TIMEOUT, rx)
                    .await
                    .context("SSH write timed out; delivery is unknown, do not blindly retry")?
                    .context("SSH connection closed before acknowledging the write")?
                    .map_err(anyhow::Error::msg)?;
                let mut result = json!({"pane":pane,"bytes_written":count});
                if let Some(generation) = connection_generation {
                    result["connection_generation"] = json!(generation);
                }
                Ok(result)
            }
            Request::FindText {
                pattern,
                pane,
                all_panes,
                regex,
                match_case,
            } => {
                self.find_text(pattern, pane, all_panes, regex, match_case)
                    .await
            }
            Request::ListCommands { pane, status } => self.list_commands(pane, status).await,
            Request::GetRunningOutput { pane, start_token } => {
                let (reply, response) = oneshot::channel();
                self.pane(pane)?
                    .input
                    .send(Command::ReadRunningOutput(start_token.get(), reply))
                    .context("pane is closed")?;
                let sample = tokio::time::timeout(TIMEOUT, response)
                    .await
                    .context("reading running command output timed out")?
                    .context("pane closed before reading running command output")?
                    .map_err(anyhow::Error::msg)?;
                let mut result = serde_json::to_value(sample)?;
                result["pane"] = pane.into();
                Ok(result)
            }
            Request::WaitRunningOutput {
                pane,
                start_token,
                pattern,
                regex,
                match_case,
                timeout_seconds,
            } => {
                ensure!(
                    (1..=3600).contains(&timeout_seconds),
                    "wait timeout must be 1..=3600 seconds"
                );
                let pattern = ssh_groups::output_pattern(&pattern, regex, match_case)?;
                let (reply, response) = oneshot::channel();
                let mut pending = PendingCommandWait {
                    input: self.pane(pane)?.input,
                    response,
                };
                pending
                    .input
                    .send(Command::WaitRunningOutput {
                        start_token: start_token.get(),
                        pattern,
                        reply,
                    })
                    .context("pane is closed")?;
                let matched = tokio::time::timeout(
                    Duration::from_secs(timeout_seconds),
                    &mut pending.response,
                )
                .await
                .context("wait-running-output timed out without a matching line")?
                .context("pane closed before the output matched")?
                .map_err(anyhow::Error::msg)?;
                let mut result = serde_json::to_value(matched)?;
                result["pane"] = pane.into();
                Ok(result)
            }
            Request::GetCommand {
                pane,
                id,
                connection_generation,
                output_file,
            } => {
                self.get_command(pane, id, connection_generation, output_file)
                    .await
            }
            Request::WaitCommand {
                pane,
                after_id,
                connection_generation,
                command,
                output_file,
                timeout_seconds,
            } => {
                self.wait_command(
                    pane,
                    after_id,
                    connection_generation,
                    timeout_seconds,
                    command.as_deref(),
                    output_file,
                )
                .await
            }
            Request::WaitCommandCapture {
                pane,
                start_token,
                output_file,
                timeout_seconds,
            } => {
                self.wait_command_capture(pane, start_token.get(), timeout_seconds, output_file)
                    .await
            }
            Request::InterruptCommand { pane, start_token } => {
                let (reply, response) = oneshot::channel();
                self.pane(pane)?
                    .input
                    .send(Command::InterruptCommand {
                        start_token: start_token.get(),
                        reply,
                    })
                    .context("pane is closed")?;
                tokio::time::timeout(TIMEOUT, response)
                    .await
                    .context("Ctrl+C write was not confirmed before the timeout")?
                    .context("pane closed before confirming the Ctrl+C write")?
                    .map_err(anyhow::Error::msg)?;
                Ok(json!({"pane":pane,"start_token":start_token,"sent":true}))
            }
            Request::Playback { pane, action } => {
                let (reply, response) = oneshot::channel();
                self.pane(pane)?
                    .input
                    .send(Command::Replay { action, reply })
                    .context("pane is closed")?;
                let status = tokio::time::timeout(TIMEOUT, response)
                    .await
                    .context("playback control timed out")?
                    .context("pane closed during playback control")?
                    .map_err(anyhow::Error::msg)?;
                let mut result = serde_json::to_value(status)?;
                result["pane"] = pane.into();
                Ok(result)
            }
            Request::Inspect { pane } => {
                let (reply, response) = oneshot::channel();
                self.pane(pane)?
                    .input
                    .send(Command::ReadModes(reply))
                    .context("pane is closed")?;
                let inspection = tokio::time::timeout(TIMEOUT, response)
                    .await
                    .context("pane inspection timed out")?
                    .context("pane closed before inspection")?;
                let mut report = inspection.modes.as_json();
                report["pane"] = pane.into();
                report["connection_generation"] = inspection.connection_generation.into();
                if let Some(command) = inspection.running_command {
                    report["running_command"] = json!({
                        "id": command.id,
                        "start_token": command.start_token,
                        "command": command.command.as_deref(),
                        "reported_directory": command.reported_directory.as_deref(),
                        "started_unix_ms": command.started_unix_ms,
                        "elapsed_ms": command.elapsed_ms(),
                    });
                }
                if let Some(ssh) = inspection.ssh {
                    report["ssh"] = serde_json::to_value(ssh.as_ref())?;
                }
                if let Some(transport) = inspection.ssh_transport {
                    report["ssh_transport"] = serde_json::to_value(transport.as_ref())?;
                }
                if let Some(connection) = inspection.ssh_connection {
                    report["ssh_connection"] = connection.as_json();
                }
                if let Some(progress) = inspection.ssh_progress {
                    report["ssh_progress"] = serde_json::to_value(progress.as_ref())?;
                }
                Ok(report)
            }
            Request::GetText { pane } => {
                let (frame, size) = Self::capture(&self.pane(pane)?).await?;
                let lines: Vec<_> = frame
                    .rows
                    .iter()
                    .map(|line| line.as_str().trim_end_matches(' ').to_owned())
                    .collect();
                Ok(json!({
                    "pane": pane,
                    "generation": frame.generation,
                    "connection_generation": frame.connection_generation,
                    "cols": size.cols,
                    "rows": size.rows,
                    "lines": lines,
                    "text": lines.join("\n"),
                    "title": frame.title,
                    "status": frame.status,
                    "alternate_screen": frame.alternate,
                }))
            }
            Request::SaveText {
                pane,
                path,
                overwrite,
            } => {
                ensure!(path.is_absolute(), "save-text path must be absolute");
                let (reply, snapshot) = oneshot::channel();
                self.pane(pane)?
                    .input
                    .send(Command::ExportText(reply))
                    .context("pane is closed")?;
                let text = tokio::time::timeout(TIMEOUT, snapshot)
                    .await
                    .context("scrollback capture timed out")?
                    .context("pane closed before capture")?
                    .map_err(anyhow::Error::msg)?;
                let bytes = text.len();
                let saved_path = tokio::task::spawn_blocking(move || {
                    crate::transcript::save(&path, &text, overwrite)?;
                    anyhow::Ok(path)
                })
                .await??;
                Ok(json!({"pane":pane,"path":saved_path,"bytes":bytes}))
            }
            Request::Screenshot { pane, path } => {
                ensure!(path.is_absolute(), "screenshot path must be absolute");
                let (target, capture_png) = self.capture_for(pane, capture_png)?;
                let (frame, size) = Self::capture(&target).await?;
                let generation = frame.generation;
                let mut result = save_screenshot(capture_png, frame, size, path).await?;
                result["pane"] = pane.into();
                result["generation"] = generation.into();
                Ok(result)
            }
        }
    }
}
async fn save_screenshot(
    capture_png: CapturePng,
    frame: impl std::borrow::Borrow<Snapshot> + Send + 'static,
    size: TerminalSize,
    path: PathBuf,
) -> Result<Value> {
    tokio::task::spawn_blocking(move || {
        let (width, height) = capture_png(frame.borrow(), size, &path)?;
        Ok(json!({"path": path, "width": width, "height": height}))
    })
    .await?
}

fn validate_grid(cols: usize, rows: usize) -> Result<()> {
    ensure!(
        (1..=1000).contains(&cols) && (1..=500).contains(&rows),
        "grid must be 1..=1000 columns and 1..=500 rows"
    );
    Ok(())
}

pub fn pipe_name(pid: u32) -> String {
    format!(r"\\.\pipe\w11term-{pid}")
}
pub fn desktop_scope() -> Result<(String, u32, u32)> {
    security::current_scope()
}

pub fn discovery_dir() -> Result<PathBuf> {
    if security::is_elevated()? {
        return Ok(security::elevated_data_dir()?.join("instances"));
    }
    Ok(
        PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is unavailable")?)
            .join("w11term")
            .join("instances"),
    )
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Instance {
    pub version: u32,
    pub pid: u32,
    pub pipe: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_windows: Option<Vec<WorkspaceHint>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceHint {
    pub window_id: WindowId,
    pub workspace: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct WorkspaceMatch {
    window_id: WindowId,
    hwnd: usize,
}

/// Owns the listener and discovery record. Drop stops it and removes our record.
pub struct Server {
    stop: Option<oneshot::Sender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
    manifest: PathBuf,
    registry: Registry,
}
impl Server {
    pub fn start(
        registry: Registry,
        capture_png: impl Fn(&Snapshot, TerminalSize, &Path) -> Result<(u32, u32)>
        + Send
        + Sync
        + 'static,
    ) -> Result<Self> {
        Self::start_in_directory(registry, capture_png, discovery_dir()?)
    }
    /// Embedders/tests can isolate discovery records without changing process environment.
    pub fn start_in_directory(
        registry: Registry,
        capture_png: impl Fn(&Snapshot, TerminalSize, &Path) -> Result<(u32, u32)>
        + Send
        + Sync
        + 'static,
        directory: PathBuf,
    ) -> Result<Self> {
        let capture_png: CapturePng = Arc::new(capture_png);
        std::fs::create_dir_all(&directory)?;
        let pid = std::process::id();
        let manifest = directory.join(format!("{pid}.json"));
        let worker_manifest = manifest.clone();
        let process_registry = registry.clone();
        let (stop_tx, stop_rx) = oneshot::channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::Builder::new().name("terminal-control".into()).spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
            runtime.block_on(async move {
                let security = match PipeSecurity::new() { Ok(s)=>Arc::new(s), Err(e)=>{let _=ready_tx.send(Err(e)); return;} };
                let name = pipe_name(pid);
                let mut listener = match security.create(&name, true) {Ok(p)=>p,Err(e)=>{let _=ready_tx.send(Err(e));return;}};
                let _ = ready_tx.send(Ok(()));
                let ingress = Arc::new(tokio::sync::Semaphore::new(32));
                let commands = Arc::new(tokio::sync::Semaphore::new(8));
                let waits = Arc::new(tokio::sync::Semaphore::new(16));
                tokio::pin!(stop_rx);
                loop {
                    tokio::select! {
                        _ = &mut stop_rx => break,
                        connected = listener.connect() => {
                            if let Err(e) = connected { log::error!("control pipe: {e}"); break; }
                            let next = match security.create(&name, false) {Ok(p)=>p,Err(e)=>{log::error!("control pipe: {e:#}");break;}};
                            let client = std::mem::replace(&mut listener, next);
                            if let Ok(permit) = ingress.clone().try_acquire_owned() {
                                let registry = registry.clone();
                                let capture_png=capture_png.clone(); let security=security.clone();
                                let commands=commands.clone(); let waits=waits.clone();
                                tokio::spawn(async move { let _permit=permit; if let Err(error)=serve(client, registry, capture_png, security, commands, waits).await { log::warn!("control request failed: {error:#}"); } });
                            } else {
                                // Bound overload handling too: never spawn an unbounded rejection task.
                                let mut client=client;
                                let _=tokio::time::timeout(Duration::from_millis(250), write_reply(&mut client,
                                    Err(anyhow::anyhow!("busy: control connection limit reached")))).await;
                            }
                        }
                    }
                }
            });
            let _=std::fs::remove_file(worker_manifest);
        })?;
        ready_rx.recv().context("control server startup failed")??;
        let server = Self {
            stop: Some(stop_tx),
            worker: Some(worker),
            manifest,
            registry: process_registry,
        };
        server.registry.set_manifest(server.manifest.clone())?;
        Ok(server)
    }

    pub fn publish_workspace(&self, workspace: Option<String>) -> Result<()> {
        self.publish_window_workspace(self.registry.window_id(), workspace)
    }

    pub fn publish_window_workspace(&self, id: WindowId, workspace: Option<String>) -> Result<()> {
        self.registry.publish_workspace(id, workspace)
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.registry.stop_publishing();
        let _ = std::fs::remove_file(&self.manifest);
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

async fn serve(
    mut pipe: NamedPipeServer,
    registry: Registry,
    capture_png: CapturePng,
    security: Arc<PipeSecurity>,
    commands: Arc<tokio::sync::Semaphore>,
    waits: Arc<tokio::sync::Semaphore>,
) -> Result<()> {
    let mut read = BufReader::new(&mut pipe).take((MAX_LINE + 1) as u64);
    let mut line = Vec::new();
    tokio::time::timeout(TIMEOUT, read.read_until(b'\n', &mut line)).await??;
    drop(read);
    let result = if let Err(error) = security.authorize(&pipe) {
        Err(error)
    } else if line.len() > MAX_LINE {
        Err(anyhow::anyhow!("request exceeds 2 MiB"))
    } else if !line.ends_with(b"\n") {
        Err(anyhow::anyhow!("request must end with newline"))
    } else {
        match serde_json::from_slice::<Request>(&line) {
            Ok(r) => {
                let is_wait = matches!(
                    r,
                    Request::WaitForText { .. }
                        | Request::WaitCommand { .. }
                        | Request::WaitCommandCapture { .. }
                        | Request::WaitRunningOutput { .. }
                        | Request::WaitSshConnected { .. }
                        | Request::WaitPaneExit { .. }
                        | Request::WaitSftpTransfer { .. }
                        | Request::WaitSshGroupRun { .. }
                        | Request::WaitSshGroupApproval { .. }
                        | Request::WaitSshGroupOutput { .. }
                        | Request::FollowSshGroupOutput { .. }
                        | Request::WaitSshGroupReady { .. }
                        | Request::Events {
                            timeout_seconds: 1..,
                            ..
                        }
                );
                let pool = if is_wait { waits } else { commands };
                match pool.try_acquire_owned() {
                    Ok(_permit) => match r {
                        Request::FollowSshGroupOutput {
                            window_id,
                            tab_id,
                            run_id,
                            pane,
                            after_line,
                            timeout_seconds,
                        } => {
                            let (mut reader, mut writer) = tokio::io::split(&mut pipe);
                            let follow = registry.follow_ssh_group_output(
                                &mut writer,
                                ssh_groups::GroupOutputFollow {
                                    window: window_id,
                                    tab: tab_id,
                                    run: run_id,
                                    pane,
                                    after_line,
                                    timeout_seconds,
                                },
                            );
                            return while_client_waits(&mut reader, follow)
                                .await
                                .unwrap_or(Ok(()));
                        }
                        request if is_wait => {
                            let execution = registry.execute(request, capture_png);
                            let Some(result) = while_client_waits(&mut pipe, execution).await
                            else {
                                return Ok(());
                            };
                            result
                        }
                        request => registry.execute(request, capture_png).await,
                    },
                    Err(_) => Err(anyhow::anyhow!(
                        "busy: control request capacity reached; request was not executed"
                    )),
                }
            }
            Err(e) => Err(e.into()),
        }
    };
    write_reply(&mut pipe, result).await
}
async fn while_client_waits<T>(
    reader: &mut (impl AsyncRead + Unpin),
    wait: impl Future<Output = T>,
) -> Option<T> {
    // A valid client sends one request and then reads replies. Any further
    // input, including a broken pipe or EOF, ends its pending wait.
    let mut probe = [0];
    tokio::select! {
        biased;
        _ = reader.read(&mut probe) => None,
        result = wait => Some(result),
    }
}

struct PendingCommandWait<T> {
    input: mpsc::UnboundedSender<Command>,
    response: oneshot::Receiver<Result<T, &'static str>>,
}

impl<T> Drop for PendingCommandWait<T> {
    fn drop(&mut self) {
        self.response.close();
        let _ = self.input.send(Command::PruneCommandWaits);
    }
}

async fn write_reply(pipe: &mut (impl AsyncWrite + Unpin), result: Result<Value>) -> Result<()> {
    let mut bytes = serde_json::to_vec(&Reply::from_result(result))?;
    bytes.push(b'\n');
    tokio::time::timeout(TIMEOUT, pipe.write_all(&bytes)).await??;
    Ok(())
}

async fn connect(pid: u32) -> Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    let start = std::time::Instant::now();
    loop {
        match ClientOptions::new()
            .security_qos_flags(windows::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION.0)
            .open(pipe_name(pid))
        {
            Ok(pipe) => {
                security::verify_server(&pipe, pid)?;
                return Ok(pipe);
            }
            Err(e) if e.raw_os_error() == Some(231) && start.elapsed() < Duration::from_secs(2) => {
                tokio::time::sleep(Duration::from_millis(20)).await
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("no accessible w11term instance at PID {pid}; nothing was started")
                });
            }
        }
    }
}
pub async fn discover() -> Result<Vec<Instance>> {
    let mut instances = Vec::new();
    for instance in discovery_records(&discovery_dir()?)? {
        if connect(instance.pid).await.is_ok() {
            instances.push(instance);
        }
    }
    Ok(instances)
}

/// Discovery files are hints. Confirm the workspace with the authenticated server
/// before using a PID, including when Windows recycles a stale record's PID.
pub struct WorkspaceOwner {
    pub pid: u32,
    pub window_id: Option<WindowId>,
    pub hwnd: usize,
    process: Arc<OwnedHandle>,
}

impl WorkspaceOwner {
    pub fn is_running(&self) -> Result<bool> {
        match unsafe { WaitForSingleObject(HANDLE(self.process.as_raw_handle()), 0) } {
            WAIT_TIMEOUT => Ok(true),
            WAIT_OBJECT_0 => Ok(false),
            _ => Err(std::io::Error::last_os_error()).context("Checking workspace process"),
        }
    }
}

pub async fn workspace_owner(key: &str) -> Result<Option<WorkspaceOwner>> {
    workspace_owner_for(key, None).await
}

pub async fn workspace_owner_for(
    key: &str,
    caller: Option<WindowId>,
) -> Result<Option<WorkspaceOwner>> {
    workspace_owner_in(&discovery_dir()?, key, caller).await
}

async fn workspace_owner_in(
    directory: &Path,
    key: &str,
    caller: Option<WindowId>,
) -> Result<Option<WorkspaceOwner>> {
    let mut records = discovery_records(directory)?;
    let current = std::process::id();
    records.sort_by_key(|record| record.pid != current);
    let mut owners = Vec::new();
    for instance in records {
        let modern = instance.workspace_windows.is_some();
        if !instance.workspace_windows.as_ref().map_or_else(
            || instance.workspace.as_deref() == Some(key),
            |windows| windows.iter().any(|w| w.workspace == key),
        ) {
            continue;
        }
        let pipe = match connect(instance.pid).await {
            Ok(pipe) => pipe,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        // Keep this process object alive through activation. Windows cannot recycle
        // the PID between the reply on this very pipe and the HWND lookup.
        let handle = match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, instance.pid) } {
            Ok(handle) => handle,
            Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => continue,
            Err(error) => return Err(error).context("Opening workspace process"),
        };
        let process = Arc::new(unsafe { OwnedHandle::from_raw_handle(handle.0) });
        let owner = WorkspaceOwner {
            pid: instance.pid,
            window_id: None,
            hwnd: 0,
            process: process.clone(),
        };
        let request = if modern {
            Request::WorkspaceWindows {
                key: key.to_owned(),
            }
        } else {
            Request::WorkspaceIdentity
        };
        let reply = match call_on(pipe, &request).await {
            Ok(reply) => reply,
            Err(_) if !owner.is_running()? => continue,
            Err(error) => return Err(error),
        };
        ensure!(
            reply.ok,
            "Could not verify workspace owner: {}",
            reply.error.unwrap_or_default()
        );
        if !owner.is_running()? {
            continue;
        }
        if modern {
            let matches: Vec<WorkspaceMatch> = serde_json::from_value(
                reply
                    .result
                    .context("Missing workspace window identities")?,
            )?;
            for matched in matches {
                ensure!(matched.window_id != 0, "Invalid workspace window ID");
                let window = WorkspaceOwner {
                    pid: owner.pid,
                    process: process.clone(),
                    window_id: Some(matched.window_id),
                    hwnd: matched.hwnd,
                };
                if window.pid == current && window.window_id == caller {
                    return Ok(Some(window));
                }
                owners.push(window);
            }
        } else if reply.result.as_ref().and_then(Value::as_str) == Some(key) {
            owners.push(owner);
        }
    }
    ensure!(
        owners.len() < 2,
        "Multiple windows have this workspace open (PIDs {}). Switch from the window you want to use.",
        owners
            .iter()
            .map(|owner| match owner.window_id {
                Some(id) => format!("{} window {id}", owner.pid),
                None => owner.pid.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(owners.pop())
}

fn discovery_records(directory: &Path) -> Result<Vec<Instance>> {
    let entries = match std::fs::read_dir(directory) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut instances = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.path().extension().is_none_or(|s| s != "json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        let Ok(instance) = serde_json::from_slice::<Instance>(&bytes) else {
            continue;
        };
        if entry.file_name() == std::ffi::OsString::from(format!("{}.json", instance.pid))
            && instance.version == VERSION
            && instance.pipe == pipe_name(instance.pid)
        {
            instances.push(instance);
        }
    }
    instances.sort_by_key(|i| i.pid);
    Ok(instances)
}
async fn resolve_pid(pid: Option<u32>) -> Result<u32> {
    Ok(match pid {
        Some(p) => p,
        None => {
            let instances = discover().await?;
            ensure!(
                !instances.is_empty(),
                "no running w11term instance; nothing was started"
            );
            ensure!(
                instances.len() == 1,
                "multiple w11term instances; specify --pid (running PIDs: {})",
                instances
                    .iter()
                    .map(|i| i.pid.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            instances[0].pid
        }
    })
}

pub async fn call(pid: Option<u32>, request: &Request) -> Result<Reply> {
    call_on(connect(resolve_pid(pid).await?).await?, request).await
}

async fn write_request(pipe: &mut NamedPipeClient, request: &Request) -> Result<()> {
    let mut bytes = serde_json::to_vec(request)?;
    bytes.push(b'\n');
    ensure!(bytes.len() <= MAX_LINE, "request exceeds 2 MiB");
    tokio::time::timeout(TIMEOUT, pipe.write_all(&bytes)).await??;
    Ok(())
}

pub async fn follow(
    pid: Option<u32>,
    request: &Request,
    mut on_reply: impl FnMut(&Reply) -> Result<()>,
) -> Result<bool> {
    let Request::FollowSshGroupOutput {
        timeout_seconds, ..
    } = request
    else {
        anyhow::bail!("request does not use streaming replies");
    };
    let mut pipe = connect(resolve_pid(pid).await?).await?;
    write_request(&mut pipe, request).await?;
    let mut read = BufReader::new(pipe);
    let mut response = Vec::new();
    let deadline =
        tokio::time::Instant::now() + Duration::from_secs((*timeout_seconds).clamp(1, 3600) + 15);
    loop {
        response.clear();
        tokio::time::timeout_at(
            deadline,
            (&mut read)
                .take((MAX_LINE + 1) as u64)
                .read_until(b'\n', &mut response),
        )
        .await
        .context("control output stream timed out")??;
        ensure!(
            !response.is_empty(),
            "control output stream ended without a final reply"
        );
        ensure!(
            response.len() <= MAX_LINE,
            "control output reply exceeds 2 MiB"
        );
        let reply = decode_reply(&response)?;
        on_reply(&reply)?;
        if !reply.ok {
            return Ok(false);
        }
        match reply
            .result
            .as_ref()
            .and_then(|result| result.get("kind"))
            .and_then(Value::as_str)
        {
            Some("settled") => return Ok(true),
            Some("gap" | "line") => {}
            _ => anyhow::bail!("unrecognized control output record"),
        }
    }
}

async fn call_on(mut pipe: NamedPipeClient, request: &Request) -> Result<Reply> {
    write_request(&mut pipe, request).await?;
    let mut read = BufReader::new(pipe).take(32 * 1024 * 1024);
    let mut response = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(match request {
            Request::Sftp {
                timeout_seconds, ..
            } => (*timeout_seconds).clamp(1, 3600) + 5,
            Request::WaitForText {
                timeout_seconds, ..
            }
            | Request::WaitCommand {
                timeout_seconds, ..
            }
            | Request::WaitCommandCapture {
                timeout_seconds, ..
            }
            | Request::WaitRunningOutput {
                timeout_seconds, ..
            }
            | Request::WaitSshConnected {
                timeout_seconds, ..
            }
            | Request::WaitPaneExit {
                timeout_seconds, ..
            }
            | Request::WaitSftpTransfer {
                timeout_seconds, ..
            }
            | Request::WaitSshGroupRun {
                timeout_seconds, ..
            }
            | Request::WaitSshGroupApproval {
                timeout_seconds, ..
            }
            | Request::WaitSshGroupOutput {
                timeout_seconds, ..
            }
            | Request::WaitSshGroupReady {
                timeout_seconds, ..
            }
            | Request::Events {
                timeout_seconds, ..
            } => (*timeout_seconds).min(3600) + 15,
            _ => 30,
        }),
        read.read_until(b'\n', &mut response),
    )
    .await
    .context("control reply timed out; command may have completed")??;
    decode_reply(&response)
}

fn decode_reply(response: &[u8]) -> Result<Reply> {
    ensure!(response.ends_with(b"\n"), "incomplete control reply");
    let reply: Reply = serde_json::from_slice(response)?;
    ensure!(
        reply.version == VERSION,
        "unsupported control protocol version"
    );
    Ok(reply)
}

#[cfg(test)]
mod workspace_tests {
    use super::*;

    #[tokio::test]
    async fn verifies_workspace_identity_against_live_server_and_ignores_dead_pid() {
        let directory = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        let server = Server::start_in_directory(
            Registry::for_workspace(Some("desk".into())),
            |_, _, _| unreachable!(),
            directory.path().into(),
        )
        .unwrap();
        assert_eq!(
            workspace_owner_in(directory.path(), "desk", None)
                .await
                .unwrap()
                .map(|owner| owner.pid),
            Some(pid)
        );
        let record = directory.path().join(format!("{pid}.json"));
        let old_hint = std::fs::read(&record).unwrap();
        let mut legacy: Value = serde_json::from_slice(&old_hint).unwrap();
        legacy.as_object_mut().unwrap().remove("workspace_windows");
        std::fs::write(&record, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let older = workspace_owner_in(directory.path(), "desk", None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!((older.pid, older.window_id), (pid, None));

        let sibling = server
            .registry
            .register_window(2, Some("desk".into()))
            .unwrap();
        assert!(
            workspace_owner_in(directory.path(), "desk", None)
                .await
                .is_err()
        );
        for id in [1, 2] {
            let exact = workspace_owner_in(directory.path(), "desk", Some(id))
                .await
                .unwrap()
                .unwrap();
            assert_eq!((exact.pid, exact.window_id), (pid, Some(id)));
        }
        sibling.unregister().unwrap();
        server.publish_workspace(Some("other".into())).unwrap();
        assert_eq!(
            workspace_owner_in(directory.path(), "other", None)
                .await
                .unwrap()
                .map(|owner| owner.pid),
            Some(pid)
        );
        std::fs::write(&record, &old_hint).unwrap();
        assert_eq!(
            workspace_owner_in(directory.path(), "desk", None)
                .await
                .unwrap()
                .map(|owner| owner.pid),
            None
        );
        drop(server);
        std::fs::write(&record, old_hint).unwrap();
        assert_eq!(
            workspace_owner_in(directory.path(), "desk", None)
                .await
                .unwrap()
                .map(|owner| owner.pid),
            None
        );
    }
}
