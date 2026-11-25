use super::*;
use anyhow::{Result, anyhow, bail, ensure};
use serde_json::Value;
use std::{
    collections::{BTreeSet, VecDeque},
    path::PathBuf,
};
use tokio::sync::{oneshot, watch};
use w11term::control::SftpCancelState;
use w11term::events::SavedDownload;
use w11term::folder_grants::Authority;
pub use w11term::sftp::TransferId;
use w11term::sftp::{
    DEFAULT_TIMEOUT, EntryKind, FilenameMatch, Job, Operation, TransferBatchProgress,
    TransferCancel, TransferOperation, TransferOutcome, TransferPhase, TransferProgress,
    normalize_sha256, validate_download_name,
};

mod bookmarks;
mod downloads;
mod editor;
mod find;
mod history;
mod preview;
mod properties;
mod sort;
mod text_draft;
mod transfer_queue;
mod uploads;

use history::{FolderHistory, HistoryDirection};
pub use preview::PreviewKind;
pub use sort::SortOrder;
use transfer_queue::TransferQueue;
use uploads::DropPreparation;
pub use uploads::DropTarget;
use w11term::sftp::DropSource;

const PAGE_SIZE: usize = 32;
const PANEL_PADDING: f64 = 10.0;

#[derive(Clone, Copy, Debug, PartialEq)]
struct DrawerLayout {
    width: f64,
    actions_max_height: f64,
}

impl DrawerLayout {
    fn for_window(width: f64, height: f64) -> Self {
        // WindowSize is in DIPs and includes the tall title bar. Reserve up to
        // three file buttons beneath the actions, sharing space on tiny windows.
        let body = (height - 48.0 - 2.0 * PANEL_PADDING).max(0.0);
        Self {
            width: (width * 0.45).clamp(1.0, 360.0),
            actions_max_height: body - (body * 0.4).min(112.0),
        }
    }

    fn content_width(self) -> f64 {
        (self.width - 2.0 * PANEL_PADDING).max(1.0)
    }
}

fn request_generation(operation: &Operation, generation: u64) -> Option<u64> {
    // A drawer opened before the first login can request its initial listing.
    // Once connected, every path belongs to the SSH peer that supplied it.
    match (operation, generation) {
        (Operation::Realpath { .. } | Operation::Ls { .. }, 0) => None,
        _ => Some(generation),
    }
}

#[derive(Clone)]
pub enum FileAction {
    Toggle,
    OpenHere,
    ShowTransfer(u64),
    ShowSavedDownload(u64, SavedDownload),
    Bookmarks(u64, u64, bookmarks::Action),
    Search(u64, u64, find::Action),
    Follow(u64, u64, bool),
    Reveal(u64, u64, String, Option<String>),
    Close(u64),
    CancelTransfer(TransferId),
    Browse(u64, u64, String),
    History(u64, u64, u64, HistoryDirection),
    GoToFolder(u64, u64, u64),
    OpenEntry(u64, u64, ListedEntry),
    EditFilter(u64, u64, u64, bool),
    FocusFilter(u64, u64, u64, u8),
    Filter(u64, u64, u64, String),
    ClearFilter(u64, u64, u64),
    Sort(u64, u64, u64, SortOrder),
    Choose(u64, u64, ListedEntry),
    SelectDownloads(u64, u64, u64, bool),
    MarkDownload(u64, u64, ListedEntry, bool),
    CopyPath(u64, u64, u64, Option<ListedEntry>),
    Preview(u64, u64, FileTarget, PreviewKind),
    ClosePreview(u64, u64, u64),
    CopyChecksum(u64, u64, u64),
    VerifyChecksum(u64, u64, u64),
    StartTextEditor(u64, u64, u64),
    ResumeTextEditor(u64, u64),
    TextEditor(u64, u64, editor::Action),
    Rename(u64, u64, ListedEntry),
    VerifyDownload(u64, u64, ListedEntry),
    Delete(u64, u64, ListedEntry),
    NewFolder(u64, u64, String),
    EditName(u64, u64, u64, String),
    ResumeDownload(u64, u64, u64, bool),
    FinishEdit(u64, u64, u64, ContentDialogResult),
    Page(u64, u64, u64, usize),
    Replace(u64, u64, bool),
    PreserveMtime(u64, u64, bool),
    AtomicUploads(u64, u64, bool),
    PreserveUploadTimes(u64, u64, bool),
    Pick(u64, u64, u64, TransferPicker),
    Picked(u64, u64, u64, TransferPicker, Result<Vec<PathBuf>, String>),
    Drop(DropTarget, Vec<PathBuf>),
    PreparedDrop(DropTarget, TransferId, Result<Vec<DropSource>, String>),
    #[cfg(feature = "ui-smoke")]
    SmokeDrop(DropTarget, PathBuf),
    Listed(
        u64,
        u64,
        String,
        Option<HistoryDirection>,
        Result<Vec<Entry>, String>,
    ),
    Tick(u64),
}

#[derive(Default)]
pub(super) struct FileDrawer {
    pub(super) open: bool,
    generation: u64,
    directory: Option<String>,
    history: FolderHistory,
    bookmarks: Option<bookmarks::List>,
    search: Option<find::Panel>,
    saved_search: Option<find::Panel>,
    /// Describes an explicit navigation request if either REALPATH or listing fails.
    opening_target: Option<String>,
    following: bool,
    last_report: Option<Arc<str>>,
    entries: Vec<Entry>,
    sort_order: SortOrder,
    filter: String,
    filter_input: Option<ElementRef<TextBox>>,
    /// Indices into the sorted listing, used only when the query is nonempty.
    matches: Vec<usize>,
    listing_revision: u64,
    page: usize,
    selected: Option<String>,
    marked_downloads: Option<BTreeSet<String>>,
    reveal: Option<RevealTarget>,
    dialog: Option<EntryDialog>,
    preview: Option<preview::EntryPreview>,
    editor: Option<editor::Editor>,
    dialog_serial: u64,
    replace: bool,
    preserve_mtime: bool,
    atomic_uploads: bool,
    preserve_upload_times: bool,
    picking: Option<u64>,
    parsing: bool,
    pending: Option<Pending>,
    queue: TransferQueue,
    last_download: Option<SavedDownload>,
    preparing_drop: Option<DropPreparation>,
    transfer_serial: u64,
    status: String,
    cached: RefCell<Option<CachedPanel>>,
}

struct CachedPanel {
    profile: Option<String>,
    shell_ready: bool,
    layout: DrawerLayout,
    view: View,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    name: String,
    kind: ListedKind,
    mtime: Option<u32>,
    size: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ListedKind {
    Known(EntryKind),
    Missing,
    Unsupported,
}

impl ListedKind {
    fn from_attrs(attrs: &Value) -> Self {
        let mode = &attrs["permissions"];
        if mode.is_null() {
            return Self::Missing;
        }
        let permissions = mode.as_u64().and_then(|m| u32::try_from(m).ok());
        match EntryKind::from_permissions(permissions) {
            Some(kind) => Self::Known(kind),
            None if permissions.is_some_and(|mode| mode & 0o170000 == 0) => Self::Missing,
            None => Self::Unsupported,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ListedEntry {
    parent: String,
    revision: u64,
    name: String,
    kind: ListedKind,
}

#[derive(Clone, PartialEq, Eq)]
pub enum FileTarget {
    Listed(ListedEntry),
    Search(u64, FilenameMatch),
}

impl FileTarget {
    fn name(&self) -> &str {
        match self {
            Self::Listed(entry) => &entry.name,
            Self::Search(_, hit) => &hit.name,
        }
    }

    fn is_regular_file(&self) -> bool {
        match self {
            Self::Listed(entry) => entry.kind == ListedKind::Known(EntryKind::File),
            Self::Search(_, hit) => hit.kind == Some(EntryKind::File),
        }
    }

    fn into_location(self) -> (String, String) {
        match self {
            Self::Listed(entry) => (entry.parent, entry.name),
            Self::Search(_, hit) => (hit.parent, hit.name),
        }
    }
}

#[derive(Clone)]
pub enum TransferPicker {
    UploadFiles(String),
    UploadFolder(String),
    Download(FileTarget, Option<DownloadVerification>),
    DownloadMarked,
    DownloadSearch(u64),
}

#[derive(Clone)]
pub struct DownloadVerification {
    sha256: String,
    resume: bool,
}

impl TransferPicker {
    fn purpose(&self, files: &FileDrawer) -> Result<file_picker::Purpose> {
        use file_picker::Purpose;
        match self {
            Self::UploadFiles(parent) | Self::UploadFolder(parent) => {
                ensure!(
                    files.directory.as_ref() == Some(parent),
                    "The directory changed. Select the file operation again."
                );
                Ok(if matches!(self, Self::UploadFolder(_)) {
                    Purpose::UploadFolder
                } else {
                    Purpose::UploadFiles
                })
            }
            Self::Download(target, verification) => files
                .download_target(target, verification.is_some())?
                .purpose(),
            Self::DownloadMarked => {
                files.marked_entries()?;
                Ok(Purpose::DownloadFolder)
            }
            Self::DownloadSearch(serial) => {
                files.search_download_selection(*serial)?;
                Ok(Purpose::SaveSearchResults)
            }
        }
    }
}

enum RevealTarget {
    Existing(String),
    Changed(EntryChange),
}
impl RevealTarget {
    fn name(&self) -> &str {
        match self {
            Self::Existing(name) => name,
            Self::Changed(change) => change.name(),
        }
    }
}

struct EntryDialog {
    serial: u64,
    parent: Option<String>,
    name: String,
    resume_download: bool,
    action: EditAction,
}

enum EditAction {
    GoToFolder { revision: u64 },
    Create,
    Rename(ListedEntry),
    VerifyDownload(FileTarget),
    Delete(ListedEntry, EntryKind),
}

impl EditAction {
    fn original(&self) -> Option<&ListedEntry> {
        match self {
            Self::GoToFolder { .. } | Self::Create | Self::VerifyDownload(_) => None,
            Self::Rename(entry) | Self::Delete(entry, _) => Some(entry),
        }
    }
}

impl EntryDialog {
    fn destination(&self) -> Result<String> {
        let parent = || self.parent.as_deref().context("Missing remote directory");
        match &self.action {
            EditAction::GoToFolder { .. } => {
                ensure!(!self.name.is_empty(), "Enter a remote folder path");
                ensure!(!self.name.contains('\0'), "A path cannot contain NUL");
                if self.name.starts_with('/') {
                    Ok(self.name.clone())
                } else {
                    let directory = self
                        .parent
                        .as_deref()
                        .context("Enter an absolute path until a remote directory is open")?;
                    // Let the server resolve dot components and symlinks; shell syntax is literal.
                    let slash = if directory.ends_with('/') { "" } else { "/" };
                    Ok(format!("{directory}{slash}{}", self.name))
                }
            }
            EditAction::Rename(original) if self.name == original.name => {
                bail!("Enter a different name")
            }
            EditAction::Delete(original, _) => delete_path(parent()?, &original.name),
            EditAction::VerifyDownload(_) => normalize_sha256(&self.name),
            EditAction::Rename(_) | EditAction::Create => entry_path(parent()?, &self.name),
        }
    }
}

enum EntryChange {
    CreateFolder(String),
    Rename { from: String, to: String },
    Delete(String),
}
impl EntryChange {
    fn name(&self) -> &str {
        match self {
            Self::CreateFolder(name) | Self::Delete(name) => name,
            Self::Rename { to, .. } => to,
        }
    }
    fn description(&self) -> String {
        match self {
            Self::CreateFolder(name) => format!("creation of {name}"),
            Self::Delete(name) => format!("deletion of {name}"),
            Self::Rename { from, to } => format!("rename of {from} to {to}"),
        }
    }
    fn completed(&self) -> String {
        match self {
            Self::CreateFolder(name) => format!("Created folder {name}"),
            Self::Delete(name) => format!("Deleted {name}"),
            Self::Rename { from, to } => format!("Renamed {from} to {to}"),
        }
    }
    fn error(&self, error: &str) -> String {
        match self {
            Self::CreateFolder(name) => format!("Could not create folder {name}: {error}"),
            Self::Delete(name) => format!("Could not delete {name}: {error}"),
            Self::Rename { from, to } => format!("Rename {from} to {to}: {error}"),
        }
    }
}

enum Step {
    Canonical(Option<HistoryDirection>),
    Listing(String, Option<HistoryDirection>),
    IdentifyDelete(ListedEntry),
    Preview(u64),
    Search(u64),
    SaveText(editor::Save),
    Transfer(TransferFile),
    Change { parent: String, change: EntryChange },
}

enum TransferFile {
    Upload {
        name: String,
        remote: String,
        atomic: bool,
    },
    UploadFolder {
        remote: String,
    },
    Download {
        remote: String,
        resume: bool,
        local: PathBuf,
    },
    DownloadFolder {
        remote: String,
        local: PathBuf,
        selected: bool,
    },
}

fn counted(amount: u64, singular: &str) -> String {
    let ending = if amount == 1 { "" } else { "s" };
    format!("{amount} {singular}{ending}")
}

fn download_folder_subject(remote: &str, selected: bool) -> String {
    if selected {
        format!("marked results from {remote}")
    } else {
        remote.to_owned()
    }
}

impl TransferFile {
    fn downloaded_item(&self) -> Option<SavedDownload> {
        match self {
            Self::Download { local, .. } => Some(SavedDownload::File(Arc::from(local.as_path()))),
            Self::DownloadFolder { local, .. } => {
                Some(SavedDownload::Folder(Arc::from(local.as_path())))
            }
            _ => None,
        }
    }

    fn is_download(&self) -> bool {
        matches!(self, Self::Download { .. } | Self::DownloadFolder { .. })
    }

    fn outcome(&self, reply: &Value) -> String {
        let bytes = &reply["bytes_transferred"];
        match (self, reply["cancelled"].as_bool() == Some(true)) {
            (Self::Upload { remote, atomic, .. }, true) => {
                let path = reply["remote"].as_str().unwrap_or(remote);
                if *atomic {
                    format!(
                        "Cancelled atomic upload of {path} ({bytes} bytes acknowledged). The destination was left in place."
                    )
                } else if reply["remote_may_be_partial"] == true {
                    format!(
                        "Cancelled upload of {path} ({bytes} bytes acknowledged). The remote file may be incomplete."
                    )
                } else {
                    format!(
                        "Cancelled upload of {path} before opening the remote file ({bytes} bytes acknowledged)."
                    )
                }
            }
            (Self::Download { remote, .. }, true) if reply["partial_file"].is_string() => {
                format!(
                    "Cancelled download of {remote}. {bytes} bytes kept. Use the same destination and SHA-256 to resume."
                )
            }
            (Self::Download { remote, .. }, true) => {
                format!(
                    "Cancelled download of {remote} ({bytes} bytes written to the temporary file)."
                )
            }
            (
                Self::DownloadFolder {
                    remote, selected, ..
                },
                true,
            ) => format!(
                "Cancelled download of {} ({bytes} bytes written to the temporary folder).",
                download_folder_subject(remote, *selected),
            ),
            (Self::UploadFolder { remote }, true) => {
                let destination = reply["resolved_remote"].as_str().unwrap_or(remote);
                let outcome = if reply["remote_may_be_partial"] == true {
                    "The remote folder may be incomplete."
                } else {
                    "The remote folder was not created."
                };
                format!(
                    "Cancelled upload of {destination} ({} completed, {} acknowledged). {outcome}",
                    counted(reply["files"].as_u64().unwrap_or_default(), "file"),
                    counted(bytes.as_u64().unwrap_or_default(), "byte"),
                )
            }
            (Self::UploadFolder { remote }, false) => format!(
                "Uploaded folder {} ({}, {}, {}, {} skipped).",
                reply["resolved_remote"].as_str().unwrap_or(remote),
                counted(reply["files"].as_u64().unwrap_or_default(), "file"),
                counted(reply["directories"].as_u64().unwrap_or_default(), "folder"),
                counted(bytes.as_u64().unwrap_or_default(), "byte"),
                reply["skipped"],
            ),
            (Self::Upload { name, .. }, false) => format!("Uploaded {name} ({bytes} bytes)"),
            (Self::Download { remote, .. }, false) if reply["sha256"].is_string() => {
                if let Some(offset) = reply["resumed_from"].as_u64().filter(|offset| *offset > 0) {
                    format!(
                        "Downloaded {remote} ({bytes} bytes). SHA-256 verified. Resumed at byte {offset}."
                    )
                } else {
                    format!("Downloaded {remote} ({bytes} bytes). SHA-256 verified.")
                }
            }
            (Self::Download { remote, .. }, false) => {
                format!("Downloaded {remote} ({bytes} bytes)")
            }
            (
                Self::DownloadFolder {
                    remote,
                    local,
                    selected,
                },
                false,
            ) => {
                format!(
                    "Downloaded {} to {} ({} files, {bytes} bytes, {} links or special files skipped).",
                    download_folder_subject(remote, *selected),
                    local.display(),
                    reply["files"],
                    reply["skipped"]
                )
            }
        }
    }

    fn interrupted(&self, bytes: u64) -> String {
        match self {
            Self::Upload {
                remote,
                atomic: true,
                ..
            } => format!(
                "Atomic upload of {remote} was interrupted after at least {bytes} bytes. Check the destination and any temporary upload before trying again."
            ),
            Self::Upload { remote, .. } => format!(
                "Upload of {remote} was interrupted with at least {bytes} bytes acknowledged. The remote file may be incomplete."
            ),
            Self::UploadFolder { remote } => format!(
                "Upload of {remote} was interrupted with at least {bytes} bytes acknowledged. The remote folder may be incomplete."
            ),
            Self::Download {
                remote,
                resume: true,
                ..
            } => format!(
                "Download of {remote} interrupted. Check the destination, then use the same path and SHA-256 to resume."
            ),
            Self::Download {
                remote,
                resume: false,
                ..
            } => format!(
                "Download of {remote} was interrupted with at least {bytes} bytes written to the temporary file. The outcome is unknown. Check the local destination."
            ),
            Self::DownloadFolder {
                remote,
                local,
                selected,
            } => format!(
                "Download of {} was interrupted with at least {bytes} bytes written to the temporary folder. The outcome is unknown. Check {}.",
                download_folder_subject(remote, *selected),
                local.display()
            ),
        }
    }

    fn reconnect_status(
        &self,
        reply: &mut oneshot::Receiver<Result<Value, String>>,
        bytes: u64,
        queue: &mut TransferQueue,
    ) -> (String, Option<SavedDownload>, TransferOutcome) {
        use TransferOutcome::*;
        let (mut status, outcome) = match reply.try_recv() {
            Ok(Ok(result)) => {
                let outcome = if result["cancelled"] == true {
                    Cancelled
                } else {
                    queue.completed_one();
                    queue.record_download(self);
                    if queue.completed >= queue.total {
                        Completed
                    } else if queue.remaining.is_empty() {
                        Cancelled
                    } else {
                        Interrupted
                    }
                };
                (
                    format!(
                        "{} SSH reconnected. Refresh to browse.",
                        self.outcome(&result)
                    ),
                    outcome,
                )
            }
            Ok(Err(error)) if let Self::UploadFolder { remote } = self => {
                (format!("Could not upload folder {remote}: {error}"), Failed)
            }
            Ok(Err(error)) => (error, Failed),
            Err(_) => (
                format!(
                    "SSH reconnected. {} Refresh to browse.",
                    self.interrupted(bytes)
                ),
                Interrupted,
            ),
        };
        let saved = queue.finish(&mut status);
        (status, saved, outcome)
    }
}

pub(super) struct TransferView<'a> {
    pub(super) id: TransferId,
    pub(super) operation: TransferOperation,
    pub(super) remote: Option<&'a str>,
    pub(super) progress: Option<TransferProgress>,
    pub(super) batch: Option<TransferBatchProgress>,
    pub(super) state: TransferPhase,
}

pub(super) fn transfer_progress_views(p: TransferProgress) -> (View, View) {
    let mut bar = ProgressBar::new().is_indeterminate(p.total.is_none());
    if let Some(total) = p.total {
        bar = bar
            .maximum(total.max(1) as f64)
            .value(p.bytes.min(total) as f64);
    }
    let text = match (p.files, p.total) {
        (Some(files), _) => format!(
            "{} completed, {} acknowledged",
            counted(files, "file"),
            counted(p.bytes, "byte")
        ),
        (None, Some(total)) => format!("{} / {total} bytes", p.bytes),
        (None, None) => format!("{} bytes", p.bytes),
    };
    (bar.into(), TextBlock::new().text(text).into())
}

struct ActiveTransfer {
    id: TransferId,
    cancel: TransferCancel,
    progress: watch::Receiver<TransferProgress>,
}

impl ActiveTransfer {
    fn cancel(&self, id: TransferId, queue: &mut TransferQueue) -> bool {
        if self.id != id || self.cancel.is_cancelled() {
            return false;
        }
        queue.remaining.clear();
        self.cancel.cancel();
        true
    }
}

struct Pending {
    step: Step,
    reply: oneshot::Receiver<Result<Value, String>>,
    transfer: Option<ActiveTransfer>,
    destination: Rc<RefCell<LocalSender<Message>>>,
    _timer: crate::ui_timer::RetryTimer,
}

fn remote_child(directory: &str, name: &str) -> Result<String> {
    ensure!(
        !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0']),
        "Invalid remote filename"
    );
    Ok(format!("{}/{}", directory.trim_end_matches('/'), name))
}

fn entry_path(parent: &str, name: &str) -> Result<String> {
    // Windows SFTP servers can interpret a backslash as a path separator, too.
    ensure!(
        !name.trim().is_empty() && !name.contains('\\') && !name.chars().any(char::is_control),
        "Enter one name without path separators or control characters"
    );
    remote_child(parent, name)
}

fn delete_path(parent: &str, name: &str) -> Result<String> {
    ensure!(
        parent.starts_with('/')
            && !parent.contains("//")
            && !parent.contains('\\')
            && !parent.chars().any(char::is_control)
            && (parent == "/"
                || parent.strip_suffix('/').unwrap_or(parent)[1..]
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != "..")),
        "A delete requires an absolute, unambiguous remote directory. Refresh the drawer."
    );
    entry_path(parent, name)
}

// The SFTP response can already be ready when the reconnect frame reaches
// the UI before its polling timer. Keep a known result without resubmission.
fn change_reconnect_status(
    change: &EntryChange,
    reply: &mut oneshot::Receiver<Result<Value, String>>,
) -> String {
    match reply.try_recv() {
        Ok(Ok(_)) => format!(
            "{} before SSH reconnected. Refresh to browse.",
            change.completed()
        ),
        Ok(Err(error)) => change.error(&error),
        // An admitted worker request has a PendingReply guard that explicitly
        // delivers an unknown-outcome error if the worker stops in flight.
        Err(oneshot::error::TryRecvError::Closed) => {
            format!("SSH reconnected before {} started.", change.description())
        }
        Err(oneshot::error::TryRecvError::Empty) => format!(
            "SSH reconnected during {}. The outcome is unknown. Refresh to check.",
            change.description()
        ),
    }
}

/// OSC 7 is a path hint for the existing SFTP connection. Preserve the raw
/// authority: a URL library can normalize an explicit "localhost" to "".
pub(super) fn shell_directory(report: Option<&str>, host: &str) -> Option<String> {
    let raw = report?;
    let (scheme, rest) = raw.split_once("://")?;
    let (authority, path) = rest.split_once('/')?;
    if !scheme.eq_ignore_ascii_case("file")
        || raw.chars().any(char::is_control)
        || raw.contains(['?', '#', '\\'])
    {
        return None;
    }
    if !authority.is_empty() {
        fn ip(value: &str) -> Option<std::net::IpAddr> {
            value
                .strip_prefix('[')
                .and_then(|v| v.strip_suffix(']'))
                .unwrap_or(value)
                .parse()
                .ok()
        }
        let matches = match (ip(authority), ip(host)) {
            (Some(a), Some(b)) => a == b,
            _ => authority
                .trim_end_matches('.')
                .eq_ignore_ascii_case(host.trim_end_matches('.')),
        };
        if !matches || authority.contains(['@', '%']) {
            return None;
        }
    }
    let mut decoded = Vec::with_capacity(path.len() + 1);
    decoded.push(b'/');
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        let byte = if byte == b'%' {
            let high = char::from(bytes.next()?).to_digit(16)?;
            let low = char::from(bytes.next()?).to_digit(16)?;
            let value = (high * 16 + low) as u8;
            if value == b'/' {
                return None;
            }
            value
        } else {
            byte
        };
        decoded.push(byte);
    }
    let path = String::from_utf8(decoded).ok()?;
    if path.starts_with("//")
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || path.split('/').any(|part| matches!(part, "." | ".."))
    {
        None
    } else {
        Some(path)
    }
}

fn directory_entries(reply: &Value, order: SortOrder) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    let mut names = std::collections::HashSet::new();
    for item in reply["entries"]
        .as_array()
        .context("Missing SFTP directory listing")?
    {
        let name = item["filename"].as_str().context("Missing SFTP filename")?;
        if name == "." || name == ".." {
            continue;
        }
        remote_child("/", name)?;
        ensure!(names.insert(name), "SFTP returned a duplicate filename");
        let attrs = &item["attrs"];
        let kind = ListedKind::from_attrs(attrs);
        entries.push(Entry {
            name: name.to_owned(),
            kind,
            mtime: attrs["mtime"]
                .as_u64()
                .and_then(|time| time.try_into().ok()),
            size: match kind {
                ListedKind::Known(EntryKind::File) => attrs["size"].as_u64(),
                _ => None,
            },
        });
    }
    order.sort(&mut entries);
    Ok(entries)
}

