//! SFTP v3 on a sibling channel of the pane's authenticated SSH connection.
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use russh_sftp::{
    client::{RawSftpSession, error::Error, rawsession::Limits},
    extensions,
    protocol::{FileAttributes, OpenFlags, StatusCode, Version},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, oneshot, watch},
    time::Instant,
};

mod atomic_upload;
mod directory;
mod group_download;
pub use group_download::{GroupDownloadFolder, group_download_name, group_download_paths};
mod group_upload;
pub use group_upload::{GroupUploadFile, GroupUploadSet, validate_group_upload_path};
mod drop_source;
mod editor;
mod find;
pub(crate) use find::MAX_PATH_BYTES as MAX_SEARCH_PATH_BYTES;
pub use find::{
    ContentMatch, ContentScan, FileSizeRange, FilenameFilter, FilenameMatch, FilenameSearch,
    FilenameSort, ModifiedDateRange, SearchProgress, valid_filename_query,
};
mod resume_download;
use resume_download::ResumeDownload;
mod checksum;
mod text_preview;
mod transfer_status;
pub use transfer_status::{
    TransferBatchProgress, TransferId, TransferOperation, TransferOutcome, TransferPhase,
};
mod upload_directory;
pub(crate) use directory::valid_component;
pub use directory::{SearchDownload, validate_download_name, validate_download_names};
pub use drop_source::{DropSource, classify_drop};

pub const CHUNK: usize = 32 * 1024;
pub const TEXT_PREVIEW_MAX_BYTES: usize = 256 * 1024;
pub const DEFAULT_TIMEOUT: u64 = 600;
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

pub fn normalize_sha256(value: &str) -> Result<String> {
    let value = value.trim();
    ensure!(
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Enter a SHA-256 digest of 64 hexadecimal digits"
    );
    Ok(value.to_ascii_lowercase())
}

fn digest_hex(digest: Sha256) -> String {
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

struct DownloadChecks {
    max_bytes: Option<u64>,
    sha256: Option<String>,
    resume: bool,
    preserve_mtime: bool,
}

fn set_download_mtime(file: &std::fs::File, mtime: Option<u32>) -> Result<()> {
    if let Some(seconds) = mtime {
        file.set_modified(UNIX_EPOCH + Duration::from_secs(u64::from(seconds)))
            .context("preserving the remote modification time")?;
    }
    Ok(())
}

enum DownloadStaging {
    Temporary(tempfile::NamedTempFile),
    Resumable(ResumeDownload),
}

impl DownloadStaging {
    fn cancelled_reply(&self, local: &Path, remote: &str, bytes: u64) -> Value {
        let mut reply = cancelled_transfer(local, remote, bytes, false);
        if let Self::Resumable(partial) = self {
            reply["partial_file"] = json!(partial.path);
        }
        reply
    }

    fn publish(
        self,
        file: std::fs::File,
        destination: PathBuf,
        overwrite: bool,
        mtime: Option<u32>,
    ) -> Result<()> {
        match self {
            Self::Temporary(temp) => {
                drop(file);
                apply_mark_of_the_web(temp.path())?;
                set_download_mtime(temp.as_file(), mtime)?;
                if overwrite {
                    temp.persist(destination)
                } else {
                    temp.persist_noclobber(destination)
                }?;
                Ok(())
            }
            Self::Resumable(partial) => {
                set_download_mtime(&file, mtime)?;
                partial.publish(file, overwrite)
            }
        }
    }
}

#[derive(Default)]
struct UploadOptions {
    overwrite: bool,
    atomic: bool,
    preserve_times: bool,
}

fn upload_times(
    metadata: &std::fs::Metadata,
    preserve_times: bool,
) -> Result<Option<FileAttributes>> {
    if !preserve_times {
        return Ok(None);
    }
    let seconds = |time: std::time::SystemTime| -> Result<u32> {
        Ok(time.duration_since(UNIX_EPOCH)?.as_secs().try_into()?)
    };
    let atime = seconds(metadata.accessed()?).context("SFTP cannot represent the access time")?;
    let mtime =
        seconds(metadata.modified()?).context("SFTP cannot represent the modification time")?;
    Ok(Some(FileAttributes {
        atime: Some(atime),
        mtime: Some(mtime),
        ..FileAttributes::empty()
    }))
}

/// Directory entry types the drawer can delete with the SFTP v3 primitives.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
}