fn listing_size_label(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let power = bytes.ilog2() / 10;
    let scale = 1_u128 << (power * 10);
    let tenths = (u128::from(bytes) * 10 + scale / 2) / scale;
    let (power, tenths) = if tenths == 10240 {
        (power + 1, 10)
    } else {
        (power, tenths)
    };
    let unit = UNITS[power as usize];
    if tenths.is_multiple_of(10) {
        format!("{} {unit}", tenths / 10)
    } else {
        format!("{}.{} {unit}", tenths / 10, tenths % 10)
    }
}

fn dialog_filename(remote: &str) -> &str {
    let leaf = remote.rsplit('/').next().unwrap_or("");
    if validate_download_name(leaf).is_ok() {
        leaf
    } else {
        "download"
    }
}

impl FileDrawer {
    pub(super) fn filter_editing(&self) -> bool {
        self.filter_input.is_some()
    }

    pub(super) fn end_filter_edit(&mut self) {
        if self.filter_input.take().is_some() {
            self.cached.get_mut().take();
        }
    }

    fn update_matches(&mut self) {
        self.matches.clear();
        if !self.filter.is_empty() {
            let needle = self.filter.to_lowercase();
            self.matches.extend(
                self.entries.iter().enumerate().filter_map(|(i, entry)| {
                    entry.name.to_lowercase().contains(&needle).then_some(i)
                }),
            );
        }
    }

    fn matching_entries(&self) -> impl ExactSizeIterator<Item = &Entry> {
        let unfiltered = self.filter.is_empty();
        let len = if unfiltered {
            self.entries.len()
        } else {
            self.matches.len()
        };
        (0..len).map(move |i| &self.entries[if unfiltered { i } else { self.matches[i] }])
    }

    fn page_entries(&self) -> impl Iterator<Item = &Entry> {
        self.matching_entries()
            .skip(self.page * PAGE_SIZE)
            .take(PAGE_SIZE)
    }

    fn set_filter(&mut self, filter: String) {
        if self.filter == filter {
            return;
        }
        self.filter = filter;
        self.update_matches();
        self.page = 0;
        if self.selected.is_some()
            && !self
                .page_entries()
                .any(|e| Some(&e.name) == self.selected.as_ref())
        {
            self.selected = None;
        }
    }

    fn set_page(&mut self, page: usize) {
        let last_page = self.matching_entries().len().saturating_sub(1) / PAGE_SIZE;
        self.page = page.min(last_page);
        self.selected = None;
    }

    pub(super) fn entry_dialog_open(&self) -> bool {
        self.dialog.is_some()
            || self.preview.is_some()
            || self.editor.as_ref().is_some_and(editor::Editor::is_open)
    }

    pub(super) fn cancel_entry_dialog(&mut self) {
        if let Some(editor) = self.editor.as_mut().filter(|e| e.is_open()) {
            editor.stash();
            self.cached.get_mut().take();
        }
        self.cancel_preview();
        if self.identifying_delete() {
            self.pending.take();
            self.status.clear();
            self.cached.get_mut().take();
        }
        if self.dialog.take().is_some() {
            self.cached.get_mut().take();
        }
    }

    fn identifying_delete(&self) -> bool {
        matches!(
            self.pending.as_ref().map(|p| &p.step),
            Some(Step::IdentifyDelete(_))
        )
    }

    fn has_entry(&self, entry: &ListedEntry) -> bool {
        self.directory.as_ref() == Some(&entry.parent)
            && self.listing_revision == entry.revision
            && self
                .page_entries()
                .any(|e| e.name == entry.name && e.kind == entry.kind)
    }

    pub(super) fn shell_origin(&self, pane: u64) -> Option<directory_split::Origin> {
        if !self.open || self.busy() || self.bookmarks.is_some() {
            return None;
        }
        let path = w11term::profiles::bookmark_directory(self.directory.as_deref()?)?;
        Some(directory_split::Origin::listing(
            pane,
            self.generation,
            path,
            self.listing_revision,
        ))
    }

    pub(super) fn search_shell_origin(
        &self,
        pane: u64,
        serial: u64,
        hit: &FilenameMatch,
    ) -> Option<directory_split::Origin> {
        if !self.open || self.busy() || self.entry_dialog_open() {
            return None;
        }
        self.search_hit_path(serial, hit).ok()?;
        directory_split::Origin::search(pane, self.generation, serial, hit)
    }

    fn check_selected(&self, entry: &ListedEntry) -> Result<()> {
        ensure!(
            self.selected.as_ref() == Some(&entry.name) && self.has_entry(entry),
            "The selection changed. Select the entry again."
        );
        Ok(())
    }

    fn selected_path(&self, entry: &ListedEntry) -> Result<String> {
        self.check_selected(entry)?;
        remote_child(&entry.parent, &entry.name)
    }

    fn checked_target_path(&self, target: &FileTarget) -> Result<String> {
        match target {
            FileTarget::Listed(entry) => {
                self.check_selected(entry)?;
                entry_path(&entry.parent, &entry.name)
            }
            FileTarget::Search(serial, hit) => self.search_hit_path(*serial, hit),
        }
    }

    fn selected_download_target<'a>(
        &self,
        entry: &'a ListedEntry,
    ) -> Result<downloads::DownloadTarget<'a>> {
        Ok(downloads::DownloadTarget {
            remote: self.selected_path(entry)?,
            name: &entry.name,
            directory: entry.kind == ListedKind::Known(EntryKind::Directory),
        })
    }

    fn download_target<'a>(
        &self,
        target: &'a FileTarget,
        verify: bool,
    ) -> Result<downloads::DownloadTarget<'a>> {
        match target {
            FileTarget::Listed(entry) => self.selected_download_target(entry),
            FileTarget::Search(serial, hit) => self.search_download_target(*serial, hit, verify),
        }
    }

    fn download_operation(
        &self,
        target: &FileTarget,
        destination: PathBuf,
        verification: Option<DownloadVerification>,
    ) -> Result<(Operation, TransferFile)> {
        self.download_target(target, verification.is_some())?
            .operation(destination, self.replace, self.preserve_mtime, verification)
    }

    fn prepare_folder_upload(
        &mut self,
        parent: &str,
        local: PathBuf,
    ) -> Result<(Operation, TransferFile)> {
        ensure!(
            self.directory.as_deref() == Some(parent),
            "The upload destination changed. Choose the folder again."
        );
        ensure!(local.is_absolute(), "Choose an absolute local folder");
        let name = local
            .file_name()
            .context("Select a folder below the drive or share root")?;
        let remote = entry_path(
            parent,
            name.to_str().context("SFTP requires a UTF-8 folder name")?,
        )?;
        self.status = format!("Uploading folder {remote}…");
        Ok((
            Operation::PutDir {
                local,
                remote: remote.clone(),
                preserve_times: self.preserve_upload_times,
            },
            TransferFile::UploadFolder { remote },
        ))
    }

    fn path_to_copy(&self, revision: u64, entry: Option<&ListedEntry>) -> Option<String> {
        if !self.open
            || self.busy()
            || self.bookmarks.is_some()
            || self.listing_revision != revision
        {
            return None;
        }
        match entry {
            Some(entry) => self.selected_path(entry).ok(),
            None => self.directory.clone(),
        }
    }

    fn history_target(
        &self,
        generation: u64,
        revision: u64,
        direction: HistoryDirection,
    ) -> Option<&str> {
        if !self.open
            || self.directory.is_none()
            || self.busy()
            || self.bookmarks.is_some()
            || self.generation != generation
            || self.listing_revision != revision
        {
            return None;
        }
        self.history.target(direction)
    }

    fn accept_listing(
        &mut self,
        direction: Option<HistoryDirection>,
        path: String,
        entries: Vec<Entry>,
    ) {
        self.cancel_preview();
        self.history.listed(&path, direction);
        self.opening_target = None;
        self.end_filter_edit();
        if self.directory.as_ref() != Some(&path) {
            self.filter.clear();
        }
        self.entries = entries;
        self.update_matches();
        self.listing_revision = self.listing_revision.wrapping_add(1);
        self.directory = Some(path);
        self.selected = None;
        self.marked_downloads = None;
        self.page = 0;
        if self.status == "Reading directory…" {
            self.status.clear();
        }
        let Some(target) = self.reveal.take() else {
            return;
        };
        // A newly listed file can appear at the path just deleted.
        if matches!(target, RevealTarget::Changed(EntryChange::Delete(_))) {
            return;
        }
        let name = target.name();
        if let Some(i) = self.entries.iter().position(|entry| {
            entry.name == name
                && (!matches!(target, RevealTarget::Changed(EntryChange::CreateFolder(_)))
                    || entry.kind != ListedKind::Known(EntryKind::File))
        }) {
            // Reveal an explicit file, rename or newly created folder even if
            // the current filter would hide it. Prefer the filtered page.
            let index = if self.filter.is_empty() {
                i
            } else {
                self.matches.binary_search(&i).unwrap_or_else(|_| {
                    self.filter.clear();
                    self.matches.clear();
                    i
                })
            };
            self.page = index / PAGE_SIZE;
            self.selected = Some(name.into());
        } else if let RevealTarget::Changed(change) = &target {
            self.status = format!(
                "{}, but the new entry is no longer listed as expected",
                change.completed()
            );
        } else {
            self.status = format!("No entry named {name} in this directory");
        }
    }

    fn begin_edit(&mut self, parent: String, action: EditAction) -> Result<()> {
        ensure!(
            self.open && !self.busy() && self.directory.as_ref() == Some(&parent),
            "The directory changed. Open the file action again."
        );
        if let Some(entry) = action.original() {
            self.check_selected(entry)?;
            entry_path(&parent, &entry.name)?;
        }
        if let EditAction::Delete(entry, kind) = &action {
            delete_path(&parent, &entry.name)?;
            ensure!(
                entry.kind == ListedKind::Missing || entry.kind == ListedKind::Known(*kind),
                "The server did not identify the listed entry as the selected type"
            );
        }
        self.show_entry_dialog(Some(parent), action);
        Ok(())
    }

    fn begin_go_to_folder(&mut self, revision: u64) {
        if self.open
            && self.generation != 0
            && !self.busy()
            && self.bookmarks.is_none()
            && revision == self.listing_revision
        {
            self.end_filter_edit();
            self.show_entry_dialog(self.directory.clone(), EditAction::GoToFolder { revision });
        }
    }

    fn begin_verified_download(&mut self, target: FileTarget, digest: String) -> Result<()> {
        ensure!(
            !matches!(&target, FileTarget::Listed(entry)
                if entry.kind == ListedKind::Known(EntryKind::Directory)),
            "Choose a file to verify"
        );
        self.checked_target_path(&target)?;
        let parent = self
            .directory
            .clone()
            .context("Open a remote folder first")?;
        self.end_filter_edit();
        self.begin_edit(parent, EditAction::VerifyDownload(target))?;
        self.dialog.as_mut().unwrap().name = digest;
        Ok(())
    }

    fn show_entry_dialog(&mut self, parent: Option<String>, action: EditAction) {
        self.dialog_serial = self.dialog_serial.wrapping_add(1);
        self.dialog = Some(EntryDialog {
            serial: self.dialog_serial,
            parent,
            name: match &action {
                EditAction::VerifyDownload(_) => String::new(),
                action => action
                    .original()
                    .map_or_else(String::new, |entry| entry.name.clone()),
            },
            resume_download: false,
            action,
        });
    }

    fn finish_edit(
        &mut self,
        serial: u64,
        result: ContentDialogResult,
    ) -> Result<Option<(EntryDialog, String)>> {
        if !self.dialog.as_ref().is_some_and(|e| e.serial == serial) {
            return Ok(None);
        }
        let dialog = self.dialog.take().unwrap();
        if result != ContentDialogResult::Primary {
            return Ok(None);
        }
        ensure!(
            self.open
                && self.directory == dialog.parent
                && !matches!(dialog.action, EditAction::GoToFolder { revision } if revision != self.listing_revision),
            "The directory changed. Open the file action again."
        );
        if let Some(entry) = dialog.action.original() {
            self.check_selected(entry)?;
        }
        if let EditAction::VerifyDownload(target) = &dialog.action {
            self.checked_target_path(target)?;
        }
        let path = dialog.destination()?;
        Ok(Some((dialog, path)))
    }

    pub(super) fn entry_dialog(
        &self,
        pane: u64,
        ssh: &SshOptions,
        context: &ViewContext<TerminalApp>,
    ) -> Option<View> {
        if let Some(editor) = self.editor.as_ref().filter(|e| e.is_open()) {
            return Some(editor.dialog(pane, self.generation, context));
        }
        if let Some(preview) = &self.preview {
            return Some(preview.dialog(pane, self.generation, self.editor.is_none(), context));
        }
        let dialog = self.dialog.as_ref()?;
        let serial = dialog.serial;
        let generation = self.generation;
        let valid = dialog.destination();
        if let EditAction::Delete(_, kind) = &dialog.action {
            let title = match kind {
                EntryKind::File => "Delete remote file?",
                EntryKind::Directory => "Delete remote folder?",
                EntryKind::Symlink => "Delete remote link?",
            };
            return Some(
                ContentDialog::new()
                    .title(title)
                    .primary_button_text("Delete")
                    .is_primary_button_enabled(valid.is_ok())
                    .close_button_text("Cancel")
                    .is_open(true)
                    .on_closed(context.callback(move |result| {
                        Message::Files(FileAction::FinishEdit(pane, generation, serial, result))
                    }))
                    .content(
                        StackPanel::new().spacing(8.0).min_width(300.0).children((
                            TextBlock::new()
                                .text(format!("{}@{}:{}", ssh.user, ssh.host, ssh.port))
                                .text_wrapping(TextWrapping::Wrap),
                            TextBlock::new()
                                .text(valid.unwrap_or_else(|e| e.to_string()))
                                .text_wrapping(TextWrapping::Wrap),
                        )),
                    ),
            );
        }
        let (title, label, confirm) = match dialog.action {
            EditAction::GoToFolder { .. } => ("Go to folder", "Remote folder path", "Open"),
            EditAction::Rename(_) => ("Rename entry", "New name", "Rename"),
            EditAction::VerifyDownload(_) => (
                "Download and verify",
                "Expected SHA-256",
                "Choose destination",
            ),
            EditAction::Create => ("New folder", "Folder name", "Create"),
            EditAction::Delete(..) => unreachable!(),
        };
        let mut fields = vec![KeyedView::new(
            "name",
            TextBox::new()
                .automation_name(label)
                .placeholder_text(label)
                .text(&dialog.name)
                .on_text_changed(context.callback(move |name| {
                    Message::Files(FileAction::EditName(pane, generation, serial, name))
                })),
        )];
        if let EditAction::VerifyDownload(target) = &dialog.action {
            fields.insert(
                0,
                KeyedView::new(
                    "download-source",
                    TextBlock::new()
                        .text(format!(
                            "{}@{}:{} · {}",
                            ssh.user,
                            ssh.host,
                            ssh.port,
                            target.name()
                        ))
                        .text_wrapping(TextWrapping::Wrap),
                ),
            );
            fields.push(KeyedView::new(
                "resume-download",
                CheckBox::new()
                    .automation_name("Resume interrupted download")
                    .is_checked(dialog.resume_download)
                    .on_is_checked_changed(context.callback(move |checked| {
                        Message::Files(FileAction::ResumeDownload(
                            pane, generation, serial, checked,
                        ))
                    }))
                    .content(TextBlock::new().text("Resume interrupted download")),
            ));
        }
        if !dialog.name.is_empty()
            && let Err(error) = &valid
        {
            fields.push(KeyedView::new(
                "error",
                TextBlock::new().text(error.to_string()),
            ));
        }
        Some(
            ContentDialog::new()
                .title(title)
                .primary_button_text(confirm)
                .is_primary_button_enabled(valid.is_ok())
                .close_button_text("Cancel")
                .is_open(true)
                .on_closed(context.callback(move |result| {
                    Message::Files(FileAction::FinishEdit(pane, generation, serial, result))
                }))
                .content(
                    StackPanel::new()
                        .spacing(8.0)
                        .min_width(300.0)
                        .keyed_children(fields),
                ),
        )
    }

    fn start_picker(&mut self) -> u64 {
        self.dialog_serial = self.dialog_serial.wrapping_add(1);
        self.picking = Some(self.dialog_serial);
        self.dialog_serial
    }

    fn finish_picker(&mut self, serial: u64) -> bool {
        if self.picking != Some(serial) {
            return false;
        }
        self.picking = None;
        self.open && self.dialog_serial == serial
    }

    fn close(&mut self) {
        self.open = false;
        self.marked_downloads = None;
        self.close_search();
        self.saved_search = None;
        if self.preparing_drop.take().is_some() {
            self.status = "Cancelled before uploading any dropped items.".into();
        }
        self.bookmarks = None;
        self.cancel_entry_dialog();
        self.end_filter_edit();
        // Still await the native window's return, even if the drawer is reopened.
        self.dialog_serial = self.dialog_serial.wrapping_add(1);
    }

    pub(super) fn awaiting_native_result(&self) -> bool {
        self.picking.is_some()
            || self.parsing
            || self.preparing_drop.is_some()
            || self.entry_dialog_open()
    }

    pub(super) fn bind_window(&mut self, sender: LocalSender<Message>) {
        self.end_filter_edit();
        self.cancel_entry_dialog();
        self.cached.get_mut().take();
        if let Some(pending) = &self.pending {
            *pending.destination.borrow_mut() = sender;
        }
    }

    pub(super) fn busy(&self) -> bool {
        self.awaiting_native_result() || self.pending.is_some()
    }

    fn transfer_file(&self) -> Option<&TransferFile> {
        match &self.pending.as_ref()?.step {
            Step::Transfer(file) => Some(file),
            _ => None,
        }
    }

    fn transfer_controls(&self, ctx: &ViewContext<TerminalApp>) -> Option<[View; 3]> {
        let transfer = self.pending.as_ref()?.transfer.as_ref()?;
        let id = transfer.id;
        let (bar, bytes) = transfer_progress_views(*transfer.progress.borrow());
        let cancel = Button::new()
            .is_enabled(!transfer.cancel.is_cancelled())
            .on_click(ctx.callback(move |_| Message::Files(FileAction::CancelTransfer(id))))
            .content("Cancel transfer");
        Some([bar, bytes, cancel])
    }

    pub(super) fn active_transfer(&self, generation: u64) -> Option<TransferView<'_>> {
        if self.generation != generation {
            return None;
        }
        if let Some(preparation) = &self.preparing_drop {
            return Some(TransferView {
                id: preparation.id,
                operation: TransferOperation::PrepareUpload,
                remote: self.directory.as_deref(),
                progress: None,
                batch: None,
                state: TransferPhase::Preparing,
            });
        }
        let file = self.transfer_file()?;
        let transfer = self.pending.as_ref()?.transfer.as_ref()?;
        let (operation, remote) = match file {
            TransferFile::Upload { remote, .. } => (TransferOperation::UploadFile, remote),
            TransferFile::UploadFolder { remote } => (TransferOperation::UploadFolder, remote),
            TransferFile::Download { remote, .. } => (TransferOperation::DownloadFile, remote),
            TransferFile::DownloadFolder { remote, .. } => {
                (TransferOperation::DownloadFolder, remote)
            }
        };
        Some(TransferView {
            id: transfer.id,
            operation,
            remote: Some(remote),
            progress: Some(*transfer.progress.borrow()),
            batch: (self.queue.total > 1).then(|| TransferBatchProgress {
                item: self.queue.next_item(),
                total: self.queue.total,
            }),
            state: if transfer.cancel.is_cancelled() {
                TransferPhase::Cancelling
            } else {
                TransferPhase::Running
            },
        })
    }

    fn transfer_stamp(&self) -> Option<(TransferId, bool)> {
        if let Some(preparation) = &self.preparing_drop {
            return Some((preparation.id, false));
        }
        let file = self.transfer_file()?;
        let active = self.pending.as_ref()?.transfer.as_ref()?;
        Some((active.id, file.is_download()))
    }

    fn cancel_transfer(&mut self, id: TransferId) -> Option<SftpCancelState> {
        if self
            .preparing_drop
            .as_ref()
            .is_some_and(|preparation| preparation.id == id)
        {
            self.preparing_drop = None;
            self.status = "Cancelled before uploading any dropped items.".into();
            self.cached.borrow_mut().take();
            return Some(SftpCancelState::CancelledBeforeStart);
        }
        let active = self.pending.as_ref()?.transfer.as_ref()?;
        if active.id != id {
            return None;
        }
        if active.cancel(id, &mut self.queue) {
            self.status = "Cancelling transfer…".into();
            self.cached.borrow_mut().take();
            Some(SftpCancelState::Cancelling)
        } else {
            Some(SftpCancelState::AlreadyCancelling)
        }
    }

    fn finish_transfer_queue(&mut self) {
        if let Some(saved) = self.queue.finish(&mut self.status) {
            self.last_download = Some(saved);
        }
    }

    fn publish_ended_transfer(
        &self,
        session: &Session,
        previous: Option<(TransferId, bool)>,
        background: bool,
        outcome: TransferOutcome,
    ) {
        if let Some((id, download)) = previous
            && self
                .transfer_stamp()
                .is_none_or(|(current, _)| current != id)
        {
            let saved = self.last_download.as_ref().filter(|_| download).cloned();
            session
                .events
                .sftp_transfer_finished(id, outcome, &self.status, saved, background);
        }
    }

    pub(super) fn observe_connection(&mut self, generation: u64) -> TransferOutcome {
        let mut outcome = TransferOutcome::Interrupted;
        if self.generation == generation {
            return outcome;
        }
        let mut last_download = self.last_download.take();
        let status = match self.pending.as_mut() {
            Some(Pending {
                step: Step::Change { change, .. },
                reply,
                ..
            }) => change_reconnect_status(change, reply),
            Some(Pending {
                step: Step::Transfer(file),
                reply,
                transfer: Some(transfer),
                ..
            }) => {
                let (status, saved, terminal) =
                    file.reconnect_status(reply, transfer.progress.borrow().bytes, &mut self.queue);
                outcome = terminal;
                if saved.is_some() {
                    last_download = saved;
                }
                status
            }
            _ => match &self.reveal {
                Some(RevealTarget::Changed(change)) => format!(
                    "{} before SSH reconnected. Refresh to browse.",
                    change.completed()
                ),
                _ if self.preparing_drop.is_some() => {
                    "SSH reconnected before the dropped upload started.".into()
                }
                _ => "SSH connected. Refresh to browse.".into(),
            },
        };
        let editor = self.detach_editor();
        self.close_search();
        *self = Self {
            open: self.open,
            following: self.following,
            sort_order: self.sort_order,
            preserve_mtime: self.preserve_mtime,
            preserve_upload_times: self.preserve_upload_times,
            dialog_serial: self.dialog_serial,
            editor,
            last_download,
            generation,
            status,
            ..Self::default()
        };
        outcome
    }

    fn observe_transfer_connection(&mut self, session: &Session, generation: u64, active: bool) {
        let previous = self.transfer_stamp();
        let outcome = self.observe_connection(generation);
        self.publish_ended_transfer(session, previous, !active || !self.open, outcome);
    }

    /// Sample only when ready to browse. A burst during a transfer or listing is
    /// coalesced into the latest report, without changing any transfer destination.
    fn follow_target(&mut self, report: &Option<Arc<str>>, host: &str) -> Option<String> {
        if !self.open
            || !self.following
            || self.busy()
            || self.filter_editing()
            || self.bookmarks.is_some()
            || self.search.is_some()
            || self.last_report == *report
        {
            return None;
        }
        self.last_report.clone_from(report);
        shell_directory(report.as_deref(), host)
            .filter(|path| Some(path) != self.directory.as_ref())
    }

    fn panel(
        &self,
        pane: u64,
        ssh: &SshOptions,
        profile: Option<&str>,
        shell_ready: bool,
        layout: DrawerLayout,
        context: &ViewContext<TerminalApp>,
    ) -> View {
        if let Some(cached) = self.cached.borrow().as_ref().filter(|saved| {
            saved.profile.as_deref() == profile
                && saved.shell_ready == shell_ready
                && saved.layout == layout
        }) {
            return cached.view.clone();
        }
        let cache = |view: View| {
            *self.cached.borrow_mut() = Some(CachedPanel {
                profile: profile.map(str::to_owned),
                shell_ready,
                layout,
                view: view.clone(),
            });
            view
        };
        if let Some(search) = &self.search {
            return cache(search.view(self, pane, shell_ready, context));
        }
        if let Some(list) = &self.bookmarks
            && Some(list.profile.as_str()) == profile
        {
            return cache(list.panel(self, pane, context));
        }
        use FileAction::*;
        let button = |label: &str, enabled: bool, action: FileAction| {
            Button::new()
                .is_enabled(enabled)
                .on_click(context.callback(move |_| Message::Files(action.clone())))
                .content(TextBlock::new().text(label))
        };
        let button_pair = |first: View, second: View| {
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .children((first, second))
        };
        let folder = self.directory.as_deref().unwrap_or(".");
        let generation = self.generation;
        let ready = !self.busy();
        let writable = ready && self.directory.is_some();
        let selected = self
            .page_entries()
            .find(|e| Some(&e.name) == self.selected.as_ref());
        let download_folder =
            selected.is_some_and(|e| e.kind == ListedKind::Known(EntryKind::Directory));
        let revision = self.listing_revision;
        let drop_target = DropTarget::new(pane, self, folder);
        let shell = if shell_ready {
            self.shell_origin(pane)
        } else {
            None
        };
        let listed = |entry: Option<&Entry>| ListedEntry {
            revision,
            parent: folder.into(),
            name: entry.map_or_else(String::new, |e| e.name.clone()),
            kind: entry.map_or(ListedKind::Missing, |e| e.kind),
        };
        let find_files = button(
            "Find files…",
            writable && generation != 0,
            Search(pane, generation, find::Action::Show(revision)),
        );
        let search_actions = if let Some(saved) = &self.saved_search {
            StackPanel::new().spacing(6.0).children((
                button(
                    "Back to search results",
                    writable && generation != 0,
                    Search(pane, generation, find::Action::Restore(saved.serial)),
                ),
                find_files,
            ))
        } else {
            find_files
        };
        let mut controls: Vec<View> = vec![
            Grid::new()
                .width(layout.content_width())
                .horizontal_alignment(HorizontalAlignment::Left)
                .columns([GridLength::Star(1.0), GridLength::Auto])
                .children((
                    TextBlock::new()
                        .text(format!("{}@{}:{}", ssh.user, ssh.host, ssh.port))
                        .text_wrapping(TextWrapping::Wrap),
                    Border::new()
                        .grid_column(1)
                        .content(button("Close", true, Close(pane))),
                )),
            Grid::new()
                .width(layout.content_width())
                .horizontal_alignment(HorizontalAlignment::Left)
                .columns([GridLength::Star(1.0), GridLength::Auto])
                .children((
                    TextBlock::new()
                        .text(folder)
                        .text_wrapping(TextWrapping::Wrap),
                    Border::new().grid_column(1).content(button(
                        "Go to folder…",
                        ready && generation != 0,
                        GoToFolder(pane, generation, revision),
                    )),
                )),
            search_actions,
            StackPanel::new()
                .orientation(if layout.content_width() >= 300.0 {
                    Orientation::Horizontal
                } else {
                    Orientation::Vertical
                })
                .spacing(6.0)
                .children((
                    button_pair(
                        button(
                            "Back",
                            writable && self.history.target(HistoryDirection::Back).is_some(),
                            History(pane, generation, revision, HistoryDirection::Back),
                        ),
                        button(
                            "Forward",
                            writable && self.history.target(HistoryDirection::Forward).is_some(),
                            History(pane, generation, revision, HistoryDirection::Forward),
                        ),
                    ),
                    button_pair(
                        button(
                            "Up",
                            writable && folder != "/",
                            Browse(
                                pane,
                                generation,
                                format!("{}/..", folder.trim_end_matches('/')),
                            ),
                        ),
                        button("Refresh", ready, Browse(pane, generation, folder.into())),
                    ),
                )),
            StackPanel::new()
                .orientation(if layout.content_width() >= 320.0 {
                    Orientation::Horizontal
                } else {
                    Orientation::Vertical
                })
                .spacing(6.0)
                .children((
                    button(
                        "Bookmarks…",
                        ready && profile.is_some(),
                        Bookmarks(pane, generation, bookmarks::Action::Show),
                    ),
                    button_pair(
                        button(
                            "Upload…",
                            writable,
                            Pick(
                                pane,
                                generation,
                                revision,
                                TransferPicker::UploadFiles(folder.into()),
                            ),
                        ),
                        button(
                            "Upload folder…",
                            writable,
                            Pick(
                                pane,
                                generation,
                                revision,
                                TransferPicker::UploadFolder(folder.into()),
                            ),
                        ),
                    ),
                )),
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .children((
                    button(
                        "New folder",
                        writable,
                        NewFolder(pane, generation, folder.into()),
                    ),
                    button(
                        "Rename…",
                        writable && selected.is_some_and(|e| entry_path(folder, &e.name).is_ok()),
                        Rename(pane, generation, listed(selected)),
                    ),
                    button(
                        "Delete…",
                        writable
                            && selected.is_some_and(|e| {
                                e.kind != ListedKind::Unsupported
                                    && delete_path(folder, &e.name).is_ok()
                            }),
                        Delete(pane, generation, listed(selected)),
                    ),
                )),
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .children((
                    button(
                        "Open folder",
                        ready
                            && selected
                                .is_some_and(|e| e.kind != ListedKind::Known(EntryKind::File)),
                        OpenEntry(pane, generation, listed(selected)),
                    ),
                    Button::new()
                        .automation_name("Open shell here")
                        .is_enabled(shell.is_some())
                        .on_click(context.callback(move |_| {
                            directory_split::message(
                                directory_split::Destination::Split(Axis::Columns),
                                shell.as_ref(),
                            )
                        }))
                        .content("Open shell here"),
                )),
            button_pair(
                button(
                    if download_folder {
                        "Download folder…"
                    } else {
                        "Download…"
                    },
                    ready && selected.is_some(),
                    Pick(
                        pane,
                        generation,
                        revision,
                        TransferPicker::Download(FileTarget::Listed(listed(selected)), None),
                    ),
                ),
                button(
                    "Download and verify…",
                    ready && selected.is_some() && !download_folder,
                    VerifyDownload(pane, generation, listed(selected)),
                ),
            ),
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .children((
                    button(
                        "Copy directory path",
                        writable,
                        CopyPath(pane, generation, revision, None),
                    ),
                    button(
                        "Copy selected path",
                        writable && selected.is_some(),
                        CopyPath(pane, generation, revision, Some(listed(selected))),
                    ),
                )),
            button_pair(
                button(
                    "Preview text…",
                    writable
                        && selected.is_some_and(|e| e.kind == ListedKind::Known(EntryKind::File)),
                    Preview(
                        pane,
                        generation,
                        FileTarget::Listed(listed(selected)),
                        PreviewKind::Text,
                    ),
                ),
                button(
                    "Properties…",
                    writable && selected.is_some(),
                    Preview(
                        pane,
                        generation,
                        FileTarget::Listed(listed(selected)),
                        PreviewKind::Properties,
                    ),
                ),
            ),
            CheckBox::new()
                .is_checked(self.following)
                .on_is_checked_changed(
                    context.callback(move |v| Message::Files(Follow(pane, generation, v))),
                )
                .content(TextBlock::new().text("Follow shell directory")),
            CheckBox::new()
                .is_enabled(ready)
                .is_checked(self.replace)
                .on_is_checked_changed(
                    context.callback(move |v| Message::Files(Replace(pane, generation, v))),
                )
                .content(TextBlock::new().text("Replace existing files")),
            CheckBox::new()
                .automation_name("Preserve modification times")
                .is_enabled(ready)
                .is_checked(self.preserve_mtime)
                .on_is_checked_changed(context.callback(move |enabled| {
                    Message::Files(PreserveMtime(pane, generation, enabled))
                }))
                .content(TextBlock::new().text("Preserve modification times")),
            CheckBox::new()
                .is_enabled(ready)
                .is_checked(self.atomic_uploads)
                .on_is_checked_changed(
                    context.callback(move |v| Message::Files(AtomicUploads(pane, generation, v))),
                )
                .content(TextBlock::new().text("Upload files atomically")),
            CheckBox::new()
                .automation_name("Preserve upload timestamps")
                .is_enabled(ready)
                .is_checked(self.preserve_upload_times)
                .on_is_checked_changed(context.callback(move |enabled| {
                    Message::Files(PreserveUploadTimes(pane, generation, enabled))
                }))
                .content(TextBlock::new().text("Preserve upload timestamps")),
        ];
        controls.extend(self.resume_draft_button(pane, context));
        let sort_button = |order, label: &str, name: &str| {
            RadioButton::new()
                .group_name(format!("sftp-sort-{pane}"))
                .automation_name(name)
                .min_width(0.0)
                .is_enabled(writable)
                .is_checked(self.sort_order == order)
                .on_checked(context.callback(move |checked| {
                    if checked {
                        Message::Files(Sort(pane, generation, revision, order))
                    } else {
                        Message::ControlIdle
                    }
                }))
                .content(label)
        };
        controls.push(
            StackPanel::new()
                .orientation(if layout.content_width() >= 320.0 {
                    Orientation::Horizontal
                } else {
                    Orientation::Vertical
                })
                .spacing(6.0)
                .children((
                    TextBlock::new()
                        .vertical_alignment(VerticalAlignment::Center)
                        .text("Sort by"),
                    sort_button(SortOrder::Name, "Name", "Sort by name"),
                    sort_button(SortOrder::NewestFirst, "Newest first", "Sort newest first"),
                    sort_button(
                        SortOrder::LargestFirst,
                        "Largest first",
                        "Sort largest first",
                    ),
                )),
        );
        let filter: View = if let Some(input) = &self.filter_input {
            Grid::new()
                .columns([GridLength::Star(1.0), GridLength::Auto])
                .children((
                    TextBox::new()
                        .element_ref(input)
                        .automation_name("Filter filenames")
                        .automation_id("sftp-filename-filter")
                        .placeholder_text("Filter filenames")
                        .text(&self.filter)
                        .on_text_changed(context.callback(move |text| {
                            Message::Files(Filter(pane, generation, revision, text))
                        })),
                    Border::new().grid_column(1).content(button(
                        "Done",
                        true,
                        EditFilter(pane, generation, revision, false),
                    )),
                ))
        } else {
            Button::new()
                .automation_name("Filter filenames…")
                .is_enabled(writable)
                .on_click(context.callback(move |_| {
                    Message::Files(EditFilter(pane, generation, revision, true))
                }))
                .content(TextBlock::new().text(if self.filter.is_empty() {
                    "Filter filenames…".into()
                } else {
                    format!("Filter: {}", self.filter)
                }))
        };
        controls.push(
            Grid::new()
                .width(layout.content_width())
                .horizontal_alignment(HorizontalAlignment::Left)
                .columns([GridLength::Star(1.0), GridLength::Auto])
                .children((
                    filter,
                    Border::new().grid_column(1).content(button(
                        "Clear filter",
                        writable && !self.filter.is_empty(),
                        ClearFilter(pane, generation, revision),
                    )),
                )),
        );
        if let Some(marked) = &self.marked_downloads {
            controls.push(
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(6.0)
                    .children((
                        Button::new()
                            .automation_name("Download marked items")
                            .is_enabled(writable && !marked.is_empty())
                            .on_click(context.callback(move |_| {
                                Message::Files(Pick(
                                    pane,
                                    generation,
                                    revision,
                                    TransferPicker::DownloadMarked,
                                ))
                            }))
                            .content(
                                TextBlock::new()
                                    .text(format!("Download marked ({})…", marked.len())),
                            ),
                        button(
                            "Exit download selection",
                            writable,
                            SelectDownloads(pane, generation, revision, false),
                        ),
                    )),
            );
        } else {
            controls.push(button(
                "Select downloads…",
                writable,
                SelectDownloads(pane, generation, revision, true),
            ));
        }
        if let Some(transfer) = self.transfer_controls(context) {
            controls.extend(transfer);
        }
        if let Some(preparation) = &self.preparing_drop {
            controls.push(button(
                "Cancel transfer",
                true,
                CancelTransfer(preparation.id),
            ));
        }
        #[cfg(feature = "ui-smoke")]
        if let Some(file) = std::env::var_os("W11TERM_SFTP_DROP_SMOKE_ITEMS") {
            controls.push(button(
                "Drop fixture items",
                writable,
                SmokeDrop(drop_target.clone(), file.into()),
            ));
        }
        if !self.status.is_empty() {
            controls.push(
                TextBlock::new()
                    .text(&self.status)
                    .max_width(layout.content_width())
                    .horizontal_alignment(HorizontalAlignment::Left)
                    .text_wrapping(TextWrapping::Wrap)
                    .into(),
            );
        }
        if let Some(download) = &self.last_download {
            controls.push(button(
                explorer::download_label(download),
                ready,
                ShowSavedDownload(pane, download.clone()),
            ));
        }
        let count = self.matching_entries().len();
        if !self.filter.is_empty() {
            let label = match count {
                0 => "No matching files or folders".into(),
                1 => "1 match".into(),
                n => format!("{n} matches"),
            };
            controls.push(TextBlock::new().text(label).into());
        }
        if count > PAGE_SIZE {
            let pages = count.div_ceil(PAGE_SIZE);
            controls.push(
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(6.0)
                    .children((
                        button(
                            "Previous",
                            ready && self.page > 0,
                            Page(pane, generation, revision, self.page.saturating_sub(1)),
                        ),
                        TextBlock::new().text(format!("{} / {pages}", self.page + 1)),
                        button(
                            "Next",
                            ready && self.page + 1 < pages,
                            Page(pane, generation, revision, self.page + 1),
                        ),
                    )),
            );
        }
        let list: Vec<_> = self
            .page_entries()
            .map(|entry| {
                let name = entry.name.clone();
                let label = if entry.kind == ListedKind::Known(EntryKind::Directory) {
                    format!("{name}/")
                } else {
                    name.clone()
                };
                let active = self.selected.as_ref() == Some(&name);
                let entry_action = Choose(pane, generation, listed(Some(entry)));
                let filename = TextBlock::new()
                    .text(&label)
                    .text_wrapping(TextWrapping::Wrap);
                let content: View = if let Some(bytes) = entry.size {
                    let spoken_size = if bytes == 1 {
                        "Size 1 byte".to_owned()
                    } else {
                        format!("Size {bytes} bytes")
                    };
                    Grid::new()
                        .columns([GridLength::Star(1.0), GridLength::Auto])
                        .column_spacing(8.0)
                        .children((
                            filename,
                            TextBlock::new()
                                .grid_column(1)
                                .vertical_alignment(VerticalAlignment::Center)
                                .automation_id("sftp-entry-size")
                                .automation_name(spoken_size)
                                .text(listing_size_label(bytes)),
                        ))
                } else {
                    filename.into()
                };
                if let Some(marked) = &self.marked_downloads {
                    let entry_action = listed(Some(entry));
                    return KeyedView::new(
                        entry.name.as_str(),
                        CheckBox::new()
                            .automation_name(format!("Mark {label} for download"))
                            .is_enabled(ready && entry.can_mark_for_download())
                            .is_checked(marked.contains(&name))
                            .horizontal_alignment(HorizontalAlignment::Stretch)
                            .on_is_checked_changed(context.callback(move |checked| {
                                Message::Files(MarkDownload(
                                    pane,
                                    generation,
                                    entry_action.clone(),
                                    checked,
                                ))
                            }))
                            .content(content),
                    );
                }
                KeyedView::new(
                    entry.name.as_str(),
                    Button::new()
                        .automation_name(label)
                        .is_enabled(ready)
                        .style(if active {
                            ButtonStyle::Accent
                        } else {
                            ButtonStyle::Subtle
                        })
                        .horizontal_alignment(HorizontalAlignment::Stretch)
                        .horizontal_content_alignment(HorizontalAlignment::Stretch)
                        .on_click(context.callback(move |_| Message::Files(entry_action.clone())))
                        .content(content),
                )
            })
            .collect();
        let actions = ScrollViewer::new()
            .automation_name("SFTP actions")
            .max_height(layout.actions_max_height)
            .horizontal_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .content(
                StackPanel::new().spacing(6.0).keyed_children(
                    controls
                        .into_iter()
                        .enumerate()
                        .map(|(index, view)| KeyedView::new(index.to_string(), view)),
                ),
            );
        let panel = Border::new()
            .padding(PANEL_PADDING)
            .background(Color::rgb(31, 32, 35))
            .drop_policy(writable.then(|| {
                DragDropPolicy::new().storage_items(
                    DragDropAction::new(DragDropOperation::Copy)
                        .caption("Upload files and folders"),
                )
            }))
            .on_drop(context.callback(move |data| Message::Files(drop_target.data(data))))
            .content(
                Grid::new()
                    .rows([GridLength::Auto, GridLength::Star(1.0)])
                    .children((
                        actions,
                        ScrollViewer::new()
                            .grid_row(1)
                            .automation_name("SFTP entries")
                            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                            .content(StackPanel::new().keyed_children(list)),
                    )),
            );
        cache(panel)
    }

    fn job(
        &mut self,
        id: u64,
        session: &Session,
        operation: Operation,
        step: Step,
        context: &ComponentContext<TerminalApp>,
    ) -> Result<()> {
        let (tx, reply) = oneshot::channel();
        let (tx_progress, transfer) = if matches!(step, Step::Transfer(_)) {
            let (tx, progress) = watch::channel(TransferProgress::default());
            let id = self.transfer_id(id);
            (
                Some(tx),
                Some(ActiveTransfer {
                    id,
                    cancel: TransferCancel::default(),
                    progress,
                }),
            )
        } else {
            (None, None)
        };
        let destination = Rc::new(RefCell::new(context.sender()));
        let tick = destination.clone();
        let timer = crate::ui_timer::RetryTimer::new(move || {
            tick.borrow().send(Message::Files(FileAction::Tick(id)));
        })?;
        session
            .input
            .send(Command::Sftp(Job {
                expected_generation: request_generation(&operation, self.generation),
                operation,
                progress: tx_progress,
                cancel: if matches!(step, Step::Preview(_)) {
                    self.preview.as_ref().map(|p| p.cancel.clone())
                } else if matches!(step, Step::Search(_)) {
                    self.search.as_ref().and_then(|p| p.cancel.clone())
                } else {
                    transfer.as_ref().map(|active| active.cancel.clone())
                },
                deadline: tokio::time::Instant::now() + Duration::from_secs(DEFAULT_TIMEOUT),
                reply: tx,
            }))
            .map_err(|_| anyhow!("SSH session has closed"))?;
        if let Some(transfer) = &transfer {
            session.events.sftp_transfer_started(transfer.id);
        }
        if matches!(&step, Step::Transfer(file) if file.is_download()) && self.queue.completed == 0
        {
            self.last_download = None;
        }
        self.pending = Some(Pending {
            step,
            reply,
            transfer,
            destination,
            _timer: timer,
        });
        Ok(())
    }

    fn browse(
        &mut self,
        id: u64,
        session: &Session,
        path: String,
        direction: Option<HistoryDirection>,
        context: &ComponentContext<TerminalApp>,
    ) -> Result<()> {
        self.cancel_preview();
        self.close_search();
        self.bookmarks = None;
        self.opening_target = None;
        self.end_filter_edit();
        self.status = "Reading directory…".into();
        self.reveal = None;
        self.job(
            id,
            session,
            Operation::Realpath { path },
            Step::Canonical(direction),
            context,
        )
    }

    fn browse_named(
        &mut self,
        pane: u64,
        session: &Session,
        path: String,
        description: String,
        direction: Option<HistoryDirection>,
        context: &ComponentContext<TerminalApp>,
    ) -> Result<()> {
        self.following = false;
        let result = self.browse(pane, session, path, direction, context);
        self.opening_target = Some(description);
        result
    }

    fn transfer_next(
        &mut self,
        id: u64,
        session: &Session,
        context: &ComponentContext<TerminalApp>,
    ) -> Result<()> {
        let (op, transfer) = self
            .queue
            .remaining
            .pop_front()
            .context("No transfer selected")?;
        self.status = match &transfer {
            TransferFile::Upload { name, .. } => format!("Uploading {name}…"),
            TransferFile::UploadFolder { remote } => format!("Uploading folder {remote}…"),
            TransferFile::Download { remote, .. } | TransferFile::DownloadFolder { remote, .. } => {
                format!(
                    "Downloading item {} of {}: {remote}…",
                    self.queue.next_item(),
                    self.queue.total
                )
            }
        };
        self.job(id, session, op, Step::Transfer(transfer), context)
    }

    fn tick(
        &mut self,
        id: u64,
        session: &Session,
        may_open_dialog: bool,
        context: &ComponentContext<TerminalApp>,
        outcome: &mut TransferOutcome,
    ) -> Result<()> {
        let Some(pending) = &mut self.pending else {
            return Ok(());
        };
        let result = match pending.reply.try_recv() {
            Err(oneshot::error::TryRecvError::Empty) => {
                if let Some(transfer) = pending.transfer.as_mut()
                    && transfer.progress.has_changed().unwrap_or(false)
                {
                    let _ = transfer.progress.borrow_and_update();
                    self.cached.borrow_mut().take();
                }
                return Ok(());
            }
            Err(oneshot::error::TryRecvError::Closed) => {
                if let Step::Change { change, .. } = &pending.step {
                    Err(format!(
                        "SSH connection closed before {} started.",
                        change.description()
                    ))
                } else if let (Step::Transfer(file), Some(transfer)) =
                    (&pending.step, &pending.transfer)
                {
                    let error = file.interrupted(transfer.progress.borrow().bytes);
                    *outcome = TransferOutcome::Interrupted;
                    self.pending.take();
                    self.cached.borrow_mut().take();
                    bail!("{error}");
                } else {
                    Err("SSH connection closed".into())
                }
            }
            Ok(result) => result,
        };
        self.cached.borrow_mut().take();
        match (self.pending.take().unwrap().step, result) {
            (Step::Preview(serial), reply) => self.finish_preview(serial, reply),
            (Step::Search(serial), reply) => self.finish_search(serial, reply),
            (Step::SaveText(save), result) => {
                if let Some((parent, name)) = self.finish_save(save, result) {
                    self.reveal = Some(RevealTarget::Existing(name));
                    if let Err(error) = self.job(
                        id,
                        session,
                        Operation::Ls {
                            path: parent.clone(),
                        },
                        Step::Listing(parent, None),
                        context,
                    ) {
                        self.reveal = None;
                        self.status
                            .push_str(&format!(". Refresh to list the new file: {error:#}"));
                    }
                }
            }
            (step, Err(error)) => {
                if let Step::IdentifyDelete(entry) = &step {
                    bail!("Could not delete {}: {error}", entry.name);
                }
                if let Step::Change { change, .. } = &step {
                    bail!("{}", change.error(&error));
                }
                if let Step::Transfer(TransferFile::Upload { name, .. }) = &step {
                    if !self.queue.overwrite_files
                        && self.entries.iter().any(|entry| &entry.name == name)
                    {
                        bail!("Upload of {name} failed (replacing files is off): {error}");
                    }
                    bail!("Upload of {name} failed: {error}");
                }
                if let Step::Transfer(TransferFile::DownloadFolder {
                    remote,
                    local,
                    selected,
                }) = &step
                {
                    bail!(
                        "Could not download {} to {}: {error}",
                        download_folder_subject(remote, *selected),
                        local.display()
                    );
                }
                if let Step::Transfer(TransferFile::Download { remote, .. }) = &step
                    && self.queue.batch_id.is_some()
                {
                    bail!("Could not download {remote}: {error}");
                }
                if let Step::Transfer(TransferFile::UploadFolder { remote }) = &step {
                    bail!("Could not upload folder {remote}: {error}");
                }
                bail!("{error}");
            }
            (Step::IdentifyDelete(entry), Ok(reply)) => {
                if self.open && may_open_dialog {
                    self.confirm_identified(entry, &reply["attrs"])?;
                } else {
                    self.status.clear();
                }
            }
            (Step::Canonical(direction), Ok(reply)) => {
                let path = reply["entries"][0]["filename"]
                    .as_str()
                    .context("Missing canonical directory")?
                    .to_owned();
                ensure!(
                    !path.is_empty() && !path.contains('\0'),
                    "Invalid canonical directory"
                );
                self.job(
                    id,
                    session,
                    Operation::Ls { path: path.clone() },
                    Step::Listing(path, direction),
                    context,
                )?;
            }
            (Step::Listing(path, direction), Ok(reply)) => {
                let generation = self.generation;
                let order = self.sort_order;
                self.parsing = true;
                let rejected = Message::Files(FileAction::Listed(
                    id,
                    generation,
                    path.clone(),
                    direction,
                    Err("Unable to display the directory; refresh to retry".into()),
                ));
                context.spawn_background_with_rejection(
                    move |_| {
                        Message::Files(FileAction::Listed(
                            id,
                            generation,
                            path,
                            direction,
                            directory_entries(&reply, order).map_err(|e| e.to_string()),
                        ))
                    },
                    rejected,
                );
            }
            (Step::Transfer(file), Ok(reply)) => {
                self.status = file.outcome(&reply);
                if reply["cancelled"] == true {
                    *outcome = TransferOutcome::Cancelled;
                    self.finish_transfer_queue();
                    return Ok(());
                }
                let upload = !file.is_download();
                self.queue.record_download(&file);
                if !upload && self.queue.batch_id.is_none() {
                    *outcome = TransferOutcome::Completed;
                    self.finish_transfer_queue();
                    return Ok(());
                }
                self.queue.completed_one();
                if self.queue.remaining.is_empty() {
                    *outcome = if self.queue.completed < self.queue.total {
                        TransferOutcome::Cancelled
                    } else {
                        TransferOutcome::Completed
                    };
                    self.finish_transfer_queue();
                    if upload {
                        let path = self.directory.clone().context("Missing upload directory")?;
                        if let Err(error) = self.job(
                            id,
                            session,
                            Operation::Ls { path: path.clone() },
                            Step::Listing(path, None),
                            context,
                        ) {
                            self.status
                                .push_str(&format!(" Directory refresh failed. {error:#}"));
                        }
                    }
                } else {
                    self.transfer_next(id, session, context)?;
                }
            }
            (Step::Change { parent, change }, Ok(_)) => {
                self.status = change.completed();
                // The old name no longer identifies the entry the user selected.
                // If the follow-up listing fails, do not leave stale rows actionable.
                if matches!(change, EntryChange::Rename { .. } | EntryChange::Delete(_)) {
                    self.entries.clear();
                    self.matches.clear();
                    self.selected = None;
                    self.marked_downloads = None;
                    self.page = 0;
                }
                self.reveal = Some(RevealTarget::Changed(change));
                self.job(
                    id,
                    session,
                    Operation::Ls {
                        path: parent.clone(),
                    },
                    Step::Listing(parent, None),
                    context,
                )?;
            }
        }
        Ok(())
    }

    fn confirm_identified(&mut self, entry: ListedEntry, attrs: &Value) -> Result<()> {
        let ListedKind::Known(kind) = ListedKind::from_attrs(attrs) else {
            bail!(
                "Could not delete {}: the server did not identify a file, folder or link",
                entry.name
            );
        };
        self.begin_edit(entry.parent.clone(), EditAction::Delete(entry, kind))?;
        self.status.clear();
        Ok(())
    }
}