impl EntryKind {
    pub fn from_permissions(permissions: Option<u32>) -> Option<Self> {
        match permissions? & 0o170000 {
            0o100000 => Some(Self::File),
            0o040000 => Some(Self::Directory),
            0o120000 => Some(Self::Symlink),
            _ => None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Operation {
    Info,
    /// Explicit Save from the editor. An absent original requests a new file.
    #[serde(skip)]
    SaveText {
        path: String,
        original: Option<String>,
        text: String,
    },
    Get {
        remote: String,
        local: PathBuf,
        #[serde(default)]
        max_bytes: Option<u64>,
        #[serde(default)]
        overwrite: bool,
        #[serde(default)]
        expected_sha256: Option<String>,
        #[serde(default)]
        resume: bool,
        #[serde(default)]
        preserve_mtime: bool,
    },
    Sha256 {
        path: String,
        #[serde(default)]
        max_bytes: Option<u64>,
    },
    GetDir {
        remote: String,
        local: PathBuf,
        #[serde(default)]
        preserve_mtime: bool,
    },
    #[serde(skip)]
    GetSearch {
        selection: SearchDownload,
        local: PathBuf,
        preserve_mtime: bool,
    },
    PutDir {
        local: PathBuf,
        remote: String,
        #[serde(default)]
        preserve_times: bool,
    },
    /// The source was chosen by a native Explorer drop on this connection.
    #[serde(skip)]
    PutDrop {
        source: DropSource,
        remote: String,
        overwrite: bool,
        atomic: bool,
        preserve_times: bool,
    },
    /// A file selected once for several generation-bound native uploads.
    #[serde(skip)]
    PutGroupFile {
        source: GroupUploadFile,
        remote: String,
    },
    Put {
        local: PathBuf,
        remote: String,
        #[serde(default)]
        overwrite: bool,
        #[serde(default)]
        atomic: bool,
        #[serde(default)]
        preserve_times: bool,
    },
    Ls {
        path: String,
    },
    Find {
        path: String,
        query: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        regex: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        match_case: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        match_path: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<EntryKind>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        exclude_dir_regex: String,
        #[serde(default, skip_serializing_if = "FileSizeRange::is_unrestricted")]
        file_size: FileSizeRange,
        #[serde(default, skip_serializing_if = "ModifiedDateRange::is_unrestricted")]
        modified_dates: ModifiedDateRange,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        content_query: String,
    },
    /// Read the selected regular file for the native drawer on its current connection.
    #[serde(skip)]
    PreviewText {
        path: String,
    },
    #[serde(skip)]
    EntryProperties {
        path: String,
    },
    /// The absolute path verified by SSHFS can only be opened on the source pane.
    #[serde(skip)]
    ListMountedFolder {
        path: String,
    },
    Opendir {
        path: String,
    },
    Readdir {
        handle: u64,
    },
    Open {
        path: String,
        flags: Vec<String>,
        #[serde(default)]
        attrs: Attributes,
    },
    Close {
        handle: u64,
    },
    Read {
        handle: u64,
        offset: u64,
        len: u32,
    },
    Write {
        handle: u64,
        offset: u64,
        data_base64: String,
    },
    Stat {
        path: String,
    },
    Lstat {
        path: String,
    },
    Fstat {
        handle: u64,
    },
    Setstat {
        path: String,
        attrs: Attributes,
    },
    Fsetstat {
        handle: u64,
        attrs: Attributes,
    },
    Mkdir {
        path: String,
        #[serde(default)]
        attrs: Attributes,
    },
    Rename {
        from: String,
        to: String,
    },
    /// Interactive drawer approval, bound to the inspected connection.
    #[serde(skip)]
    RenameEntry {
        from: String,
        to: String,
        search_kind: Option<EntryKind>,
    },
    /// Delete the selected entry with the type and SSH connection the user approved.
    #[serde(skip)]
    RemoveEntry {
        path: String,
        kind: EntryKind,
        check_parent: bool,
    },
    RemoveFile {
        path: String,
    },
    RemoveDir {
        path: String,
    },
    Realpath {
        path: String,
    },
    Readlink {
        path: String,
    },
    Symlink {
        path: String,
        target: String,
        #[serde(default)]
        spec_order: bool,
    },
    Hardlink {
        from: String,
        to: String,
    },
    Fsync {
        handle: u64,
    },
    Statvfs {
        path: String,
    },
    Limits,
    ExpandPath {
        path: String,
    },
}

/// Only fields supported by SFTP v3. Paired fields must be supplied together.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attributes {
    pub size: Option<u64>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub permissions: Option<u32>,
    pub atime: Option<u32>,
    pub mtime: Option<u32>,
}
impl From<FileAttributes> for Attributes {
    fn from(attrs: FileAttributes) -> Self {
        Self {
            size: attrs.size,
            uid: attrs.uid,
            gid: attrs.gid,
            permissions: attrs.permissions,
            atime: attrs.atime,
            mtime: attrs.mtime,
        }
    }
}
impl Attributes {
    fn protocol(&self) -> Result<FileAttributes> {
        ensure!(
            self.uid.is_some() == self.gid.is_some(),
            "uid and gid must be supplied together"
        );
        ensure!(
            self.atime.is_some() == self.mtime.is_some(),
            "atime and mtime must be supplied together"
        );
        Ok(FileAttributes {
            size: self.size,
            uid: self.uid,
            gid: self.gid,
            permissions: self.permissions,
            atime: self.atime,
            mtime: self.mtime,
            ..FileAttributes::empty()
        })
    }
}

pub fn open_flags(flags: &[String]) -> Result<OpenFlags> {
    let mut result = OpenFlags::empty();
    for flag in flags {
        result |= match flag.as_str() {
            "read" => OpenFlags::READ,
            "write" => OpenFlags::WRITE,
            "append" => OpenFlags::APPEND,
            "create" => OpenFlags::CREATE,
            "truncate" => OpenFlags::TRUNCATE,
            "exclusive" => OpenFlags::EXCLUDE,
            _ => bail!("unknown open flag {flag}"),
        };
    }
    ensure!(
        result.intersects(OpenFlags::READ | OpenFlags::WRITE),
        "open requires read and/or write"
    );
    ensure!(
        !result.intersects(OpenFlags::APPEND | OpenFlags::TRUNCATE | OpenFlags::CREATE)
            || result.contains(OpenFlags::WRITE),
        "append/create/truncate require write"
    );
    ensure!(
        !result.contains(OpenFlags::EXCLUDE) || result.contains(OpenFlags::CREATE),
        "exclusive requires create"
    );
    Ok(result)
}

/// Bytes written to the local file or acknowledged by the remote server.
/// The total is a metadata estimate. Completion is reported by the job reply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TransferProgress {
    pub bytes: u64,
    pub total: Option<u64>,
    /// Complete files acknowledged through CLOSE during a directory upload.
    pub files: Option<u64>,
}

/// Explicit user cancellation belongs to one transfer, independent of its reply
/// receiver. An admitted SFTP request still receives its status before CLOSE.
#[derive(Clone, Debug, Default)]
pub struct TransferCancel(Arc<AtomicBool>);

impl TransferCancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

fn cancelled_transfer(local: &Path, remote: &str, bytes: u64, partial: bool) -> Value {
    json!({"cancelled":true, "local":local, "remote":remote, "bytes_transferred":bytes, "remote_may_be_partial":partial})
}

fn report(progress: Option<&watch::Sender<TransferProgress>>, bytes: u64, total: Option<u64>) {
    if let Some(progress) = progress {
        progress.send_replace(TransferProgress {
            bytes,
            total: total.filter(|size| *size >= bytes),
            files: None,
        });
    }
}

type JobResult = Result<Value, String>;

#[derive(Debug)]
pub enum JobProgress {
    Transfer(watch::Sender<TransferProgress>),
    Search(watch::Sender<SearchProgress>),
}

#[derive(Debug)]
pub struct Job {
    /// A UI action approved against a particular live SSH connection.
    /// The worker owns that connection and rejects stale approvals before I/O.
    pub expected_generation: Option<u64>,
    pub operation: Operation,
    pub deadline: Instant,
    pub reply: oneshot::Sender<JobResult>,
    pub progress: Option<JobProgress>,
    /// Used by native transfers. A stopped transfer replies with cancelled,
    /// bytes_transferred and remote_may_be_partial after closing its handle.
    pub cancel: Option<TransferCancel>,
}

// The SSH transport aborts its SFTP worker when the connection goes away.
// Preserve the uncertain outcome of an admitted request across that teardown.
struct PendingReply {
    sender: Option<oneshot::Sender<JobResult>>,
    stage: JobStage,
}

enum JobStage {
    Ordinary,
    ResumableDownload,
    DirectoryDownload {
        staging: Option<tempfile::TempDir>,
        deadline: Instant,
    },
    DirectoryUpload(upload_directory::UploadState),
    Upload,
    NativeUpload {
        remote: String,
        open_dispatched: bool,
        confirmed: u64,
    },
    Checking(&'static str),
    EditorSave {
        temporary: String,
        publishing: bool,
    },
    AtomicUpload {
        temporary: String,
        publishing: bool,
    },
    AwaitingStatus(&'static str),
}

impl JobStage {
    fn take_directory(&mut self) -> Option<tempfile::TempDir> {
        match self {
            Self::DirectoryDownload { staging, .. } => staging.take(),
            _ => None,
        }
    }

    fn interrupted(&self) -> Cow<'static, str> {
        match self {
            Self::Ordinary => "SFTP operation interrupted; outcome may be unknown".into(),
            Self::ResumableDownload => "Resumable download interrupted. Check the destination, then retry with the same local path and SHA-256 to continue from any partial file".into(),
            Self::DirectoryDownload { .. } => "Folder download interrupted before publication".into(),
            Self::DirectoryUpload(state) => state.interrupted().into(),
            Self::Upload => {
                "SFTP upload interrupted; remote destination may contain a partial file".into()
            }
            Self::NativeUpload { remote, open_dispatched: true, confirmed } =>
                format!("Upload interrupted after {confirmed} confirmed bytes; remote destination {remote} may contain a partial file").into(),
            Self::NativeUpload { open_dispatched: false, .. } =>
                "Upload interrupted before opening the remote destination; 0 confirmed bytes".into(),
            Self::EditorSave { temporary, publishing } => {
                let state = if *publishing {
                    "publication outcome unknown; refresh and check the destination"
                } else {
                    "destination was not replaced"
                };
                format!("Save interrupted; {state}. Temporary file may remain at {temporary}").into()
            }
            Self::AtomicUpload { temporary, publishing } => {
                let state = if *publishing {
                    "publication outcome unknown; refresh and check the destination"
                } else {
                    "destination was not replaced"
                };
                format!("Atomic upload interrupted; {state}. Temporary file may remain at {temporary}").into()
            }
            Self::Checking(message) | Self::AwaitingStatus(message) => (*message).into(),
        }
    }
}

impl PendingReply {
    fn finish(mut self, result: JobResult) -> Result<(), JobResult> {
        self.sender.take().unwrap().send(result)
    }
}

impl Drop for PendingReply {
    fn drop(&mut self) {
        if let Some(sender) = self.sender.take() {
            let result = Err(self.stage.interrupted().into());
            if let Some(stage) = self.stage.take_directory() {
                // The SSH connection was torn down. The cleanup task owns the
                // reply and will deliver it after the uncommitted tree is gone.
                directory::clean_up(stage, sender, result);
            } else {
                let _ = sender.send(result);
            }
        }
    }
}

pub(crate) async fn run(
    session: Arc<russh::client::Handle<crate::ssh::Handler>>,
    generation: u64,
    mut jobs: mpsc::Receiver<Job>,
    mut cleanup: mpsc::UnboundedReceiver<Job>,
) {
    let mut engine: Option<Engine> = None;
    loop {
        let job = tokio::select! {
            biased;
            Some(job) = cleanup.recv() => job,
            job = jobs.recv() => match job {Some(job)=>job,None=>break},
            _ = tokio::time::sleep(Duration::from_secs(300)) => { engine = None; continue; }
        };
        let Job {
            expected_generation,
            operation,
            deadline,
            reply,
            progress,
            cancel,
        } = job;
        if expected_generation.is_none() {
            let missing = match operation {
                Operation::SaveText { .. } => Some("The editor needs its original SSH connection"),
                Operation::RenameEntry { .. } => {
                    Some("Missing SSH connection approval; rename was not started")
                }
                Operation::RemoveEntry { .. } => {
                    Some("Missing SSH connection approval; delete was not started")
                }
                Operation::ListMountedFolder { .. } => {
                    Some("Missing SSH connection for this mounted folder")
                }
                Operation::PreviewText { .. } => Some("Missing SSH connection for text preview"),
                Operation::PutDrop { .. } => Some("Missing SSH connection for this dropped upload"),
                Operation::PutGroupFile { .. } => {
                    Some("Missing SSH connection for this group upload")
                }
                Operation::EntryProperties { .. } => {
                    Some("Missing SSH connection for file properties")
                }
                Operation::GetSearch { .. } => Some("Missing SSH connection for search results"),
                _ => None,
            };
            if let Some(message) = missing {
                let _ = reply.send(Err(message.into()));
                continue;
            }
        }
        if expected_generation.is_some_and(|expected| expected != generation) {
            let _ = reply.send(Err(
                "SSH connection changed; operation was not started".into()
            ));
            continue;
        }
        if reply.is_closed() && !matches!(operation, Operation::Close { .. }) {
            continue;
        }
        if cancel.as_ref().is_some_and(TransferCancel::is_cancelled)
            && let Some(message) = match operation {
                Operation::PreviewText { .. } => Some("Text preview cancelled"),
                Operation::EntryProperties { .. } => Some("File properties cancelled"),
                Operation::Sha256 { .. } => Some("SHA-256 calculation cancelled"),
                _ => None,
            }
        {
            let _ = reply.send(Err(message.into()));
            continue;
        }
        if cancel.as_ref().is_some_and(TransferCancel::is_cancelled)
            && let Operation::PutGroupFile { source, remote } = &operation
        {
            let _ = reply.send(Ok(cancelled_transfer(source.path(), remote, 0, false)));
            continue;
        }
        if cancel.as_ref().is_some_and(TransferCancel::is_cancelled)
            && let Operation::PutDrop { source, remote, .. } = &operation
        {
            let result = if source.is_directory() {
                upload_directory::UploadState::new(remote.clone(), deadline)
                    .cancelled(source.path())
            } else {
                cancelled_transfer(source.path(), remote, 0, false)
            };
            let _ = reply.send(Ok(result));
            continue;
        }
        if cancel.as_ref().is_some_and(TransferCancel::is_cancelled)
            && let Operation::PutDir { local, remote, .. } = &operation
        {
            let state = upload_directory::UploadState::new(remote.clone(), deadline);
            let _ = reply.send(Ok(state.cancelled(local)));
            continue;
        }
        if cancel.as_ref().is_some_and(TransferCancel::is_cancelled) {
            let paths = match &operation {
                Operation::Put { local, remote, .. }
                | Operation::Get { local, remote, .. }
                | Operation::GetDir { local, remote, .. } => Some((local, remote.as_str())),
                Operation::GetSearch {
                    selection, local, ..
                } => Some((local, selection.root())),
                _ => None,
            };
            if let Some((local, remote)) = paths {
                let _ = reply.send(Ok(cancelled_transfer(local, remote, 0, false)));
                continue;
            }
        }
        let detached_close = reply.is_closed() && matches!(operation, Operation::Close { .. });
        let deadline = if detached_close {
            Instant::now() + Duration::from_secs(15)
        } else {
            deadline
        };
        if deadline <= Instant::now() {
            let _ = reply.send(Err(
                "SFTP request expired in the queue; operation was not started".into(),
            ));
            continue;
        }
        let creates_handle = matches!(
            operation,
            Operation::Open { .. } | Operation::Opendir { .. }
        );
        let mut reply = PendingReply {
            sender: Some(reply),
            stage: match &operation {
                Operation::PutDrop { source, remote, .. } if source.is_directory() => {
                    JobStage::DirectoryUpload(upload_directory::UploadState::new(
                        remote.clone(),
                        deadline,
                    ))
                }
                Operation::Put { atomic: true, .. } | Operation::PutDrop { atomic: true, .. } => {
                    JobStage::Checking("Atomic upload did not open a temporary file")
                }
                Operation::PutDrop { remote, .. } | Operation::PutGroupFile { remote, .. } => {
                    JobStage::NativeUpload {
                        remote: remote.clone(),
                        open_dispatched: false,
                        confirmed: 0,
                    }
                }
                Operation::PutDir { remote, .. } => JobStage::DirectoryUpload(
                    upload_directory::UploadState::new(remote.clone(), deadline),
                ),
                Operation::Put { remote, .. } if cancel.is_some() => JobStage::NativeUpload {
                    remote: remote.clone(),
                    open_dispatched: false,
                    confirmed: 0,
                },
                Operation::Put { .. } => JobStage::Upload,
                Operation::Get { resume: true, .. } => JobStage::ResumableDownload,
                Operation::PreviewText { .. } => JobStage::Checking("Text preview did not finish"),
                Operation::Sha256 { .. } => {
                    JobStage::Checking("SHA-256 calculation did not finish")
                }
                Operation::Find { content_query, .. } if !content_query.is_empty() => {
                    JobStage::Checking("File text search did not finish")
                }
                Operation::Find { .. } => JobStage::Checking("Filename search did not finish"),
                Operation::GetDir { .. } | Operation::GetSearch { .. } => {
                    JobStage::DirectoryDownload {
                        staging: None,
                        deadline,
                    }
                }
                Operation::RenameEntry { .. } => JobStage::Checking("Rename was not started"),
                Operation::SaveText { .. } => JobStage::Checking("Save was not started"),
                Operation::RemoveEntry { .. } => JobStage::Checking("Delete was not started"),
                _ => JobStage::Ordinary,
            },
        };
        // Detaching a caller does not cancel a transfer. Only its explicit
        // cancellation handle stops it, without resetting other CLI handles.
        let result = match tokio::time::timeout_at(deadline, async {
            if engine.is_none() {
                engine = Some(Engine::connect(&session).await?);
            }
            engine
                .as_mut()
                .unwrap()
                .execute(
                    operation,
                    progress.as_ref(),
                    cancel.as_ref(),
                    &mut reply.stage,
                )
                .await
        })
        .await
        {
            Ok(result) => {
                let lost = result
                    .as_ref()
                    .err()
                    .and_then(|e| e.downcast_ref::<Error>())
                    .is_some_and(transport_error);
                if lost {
                    engine = None;
                }
                if matches!(reply.stage, JobStage::Checking(_))
                    || matches!(&reply.stage, JobStage::DirectoryUpload(state) if state.root_dispatched || lost)
                    || matches!(
                        reply.stage,
                        JobStage::NativeUpload {
                            open_dispatched: true,
                            ..
                        }
                    )
                    || (lost
                        && matches!(
                            reply.stage,
                            JobStage::AwaitingStatus(_)
                                | JobStage::NativeUpload { .. }
                                | JobStage::EditorSave { .. }
                                | JobStage::AtomicUpload { .. }
                        ))
                {
                    result.with_context(|| reply.stage.interrupted())
                } else if lost
                    && matches!(
                        reply.stage,
                        JobStage::DirectoryDownload { .. } | JobStage::ResumableDownload
                    )
                {
                    result.context(reply.stage.interrupted())
                } else if lost {
                    result.context("SFTP channel failed; outcome may be unknown; handles invalidated, do not blindly retry")
                } else {
                    result
                }
            }
            Err(_) => {
                engine = None;
                if matches!(
                    reply.stage,
                    JobStage::DirectoryDownload { .. }
                        | JobStage::ResumableDownload
                        | JobStage::DirectoryUpload(_)
                        | JobStage::EditorSave { .. }
                        | JobStage::AtomicUpload { .. }
                        | JobStage::Checking(_)
                        | JobStage::AwaitingStatus(_)
                        | JobStage::NativeUpload { .. }
                ) {
                    Err(anyhow::anyhow!(
                        "SFTP timed out; {}; handles invalidated",
                        reply.stage.interrupted()
                    ))
                } else {
                    Err(anyhow::anyhow!(
                        "SFTP timed out; partial uploads may remain and the operation may have completed; handles invalidated, do not blindly retry"
                    ))
                }
            }
        };
        let result = result.map_err(|e| format!("SFTP: {e:#}"));
        if let Some(stage) = reply.stage.take_directory() {
            let _ = directory::clean_up(stage, reply.sender.take().unwrap(), result).await;
            continue;
        }
        if let Err(Ok(value)) = reply.finish(result)
            && creates_handle
            && let Some(handle) = value["handle"].as_u64()
            && let Some(active) = engine.as_mut()
        {
            let closed =
                tokio::time::timeout(Duration::from_secs(5), active.close_handle(handle)).await;
            if !matches!(closed, Ok(Ok(_))) {
                engine = None;
            }
        }
    }
}

struct Handle {
    remote: String,
    directory: bool,
}
struct Engine {
    raw: RawSftpSession,
    version: Version,
    handles: HashMap<u64, Handle>,
    read_size: usize,
    write_size: usize,
}
// Bare russh Channels do not close on drop. Guard negotiation too, so rejection
// or cancellation cannot leave unused session channels on a long-lived SSH link.
struct PendingChannel(Option<russh::Channel<russh::client::Msg>>);
impl Drop for PendingChannel {
    fn drop(&mut self) {
        if let Some(channel) = self.0.take() {
            drop(channel.into_stream());
        }
    }
}
impl Engine {
    async fn connect(session: &russh::client::Handle<crate::ssh::Handler>) -> Result<Self> {
        let channel = session
            .channel_open_session()
            .await
            .context("opening SFTP channel")?;
        let mut pending = PendingChannel(Some(channel));
        let channel = pending.0.as_mut().unwrap();
        channel.request_subsystem(true, "sftp").await?;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match channel.wait().await {
                    Some(russh::ChannelMsg::Success) => break Ok(()),
                    Some(russh::ChannelMsg::Failure | russh::ChannelMsg::Close) | None => {
                        bail!("server rejected or closed the SFTP subsystem")
                    }
                    _ => {}
                }
            }
        })
        .await
        .context("SFTP subsystem request timed out")??;
        let mut raw = RawSftpSession::new(pending.0.take().unwrap().into_stream());
        let version = raw.init().await.context("initializing SFTP")?;
        ensure!(
            version.version == 3,
            "unsupported SFTP version {} (requires v3)",
            version.version
        );
        let mut read_size = CHUNK;
        let mut write_size = CHUNK;
        if version
            .extensions
            .get(extensions::LIMITS)
            .is_some_and(|v| v == "1")
        {
            let limits = Limits::from(raw.limits().await?);
            read_size = read_size.min(limits.read_len.unwrap_or(CHUNK as u64) as usize);
            // Leave room for packet headers and server handles.
            write_size = write_size
                .min(limits.write_len.unwrap_or(CHUNK as u64) as usize)
                .min(limits.packet_len.unwrap_or(u64::MAX).saturating_sub(1024) as usize);
            raw.set_limits(limits);
        }
        ensure!(
            read_size > 0 && write_size > 0,
            "server advertised unusable packet limits"
        );
        Ok(Self {
            raw,
            version,
            handles: HashMap::new(),
            read_size,
            write_size,
        })
    }
    fn extension(&self, name: &str, version: &str) -> Result<()> {
        ensure!(
            self.version
                .extensions
                .get(name)
                .is_some_and(|v| v == version),
            "server does not advertise {name} version {version}"
        );
        Ok(())
    }
    fn handle(&self, id: u64, directory: Option<bool>) -> Result<String> {
        let h = self
            .handles
            .get(&id)
            .context("unknown, closed, expired, or wrong-pane SFTP handle")?;
        if let Some(directory) = directory {
            ensure!(h.directory == directory, "incorrect SFTP handle type");
        }
        Ok(h.remote.clone())
    }
    async fn remember(&mut self, remote: String, directory: bool) -> Result<Value> {
        if self.handles.len() >= 128 {
            let _ = self.raw.close(remote).await;
            bail!("128 open handles; close unused handles");
        }
        let id = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
        self.handles.insert(id, Handle { remote, directory });
        Ok(json!({"handle":id,"directory":directory}))
    }
    async fn close_handle(&mut self, handle: u64) -> Result<()> {
        self.raw.close(self.handle(handle, None)?).await?;
        self.handles.remove(&handle);
        Ok(())
    }

    async fn check_search_parent(&self, parent: &str) -> Result<()> {
        let expected = if parent.is_empty() { "/" } else { parent };
        ensure!(
            self.canonical_remote_path(expected).await? == expected,
            "The search result's containing folder changed"
        );
        Ok(())
    }

    async fn execute(
        &mut self,
        op: Operation,
        job_progress: Option<&JobProgress>,
        cancel: Option<&TransferCancel>,
        stage: &mut JobStage,
    ) -> Result<Value> {
        use Operation::*;
        let progress = match job_progress {
            Some(JobProgress::Transfer(sender)) => Some(sender),
            _ => None,
        };
        let ok = json!({});
        let mounted = matches!(op, ListMountedFolder { .. });
        Ok(match op {
            SaveText {
                path,
                original,
                text,
            } => {
                self.save_text(&path, original.as_deref(), &text, stage)
                    .await?;
                ok
            }
            Info => {
                json!({"version":self.version.version,"extensions":self.version.extensions,"open_handles":self.handles.len(),"read_chunk":self.read_size,"write_chunk":self.write_size})
            }
            Get {
                remote,
                local,
                overwrite,
                max_bytes,
                expected_sha256,
                resume,
                preserve_mtime,
            } => {
                self.get(
                    remote,
                    local,
                    overwrite,
                    DownloadChecks {
                        max_bytes,
                        sha256: expected_sha256
                            .as_deref()
                            .map(normalize_sha256)
                            .transpose()?,
                        resume,
                        preserve_mtime,
                    },
                    progress,
                    cancel,
                )
                .await?
            }
            GetDir {
                remote,
                local,
                preserve_mtime,
            } => {
                self.get_dir(
                    directory::DownloadSource::Entire(remote),
                    local,
                    preserve_mtime,
                    progress,
                    cancel,
                    stage,
                )
                .await?
            }
            GetSearch {
                selection,
                local,
                preserve_mtime,
            } => {
                self.get_dir(
                    directory::DownloadSource::Selection(selection),
                    local,
                    preserve_mtime,
                    progress,
                    cancel,
                    stage,
                )
                .await?
            }
            PutDir {
                local,
                preserve_times,
                ..
            } => {
                let JobStage::DirectoryUpload(state) = stage else {
                    unreachable!("directory upload job stage");
                };
                let source = tokio::task::spawn_blocking(move || {
                    let dir = upload_directory::source_root(&local)?;
                    Ok::<_, anyhow::Error>((local, dir))
                })
                .await??;
                self.put_directory(source, preserve_times, progress, cancel, state)
                    .await?
            }
            PutDrop {
                source,
                remote,
                overwrite,
                atomic,
                preserve_times,
            } => {
                let (local, checked) =
                    tokio::task::spawn_blocking(move || source.open_confirmed(overwrite)).await??;
                match (checked, stage) {
                    (
                        upload_directory::Source::Directory(dir),
                        JobStage::DirectoryUpload(state),
                    ) => {
                        self.put_directory((local, dir), preserve_times, progress, cancel, state)
                            .await?
                    }
                    (
                        upload_directory::Source::File(file, _),
                        stage @ (JobStage::NativeUpload { .. } | JobStage::Checking(_)),
                    ) => {
                        self.put(
                            (local, tokio::fs::File::from_std(file)),
                            remote,
                            UploadOptions {
                                overwrite,
                                atomic,
                                preserve_times,
                            },
                            progress,
                            cancel,
                            stage,
                        )
                        .await?
                    }
                    _ => bail!("dropped item has an unexpected type"),
                }
            }
            PutGroupFile { source, remote } => {
                validate_group_upload_path(&remote)?;
                let local = source.path().to_owned();
                let file = tokio::task::spawn_blocking(move || source.reader()).await??;
                self.put(
                    (local, tokio::fs::File::from_std(file)),
                    remote,
                    UploadOptions::default(),
                    progress,
                    cancel,
                    stage,
                )
                .await?
            }
            PreviewText { path } => json!({"text": self.read_preview_text(&path, cancel).await?}),
            Sha256 { path, max_bytes } => self.remote_sha256(&path, max_bytes, cancel).await?,
            Find {
                path,
                query,
                regex,
                match_case,
                match_path,
                kind,
                exclude_dir_regex,
                file_size,
                modified_dates,
                content_query,
            } => serde_json::to_value(
                self.find_filenames(
                    &path,
                    &query,
                    FilenameFilter {
                        regex,
                        match_case,
                        match_path,
                        kind,
                        exclude_dir_regex,
                        file_size,
                        modified_dates,
                        content_query,
                    },
                    cancel,
                    match job_progress {
                        Some(JobProgress::Search(sender)) => Some(sender),
                        _ => None,
                    },
                )
                .await?,
            )?,
            EntryProperties { path } => {
                let attrs = Attributes::from(self.raw.lstat(&path).await?.attrs);
                let mut result = json!({ "attrs": attrs });
                if EntryKind::from_permissions(attrs.permissions) == Some(EntryKind::Symlink) {
                    ensure!(
                        !cancel.is_some_and(TransferCancel::is_cancelled),
                        "File properties cancelled"
                    );
                    match self.raw.readlink(&path).await {
                        Ok(names) => match names.files.as_slice() {
                            [name] if !name.filename.is_empty() => {
                                result["target"] = json!(name.filename)
                            }
                            _ => {
                                result["target_error"] =
                                    json!("The server returned no single link target")
                            }
                        },
                        Err(e) if transport_error(&e) => return Err(e.into()),
                        Err(e) => result["target_error"] = json!(e.to_string()),
                    }
                }
                result
            }
            Put {
                local,
                remote,
                overwrite,
                atomic,
                preserve_times,
            } => {
                if cancel.is_some_and(TransferCancel::is_cancelled) {
                    return Ok(cancelled_transfer(&local, &remote, 0, false));
                }
                ensure!(local.is_absolute(), "local path must be absolute");
                let source = tokio::fs::File::open(&local)
                    .await
                    .context("opening local source")?;
                self.put(
                    (local, source),
                    remote,
                    UploadOptions {
                        overwrite,
                        atomic,
                        preserve_times,
                    },
                    progress,
                    cancel,
                    stage,
                )
                .await?
            }
            Opendir { path } => {
                let h = self.raw.opendir(path).await?.handle;
                self.remember(h, true).await?
            }
            Open { path, flags, attrs } => {
                let h = self
                    .raw
                    .open(path, open_flags(&flags)?, attrs.protocol()?)
                    .await?
                    .handle;
                self.remember(h, false).await?
            }
            Close { handle } => {
                self.close_handle(handle).await?;
                ok
            }
            Readdir { handle } => match self.raw.readdir(self.handle(handle, Some(true))?).await {
                Ok(names) => json!({"entries":names.files,"eof":false}),
                Err(e) if eof(&e) => json!({"entries":[],"eof":true}),
                Err(e) => return Err(e.into()),
            },
            Ls { path } | ListMountedFolder { path } => {
                if mounted {
                    let names = self
                        .raw
                        .realpath(&path)
                        .await
                        .context("SFTP could not resolve this mount path")?
                        .files;
                    ensure!(
                        matches!(names.as_slice(), [entry] if entry.filename == path),
                        "SFTP resolves this mount to a different path"
                    );
                }
                let h = self.raw.opendir(path).await?.handle;
                let result = async {
                    let mut entries = Vec::new();
                    let mut bytes = 0;
                    loop {
                        match self.raw.readdir(&h).await {
                            Ok(names) => {
                                bytes += serde_json::to_vec(&names.files)?.len();
                                ensure!(
                                    bytes <= 8 * 1024 * 1024,
                                    "directory listing exceeds 8 MiB; use opendir/readdir/close"
                                );
                                entries.extend(names.files);
                            }
                            Err(e) if eof(&e) => break,
                            Err(e) => return Err(e.into()),
                        }
                    }
                    Ok(json!({"entries":entries}))
                }
                .await;
                let closed = self.raw.close(h).await;
                let value: Value = result?;
                closed?;
                value
            }
            Read {
                handle,
                offset,
                len,
            } => {
                ensure!(
                    len > 0 && len as usize <= self.read_size,
                    "read length must be 1..={}",
                    self.read_size
                );
                match self
                    .raw
                    .read(self.handle(handle, Some(false))?, offset, len)
                    .await
                {
                    Ok(data) => {
                        ensure!(
                            data.data.len() <= len as usize,
                            "server returned more SFTP data than requested"
                        );
                        json!({"data_base64":STANDARD.encode(&data.data),"bytes_read":data.data.len(),"eof":false})
                    }
                    Err(e) if eof(&e) => json!({"data_base64":"","bytes_read":0,"eof":true}),
                    Err(e) => return Err(e.into()),
                }
            }
            Write {
                handle,
                offset,
                data_base64,
            } => {
                ensure!(
                    data_base64.len() <= self.write_size.div_ceil(3) * 4,
                    "write exceeds {} bytes",
                    self.write_size
                );
                let data = STANDARD
                    .decode(data_base64)
                    .context("invalid data_base64")?;
                ensure!(
                    data.len() <= self.write_size,
                    "write exceeds {} bytes",
                    self.write_size
                );
                let count = data.len();
                self.raw
                    .write(self.handle(handle, Some(false))?, offset, data)
                    .await?;
                json!({"bytes_written":count})
            }
            Stat { path } => json!({"attrs":self.raw.stat(path).await?.attrs}),
            Lstat { path } => json!({"attrs":self.raw.lstat(path).await?.attrs}),
            Fstat { handle } => {
                json!({"attrs":self.raw.fstat(self.handle(handle,None)?).await?.attrs})
            }
            Setstat { path, attrs } => {
                self.raw.setstat(path, attrs.protocol()?).await?;
                ok
            }
            Fsetstat { handle, attrs } => {
                self.raw
                    .fsetstat(self.handle(handle, None)?, attrs.protocol()?)
                    .await?;
                ok
            }
            Mkdir { path, attrs } => {
                self.raw.mkdir(path, attrs.protocol()?).await.map_err(|error| {
                    if matches!(error, Error::Status(ref status) if !matches!(status.status_code, StatusCode::NoConnection | StatusCode::ConnectionLost)) {
                        anyhow::Error::from(error)
                    } else {
                        anyhow::Error::from(error).context("Folder creation outcome unknown; refresh to check")
                    }
                })?;
                ok
            }
            Rename { from, to } => {
                self.raw.rename(from, to).await?;
                ok
            }
            RenameEntry {
                from,
                to,
                search_kind,
            } => {
                if let Some(kind) = search_kind {
                    let (parent, _) = from
                        .rsplit_once('/')
                        .context("Missing search result parent")?;
                    ensure!(
                        to.rsplit_once('/')
                            .is_some_and(|(target, _)| target == parent),
                        "The search result's containing folder changed"
                    );
                    self.check_search_parent(parent).await?;
                    let attributes = self.raw.lstat(&from).await?.attrs;
                    ensure!(
                        EntryKind::from_permissions(attributes.permissions) == Some(kind),
                        "The search result's entry type changed. Refresh the results."
                    );
                }
                // LSTAT sees the directory entry even if it is a dangling link.
                // A concurrent creator after this check is the v3 server's responsibility.
                match self.raw.lstat(&to).await {
                    Ok(_) => bail!("An entry with that name already exists"),
                    Err(Error::Status(status)) if status.status_code == StatusCode::NoSuchFile => {}
                    Err(error) => {
                        return Err(anyhow::Error::from(error)
                            .context("Could not check the destination name"));
                    }
                }
                *stage = JobStage::AwaitingStatus("Rename outcome unknown; refresh to check");
                self.raw.rename(from, to).await?;
                ok
            }
            RemoveEntry {
                path,
                kind,
                check_parent,
            } => {
                // A trailing slash can make even LSTAT dereference a directory link;
                // Windows SFTP servers can also interpret backslashes in a leaf.
                let name = path.rsplit('/').next().unwrap_or("");
                ensure!(
                    !matches!(name, "" | "." | "..")
                        && !name.contains('\\')
                        && !path.contains('\0'),
                    "Expected a remote directory entry name"
                );
                if check_parent {
                    let (parent, _) = path
                        .rsplit_once('/')
                        .context("Missing search result parent")?;
                    self.check_search_parent(parent).await?;
                }
                let attrs = self
                    .raw
                    .lstat(&path)
                    .await
                    .context("Could not check the selected entry")?
                    .attrs;
                let actual = EntryKind::from_permissions(attrs.permissions)
                    .context("The server reported an unsupported type for this entry")?;
                ensure!(
                    actual == kind,
                    "The selected entry type changed; refresh to check"
                );
                // v3 has no conditional unlink. Use exactly one request against
                // this name; the server enforces directory emptiness for RMDIR.
                *stage = JobStage::AwaitingStatus("Delete outcome unknown; refresh to check");
                match kind {
                    EntryKind::File | EntryKind::Symlink => self.raw.remove(path).await?,
                    EntryKind::Directory => self.raw.rmdir(path).await?,
                };
                ok
            }
            RemoveFile { path } => {
                self.raw.remove(path).await?;
                ok
            }
            RemoveDir { path } => {
                self.raw.rmdir(path).await?;
                ok
            }
            Realpath { path } => json!({"entries":self.raw.realpath(path).await?.files}),
            Readlink { path } => json!({"entries":self.raw.readlink(path).await?.files}),
            Symlink {
                path,
                target,
                spec_order,
            } => {
                // OpenSSH deliberately reverses the draft-v3 wire fields. Most deployed
                // servers follow it; strict draft servers can request the original order.
                if spec_order {
                    self.raw.symlink(path, target).await?;
                } else {
                    self.raw.symlink(target, path).await?;
                }
                ok
            }
            Hardlink { from, to } => {
                self.extension(extensions::HARDLINK, "1")?;
                self.raw.hardlink(from, to).await?;
                ok
            }
            Fsync { handle } => {
                self.extension(extensions::FSYNC, "1")?;
                self.raw.fsync(self.handle(handle, Some(false))?).await?;
                ok
            }
            Statvfs { path } => {
                self.extension(extensions::STATVFS, "2")?;
                serde_json::to_value(self.raw.statvfs(path).await?)?
            }
            Limits => {
                self.extension(extensions::LIMITS, "1")?;
                serde_json::to_value(self.raw.limits().await?)?
            }
            ExpandPath { path } => {
                self.extension(extensions::EXPAND_PATH, "1")?;
                json!({"entries":self.raw.expand_path(path).await?.files})
            }
        })
    }
    async fn read_contents(
        &self,
        h: &str,
        file: &mut (impl tokio::io::AsyncWrite + Unpin),
        mut offset: u64,
        limit: Option<(u64, &str)>,
        cancel: Option<&TransferCancel>,
        mut advanced: impl FnMut(u64, &[u8]) -> Result<()>,
    ) -> Result<u64> {
        if let Some((max, reason)) = limit {
            ensure!(offset <= max, "download exceeds {reason} ({max} bytes)");
        }
        while !cancel.is_some_and(TransferCancel::is_cancelled) {
            // At a byte limit, one additional byte distinguishes an exact fit from truncation.
            let len = limit.map_or(self.read_size as u64, |(max, _)| {
                (max - offset).saturating_add(1).min(self.read_size as u64)
            }) as u32;
            match self.raw.read(h, offset, len).await {
                Ok(data) => {
                    if cancel.is_some_and(TransferCancel::is_cancelled) {
                        break;
                    }
                    ensure!(
                        data.data.len() <= len as usize,
                        "server returned more SFTP data than requested"
                    );
                    ensure!(
                        !data.data.is_empty(),
                        "server returned empty data without EOF"
                    );
                    let next = offset
                        .checked_add(data.data.len() as u64)
                        .context("file size overflow")?;
                    if let Some((max, reason)) = limit {
                        ensure!(next <= max, "download exceeds {reason} ({max} bytes)");
                    }
                    file.write_all(&data.data).await?;
                    offset = next;
                    advanced(offset, &data.data)?;
                }
                Err(e) if eof(&e) => break,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(offset)
    }
    async fn get(
        &self,
        remote: String,
        local: PathBuf,
        overwrite: bool,
        checks: DownloadChecks,
        progress: Option<&watch::Sender<TransferProgress>>,
        cancel: Option<&TransferCancel>,
    ) -> Result<Value> {
        let cancelled = || cancel.is_some_and(TransferCancel::is_cancelled);
        if cancelled() {
            return Ok(cancelled_transfer(&local, &remote, 0, false));
        }
        ensure!(local.is_absolute(), "local path must be absolute");
        if !overwrite {
            ensure!(
                !local.try_exists()?,
                "The local destination already exists at {}",
                local.display()
            );
        }
        let parent = local
            .parent()
            .context("local destination has no parent")?
            .to_owned();
        let resumed_digest = if checks.resume {
            Some(
                checks
                    .sha256
                    .clone()
                    .context("--resume requires --sha256")?,
            )
        } else {
            None
        };
        let staging_local = local.clone();
        let before_create = cancel.cloned();
        let Some((staging, file)) = tokio::task::spawn_blocking(move || -> Result<_> {
            if before_create
                .as_ref()
                .is_some_and(TransferCancel::is_cancelled)
            {
                return Ok(None);
            }
            let stage = if let Some(expected) = resumed_digest {
                let (partial, file) = ResumeDownload::open(&staging_local, &expected)?;
                (DownloadStaging::Resumable(partial), file)
            } else {
                let temp = tempfile::NamedTempFile::new_in(parent)?;
                let file = temp.reopen()?;
                (DownloadStaging::Temporary(temp), file)
            };
            Ok(Some(stage))
        })
        .await??
        else {
            return Ok(cancelled_transfer(&local, &remote, 0, false));
        };
        let mut file = tokio::fs::File::from_std(file);
        let resumed_from = if checks.resume {
            file.metadata().await?.len()
        } else {
            0
        };
        let mut digest = checks.sha256.as_ref().map(|_| Sha256::new());
        if let Some(hash) = digest.as_mut().filter(|_| resumed_from != 0) {
            if let Some(max) = checks.max_bytes {
                ensure!(
                    resumed_from <= max,
                    "download exceeds --max-bytes limit ({max} bytes)"
                );
            }
            let mut prefix = (&mut file).take(resumed_from);
            let mut buffer = vec![0; CHUNK];
            while !cancelled() && prefix.limit() != 0 {
                let len = prefix.read(&mut buffer).await?;
                ensure!(len > 0, "Partial download ended before its reported size");
                hash.update(&buffer[..len]);
            }
        }
        if cancelled() {
            return Ok(staging.cancelled_reply(&local, &remote, resumed_from));
        }
        let h = self
            .raw
            .open(&remote, OpenFlags::READ, FileAttributes::empty())
            .await?
            .handle;
        let copied = async {
            // Some servers omit size or disallow FSTAT on readable files.
            let attrs = if (progress.is_some() || checks.preserve_mtime) && !cancelled() {
                match self.raw.fstat(&h).await {
                    Ok(reply) => reply.attrs,
                    Err(error) if transport_error(&error) => return Err(error.into()),
                    Err(_) => FileAttributes::empty(),
                }
            } else {
                FileAttributes::empty()
            };
            let total = attrs.size;
            report(progress, resumed_from, total);
            let offset = self
                .read_contents(
                    &h,
                    &mut file,
                    resumed_from,
                    checks.max_bytes.map(|n| (n, "--max-bytes limit")),
                    cancel,
                    |offset, chunk| {
                        if let Some(digest) = &mut digest {
                            digest.update(chunk);
                        }
                        report(progress, offset, total);
                        Ok(())
                    },
                )
                .await?;
            if !cancelled() {
                file.sync_all().await?;
            }
            Ok::<_, anyhow::Error>((offset, attrs.mtime.filter(|_| checks.preserve_mtime)))
        }
        .await;
        let closed = self.raw.close(h).await;
        let (bytes, mtime) = copied?;
        closed?;
        if cancelled() {
            return Ok(staging.cancelled_reply(&local, &remote, bytes));
        }
        let file = file.into_std().await;
        let verified_sha256 = digest.map(digest_hex);
        if let (Some(expected), Some(actual)) = (&checks.sha256, &verified_sha256)
            && actual != expected
        {
            let mismatch = format!(
                "SHA-256 mismatch. Expected {expected}, received {actual}. The download was not installed."
            );
            if let DownloadStaging::Resumable(partial) = staging {
                tokio::task::spawn_blocking(move || partial.discard(file))
                    .await?
                    .context(mismatch.clone())?;
            }
            bail!(mismatch);
        }
        // This check runs inside the blocking task, so cancellation while the
        // task is queued cannot install the temporary file. Once persist starts,
        // the actual commit result wins even if a cancellation arrives later.
        let destination = local.clone();
        let cancel = cancel.cloned();
        let cancelled_reply = staging.cancelled_reply(&local, &remote, bytes);
        let committed = tokio::task::spawn_blocking(move || -> Result<bool> {
            if cancel.as_ref().is_some_and(TransferCancel::is_cancelled) {
                return Ok(false);
            }
            staging.publish(file, destination, overwrite, mtime)?;
            Ok(true)
        })
        .await??;
        if !committed {
            return Ok(cancelled_reply);
        }
        let mut reply = json!({"local":local,"remote":remote,"bytes_transferred":bytes});
        if let Some(sha256) = verified_sha256 {
            reply["sha256"] = json!(sha256);
        }
        if checks.resume {
            reply["resumed_from"] = json!(resumed_from);
        }
        Ok(reply)
    }
    async fn put(
        &self,
        (local, mut file): (PathBuf, tokio::fs::File),
        remote: String,
        options: UploadOptions,
        progress: Option<&watch::Sender<TransferProgress>>,
        cancel: Option<&TransferCancel>,
        stage: &mut JobStage,
    ) -> Result<Value> {
        if options.atomic {
            return self
                .put_atomic((local, file), remote, options, progress, cancel, stage)
                .await;
        }
        let cancelled = || cancel.is_some_and(TransferCancel::is_cancelled);
        if cancelled() {
            return Ok(cancelled_transfer(&local, &remote, 0, false));
        }
        let metadata = file.metadata().await?;
        ensure!(metadata.is_file(), "local source must be a regular file");
        let times = upload_times(&metadata, options.preserve_times)?;
        let total = Some(metadata.len());
        report(progress, 0, total);
        let flags = OpenFlags::WRITE
            | OpenFlags::CREATE
            | if options.overwrite {
                OpenFlags::TRUNCATE
            } else {
                OpenFlags::EXCLUDE
            };
        if cancelled() {
            return Ok(cancelled_transfer(&local, &remote, 0, false));
        }
        if let JobStage::NativeUpload {
            open_dispatched, ..
        } = stage
        {
            *open_dispatched = true;
        }
        let h = match self.raw.open(&remote, flags, FileAttributes::empty()).await {
            Ok(h) => h.handle,
            Err(error) => {
                if !transport_error(&error)
                    && let JobStage::NativeUpload {
                        open_dispatched, ..
                    } = stage
                {
                    *open_dispatched = false;
                }
                return Err(error.into());
            }
        };
        let copied = async {
            let (bytes, complete) = self
                .write_upload(&h, &mut file, total, progress, cancel, stage)
                .await?;
            if complete && times.is_some() {
                if cancelled() {
                    return Ok((bytes, false));
                }
                self.apply_upload_times(&h, times).await?;
            }
            Ok::<_, anyhow::Error>((bytes, complete))
        }
        .await;
        let closed = self.raw.close(h).await;
        let (bytes, complete) =
            copied.context("upload failed; remote destination may contain a partial file")?;
        closed.context("upload close failed; remote contents are uncertain")?;
        if !complete {
            return Ok(cancelled_transfer(&local, &remote, bytes, true));
        }
        Ok(json!({"local":local,"remote":remote,"bytes_transferred":bytes}))
    }

    async fn apply_upload_times(&self, handle: &str, times: Option<FileAttributes>) -> Result<()> {
        if let Some(times) = times {
            self.raw
                .fsetstat(handle, times)
                .await
                .context("the server did not confirm the upload timestamps")?;
        }
        Ok(())
    }

    async fn write_upload(
        &self,
        handle: &str,
        file: &mut tokio::fs::File,
        total: Option<u64>,
        progress: Option<&watch::Sender<TransferProgress>>,
        cancel: Option<&TransferCancel>,
        stage: &mut JobStage,
    ) -> Result<(u64, bool)> {
        let cancelled = || cancel.is_some_and(TransferCancel::is_cancelled);
        let mut buffer = vec![0; self.write_size];
        let mut offset = 0u64;
        while !cancelled() {
            let len = file.read(&mut buffer).await?;
            if cancelled() {
                break;
            }
            if len == 0 {
                return Ok((offset, true));
            }
            self.raw
                .write(handle, offset, buffer[..len].to_vec())
                .await?;
            offset = offset
                .checked_add(len as u64)
                .context("file size overflow")?;
            if let JobStage::NativeUpload { confirmed, .. } = stage {
                *confirmed = offset;
            }
            report(progress, offset, total);
        }
        Ok((offset, false))
    }
}
/// Attach Windows Mark-of-the-Web (`Zone.Identifier`, ZoneId=3 / Internet) to a
/// staged downloaded file before publication so SmartScreen, PowerShell
/// `RemoteSigned`, and Office Protected View treat remote SFTP files as untrusted.
/// Gracefully tolerates filesystems (e.g. FAT32/exFAT) that do not support NTFS ADS.
pub(crate) fn apply_mark_of_the_web(path: &Path) -> Result<()> {
    use std::io::Write;
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    let mut ads = path.as_os_str().to_owned();
    ads.push(":Zone.Identifier");
    match std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(&ads)
    {
        Ok(mut stream) => {
            stream.write_all(b"[ZoneTransfer]\r\nZoneId=3\r\n")?;
            stream.sync_all()?;
            Ok(())
        }
        Err(error) if matches!(error.raw_os_error(), Some(1 | 50 | 87 | 123)) => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("writing Zone.Identifier for {}", path.display()))
        }
    }
}