impl LiveTab {
    pub(super) fn observe_file_connection(&mut self, generation: u64, active: bool) {
        self.files
            .observe_transfer_connection(&self.session, generation, active);
    }

    pub(super) fn follow_shell_directory(
        &mut self,
        frame: &w11term::terminal::Snapshot,
        context: &ComponentContext<TerminalApp>,
    ) {
        if !self.files.following || !self.files.open {
            return;
        }
        let Source::Ssh(ssh) = &self.source else {
            return;
        };
        self.files
            .observe_transfer_connection(&self.session, frame.connection_generation, true);
        if let Some(path) = self
            .files
            .follow_target(&frame.reported_directory, &ssh.host)
        {
            self.files.cached.borrow_mut().take();
            if let Err(error) =
                self.files
                    .browse(self.session.pane_id, &self.session, path, None, context)
            {
                self.files.status = format!("{error:#}");
            }
        }
    }
}

impl TerminalApp {
    pub(super) fn control_cancel_sftp_transfer(
        &mut self,
        transfer_id: TransferId,
    ) -> Result<Value> {
        let id = self
            .tab_for_pane(transfer_id.pane)
            .context("Unknown terminal pane")?;
        let active = self.tabs.active_id() == Some(id);
        let Some(Page::Terminal(tab)) = self.tabs.get_mut(id).map(|entry| &mut entry.page) else {
            bail!("Unknown terminal pane");
        };
        let generation = tab.session.frames.load().connection_generation;
        tab.observe_file_connection(generation, active);
        let previous = tab.files.transfer_stamp();
        let state = tab
            .files
            .cancel_transfer(transfer_id)
            .context("SFTP transfer is no longer active on that connection")?;
        tab.files.publish_ended_transfer(
            &tab.session,
            previous,
            !active || !tab.files.open,
            TransferOutcome::Cancelled,
        );
        Ok(serde_json::json!({ "transfer_id": transfer_id, "state": state }))
    }

    pub(super) fn browse_mounted_share(
        &mut self,
        authority: &Authority,
        path: &str,
        context: &ComponentContext<Self>,
    ) -> Result<()> {
        let pane = authority.pane;
        let id = self
            .tab_for_pane(pane)
            .context("The original SSH pane is closed")?;
        ensure!(
            self.folder_authority(pane).as_ref() == Some(authority),
            "The SSH connection has changed"
        );
        let active = self.tabs.active_id() == Some(id);
        let Some(AppTab {
            page: super::Page::Terminal(tab),
            ..
        }) = self.tabs.get_mut(id)
        else {
            bail!("The original SSH pane is closed");
        };
        let generation = tab.session.frames.load().connection_generation;
        ensure!(
            generation == authority.connection_generation(),
            "The SSH connection has changed"
        );
        tab.observe_file_connection(generation, active);
        let files = &mut tab.files;
        ensure!(
            !files.busy(),
            "The SFTP drawer is busy. Try again when the current operation finishes."
        );
        files.job(
            pane,
            &tab.session,
            Operation::ListMountedFolder { path: path.into() },
            Step::Listing(path.into(), None),
            context,
        )?;
        files.open = true;
        files.end_filter_edit();
        files.bookmarks = None;
        files.opening_target = None;
        files.following = false;
        files.reveal = None;
        files.directory = None;
        files.entries.clear();
        files.filter.clear();
        files.matches.clear();
        files.selected = None;
        files.marked_downloads = None;
        files.page = 0;
        files.status = "Reading directory…".into();
        files.cached.borrow_mut().take();
        self.tabs.select(id);
        self.initial_grid.set(None);
        crate::input::clear_grid();
        self.sync_selection(context);
        self.activate(context);
        Ok(())
    }

    pub(super) fn cancel_inactive_entry_dialog(&mut self) {
        let inactive = self
            .tabs
            .entries()
            .iter()
            .find_map(|entry| match &entry.value.page {
                Page::Terminal(tab)
                    if Some(entry.id) != self.tabs.active_id()
                        && (tab.files.entry_dialog_open()
                            || tab.files.identifying_delete()
                            || tab.files.filter_editing()) =>
                {
                    Some(entry.id)
                }
                _ => None,
            });
        if let Some(id) = inactive
            && let Some(AppTab {
                page: Page::Terminal(tab),
                ..
            }) = self.tabs.get_mut(id)
        {
            tab.files.cancel_entry_dialog();
            tab.files.end_filter_edit();
        }
    }

    pub(super) fn drawer_shell_eligible(&self, tab: &LiveTab, generation: u64) -> bool {
        tab.session.is_direct_ssh_shell()
            && tab.session.frames.load().connection_generation == generation
            && !self
                .outgoing
                .as_ref()
                .is_some_and(|transfer| self.tabs.active_group_id() == Some(transfer.group))
    }

    pub(super) fn with_file_drawer(&self, content: View, context: &ViewContext<Self>) -> View {
        let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.active()
        else {
            return content;
        };
        let Source::Ssh(ssh) = &tab.source else {
            return content;
        };
        if !tab.files.open {
            return content;
        }
        let connection = tab.session.ssh_connection_at(tab.files.generation);
        let bookmark_profile = tab.profile_name.as_deref().filter(|name| {
            self.catalog.profiles.get(*name).is_some_and(|saved| {
                connection.as_ref().is_some_and(|link| {
                    w11term::profiles::BookmarkPeer::connection_matches_profile(link, saved)
                })
            })
        });
        let shell_ready =
            connection.is_some() && self.drawer_shell_eligible(tab, tab.files.generation);
        let layout = DrawerLayout::for_window(self.width, self.window_height);
        Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Pixel(layout.width)])
            .keyed_children([
                KeyedView::new("terminal", Border::new().grid_column(0).content(content)),
                KeyedView::new(
                    "files",
                    Border::new()
                        .automation_id(format!("w11term-sftp-{}", tab.session.pane_id))
                        .grid_column(1)
                        .content(tab.files.panel(
                            tab.session.pane_id,
                            ssh,
                            bookmark_profile,
                            shell_ready,
                            layout,
                            context,
                        )),
                ),
            ])
    }

    pub(super) fn file_action(&mut self, action: FileAction, context: &ComponentContext<Self>) {
        use FileAction::*;
        let pane = match &action {
            CancelTransfer(target) => target.pane,
            Drop(target, ..) | PreparedDrop(target, ..) => target.pane,
            #[cfg(feature = "ui-smoke")]
            SmokeDrop(target, ..) => target.pane,
            Toggle | OpenHere => match self.tabs.active().and_then(AppTab::pane_id) {
                Some(id) => id,
                None => return,
            },
            ShowTransfer(id)
            | ShowSavedDownload(id, _)
            | Close(id)
            | Bookmarks(id, ..)
            | Search(id, ..)
            | Follow(id, ..)
            | Reveal(id, ..)
            | Browse(id, ..)
            | History(id, ..)
            | GoToFolder(id, ..)
            | OpenEntry(id, ..)
            | EditFilter(id, ..)
            | FocusFilter(id, ..)
            | Filter(id, ..)
            | ClearFilter(id, ..)
            | Sort(id, ..)
            | Choose(id, ..)
            | SelectDownloads(id, ..)
            | MarkDownload(id, ..)
            | CopyPath(id, ..)
            | Preview(id, ..)
            | ClosePreview(id, ..)
            | CopyChecksum(id, ..)
            | VerifyChecksum(id, ..)
            | StartTextEditor(id, ..)
            | ResumeTextEditor(id, ..)
            | TextEditor(id, ..)
            | Rename(id, ..)
            | VerifyDownload(id, ..)
            | Delete(id, ..)
            | NewFolder(id, ..)
            | EditName(id, ..)
            | ResumeDownload(id, ..)
            | FinishEdit(id, ..)
            | Page(id, ..)
            | Replace(id, ..)
            | PreserveMtime(id, ..)
            | AtomicUploads(id, ..)
            | PreserveUploadTimes(id, ..)
            | Pick(id, ..)
            | Picked(id, ..)
            | Listed(id, ..)
            | Tick(id) => *id,
        };
        let Some(id) = self.tab_for_pane(pane) else {
            return;
        };
        let visible = self.tabs.active_id() == Some(id);
        let transferring = self
            .outgoing
            .as_ref()
            .is_some_and(|transfer| self.tabs.group_id(id) == Some(transfer.group));
        let Some(AppTab {
            page: super::Page::Terminal(tab),
            ..
        }) = self.tabs.get_mut(id)
        else {
            return;
        };
        let Source::Ssh(ssh) = &tab.source else {
            return;
        };
        let session = &tab.session;
        let files = &mut tab.files;
        let frame = session.frames.load();
        let may_open_dialog = visible
            && !transferring
            && frame.prompt.is_none()
            && session.prompts.current().is_none()
            && self.title_editor.is_none()
            && self.picker.is_none()
            && self.switcher.is_none()
            && !self.clipboard.is_open();
        let dialog_was_open = files.entry_dialog_open();
        let drawer_was_open = files.open;
        let drop_allowed = may_open_dialog
            || (!visible
                && !transferring
                && frame.prompt.is_none()
                && session.prompts.current().is_none());
        let was_editing = files.filter_editing();
        let finish_filter =
            may_open_dialog && was_editing && matches!(action, EditFilter(_, _, _, false));
        files.observe_transfer_connection(session, frame.connection_generation, visible);
        if !matches!(
            action,
            Tick(_) | Picked(..) | Listed(..) | Drop(..) | PreparedDrop(..) | CancelTransfer(_)
        ) && !visible
        {
            return;
        }
        if matches!(&action, Preview(_, g, ..) | ClosePreview(_, g, ..) | CopyChecksum(_, g, ..) | VerifyChecksum(_, g, ..) | NewFolder(_, g, ..) | Choose(_, g, ..) | SelectDownloads(_, g, ..) | MarkDownload(_, g, ..) | Rename(_, g, ..) | VerifyDownload(_, g, ..) | Delete(_, g, ..) | EditName(_, g, ..) | ResumeDownload(_, g, ..) | FinishEdit(_, g, ..) | Follow(_, g, ..) | Reveal(_, g, ..) | Browse(_, g, ..) | GoToFolder(_, g, ..) | OpenEntry(_, g, ..) | Replace(_, g, ..) | PreserveMtime(_, g, ..) | AtomicUploads(_, g, ..) | PreserveUploadTimes(_, g, ..) | Pick(_, g, ..) | Picked(_, g, ..) | Listed(_, g, ..) | Filter(_, g, ..) | ClearFilter(_, g, ..) | FocusFilter(_, g, ..) | EditFilter(_, g, ..) | Page(_, g, ..) | Sort(_, g, ..) if *g != files.generation)
        {
            if visible
                && (dialog_was_open != files.entry_dialog_open()
                    || was_editing != files.filter_editing())
            {
                self.connection_ui(id);
            }
            return;
        }
        if matches!(&action, Filter(_, _, r, _) | ClearFilter(_, _, r) | FocusFilter(_, _, r, _) | EditFilter(_, _, r, _) | Page(_, _, r, _) | Sort(_, _, r, _) | SelectDownloads(_, _, r, _) if *r != files.listing_revision)
        {
            return;
        }
        if let Sort(_, _, _, order) = &action
            && (!may_open_dialog
                || !files.open
                || files.directory.is_none()
                || files.bookmarks.is_some()
                || files.busy()
                || *order == files.sort_order)
        {
            return;
        }
        let editor_typing = matches!(
            &action,
            TextEditor(_, _, editor::Action::Text(_) | editor::Action::Name(_))
        );
        let previous_transfer = if matches!(
            action,
            Tick(_) | Toggle | Close(_) | CancelTransfer(_) | PreparedDrop(..)
        ) {
            files.transfer_stamp()
        } else {
            None
        };
        let keep_directory = editor_typing
            || matches!(
                &action,
                Filter(..) | ClearFilter(..) | FocusFilter(..) | Sort(..) | ShowTransfer(..)
            );
        if !editor_typing && !matches!(action, Tick(_) | EditName(..) | FocusFilter(..)) {
            files.cached.borrow_mut().take();
        }
        let mut outcome = TransferOutcome::Failed;
        let result = (|| -> Result<()> {
            match action {
                Search(_, generation, action)
                    if files.open && may_open_dialog && generation == files.generation =>
                {
                    files.filename_search(pane, session, action, context)?;
                }
                Search(..) => {}
                Bookmarks(_, generation, action)
                    if files.open && generation == files.generation =>
                {
                    files.bookmark_action(
                        pane,
                        action,
                        tab.profile_name.as_deref(),
                        &self.store,
                        session,
                        context,
                    )?;
                }
                Bookmarks(..) => {}
                Toggle => {
                    if files.open {
                        files.close();
                        outcome = TransferOutcome::Cancelled;
                    } else {
                        files.open = true;
                    }
                    self.initial_grid.set(None);
                    crate::input::clear_grid();
                    if files.open && files.directory.is_none() && !files.busy() {
                        files.browse(pane, session, ".".into(), None, context)?;
                    }
                }
                ShowTransfer(_) => {
                    files.open = true;
                    files.bookmarks = None;
                    files.close_search();
                    self.initial_grid.set(None);
                    crate::input::clear_grid();
                }
                ShowSavedDownload(_, saved) => {
                    if may_open_dialog
                        && files.open
                        && !files.busy()
                        && files.last_download.as_ref() == Some(&saved)
                    {
                        explorer::show_download(saved, context);
                    }
                }
                OpenHere => {
                    files.bookmarks = None;
                    files.open = true;
                    self.initial_grid.set(None);
                    crate::input::clear_grid();
                    if !files.busy() {
                        let path = shell_directory(frame.reported_directory.as_deref(), &ssh.host)
                            .unwrap_or_else(|| ".".into());
                        files.browse(pane, session, path, None, context)?;
                    }
                }
                Close(_) => {
                    files.close();
                    outcome = TransferOutcome::Cancelled;
                }
                ClosePreview(_, _, serial) => files.close_preview(serial),
                CopyChecksum(_, _, serial) => {
                    if may_open_dialog {
                        files.copy_checksum(serial);
                    }
                }
                VerifyChecksum(_, _, serial) => {
                    if may_open_dialog {
                        files.verify_checksum(serial)?;
                    }
                }
                StartTextEditor(_, generation, serial) => {
                    if may_open_dialog && files.generation == generation {
                        files.start_editor(serial);
                    }
                }
                ResumeTextEditor(_, serial) => {
                    if may_open_dialog {
                        files.resume_editor(serial);
                    }
                }
                TextEditor(_, serial, action) => {
                    if may_open_dialog
                        && let Some((save, operation)) = files.editor_action(serial, action)
                        && let Err(error) =
                            files.job(pane, session, operation, Step::SaveText(save), context)
                        && let Some(editor) = &mut files.editor
                    {
                        editor.failed(format!("{error:#}"));
                    }
                }
                Follow(_, _, enabled) => {
                    if files.open && files.following != enabled {
                        files.following = enabled;
                        files.last_report = None;
                    }
                }
                Reveal(_, _, directory, name) => {
                    if !files.busy() {
                        files.bookmarks = None;
                        files.open = true;
                        files.following = false;
                        self.initial_grid.set(None);
                        crate::input::clear_grid();
                        files.browse(pane, session, directory, None, context)?;
                        files.reveal = name.map(RevealTarget::Existing);
                    }
                }
                Tick(_) => files.tick(pane, session, may_open_dialog, context, &mut outcome)?,
                CancelTransfer(target) => {
                    files.cancel_transfer(target);
                    outcome = TransferOutcome::Cancelled;
                }
                Listed(_, _, path, direction, result) if files.parsing => {
                    files.parsing = false;
                    files.accept_listing(direction, path, result.map_err(|e| anyhow!("{e}"))?);
                }
                PreparedDrop(target, token, result) => {
                    if files.finish_drop(&target, token) && drop_allowed {
                        files.queue = TransferQueue::dropped(
                            &target,
                            token,
                            result.map_err(anyhow::Error::msg)?,
                        )?;
                        files.transfer_next(pane, session, context)?;
                    } else if previous_transfer.is_some_and(|(id, _)| id == token)
                        && files.preparing_drop.is_none()
                    {
                        files.status = "Cancelled before uploading any dropped items.".into();
                        outcome = TransferOutcome::Cancelled;
                    }
                }
                Picked(_, _, serial, picker, result) => {
                    if !files.finish_picker(serial) || !may_open_dialog {
                        return Ok(());
                    }
                    let paths = result.map_err(|e| anyhow!("{e}"))?;
                    if paths.is_empty() {
                        return Ok(());
                    }
                    match picker {
                        TransferPicker::UploadFiles(parent) => {
                            ensure!(
                                files.directory.as_ref() == Some(&parent),
                                "The upload destination changed. Choose the files again."
                            );
                            let id = files.transfer_id(pane);
                            files.queue = TransferQueue::files(
                                paths,
                                &parent,
                                files.replace,
                                files.atomic_uploads,
                                files.preserve_upload_times,
                                id,
                            )?;
                            files.transfer_next(pane, session, context)?;
                        }
                        TransferPicker::UploadFolder(parent) => {
                            let [local] = paths
                                .try_into()
                                .map_err(|_| anyhow!("Choose one folder to upload"))?;
                            let (operation, transfer) =
                                files.prepare_folder_upload(&parent, local)?;
                            files.job(
                                pane,
                                session,
                                operation,
                                Step::Transfer(transfer),
                                context,
                            )?;
                        }
                        TransferPicker::Download(target, verification) => {
                            if matches!(target, FileTarget::Search(..)) {
                                ensure!(!files.busy(), "Another SFTP operation is active");
                            }
                            let [local] = paths
                                .try_into()
                                .map_err(|_| anyhow!("Choose one download destination"))?;
                            let (operation, transfer) =
                                files.download_operation(&target, local, verification)?;
                            files.status = format!("Downloading {}…", target.name());
                            files.job(
                                pane,
                                session,
                                operation,
                                Step::Transfer(transfer),
                                context,
                            )?;
                        }
                        TransferPicker::DownloadMarked => {
                            let [parent] = paths
                                .try_into()
                                .map_err(|_| anyhow!("Choose one download destination folder"))?;
                            let id = files.transfer_id(pane);
                            files.queue = files.download_queue(&parent, id)?;
                            files.marked_downloads = None;
                            files.transfer_next(pane, session, context)?;
                        }
                        TransferPicker::DownloadSearch(serial) => {
                            ensure!(!files.busy(), "Another SFTP operation is active");
                            let [local] = paths
                                .try_into()
                                .map_err(|_| anyhow!("Choose one new download folder"))?;
                            let selection = files.search_download_selection(serial)?;
                            let remote = selection.root().to_owned();
                            files.status = format!("Downloading marked results from {remote}…");
                            let operation = Operation::GetSearch {
                                selection,
                                local: local.clone(),
                                preserve_mtime: files.preserve_mtime,
                            };
                            files.job(
                                pane,
                                session,
                                operation,
                                Step::Transfer(TransferFile::DownloadFolder {
                                    remote,
                                    local,
                                    selected: true,
                                }),
                                context,
                            )?;
                        }
                    }
                }
                EditName(_, _, serial, name) => {
                    if let Some(dialog) = files.dialog.as_mut().filter(|e| {
                        e.serial == serial && !matches!(e.action, EditAction::Delete(..))
                    }) {
                        dialog.name = name;
                    }
                }
                ResumeDownload(_, _, serial, checked) => {
                    if let Some(dialog) = files.dialog.as_mut().filter(|e| {
                        e.serial == serial && matches!(e.action, EditAction::VerifyDownload(_))
                    }) {
                        dialog.resume_download = checked;
                    }
                }
                FinishEdit(_, _, serial, result) => {
                    if !may_open_dialog {
                        files.cancel_entry_dialog();
                        return Ok(());
                    }
                    if let Some((
                        EntryDialog {
                            parent,
                            name,
                            action,
                            resume_download,
                            ..
                        },
                        path,
                    )) = files.finish_edit(serial, result)?
                    {
                        let (change, operation) = match action {
                            EditAction::GoToFolder { .. } => {
                                return files.browse_named(
                                    pane,
                                    session,
                                    path,
                                    format!("folder {name}"),
                                    None,
                                    context,
                                );
                            }
                            EditAction::Delete(entry, kind) => {
                                files.status = format!("Deleting {}…", entry.name);
                                (
                                    EntryChange::Delete(entry.name),
                                    Operation::RemoveEntry { path, kind },
                                )
                            }
                            EditAction::VerifyDownload(target) => {
                                context.sender().send(Message::Files(Pick(
                                    pane,
                                    files.generation,
                                    files.listing_revision,
                                    TransferPicker::Download(
                                        target,
                                        Some(DownloadVerification {
                                            sha256: path,
                                            resume: resume_download,
                                        }),
                                    ),
                                )));
                                return Ok(());
                            }
                            EditAction::Rename(entry) => {
                                files.status = format!("Renaming {} to {name}…", entry.name);
                                let from = entry_path(&entry.parent, &entry.name)?;
                                (
                                    EntryChange::Rename {
                                        from: entry.name,
                                        to: name,
                                    },
                                    Operation::RenameEntry { from, to: path },
                                )
                            }
                            EditAction::Create => {
                                files.status = format!("Creating folder {name}…");
                                (
                                    EntryChange::CreateFolder(name),
                                    Operation::Mkdir {
                                        path,
                                        attrs: Default::default(),
                                    },
                                )
                            }
                        };
                        let parent = parent.context("Missing remote directory")?;
                        let deleting = matches!(change, EntryChange::Delete(_));
                        files.job(
                            pane,
                            session,
                            operation,
                            Step::Change { parent, change },
                            context,
                        )?;
                        if deleting {
                            files.listing_revision = files.listing_revision.wrapping_add(1);
                            files.selected = None;
                        }
                    }
                }
                Preview(_, _, target, kind) => {
                    if may_open_dialog
                        && files.open
                        && (!files.busy() || matches!(kind, PreviewKind::Checksum(_)))
                    {
                        let (serial, operation) = files.begin_preview(target, kind)?;
                        if let Err(error) =
                            files.job(pane, session, operation, Step::Preview(serial), context)
                        {
                            files.finish_preview(serial, Err(format!("{error:#}")));
                        }
                    }
                }
                _ if files.busy() || !files.open => {}
                EditFilter(_, _, revision, enabled) => {
                    if enabled && files.directory.is_some() && may_open_dialog {
                        if !files.filter_editing() {
                            files.filter_input = Some(ElementRef::new());
                            context.sender().send(Message::Files(FocusFilter(
                                pane,
                                files.generation,
                                revision,
                                0,
                            )));
                        }
                    } else {
                        files.end_filter_edit();
                    }
                }
                FocusFilter(_, generation, revision, attempt) => {
                    if let Some(input) = &files.filter_input {
                        let next = FocusFilter(pane, generation, revision, attempt + 1);
                        let sender = context.sender();
                        let retry = next.clone();
                        let queued = input.request_focus_result(move |r| {
                            if attempt < 4 && !r.unwrap_or(false) {
                                sender.send(Message::Files(retry.clone()));
                            }
                        });
                        if !queued && attempt < 4 {
                            context.spawn_background(move |_| {
                                std::thread::sleep(Duration::from_millis(20));
                                Message::Files(next)
                            });
                        }
                    }
                }
                Filter(_, _, _, query) => {
                    if files.filter_editing() {
                        files.set_filter(query);
                    }
                }
                ClearFilter(..) => files.set_filter(String::new()),
                Sort(_, _, _, order) => files.set_sort_order(order),
                History(_, generation, revision, direction) => {
                    if may_open_dialog
                        && let Some(path) = files
                            .history_target(generation, revision, direction)
                            .map(str::to_owned)
                    {
                        let description = format!("folder {path}");
                        files.browse_named(
                            pane,
                            session,
                            path,
                            description,
                            Some(direction),
                            context,
                        )?;
                    }
                }
                GoToFolder(_, _, revision) => {
                    if may_open_dialog {
                        files.begin_go_to_folder(revision);
                    }
                }
                NewFolder(_, _, parent)
                    if frame.prompt.is_none() && self.title_editor.is_none() =>
                {
                    files.end_filter_edit();
                    files.begin_edit(parent, EditAction::Create)?;
                }
                Rename(_, _, entry) if frame.prompt.is_none() && self.title_editor.is_none() => {
                    files.end_filter_edit();
                    files.begin_edit(entry.parent.clone(), EditAction::Rename(entry))?;
                }
                VerifyDownload(_, _, entry) => {
                    if !may_open_dialog {
                        return Ok(());
                    }
                    files.begin_verified_download(FileTarget::Listed(entry), String::new())?;
                }
                Delete(_, _, entry) if may_open_dialog => {
                    files.check_selected(&entry)?;
                    files.end_filter_edit();
                    let path = delete_path(&entry.parent, &entry.name)?;
                    match entry.kind {
                        ListedKind::Known(kind) => files
                            .begin_edit(entry.parent.clone(), EditAction::Delete(entry, kind))?,
                        ListedKind::Missing => {
                            files.status = format!("Checking {}…", entry.name);
                            files.job(
                                pane,
                                session,
                                Operation::Lstat { path },
                                Step::IdentifyDelete(entry),
                                context,
                            )?;
                        }
                        ListedKind::Unsupported => {
                            bail!("Could not delete {}: unsupported file type", entry.name)
                        }
                    }
                }
                NewFolder(..) | Rename(..) | Delete(..) => {}
                Browse(_, _, path) => {
                    if files.directory.as_ref() != Some(&path) {
                        files.following = false;
                    }
                    files.browse(pane, session, path, None, context)?;
                }
                Choose(_, _, entry) => {
                    if files.marked_downloads.is_none() && files.has_entry(&entry) {
                        files.selected = Some(entry.name);
                        files.end_filter_edit();
                    }
                }
                SelectDownloads(_, _, _, enabled) => {
                    if may_open_dialog
                        && files.open
                        && files.directory.is_some()
                        && !files.busy()
                        && enabled != files.marked_downloads.is_some()
                    {
                        files.selected = None;
                        files.end_filter_edit();
                        files.marked_downloads = enabled.then(BTreeSet::new);
                    }
                }
                MarkDownload(_, _, entry, checked) => {
                    if may_open_dialog {
                        files.set_download_mark(entry, checked);
                    }
                }
                CopyPath(_, generation, revision, entry) => {
                    if generation == files.generation
                        && may_open_dialog
                        && let Some(path) = files.path_to_copy(revision, entry.as_ref())
                    {
                        files.status = match w11term::clipboard::write(&path) {
                            Ok(()) if entry.is_some() => "Copied selected path".into(),
                            Ok(()) => "Copied directory path".into(),
                            Err(error) => format!("Could not copy path: {error}"),
                        };
                    }
                }
                OpenEntry(_, _, entry) => {
                    let path = files.selected_path(&entry)?;
                    files.following = false;
                    files.browse(pane, session, path, None, context)?;
                }
                Page(_, _, _, page) => {
                    files.end_filter_edit();
                    files.set_page(page);
                }
                Replace(_, _, replace) => files.replace = replace,
                PreserveMtime(_, _, enabled) => files.preserve_mtime = enabled,
                AtomicUploads(_, _, atomic) => files.atomic_uploads = atomic,
                PreserveUploadTimes(_, _, enabled) => files.preserve_upload_times = enabled,
                Drop(target, paths) => {
                    if drop_allowed {
                        files.classify_drop(target, paths, session, context);
                    }
                }
                #[cfg(feature = "ui-smoke")]
                SmokeDrop(target, fixture) => {
                    context.spawn_background(move |_| {
                        Message::Files(target.fixture(&fixture).unwrap_or_else(|error| {
                            eprintln!("Could not read smoke drop items: {error:#}");
                            target.data(DroppedData::Unsupported)
                        }))
                    });
                }
                Pick(_, _, revision, picker) => {
                    if !may_open_dialog
                        || (matches!(
                            picker,
                            TransferPicker::DownloadMarked
                                | TransferPicker::DownloadSearch(_)
                                | TransferPicker::Download(FileTarget::Search(..), _)
                        ) && files.busy())
                        || revision != files.listing_revision
                        || files.bookmarks.is_some()
                    {
                        return Ok(());
                    }
                    let purpose = picker.purpose(files)?;
                    let owner = crate::input::window().context("Window is not ready")?.0 as usize;
                    files.end_filter_edit();
                    let serial = files.start_picker();
                    let generation = files.generation;
                    let reject = Message::Files(Picked(
                        pane,
                        generation,
                        serial,
                        picker.clone(),
                        Err("File picker unavailable".into()),
                    ));
                    context.spawn_background_with_rejection(
                        move |_| {
                            let result = file_picker::pick_paths(owner, purpose)
                                .map_err(|error| format!("{error:#}"));
                            Message::Files(Picked(pane, generation, serial, picker, result))
                        },
                        reject,
                    );
                }
                Listed(..) => {}
            }
            Ok(())
        })();
        if let Err(error) = result {
            files.status = match (files.opening_target.take(), files.reveal.take()) {
                (Some(target), _) => format!("Could not open {target}: {error:#}"),
                (_, Some(RevealTarget::Changed(change))) => {
                    format!(
                        "{}, but refreshing the directory failed: {error:#}",
                        change.completed()
                    )
                }
                _ => format!("{error:#}"),
            };
            files.finish_transfer_queue();
            files.cached.borrow_mut().take();
        }
        files.publish_ended_transfer(session, previous_transfer, !visible || !files.open, outcome);
        if visible {
            let return_to_terminal = finish_filter && !tab.files.filter_editing();
            if !keep_directory {
                tab.follow_shell_directory(&frame, context);
            }
            if drawer_was_open != tab.files.open
                || dialog_was_open != tab.files.entry_dialog_open()
                || was_editing != tab.files.filter_editing()
            {
                self.connection_ui(id);
            }
            if return_to_terminal {
                self.keyboard.finish_drawer_edit();
            }
        }
    }
}