fn eof(error: &Error) -> bool {
    matches!(error,Error::Status(s) if s.status_code==StatusCode::Eof)
}
fn transport_error(error: &Error) -> bool {
    matches!(
        error,
        Error::Timeout | Error::IO(_) | Error::UnexpectedPacket | Error::UnexpectedBehavior(_)
    ) || matches!(error,Error::Status(s) if matches!(s.status_code,StatusCode::ConnectionLost|StatusCode::NoConnection))
}

#[cfg(test)]
mod tests {
    use super::EntryKind;

    #[test]
    fn drawer_deletion_requires_explicit_regular_directory_or_link_type_bits() {
        for (mode, kind) in [
            (0o100000, EntryKind::File),
            (0o040000, EntryKind::Directory),
            (0o120000, EntryKind::Symlink),
        ] {
            assert_eq!(EntryKind::from_permissions(Some(mode | 0o7755)), Some(kind));
        }
        for mode in [
            None,
            Some(0o755),
            Some(0o010644),
            Some(0o020640),
            Some(0o060660),
            Some(0o140777),
        ] {
            assert_eq!(EntryKind::from_permissions(mode), None);
        }
    }

    #[test]
    fn mark_of_the_web_tags_staged_downloads_and_survives_atomic_persist() {
        let dir = tempfile::tempdir().unwrap();
        let mut staged = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        std::io::Write::write_all(&mut staged, b"downloaded payload").unwrap();
        super::apply_mark_of_the_web(staged.path()).unwrap();
        let dest = dir.path().join("remote.ps1");
        staged.persist(&dest).unwrap();

        let mut ads = dest.as_os_str().to_owned();
        ads.push(":Zone.Identifier");
        let zone = std::fs::read_to_string(&ads).unwrap();
        assert!(zone.contains("[ZoneTransfer]"));
        assert!(zone.contains("ZoneId=3"));
    }
}