#[cfg(feature = "ui-smoke")]
impl TerminalApp {
    pub(super) fn file_smoke_step(&mut self, step: u8, context: &ComponentContext<Self>) {
        let local = PathBuf::from(
            std::env::var_os("W11TERM_FILE_SMOKE").expect("isolated transfer directory"),
        );
        assert!(local.is_absolute());
        let id = self
            .tabs
            .entries()
            .iter()
            .find(|e| e.value.profile_name() == Some("fixture"))
            .expect("fixture pane")
            .id;
        let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.get(id)
        else {
            panic!("expected SSH pane")
        };
        assert!(matches!(&tab.source, Source::Ssh(ssh) if ssh.host == "127.0.0.1"));
        let pane = tab.session.pane_id;
        let files = &tab.files;
        let status = files.status.clone();
        let upload = DropTarget::new(pane, files, files.directory.as_deref().unwrap_or("")).data(
            DroppedData::StorageItems(vec![windows_reactor::DroppedStorageItem {
                name: "upload.bin".into(),
                path: local.join("upload.bin").to_str().unwrap().into(),
            }]),
        );
        let next = match step {
            0 if tab.session.frames.load().reported_directory.is_none() => 0,
            0 => {
                let frame = tab.session.frames.load();
                let command = frame.command_history.back().expect("fixture shell status");
                assert_eq!(command.output.as_deref(), Ok("fixture status\r\n"));
                assert_eq!(command.exit_code, Some(7));
                assert_eq!(frame.copy_output_unavailable(), None);
                self.update(super::tab_menu_message("Command history".into()), context);
                self.history_smoke_select_older(context);
                8
            }
            8 => {
                self.history_smoke_verify_older();
                self.update(
                    Message::History(super::command_history::Action::InPane(
                        pane,
                        super::command_history::HistoryAction::Return,
                    )),
                    context,
                );
                assert_eq!(self.tabs.active_id(), Some(id));
                self.file_action(FileAction::Toggle, context);
                1
            }
            _ if files.busy() => step,
            1 => {
                assert_eq!(files.directory.as_deref(), Some("/"), "{status}");
                self.update(super::tab_menu_message("Open files here".into()), context);
                6
            }
            6 => {
                assert_eq!(
                    files.directory.as_deref(),
                    Some("/shell directory"),
                    "{status}"
                );
                self.file_action(upload, context);
                // The transfer should still finish on its original SSH channel after
                // a tab selection change. An ordinary demo tab owns a different transport.
                self.open("Other pane".into(), Source::Demo, None, context);
                2
            }
            2 => {
                assert!(status.starts_with("Uploaded upload.bin"), "{status}");
                assert!(files.entries.iter().any(|e| e.name == "upload.bin"));
                self.tabs.select(id);
                self.activate(context);
                self.file_action(upload, context);
                3
            }
            3 => {
                assert!(
                    status.contains("replacing files is off"),
                    "Expected overwrite refusal, got {status}"
                );
                let entry = ListedEntry {
                    parent: files.directory.clone().unwrap(),
                    revision: files.listing_revision,
                    name: "upload.bin".into(),
                    kind: files
                        .entries
                        .iter()
                        .find(|e| e.name == "upload.bin")
                        .unwrap()
                        .kind,
                };
                let Page::Terminal(tab) = &mut self.tabs.get_mut(id).unwrap().page else {
                    unreachable!()
                };
                let serial = tab.files.start_picker();
                tab.files.selected = Some(entry.name.clone());
                let generation = tab.files.generation;
                self.file_action(
                    FileAction::Picked(
                        pane,
                        generation,
                        serial,
                        TransferPicker::Download(FileTarget::Listed(entry), None),
                        Ok(vec![local.join("download.bin")]),
                    ),
                    context,
                );
                4
            }
            4 => {
                assert!(status.starts_with("Downloaded "), "{status}");
                let metrics = tab.surface.metrics.get();
                let (cols, rows) = (metrics.cols + 3, metrics.rows);
                context.spawn_background(move |_| {
                    let result = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap()
                        .block_on(w11term::control::call(
                            Some(std::process::id()),
                            &w11term::control::Request::Resize { pane, cols, rows },
                        ));
                    match result {
                        Ok(reply) if reply.ok => Message::Smoke(5),
                        other => {
                            eprintln!("Unable to resize with the drawer open: {other:?}");
                            Message::Smoke(u8::MAX)
                        }
                    }
                });
                return;
            }
            5 => {
                self.update(tab_menu_message("Keyboard hints".into()), context);
                assert!(self.hints.borrow().as_ref().is_some_and(|h| {
                    h.visible_labels()
                        .any(|item| item.label == "a" && item.target.text() == "src/trace.rs")
                }));
                use windows::Win32::{
                    Foundation::{LPARAM, WPARAM},
                    UI::WindowsAndMessaging::{PostMessageW, WM_KEYDOWN, WM_KEYUP},
                };
                let window = crate::input::window().unwrap();
                for &key in b"AR" {
                    for message in [WM_KEYDOWN, WM_KEYUP] {
                        unsafe {
                            PostMessageW(Some(window), message, WPARAM(key as usize), LPARAM(1))
                                .unwrap();
                        }
                    }
                }
                7
            }
            7 if files.directory.as_deref() != Some("/shell directory/src") => 7,
            7 => {
                assert_eq!(files.selected.as_deref(), Some("trace.rs"));
                assert!(self.hints.borrow().is_none());
                self.file_action(FileAction::Follow(pane, files.generation, true), context);
                9
            }
            9 => {
                assert_eq!(
                    files.directory.as_deref(),
                    Some("/shell directory"),
                    "{status}"
                );
                // The loopback shell echoes terminal output over the established SSH channel.
                let (ack, _) = oneshot::channel();
                tab.session
                    .input
                    .send(Command::Raw(
                        b"\x1b]7;file://127.0.0.1/shell%20directory/src\x07".to_vec(),
                        ack,
                    ))
                    .unwrap();
                10
            }
            10 if files.directory.as_deref() != Some("/shell directory/src") => 10,
            10 => {
                assert!(files.entries.iter().any(|entry| entry.name == "trace.rs"));
                self.file_action(
                    FileAction::Browse(pane, files.generation, "/".into()),
                    context,
                );
                11
            }
            11 => {
                assert_eq!(files.directory.as_deref(), Some("/"), "{status}");
                assert!(!files.following);
                let (ack, _) = oneshot::channel();
                tab.session
                    .input
                    .send(Command::Raw(
                        b"\x1b]7;file://127.0.0.1/shell%20directory\x07".to_vec(),
                        ack,
                    ))
                    .unwrap();
                12
            }
            12 if tab.session.frames.load().reported_directory.as_deref()
                != Some("file://127.0.0.1/shell%20directory") =>
            {
                12
            }
            12 => {
                assert_eq!(files.directory.as_deref(), Some("/"));
                let groups: Vec<_> = self.tabs.headers().into_iter().map(|h| h.id).collect();
                for group in groups {
                    self.close_tab(group, context);
                }
                eprintln!("File drawer smoke passed");
                return;
            }
            _ => panic!("Unknown file smoke step"),
        };
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_millis(120));
            Message::Smoke(next)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_sizes_round_to_short_binary_units_without_losing_zero() {
        for (size, label) in [
            (0, "0 B"),
            (1, "1 B"),
            (1023, "1023 B"),
            (1024, "1 KiB"),
            (1536, "1.5 KiB"),
            ((1 << 20) - 52, "1023.9 KiB"),
            ((1 << 20) - 51, "1 MiB"),
            (5 << 30, "5 GiB"),
            (u64::MAX, "16 EiB"),
        ] {
            assert_eq!(listing_size_label(size), label);
        }
    }
    use serde_json::json;

    fn listing() -> Vec<Entry> {
        (0..78)
            .map(|index| Entry {
                mtime: None,
                size: None,
                name: if index % 2 == 0 {
                    format!("{index:02} [Ångström] 雪")
                } else {
                    format!("{index:02} plain")
                },
                kind: ListedKind::Known(EntryKind::File),
            })
            .collect()
    }

    #[test]
    fn folder_history_uses_displayed_listings_and_current_connection() {
        use HistoryDirection::{Back, Forward};
        let mut drawer = FileDrawer {
            open: true,
            generation: 3,
            ..Default::default()
        };
        drawer.accept_listing(None, "/home".into(), vec![]);
        drawer.accept_listing(None, "/work".into(), vec![]);
        drawer.accept_listing(None, "/srv".into(), vec![]);
        let revision = drawer.listing_revision;
        assert_eq!(drawer.history_target(3, revision, Back), Some("/work"));
        assert_eq!(drawer.history_target(3, revision, Forward), None);
        assert_eq!(drawer.history_target(2, revision, Back), None);
        assert_eq!(drawer.history_target(3, revision - 1, Back), None);

        // Resolving/reading the target may still fail. Asking for it must
        // leave the current listing and the next Back destination intact.
        drawer.parsing = true;
        assert_eq!(drawer.history_target(3, revision, Back), None);
        drawer.parsing = false;
        assert_eq!(drawer.history_target(3, revision, Back), Some("/work"));
        assert_eq!(drawer.directory.as_deref(), Some("/srv"));
        drawer.accept_listing(Some(Back), "/work".into(), vec![]);
        drawer.accept_listing(None, "/work".into(), vec![]); // Refresh
        assert_eq!(drawer.history.target(Forward), Some("/srv"));
        assert_eq!(drawer.history.target(Back), Some("/home"));
        drawer.accept_listing(Some(Forward), "/srv".into(), vec![]);
        drawer.accept_listing(Some(Back), "/work".into(), vec![]);
        drawer.accept_listing(None, "/new".into(), vec![]);
        assert_eq!(drawer.history.target(Back), Some("/work"));
        assert_eq!(drawer.history.target(Forward), None);
        drawer.observe_connection(4);
        assert_eq!(drawer.history.target(Back), None);
        assert_eq!(drawer.history.target(Forward), None);
    }

    #[test]
    fn drawer_reserves_file_rows_after_short_window_resizes() {
        // 982x551 device pixels on a 150% display was the failing layout.
        let short = DrawerLayout::for_window(982.0 / 1.5, 551.0 / 1.5);
        let remaining = 551.0 / 1.5 - 48.0 - 2.0 * PANEL_PADDING - short.actions_max_height;
        assert!(remaining >= 3.0 * 32.0);
        assert!(short.width < 360.0);
        let tall = DrawerLayout::for_window(1200.0, 900.0);
        assert!(tall.actions_max_height > 600.0);
        assert_eq!(tall.content_width(), 340.0);
        let tiny = DrawerLayout::for_window(480.0, 200.0);
        assert!(tiny.actions_max_height > 32.0);
        assert!(200.0 - 48.0 - 2.0 * PANEL_PADDING - tiny.actions_max_height >= 32.0);
    }

    #[test]
    fn shell_here_binds_to_the_displayed_listing_and_leaves_browsing_state_alone() {
        let mut drawer = FileDrawer {
            open: true,
            generation: 3,
            following: true,
            ..Default::default()
        };
        assert!(drawer.shell_origin(17).is_none());
        let path = "/ops/shell's $(release);雪";
        drawer.accept_listing(None, path.into(), listing());
        drawer.set_filter("plain".into());
        drawer.set_page(1);
        let selected = drawer.page_entries().next().unwrap().name.clone();
        drawer.selected = Some(selected.clone());
        let target = drawer.shell_origin(17).unwrap();
        assert_eq!(
            target,
            directory_split::Origin::listing(17, 3, path, drawer.listing_revision)
        );
        assert_ne!(Some(&target), drawer.shell_origin(18).as_ref());
        assert!(drawer.following);
        assert_eq!(drawer.filter, "plain");
        assert_eq!(drawer.page, 1);
        assert_eq!(drawer.selected.as_ref(), Some(&selected));

        drawer.parsing = true;
        assert!(drawer.shell_origin(17).is_none());
        drawer.parsing = false;
        drawer.accept_listing(None, path.into(), vec![]);
        assert_ne!(drawer.shell_origin(17).unwrap(), target);
        drawer.open = false;
        assert!(drawer.shell_origin(17).is_none());
        drawer.open = true;
        drawer.observe_connection(4);
        assert!(drawer.shell_origin(17).is_none());
        drawer.accept_listing(None, path.into(), vec![]);
        assert_ne!(drawer.shell_origin(17).unwrap(), target);
        for invalid in [
            "relative",
            "//host/folder",
            "/tmp/../etc",
            "/tmp/line\nbreak",
        ] {
            drawer.accept_listing(None, invalid.into(), vec![]);
            assert!(drawer.shell_origin(17).is_none(), "{invalid:?}");
        }
    }

    #[test]
    fn filename_filter_searches_every_retained_page_in_order_and_clamps_pages() {
        let mut drawer = FileDrawer::default();
        drawer.accept_listing(None, "/tmp".into(), listing());
        let revision = drawer.listing_revision;
        drawer.set_page(2);
        drawer.set_filter(" [ÅNGSTRÖM] ".into());
        assert_eq!(drawer.page, 0);
        assert_eq!(drawer.matching_entries().len(), 39);
        assert_eq!(drawer.page_entries().count(), 32);
        assert_eq!(
            drawer.page_entries().last().unwrap().name,
            "62 [Ångström] 雪"
        );
        drawer.set_page(usize::MAX);
        assert_eq!(drawer.page, 1);
        assert_eq!(drawer.page_entries().count(), 7);
        assert_eq!(
            drawer.page_entries().next().unwrap().name,
            "64 [Ångström] 雪"
        );
        drawer.set_filter("[.*]".into());
        drawer.set_page(usize::MAX);
        assert_eq!(drawer.page, 0);
        assert_eq!(drawer.matching_entries().len(), 0);
        drawer.set_filter(String::new());
        assert!(drawer.matching_entries().eq(listing().iter()));
        assert_eq!(drawer.listing_revision, revision);
        assert!(!drawer.busy());
    }

    #[test]
    fn copy_path_uses_the_displayed_directory_and_literal_selected_name() {
        let mut drawer = FileDrawer {
            open: true,
            ..Default::default()
        };
        assert_eq!(drawer.path_to_copy(0, None), None);
        let name = "Å folder's [logs] $x";
        let kind = ListedKind::Known(EntryKind::Symlink);
        drawer.accept_listing(
            None,
            "/".into(),
            vec![Entry {
                name: name.into(),
                kind,
                mtime: None,
                size: None,
            }],
        );
        let revision = drawer.listing_revision;
        let listed = ListedEntry {
            parent: "/".into(),
            revision,
            name: name.into(),
            kind,
        };
        drawer.selected = Some(name.into());
        assert_eq!(drawer.path_to_copy(revision, None).as_deref(), Some("/"));
        assert_eq!(
            drawer.path_to_copy(revision, Some(&listed)),
            Some(format!("/{name}"))
        );
        drawer.set_filter("other".into());
        assert!(drawer.path_to_copy(revision, Some(&listed)).is_none());
        drawer.accept_listing(None, "/new dir".into(), vec![]);
        assert!(drawer.path_to_copy(revision, None).is_none());
        assert_eq!(
            drawer
                .path_to_copy(drawer.listing_revision, None)
                .as_deref(),
            Some("/new dir")
        );
        drawer.open = false;
        assert!(drawer.path_to_copy(drawer.listing_revision, None).is_none());
    }

    #[test]
    fn filter_rejects_hidden_rows_and_invalidates_selected_actions() {
        let mut drawer = FileDrawer {
            open: true,
            ..Default::default()
        };
        drawer.accept_listing(None, "/tmp".into(), listing());
        let entry = ListedEntry {
            parent: "/tmp".into(),
            revision: drawer.listing_revision,
            name: "01 plain".into(),
            kind: ListedKind::Known(EntryKind::File),
        };
        drawer.selected = Some(entry.name.clone());
        drawer.set_filter("PLAIN".into());
        assert!(drawer.check_selected(&entry).is_ok());
        drawer.set_filter("雪".into());
        assert!(drawer.selected.is_none());
        assert!(!drawer.has_entry(&entry));
        assert!(drawer.selected_path(&entry).is_err());
        assert!(
            drawer
                .begin_edit(entry.parent.clone(), EditAction::Rename(entry.clone()))
                .is_err()
        );
        assert!(
            drawer
                .begin_edit(
                    entry.parent.clone(),
                    EditAction::Delete(entry.clone(), EntryKind::File)
                )
                .is_err()
        );
        drawer.set_filter("plain".into());
        assert!(drawer.has_entry(&entry));
        assert!(drawer.selected_path(&entry).is_err());
        drawer.set_page(1);
        assert!(!drawer.has_entry(&entry));
    }

    #[test]
    fn filter_follows_canonical_directory_and_explicit_reveals_use_the_right_page() {
        let mut drawer = FileDrawer {
            open: true,
            ..Default::default()
        };
        drawer.accept_listing(None, "/tmp".into(), listing());
        drawer.set_filter("雪".into());
        drawer.reveal = Some(RevealTarget::Existing("70 [Ångström] 雪".into()));
        drawer.accept_listing(None, "/tmp".into(), listing());
        assert_eq!(drawer.filter, "雪");
        assert_eq!(drawer.page, 1);
        assert_eq!(drawer.selected.as_deref(), Some("70 [Ångström] 雪"));
        drawer.reveal = Some(RevealTarget::Changed(EntryChange::Rename {
            from: "old".into(),
            to: "65 plain".into(),
        }));
        drawer.accept_listing(None, "/tmp".into(), listing());
        assert!(drawer.filter.is_empty());
        assert_eq!(drawer.page, 2);
        assert_eq!(drawer.selected.as_deref(), Some("65 plain"));
        drawer.set_filter("plain".into());
        drawer.accept_listing(None, "/tmp/sub".into(), listing());
        assert!(drawer.filter.is_empty());
        assert!(drawer.selected.is_none());
        assert_eq!(drawer.page, 0);
        drawer.filter_input = Some(ElementRef::new());
        drawer.set_filter("snow".into());
        drawer.observe_connection(17);
        assert!(drawer.filter.is_empty());
        assert!(!drawer.filter_editing());
    }

    #[test]
    fn both_directory_read_steps_stay_bound_to_the_connection_that_supplied_the_path() {
        let path = "/srv/logs".to_owned();
        for request in [
            Operation::Realpath { path: path.clone() },
            Operation::Ls { path: path.clone() },
        ] {
            assert_eq!(request_generation(&request, 5), Some(5));
            assert_eq!(request_generation(&request, 0), None);
        }
        assert_eq!(
            request_generation(&Operation::ListMountedFolder { path }, 0),
            Some(0)
        );
    }

    #[test]
    fn follow_coalesces_reports_while_busy_and_ignores_stale_or_foreign_reports() {
        let mut drawer = FileDrawer {
            following: true,
            open: true,
            directory: Some("/a".into()),
            ..Default::default()
        };
        let report = |s: &str| Some(Arc::<str>::from(s));
        assert!(
            drawer
                .follow_target(&report("file://peer/a"), "peer")
                .is_none()
        );
        let serial = drawer.start_picker();
        assert!(
            drawer
                .follow_target(&report("file://peer/b"), "peer")
                .is_none()
        );
        assert!(drawer.finish_picker(serial));
        let latest = report("file://peer/c");
        assert_eq!(drawer.follow_target(&latest, "peer").as_deref(), Some("/c"));
        assert!(drawer.follow_target(&latest, "peer").is_none());
        assert!(
            drawer
                .follow_target(&report("file://other/d"), "peer")
                .is_none()
        );
        drawer.open = false;
        assert!(
            drawer
                .follow_target(&report("file://peer/z"), "peer")
                .is_none()
        );
        drawer.open = true;
        drawer.observe_connection(2);
        assert!(drawer.following);
        assert_eq!(drawer.follow_target(&latest, "peer").as_deref(), Some("/c"));
    }

    #[test]
    fn shell_directory_uses_only_an_absolute_path_on_this_ssh_host() {
        let path = |url| shell_directory(Some(url), "Host.Example");
        assert_eq!(
            path("file://HOST.EXAMPLE/home/%E6%97%A5%20A+B").as_deref(),
            Some("/home/日 A+B")
        );
        assert_eq!(path("file:///").as_deref(), Some("/"));
        assert_eq!(
            shell_directory(Some("file://[::1]/src"), "::1").as_deref(),
            Some("/src")
        );
        assert!(shell_directory(None, "host.example").is_none());
        for url in [
            "file://elsewhere/tmp",
            "file://localhost/tmp",
            "file://host.example:22/tmp",
            "file://user@host.example/tmp",
            "https://host.example/tmp",
            "file:relative",
            "file:////share",
            "file:///a/%2E%2e/b",
            "file:///%00",
            "file:///%7f",
            "file:///%2f",
            "file:///%5c",
            "file:///one?two",
            "file:///one#two",
            "file:///%",
            "file:///%FF",
        ] {
            assert!(path(url).is_none(), "{url}");
        }
    }

    #[test]
    fn iterm_report_preserves_posix_paths_when_opening_sftp() {
        let (writer, _) = tokio::sync::mpsc::unbounded_channel();
        let mut term = w11term::terminal::create(Default::default(), writer);
        term.enable_iterm_directory_reports("pilot", "host.example");
        term.advance_bytes(b"\x1b]1337;RemoteHost=pilot@host.example\x07");
        for path in ["/", "/C|/project", "/srv/100%2F;#雪", "/C:/a file?"] {
            term.advance_bytes(format!("\x1b]1337;CurrentDir={path}\x07"));
            let report = term.reported_current_dir().map(AsRef::as_ref);
            assert_eq!(
                shell_directory(report, "host.example").as_deref(),
                Some(path)
            );
        }
    }

    #[test]
    fn remote_paths_and_directory_types_use_sftp_semantics() {
        let listing = json!({"entries":[
            {"filename":"link", "attrs":{"permissions": 0o120777}}, {"filename":".", "attrs":{}},
            {"filename":"folder", "attrs":{"permissions":0o040755}}, {"filename":"file", "attrs":{"permissions":0o100644}},
        ]});
        let entries = directory_entries(&listing, SortOrder::Name).unwrap();
        assert_eq!(
            (&*entries[0].name, entries[0].kind),
            ("folder", ListedKind::Known(EntryKind::Directory))
        );
        assert_eq!(entries[1].kind, ListedKind::Known(EntryKind::File));
        assert_eq!(entries[2].kind, ListedKind::Known(EntryKind::Symlink));
        assert_eq!(remote_child("/", "résumé.txt").unwrap(), "/résumé.txt");
        assert_eq!(remote_child("/home/user/", "a").unwrap(), "/home/user/a");
        for invalid in ["", ".", "..", "a/b", "bad\0name"] {
            assert!(remote_child("/", invalid).is_err());
        }
    }

    #[test]
    fn new_folder_is_one_remote_component_and_preserves_unicode_and_spaces() {
        assert_eq!(
            entry_path("/work/", " Résumé 2026 ").unwrap(),
            "/work/ Résumé 2026 "
        );
        for name in [
            "",
            " \t ",
            ".",
            "..",
            "a/b",
            "/abs",
            r"a\b",
            r"..\escape",
            "bad\0name",
            "line\nbreak",
        ] {
            assert!(entry_path("/work", name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn go_to_folder_uses_literal_paths_and_leaves_resolution_to_sftp() {
        let mut drawer = FileDrawer {
            open: true,
            generation: 4,
            ..Default::default()
        };
        drawer.accept_listing(None, "/work/チーム".into(), listing());
        drawer.set_filter("plain".into());
        drawer.set_page(1);
        let selected = drawer.page_entries().next().unwrap().name.clone();
        drawer.selected = Some(selected.clone());
        let revision = drawer.listing_revision;
        drawer.begin_go_to_folder(revision - 1);
        assert!(drawer.dialog.is_none());
        for (entered, expected) in [
            ("/absolute/雪 $HOME/", "/absolute/雪 $HOME/"),
            ("//path//as/typed", "//path//as/typed"),
            ("../a/./b", "/work/チーム/../a/./b"),
            (" spaced ", "/work/チーム/ spaced "),
            ("~/$(cmd);*\\name", "/work/チーム/~/$(cmd);*\\name"),
            (".", "/work/チーム/."),
        ] {
            drawer.begin_go_to_folder(revision);
            drawer.dialog.as_mut().unwrap().name = entered.into();
            let (_, path) = drawer
                .finish_edit(drawer.dialog_serial, ContentDialogResult::Primary)
                .unwrap()
                .unwrap();
            assert_eq!(path, expected);
            assert_eq!(drawer.directory.as_deref(), Some("/work/チーム"));
            assert_eq!(drawer.listing_revision, revision);
            assert_eq!(drawer.selected.as_ref(), Some(&selected));
            assert_eq!(drawer.filter, "plain");
            assert_eq!(drawer.page, 1);
        }
        drawer.directory = Some("/".into());
        drawer.begin_go_to_folder(revision);
        drawer.dialog.as_mut().unwrap().name = "entry".into();
        assert_eq!(
            drawer.dialog.as_ref().unwrap().destination().unwrap(),
            "/entry"
        );
    }

    #[test]
    fn go_to_folder_recovers_an_unlisted_home_and_rejects_stale_dialogs() {
        let mut drawer = FileDrawer {
            open: true,
            ..Default::default()
        };
        drawer.begin_go_to_folder(0);
        assert!(drawer.dialog.is_none());
        drawer.observe_connection(1);
        drawer.begin_go_to_folder(0);
        let dialog = drawer.dialog.as_mut().unwrap();
        assert!(dialog.destination().is_err());
        dialog.name = "/bad\0path".into();
        assert!(dialog.destination().is_err());
        dialog.name = "../relative".into();
        assert!(dialog.destination().is_err());
        dialog.name = "/recovery".into();
        assert_eq!(dialog.destination().unwrap(), "/recovery");
        let cancelled = dialog.serial;
        assert!(
            drawer
                .finish_edit(cancelled, ContentDialogResult::None)
                .unwrap()
                .is_none()
        );
        drawer.begin_go_to_folder(0);
        assert!(
            drawer
                .finish_edit(cancelled, ContentDialogResult::Primary)
                .unwrap()
                .is_none()
        );
        drawer.dialog.as_mut().unwrap().name = "/recovery".into();
        let (_, path) = drawer
            .finish_edit(drawer.dialog_serial, ContentDialogResult::Primary)
            .unwrap()
            .unwrap();
        assert_eq!(path, "/recovery");
        drawer.accept_listing(None, path, listing());
        let revision = drawer.listing_revision;
        drawer.begin_go_to_folder(revision);
        drawer.dialog.as_mut().unwrap().name = ".".into();
        drawer.accept_listing(None, "/recovery".into(), Vec::new());
        assert!(
            drawer
                .finish_edit(drawer.dialog_serial, ContentDialogResult::Primary)
                .is_err()
        );
        drawer.begin_go_to_folder(drawer.listing_revision);
        let obsolete = drawer.dialog_serial;
        drawer.observe_connection(2);
        assert!(
            drawer
                .finish_edit(obsolete, ContentDialogResult::Primary)
                .unwrap()
                .is_none()
        );
        assert!(drawer.directory.is_none());
    }

    #[test]
    fn folder_dialog_captures_parent_and_defers_follow_until_its_own_completion() {
        let mut drawer = FileDrawer {
            open: true,
            generation: 4,
            following: true,
            directory: Some("/work".into()),
            ..Default::default()
        };
        assert!(
            drawer
                .begin_edit("/obsolete".into(), EditAction::Create)
                .is_err()
        );
        drawer
            .begin_edit("/work".into(), EditAction::Create)
            .unwrap();
        let first = drawer.dialog_serial;
        let changed = Some(Arc::from("file://server/new"));
        assert!(drawer.follow_target(&changed, "server").is_none());
        drawer.cancel_entry_dialog();
        drawer
            .begin_edit("/work".into(), EditAction::Create)
            .unwrap();
        let current = drawer.dialog_serial;
        drawer.dialog.as_mut().unwrap().name = "prepared".into();
        assert!(
            drawer
                .finish_edit(first, ContentDialogResult::Primary)
                .unwrap()
                .is_none()
        );
        assert!(drawer.entry_dialog_open());
        let (dialog, path) = drawer
            .finish_edit(current, ContentDialogResult::Primary)
            .unwrap()
            .unwrap();
        assert_eq!(
            (dialog.parent.as_deref(), &*dialog.name, &*path),
            (Some("/work"), "prepared", "/work/prepared")
        );
        assert!(
            drawer
                .finish_edit(current, ContentDialogResult::Primary)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            drawer.follow_target(&changed, "server").as_deref(),
            Some("/new")
        );
        drawer
            .begin_edit("/work".into(), EditAction::Create)
            .unwrap();
        drawer.directory = Some("/other".into());
        assert!(
            drawer
                .finish_edit(drawer.dialog_serial, ContentDialogResult::Primary)
                .is_err()
        );
        drawer
            .begin_edit("/other".into(), EditAction::Create)
            .unwrap();
        drawer.observe_connection(5);
        assert!(
            drawer
                .finish_edit(drawer.dialog_serial, ContentDialogResult::Primary)
                .unwrap()
                .is_none()
        );
        assert!(drawer.directory.is_none());
    }

    #[test]
    fn rename_is_bound_to_the_listed_source_until_confirmed() {
        let source = ListedEntry {
            parent: "/work".into(),
            revision: 1,
            name: "alias".into(),
            kind: ListedKind::Known(EntryKind::Symlink),
        };
        let mut drawer = FileDrawer {
            open: true,
            following: true,
            directory: Some(source.parent.clone()),
            listing_revision: source.revision,
            entries: vec![Entry {
                name: source.name.clone(),
                kind: source.kind,
                mtime: None,
                size: None,
            }],
            selected: Some(source.name.clone()),
            ..Default::default()
        };
        let begin = |drawer: &mut FileDrawer| {
            drawer.begin_edit(source.parent.clone(), EditAction::Rename(source.clone()))
        };
        begin(&mut drawer).unwrap();
        assert_eq!(drawer.dialog.as_ref().unwrap().name, source.name);
        assert!(drawer.dialog.as_ref().unwrap().destination().is_err()); // unchanged
        assert!(
            drawer
                .follow_target(&Some(Arc::from("file://host/elsewhere")), "host")
                .is_none()
        );
        drawer.dialog.as_mut().unwrap().name = "new name 雪".into();
        let old = drawer.dialog_serial;
        assert!(
            drawer
                .finish_edit(old, ContentDialogResult::None)
                .unwrap()
                .is_none()
        );
        begin(&mut drawer).unwrap();
        let current = drawer.dialog_serial;
        assert!(
            drawer
                .finish_edit(old, ContentDialogResult::Primary)
                .unwrap()
                .is_none()
        );
        drawer.dialog.as_mut().unwrap().name = "new name 雪".into();
        let (dialog, path) = drawer
            .finish_edit(current, ContentDialogResult::Primary)
            .unwrap()
            .unwrap();
        assert_eq!(path, "/work/new name 雪");
        assert_eq!(dialog.action.original().unwrap().name, "alias");
        assert!(
            drawer
                .finish_edit(current, ContentDialogResult::Primary)
                .unwrap()
                .is_none()
        );

        begin(&mut drawer).unwrap();
        drawer.dialog.as_mut().unwrap().name = "new".into();
        drawer.listing_revision += 1; // same parent and filename in a replacement listing
        assert!(
            drawer
                .finish_edit(drawer.dialog_serial, ContentDialogResult::Primary)
                .is_err()
        );
        assert!(begin(&mut drawer).is_err());
        drawer.listing_revision = source.revision;
        drawer.selected = None;
        assert!(begin(&mut drawer).is_err());
        drawer.selected = Some(source.name.clone());
        begin(&mut drawer).unwrap();
        drawer.observe_connection(2);
        assert!(
            drawer
                .finish_edit(current, ContentDialogResult::Primary)
                .unwrap()
                .is_none()
        );
        assert!(begin(&mut drawer).is_err());
    }

    #[test]
    fn rename_preserves_a_known_result_while_the_connection_changes() {
        let change = EntryChange::Rename {
            from: "old".into(),
            to: "new".into(),
        };
        let (pending, mut reply) = oneshot::channel();
        assert!(change_reconnect_status(&change, &mut reply).contains("outcome is unknown"));
        pending.send(Ok(json!({}))).unwrap();
        assert!(change_reconnect_status(&change, &mut reply).starts_with("Renamed old to new"));
        let (pending, mut reply) = oneshot::channel();
        pending
            .send(Err("outcome unknown; refresh".into()))
            .unwrap();
        assert_eq!(
            change_reconnect_status(&change, &mut reply),
            "Rename old to new: outcome unknown; refresh"
        );
        let (pending, mut reply) = oneshot::channel();
        drop(pending);
        assert!(
            change_reconnect_status(&change, &mut reply)
                .contains("before rename of old to new started")
        );
        let mut drawer = FileDrawer {
            reveal: Some(RevealTarget::Changed(change)),
            ..Default::default()
        };
        drawer.observe_connection(2);
        assert!(
            drawer
                .status
                .starts_with("Renamed old to new before SSH reconnected")
        );
    }

    #[test]
    fn deletion_requires_a_canonical_parent_and_a_single_safe_name() {
        for parent in [
            "",
            ".",
            "relative",
            "//server/share",
            "/one/../two",
            "/one/./two",
            "/one//two",
            "/bad\npath",
            r"/one\two",
        ] {
            assert!(delete_path(parent, "child").is_err(), "{parent:?}");
        }
        for name in [".", "..", "one/two", r"..\outside", "bad\0name"] {
            assert!(delete_path("/home/user", name).is_err(), "{name:?}");
        }
        assert_eq!(delete_path("/", "entry").unwrap(), "/entry");
        assert_eq!(
            delete_path("/work 雪", "résumé").unwrap(),
            "/work 雪/résumé"
        );
    }

    #[test]
    fn deletion_resolves_only_identified_types_for_the_same_selected_entry() {
        let attrs = |mode| json!({"permissions": mode});
        for (mode, expected) in [
            (None, ListedKind::Missing),
            (Some(0o644), ListedKind::Missing),
            (Some(0o010600), ListedKind::Unsupported),
            (Some(0o100600), ListedKind::Known(EntryKind::File)),
            (Some(0o040700), ListedKind::Known(EntryKind::Directory)),
            (Some(0o120777), ListedKind::Known(EntryKind::Symlink)),
        ] {
            assert_eq!(ListedKind::from_attrs(&attrs(mode)), expected);
        }
        let entry = ListedEntry {
            parent: "/work".into(),
            name: "unknown".into(),
            kind: ListedKind::Missing,
            revision: 3,
        };
        let mut drawer = FileDrawer {
            open: true,
            directory: Some(entry.parent.clone()),
            generation: 2,
            listing_revision: entry.revision,
            selected: Some(entry.name.clone()),
            entries: vec![Entry {
                name: entry.name.clone(),
                kind: entry.kind,
                mtime: None,
                size: None,
            }],
            ..Default::default()
        };
        for unsupported in [None, Some(0o644), Some(0o060640)] {
            assert!(
                drawer
                    .confirm_identified(entry.clone(), &attrs(unsupported))
                    .is_err()
            );
            assert!(!drawer.entry_dialog_open());
        }
        let link = attrs(Some(0o120777));
        drawer.listing_revision += 1;
        assert!(drawer.confirm_identified(entry.clone(), &link).is_err());
        drawer.listing_revision = entry.revision;
        drawer.selected = None;
        assert!(drawer.confirm_identified(entry.clone(), &link).is_err());
        drawer.selected = Some(entry.name.clone());
        drawer.confirm_identified(entry.clone(), &link).unwrap();
        let old = drawer.dialog_serial;
        drawer.cancel_entry_dialog();
        drawer.confirm_identified(entry.clone(), &link).unwrap();
        assert!(
            drawer
                .finish_edit(old, ContentDialogResult::Primary)
                .unwrap()
                .is_none()
        );
        let serial = drawer.dialog_serial;
        drawer.selected = None;
        assert!(
            drawer
                .finish_edit(serial, ContentDialogResult::Primary)
                .is_err()
        );
        drawer.selected = Some(entry.name.clone());
        drawer.confirm_identified(entry.clone(), &link).unwrap();
        let (dialog, path) = drawer
            .finish_edit(drawer.dialog_serial, ContentDialogResult::Primary)
            .unwrap()
            .unwrap();
        assert_eq!(path, "/work/unknown");
        assert!(matches!(
            dialog.action,
            EditAction::Delete(_, EntryKind::Symlink)
        ));
        assert!(
            drawer
                .finish_edit(drawer.dialog_serial, ContentDialogResult::Primary)
                .unwrap()
                .is_none()
        );

        drawer.confirm_identified(entry, &link).unwrap();
        let serial = drawer.dialog_serial;
        drawer.observe_connection(3);
        assert!(
            drawer
                .finish_edit(serial, ContentDialogResult::Primary)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn completed_deletion_leaves_a_replacement_unselected_and_preserves_success() {
        let change = EntryChange::Delete("alias".into());
        let mut drawer = FileDrawer {
            status: change.completed(),
            reveal: Some(RevealTarget::Changed(change)),
            selected: Some("alias".into()),
            listing_revision: 3,
            page: 1,
            ..Default::default()
        };
        drawer.accept_listing(
            None,
            "/work".into(),
            vec![Entry {
                name: "alias".into(),
                kind: ListedKind::Known(EntryKind::File),
                mtime: None,
                size: None,
            }],
        );
        assert_eq!(drawer.status, "Deleted alias");
        assert_eq!(drawer.selected, None);
        assert!(drawer.reveal.is_none());
        assert_eq!(drawer.page, 0);
        assert_eq!(drawer.listing_revision, 4);
    }

    #[test]
    fn cancel_targets_one_transfer_and_discards_its_remaining_upload_queue() {
        let id = TransferId {
            pane: 4,
            generation: 2,
            serial: 7,
        };
        let active = ActiveTransfer {
            id,
            cancel: TransferCancel::default(),
            progress: watch::channel(TransferProgress::default()).1,
        };
        let worker = active.cancel.clone();
        let mut queue = TransferQueue::files(
            vec![r"C:\second.txt".into(), r"C:\third.txt".into()],
            "/",
            false,
            false,
            false,
            id,
        )
        .unwrap();
        for stale in [
            TransferId { pane: 5, ..id },
            TransferId {
                generation: 1,
                ..id
            },
            TransferId { serial: 6, ..id },
        ] {
            assert!(!active.cancel(stale, &mut queue));
            assert_eq!(queue.remaining.len(), 2);
            assert!(!worker.is_cancelled());
        }
        assert!(active.cancel(id, &mut queue));
        assert!(queue.remaining.is_empty());
        assert!(worker.is_cancelled());
        assert!(!active.cancel(id, &mut queue));
    }

    #[test]
    fn transfer_status_respects_completion_cancellation_and_uncertain_remote_results() {
        let upload = TransferFile::Upload {
            name: "large.bin".into(),
            remote: "/srv/uploads/large.bin".into(),
            atomic: false,
        };
        let download = TransferFile::Download {
            remote: "/large.bin".into(),
            resume: false,
            local: r"C:\download\large.bin".into(),
        };
        let complete = json!({"bytes_transferred": 128});
        let cancelled = json!({"cancelled": true, "remote": "/srv/uploads/large.bin", "bytes_transferred": 64, "remote_may_be_partial": true});
        let unopened = json!({"cancelled": true, "remote": "/srv/uploads/large.bin", "bytes_transferred": 0, "remote_may_be_partial": false});
        assert_eq!(upload.outcome(&complete), "Uploaded large.bin (128 bytes)");
        assert_eq!(
            download.outcome(&complete),
            "Downloaded /large.bin (128 bytes)"
        );
        assert!(upload.outcome(&cancelled).contains("64 bytes acknowledged"));
        assert!(
            upload
                .outcome(&cancelled)
                .contains("remote file may be incomplete")
        );
        assert!(
            upload
                .outcome(&unopened)
                .contains("before opening the remote file")
        );
        let message = download.outcome(&cancelled);
        assert!(message.contains("64 bytes written to the temporary file"));

        let (worker, mut reply) = oneshot::channel();
        let (interrupted, saved, outcome) =
            upload.reconnect_status(&mut reply, 32, &mut TransferQueue::default());
        assert!(saved.is_none());
        assert_eq!(outcome, TransferOutcome::Interrupted);
        assert!(interrupted.contains("/srv/uploads/large.bin"));
        assert!(interrupted.contains("at least 32 bytes acknowledged"));
        assert!(interrupted.contains("remote file may be incomplete"));
        worker.send(Ok(cancelled)).unwrap();
        assert!(
            upload
                .reconnect_status(&mut reply, 32, &mut TransferQueue::default())
                .0
                .starts_with("Cancelled upload of /srv/uploads/large.bin (64 bytes acknowledged)")
        );
        let (worker, mut reply) = oneshot::channel();
        worker
            .send(Err("WRITE timed out; outcome is unknown".into()))
            .unwrap();
        assert_eq!(
            upload.reconnect_status(&mut reply, 32, &mut TransferQueue::default()),
            (
                "WRITE timed out; outcome is unknown".into(),
                None,
                TransferOutcome::Failed
            )
        );
        let (worker, mut reply) = oneshot::channel();
        worker.send(Ok(complete)).unwrap();
        let (status, saved, outcome) =
            download.reconnect_status(&mut reply, 64, &mut TransferQueue::default());
        assert!(status.starts_with("Downloaded /large.bin (128 bytes)"));
        assert_eq!(saved, download.downloaded_item());
        assert_eq!(outcome, TransferOutcome::Completed);
    }

    #[test]
    fn folder_reconnect_status_keeps_success_failure_and_unknown_outcomes_distinct() {
        let change = EntryChange::CreateFolder("test".into());
        let (pending, mut reply) = oneshot::channel();
        assert!(change_reconnect_status(&change, &mut reply).contains("outcome is unknown"));
        pending.send(Ok(json!({}))).unwrap();
        assert!(change_reconnect_status(&change, &mut reply).starts_with("Created folder test"));
        let (pending, mut reply) = oneshot::channel();
        pending.send(Err("permission denied".into())).unwrap();
        assert_eq!(
            change_reconnect_status(&change, &mut reply),
            "Could not create folder test: permission denied"
        );
        let (pending, mut reply) = oneshot::channel();
        drop(pending);
        assert!(
            change_reconnect_status(&change, &mut reply)
                .contains("before creation of test started")
        );
    }

    #[test]
    fn picker_never_seeds_a_windows_path_from_a_remote_filename() {
        for remote in [
            "/home/u/C:\\docs\\memo",
            "/tmp/NUL.txt",
            "/tmp/COM¹.bin",
            "/tmp/..",
            "/tmp/file:stream",
            "/tmp/ ",
        ] {
            assert_eq!(dialog_filename(remote), "download");
        }
        assert_eq!(dialog_filename("/srv/reports/report 1.txt"), "report 1.txt");
    }

    #[test]
    fn folder_upload_uses_the_selected_local_basename_and_original_remote_directory() {
        let mut drawer = FileDrawer {
            open: true,
            directory: Some("/srv/logs".into()),
            replace: true,
            ..Default::default()
        };
        let picker = TransferPicker::UploadFolder("/srv/logs".into());
        assert!(matches!(
            picker.purpose(&drawer),
            Ok(file_picker::Purpose::UploadFolder)
        ));
        let local = PathBuf::from(r"C:\archive\run 4 [雪]");
        let (operation, transfer) = drawer
            .prepare_folder_upload("/srv/logs", local.clone())
            .unwrap();
        assert_eq!(request_generation(&operation, 7), Some(7));
        assert!(
            matches!(operation, Operation::PutDir { local: source, remote, .. }
            if source == local && remote == "/srv/logs/run 4 [雪]")
        );
        assert_eq!(drawer.status, "Uploading folder /srv/logs/run 4 [雪]…");
        assert_eq!(
            transfer.outcome(&json!({"files":2,"directories":3,"bytes_transferred":7,"skipped":1})),
            "Uploaded folder /srv/logs/run 4 [雪] (2 files, 3 folders, 7 bytes, 1 skipped)."
        );
        for invalid in [r"C:\", r"\\server\share", r"relative\dir", "C:\\bad\nname"] {
            assert!(
                drawer
                    .prepare_folder_upload("/srv/logs", invalid.into())
                    .is_err(),
                "{invalid}"
            );
        }
        drawer.directory = Some("/other".into());
        assert!(picker.purpose(&drawer).is_err());
        assert!(drawer.prepare_folder_upload("/srv/logs", local).is_err());
    }

    #[test]
    fn folder_upload_reports_its_destination_and_distinguishes_partial_remote_trees() {
        let folder = TransferFile::UploadFolder {
            remote: "/reports/run".into(),
        };
        let partial = json!({
            "cancelled": true,
            "remote_may_be_partial": true,
            "resolved_remote": "/archived/run",
            "files": 3,
            "bytes_transferred": 42,
        });
        let message = folder.outcome(&partial);
        assert!(message.contains(
            "Cancelled upload of /archived/run (3 files completed, 42 bytes acknowledged)"
        ));
        assert!(message.contains("remote folder may be incomplete"));
        let before_mkdir =
            json!({"cancelled":true,"remote_may_be_partial":false,"files":0,"bytes_transferred":0});
        assert!(
            folder
                .outcome(&before_mkdir)
                .contains("remote folder was not created")
        );
        let (worker, mut reply) = oneshot::channel();
        let (interrupted, _, _) =
            folder.reconnect_status(&mut reply, 42, &mut TransferQueue::default());
        assert!(interrupted.contains("/reports/run"));
        assert!(interrupted.contains("at least 42 bytes acknowledged"));
        assert!(interrupted.contains("remote folder may be incomplete"));
        worker.send(Err("Permission denied".into())).unwrap();
        assert_eq!(
            folder.reconnect_status(&mut reply, 0, &mut TransferQueue::default()),
            (
                "Could not upload folder /reports/run: Permission denied".into(),
                None,
                TransferOutcome::Failed,
            )
        );
    }

    #[test]
    fn download_picker_joins_only_a_current_directory_basename_to_the_chosen_parent() {
        let mut drawer = FileDrawer {
            open: true,
            replace: true,
            ..Default::default()
        };
        let destination = PathBuf::from(r"C:\chosen");
        let directory = ListedKind::Known(EntryKind::Directory);
        let list = |drawer: &mut FileDrawer, name: &str, kind: ListedKind| {
            drawer.accept_listing(
                None,
                "/srv".into(),
                vec![Entry {
                    name: name.into(),
                    kind,
                    mtime: None,
                    size: None,
                }],
            );
            drawer.selected = Some(name.into());
            ListedEntry {
                parent: "/srv".into(),
                revision: drawer.listing_revision,
                name: name.into(),
                kind,
            }
        };
        let entry = list(&mut drawer, "report files", directory);
        let (op, status) = drawer
            .download_operation(
                &FileTarget::Listed(entry.clone()),
                destination.clone(),
                None,
            )
            .unwrap();
        assert!(matches!(op, Operation::GetDir {remote, local, ..}
            if remote == "/srv/report files" && local == destination.join("report files")));
        assert_eq!(
            status.outcome(&json!({"bytes_transferred":6,"files":2,"skipped":3})),
            r"Downloaded /srv/report files to C:\chosen\report files (2 files, 6 bytes, 3 links or special files skipped)."
        );
        assert!(
            status
                .outcome(&json!({"bytes_transferred":4,"cancelled":true}))
                .contains("4 bytes written to the temporary folder")
        );
        assert!(
            status
                .interrupted(3)
                .contains("at least 3 bytes written to the temporary folder")
        );

        let file = list(
            &mut drawer,
            "report files",
            ListedKind::Known(EntryKind::File),
        );
        assert!(
            drawer
                .download_operation(
                    &FileTarget::Listed(entry.clone()),
                    destination.clone(),
                    None
                )
                .is_err()
        );
        let (op, _) = drawer
            .download_operation(&FileTarget::Listed(file), destination.clone(), None)
            .unwrap();
        assert!(
            matches!(op, Operation::Get { overwrite: true, local, .. } if local == destination)
        );
        for name in [r"C:\redirect", "..", "NUL.log", "folder:stream"] {
            let entry = list(&mut drawer, name, directory);
            assert!(
                drawer
                    .download_operation(
                        &FileTarget::Listed(entry.clone()),
                        destination.clone(),
                        None
                    )
                    .is_err()
            );
        }
    }

    #[test]
    fn stale_picker_replies_cannot_start_a_download_or_consume_a_new_request() {
        let mut drawer = FileDrawer {
            open: true,
            generation: 1,
            ..Default::default()
        };
        let closed = drawer.start_picker();
        drawer.close();
        drawer.open = true;
        assert!(drawer.awaiting_native_result());
        assert!(!drawer.finish_picker(closed));
        assert!(!drawer.busy());

        let active = drawer.start_picker();
        assert!(!drawer.finish_picker(closed));
        assert!(drawer.busy());
        assert!(drawer.finish_picker(active));
        assert!(!drawer.finish_picker(active));
        let disconnected = drawer.start_picker();
        drawer.observe_connection(2);
        assert!(!drawer.finish_picker(disconnected));
    }

    #[test]
    fn reconnect_discards_old_directory_picker_and_overwrite_consent() {
        let mut drawer = FileDrawer {
            open: true,
            generation: 1,
            directory: Some("/old".into()),
            picking: Some(1),
            replace: true,
            queue: TransferQueue::files(
                vec![r"C:\queued".into()],
                "/old",
                true,
                false,
                false,
                TransferId {
                    pane: 1,
                    generation: 1,
                    serial: 1,
                },
            )
            .unwrap(),
            ..Default::default()
        };
        drawer.observe_connection(2);
        assert!(drawer.open);
        assert!(drawer.directory.is_none() && drawer.queue.remaining.is_empty());
        assert!(!drawer.replace && !drawer.busy());
        assert_eq!(drawer.generation, 2);
    }

    #[test]
    fn duplicate_listing_entries_fail_before_creating_native_item_keys() {
        let listing = json!({"entries":[{"filename":"twice"},{"filename":"twice"}]});
        assert!(directory_entries(&listing, SortOrder::Name).is_err());
    }
}
