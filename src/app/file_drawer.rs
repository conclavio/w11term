use super::*;
use anyhow::{Result, anyhow, bail, ensure};
use serde_json::Value;
use std::{collections::VecDeque, path::PathBuf};
use tokio::sync::{oneshot, watch};
use w11term::folder_grants::Authority;
use w11term::sftp::{DEFAULT_TIMEOUT, EntryKind, Job, Operation, TransferProgress};

const PAGE_SIZE: usize = 32;

#[derive(Clone)]
pub enum FileAction {
    Toggle,
    OpenHere,
    Follow(u64, u64, bool),
    Reveal(u64, u64, String, Option<String>),
    Close(u64),
    Browse(u64, u64, String),
    Choose(u64, u64, ListedEntry),
    Rename(u64, u64, ListedEntry),
    Delete(u64, u64, ListedEntry),
    NewFolder(u64, u64, String),
    EditName(u64, u64, u64, String),
    FinishEdit(u64, u64, u64, ContentDialogResult),
    Page(u64, usize),
    Replace(u64, u64, bool),
    Pick(u64, u64, String, Option<String>),
    Picked(u64, u64, Option<String>, Result<Option<PathBuf>, String>),
    Drop(u64, u64, String, Vec<PathBuf>),
    Listed(u64, u64, String, Result<Vec<Entry>, String>),
    Tick(u64),
}

#[derive(Default)]
pub(super) struct FileDrawer {
    pub(super) open: bool,
    generation: u64,
    directory: Option<String>,
    following: bool,
    last_report: Option<Arc<str>>,
    entries: Vec<Entry>,
    listing_revision: u64,
    page: usize,
    selected: Option<String>,
    reveal: Option<RevealTarget>,
    editor: Option<NameEditor>,
    editor_serial: u64,
    replace: bool,
    picking: bool,
    parsing: bool,
    pending: Option<Pending>,
    uploads: VecDeque<PathBuf>,
    status: String,
    cached: RefCell<Option<View>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    name: String,
    kind: ListedKind,
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

#[derive(Clone)]
pub struct ListedEntry {
    parent: String,
    revision: u64,
    name: String,
    kind: ListedKind,
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

struct NameEditor {
    serial: u64,
    parent: String,
    name: String,
    action: EditAction,
}

enum EditAction {
    Create,
    Rename(ListedEntry),
    Delete(ListedEntry, EntryKind),
}

impl EditAction {
    fn original(&self) -> Option<&ListedEntry> {
        match self {
            Self::Create => None,
            Self::Rename(entry) | Self::Delete(entry, _) => Some(entry),
        }
    }
}

impl NameEditor {
    fn destination(&self) -> Result<String> {
        match &self.action {
            EditAction::Rename(original) => {
                ensure!(self.name != original.name, "Enter a different name")
            }
            EditAction::Delete(original, _) => return delete_path(&self.parent, &original.name),
            EditAction::Create => {}
        }
        entry_path(&self.parent, &self.name)
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
    Canonical,
    Listing(String),
    IdentifyDelete(ListedEntry),
    Upload(String),
    Download(String),
    Change { parent: String, change: EntryChange },
}

struct Pending {
    step: Step,
    reply: oneshot::Receiver<Result<Value, String>>,
    progress: Option<watch::Receiver<TransferProgress>>,
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

fn directory_entries(reply: &Value) -> Result<Vec<Entry>> {
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
        entries.push(Entry {
            name: name.to_owned(),
            kind: ListedKind::from_attrs(&item["attrs"]),
        });
    }
    entries.sort_by(|a, b| {
        (a.kind != ListedKind::Known(EntryKind::Directory), &a.name)
            .cmp(&(b.kind != ListedKind::Known(EntryKind::Directory), &b.name))
    });
    Ok(entries)
}

fn dialog_filename(remote: &str) -> &str {
    let leaf = remote.rsplit('/').next().unwrap_or("");
    let stem = leaf
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    if leaf.is_empty()
        || leaf.ends_with(['.', ' '])
        || leaf
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        || matches!(
            stem.as_str(),
            "CON" | "NUL" | "PRN" | "AUX" | "CONIN$" | "CONOUT$"
        )
        || ["LPT", "COM"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|s| {
                matches!(
                    s,
                    "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        })
    {
        "download"
    } else {
        leaf
    }
}

impl FileDrawer {
    pub(super) fn editing_name(&self) -> bool {
        self.editor.is_some()
    }

    pub(super) fn cancel_name_edit(&mut self) {
        if self.identifying_delete() {
            self.pending.take();
            self.status.clear();
            self.cached.get_mut().take();
        }
        if self.editor.take().is_some() {
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
                .entries
                .iter()
                .skip(self.page * PAGE_SIZE)
                .take(PAGE_SIZE)
                .any(|e| e.name == entry.name && e.kind == entry.kind)
    }

    fn check_selected(&self, entry: &ListedEntry) -> Result<()> {
        ensure!(
            self.selected.as_ref() == Some(&entry.name) && self.has_entry(entry),
            "The selection changed. Select the entry again."
        );
        Ok(())
    }

    fn accept_listing(&mut self, path: String, entries: Vec<Entry>) {
        self.entries = entries;
        self.listing_revision = self.listing_revision.wrapping_add(1);
        self.directory = Some(path);
        self.selected = None;
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
            self.page = i / PAGE_SIZE;
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
        self.editor_serial = self.editor_serial.wrapping_add(1);
        self.editor = Some(NameEditor {
            serial: self.editor_serial,
            parent,
            name: action
                .original()
                .map_or_else(String::new, |entry| entry.name.clone()),
            action,
        });
        Ok(())
    }

    fn finish_edit(
        &mut self,
        serial: u64,
        result: ContentDialogResult,
    ) -> Result<Option<(NameEditor, String)>> {
        if !self.editor.as_ref().is_some_and(|e| e.serial == serial) {
            return Ok(None);
        }
        let editor = self.editor.take().unwrap();
        if result != ContentDialogResult::Primary {
            return Ok(None);
        }
        ensure!(
            self.open && self.directory.as_ref() == Some(&editor.parent),
            "The directory changed. Open the file action again."
        );
        if let Some(entry) = editor.action.original() {
            self.check_selected(entry)?;
        }
        let path = editor.destination()?;
        Ok(Some((editor, path)))
    }

    pub(super) fn name_dialog(
        &self,
        pane: u64,
        ssh: &SshOptions,
        context: &ViewContext<TerminalApp>,
    ) -> Option<View> {
        let editor = self.editor.as_ref()?;
        let serial = editor.serial;
        let generation = self.generation;
        let valid = editor.destination();
        if let EditAction::Delete(_, kind) = &editor.action {
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
        let (title, label, confirm) = if matches!(editor.action, EditAction::Rename(_)) {
            ("Rename entry", "New name", "Rename")
        } else {
            ("New folder", "Folder name", "Create")
        };
        let mut fields = vec![KeyedView::new(
            "name",
            TextBox::new()
                .automation_name(label)
                .text(&editor.name)
                .on_text_changed(context.callback(move |name| {
                    Message::Files(FileAction::EditName(pane, generation, serial, name))
                })),
        )];
        if !editor.name.is_empty()
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

    pub(super) fn awaiting_native_result(&self) -> bool {
        self.picking || self.parsing || self.editing_name()
    }

    pub(super) fn bind_window(&mut self, sender: LocalSender<Message>) {
        self.cancel_name_edit();
        self.cached.get_mut().take();
        if let Some(pending) = &self.pending {
            *pending.destination.borrow_mut() = sender;
        }
    }

    pub(super) fn busy(&self) -> bool {
        self.awaiting_native_result() || self.pending.is_some()
    }

    pub(super) fn observe_connection(&mut self, generation: u64) {
        if self.generation == generation {
            return;
        }
        let status = match self.pending.as_mut() {
            Some(Pending { step: Step::Change { change, .. }, reply, .. }) => {
                change_reconnect_status(change, reply)
            }
            Some(Pending { step: Step::Upload(_), .. }) =>
                "SSH reconnected during upload. The remote file may be incomplete. Refresh to browse.".into(),
            _ => match &self.reveal {
                Some(RevealTarget::Changed(change)) => format!("{} before SSH reconnected. Refresh to browse.", change.completed()),
                _ => "SSH connected. Refresh to browse.".into(),
            }
        };
        *self = Self {
            open: self.open,
            following: self.following,
            generation,
            status,
            ..Self::default()
        };
    }

    /// Sample only when ready to browse. A burst during a transfer or listing is
    /// coalesced into the latest report, without changing any transfer destination.
    fn follow_target(&mut self, report: &Option<Arc<str>>, host: &str) -> Option<String> {
        if !self.open || !self.following || self.busy() || self.last_report == *report {
            return None;
        }
        self.last_report.clone_from(report);
        shell_directory(report.as_deref(), host)
            .filter(|path| Some(path) != self.directory.as_ref())
    }

    fn panel(&self, pane: u64, ssh: &SshOptions, context: &ViewContext<TerminalApp>) -> View {
        if let Some(view) = self.cached.borrow().as_ref() {
            return view.clone();
        }
        use FileAction::*;
        let button = |label: &str, enabled: bool, action: FileAction| {
            Button::new()
                .is_enabled(enabled)
                .on_click(context.callback(move |_| Message::Files(action.clone())))
                .content(TextBlock::new().text(label))
        };
        let folder = self.directory.as_deref().unwrap_or(".");
        let generation = self.generation;
        let ready = !self.busy();
        let writable = ready && self.directory.is_some();
        let selected = self
            .entries
            .iter()
            .find(|e| Some(&e.name) == self.selected.as_ref());
        let path = selected.and_then(|e| remote_child(folder, &e.name).ok());
        let revision = self.listing_revision;
        let listed = |entry: Option<&Entry>| ListedEntry {
            revision,
            parent: folder.into(),
            name: entry.map_or_else(String::new, |e| e.name.clone()),
            kind: entry.map_or(ListedKind::Missing, |e| e.kind),
        };
        let mut controls: Vec<View> = vec![
            Grid::new()
                .columns([GridLength::Star(1.0), GridLength::Auto])
                .children((
                    TextBlock::new()
                        .text(format!("{}@{}:{}", ssh.user, ssh.host, ssh.port))
                        .text_wrapping(TextWrapping::Wrap),
                    Border::new()
                        .grid_column(1)
                        .content(button("Close", true, Close(pane))),
                )),
            TextBlock::new()
                .text(folder)
                .text_wrapping(TextWrapping::Wrap)
                .into(),
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .children((
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
                    button(
                        "Upload…",
                        writable,
                        Pick(pane, generation, folder.into(), None),
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
                        Browse(pane, generation, path.clone().unwrap_or_default()),
                    ),
                    button(
                        "Download…",
                        ready
                            && selected
                                .is_some_and(|e| e.kind != ListedKind::Known(EntryKind::Directory)),
                        Pick(pane, generation, folder.into(), path),
                    ),
                )),
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
        ];
        if let Some(pending) = &self.pending
            && let Some(progress) = &pending.progress
        {
            let p = *progress.borrow();
            let mut bar = ProgressBar::new().is_indeterminate(p.total.is_none());
            if let Some(total) = p.total {
                bar = bar
                    .maximum(total.max(1) as f64)
                    .value(p.bytes.min(total) as f64);
            }
            controls.push(bar.into());
            let bytes = match p.total {
                Some(total) => format!("{} / {total} bytes", p.bytes),
                None => format!("{} bytes", p.bytes),
            };
            controls.push(TextBlock::new().text(bytes).into());
        }
        if !self.status.is_empty() {
            controls.push(
                TextBlock::new()
                    .text(&self.status)
                    .text_wrapping(TextWrapping::Wrap)
                    .into(),
            );
        }
        if self.entries.len() > PAGE_SIZE {
            let pages = self.entries.len().div_ceil(PAGE_SIZE);
            controls.push(
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(6.0)
                    .children((
                        button(
                            "Previous",
                            ready && self.page > 0,
                            Page(pane, self.page.saturating_sub(1)),
                        ),
                        TextBlock::new().text(format!("{} / {pages}", self.page + 1)),
                        button(
                            "Next",
                            ready && self.page + 1 < pages,
                            Page(pane, self.page + 1),
                        ),
                    )),
            );
        }
        let list: Vec<_> = self
            .entries
            .iter()
            .skip(self.page * PAGE_SIZE)
            .take(PAGE_SIZE)
            .map(|entry| {
                let name = entry.name.clone();
                let label = if entry.kind == ListedKind::Known(EntryKind::Directory) {
                    format!("{name}/")
                } else {
                    name.clone()
                };
                let active = self.selected.as_ref() == Some(&name);
                let entry_action = Choose(pane, generation, listed(Some(entry)));
                KeyedView::new(
                    entry.name.as_str(),
                    Button::new()
                        .is_enabled(ready)
                        .style(if active {
                            ButtonStyle::Accent
                        } else {
                            ButtonStyle::Subtle
                        })
                        .horizontal_alignment(HorizontalAlignment::Stretch)
                        .on_click(context.callback(move |_| Message::Files(entry_action.clone())))
                        .content(
                            TextBlock::new()
                                .text(label)
                                .text_wrapping(TextWrapping::Wrap),
                        ),
                )
            })
            .collect();
        let drop_folder = folder.to_owned();
        let panel = Border::new()
            .padding(10.0)
            .background(Color::rgb(31, 32, 35))
            .drop_policy(writable.then(|| {
                DragDropPolicy::new().storage_items(
                    DragDropAction::new(DragDropOperation::Copy).caption("Upload files"),
                )
            }))
            .on_drop(context.callback(move |d| {
                let paths = match d {
                    DroppedData::StorageItems(items) => items
                        .into_iter()
                        .map(|item| PathBuf::from(item.path))
                        .collect(),
                    _ => vec![],
                };
                Message::Files(Drop(pane, generation, drop_folder.clone(), paths))
            }))
            .content(
                Grid::new()
                    .rows([GridLength::Auto, GridLength::Star(1.0)])
                    .children((
                        StackPanel::new().spacing(6.0).keyed_children(
                            controls
                                .into_iter()
                                .enumerate()
                                .map(|(index, view)| KeyedView::new(index.to_string(), view)),
                        ),
                        ScrollViewer::new()
                            .grid_row(1)
                            .content(StackPanel::new().keyed_children(list)),
                    )),
            );
        *self.cached.borrow_mut() = Some(panel.clone());
        panel
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
        let (tx_progress, progress) = if matches!(step, Step::Upload(_) | Step::Download(_)) {
            let (tx, rx) = watch::channel(TransferProgress::default());
            (Some(tx), Some(rx))
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
                expected_generation: (matches!(
                    step,
                    Step::Change { .. } | Step::IdentifyDelete(_)
                ) || matches!(
                    operation,
                    Operation::ListMountedFolder { .. }
                ))
                .then_some(self.generation),
                operation,
                progress: tx_progress,
                deadline: tokio::time::Instant::now() + Duration::from_secs(DEFAULT_TIMEOUT),
                reply: tx,
            }))
            .map_err(|_| anyhow!("SSH session has closed"))?;
        self.pending = Some(Pending {
            step,
            reply,
            progress,
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
        context: &ComponentContext<TerminalApp>,
    ) -> Result<()> {
        self.status = "Reading directory…".into();
        self.reveal = None;
        self.job(
            id,
            session,
            Operation::Realpath { path },
            Step::Canonical,
            context,
        )
    }

    fn upload_next(
        &mut self,
        id: u64,
        session: &Session,
        context: &ComponentContext<TerminalApp>,
    ) -> Result<()> {
        let local = self.uploads.pop_front().context("No file selected")?;
        ensure!(
            local.is_absolute(),
            "Select a local file with an absolute path"
        );
        let name = local
            .file_name()
            .and_then(|n| n.to_str())
            .context("SFTP requires a UTF-8 filename")?
            .to_owned();
        let remote = remote_child(
            self.directory
                .as_deref()
                .context("Open a remote directory first")?,
            &name,
        )?;
        self.status = format!("Uploading {name}…");
        self.job(
            id,
            session,
            Operation::Put {
                local,
                remote,
                overwrite: self.replace,
            },
            Step::Upload(name),
            context,
        )
    }

    fn tick(
        &mut self,
        id: u64,
        session: &Session,
        may_open_dialog: bool,
        context: &ComponentContext<TerminalApp>,
    ) -> Result<()> {
        let Some(pending) = &mut self.pending else {
            return Ok(());
        };
        let result = match pending.reply.try_recv() {
            Err(oneshot::error::TryRecvError::Empty) => {
                if let Some(progress) = pending.progress.as_mut()
                    && progress.has_changed().unwrap_or(false)
                {
                    let _ = progress.borrow_and_update();
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
                } else {
                    Err("SSH connection closed".into())
                }
            }
            Ok(result) => result,
        };
        self.cached.borrow_mut().take();
        let step = self.pending.take().unwrap().step;
        let reply = match result {
            Ok(reply) => reply,
            Err(error) => {
                if let Step::IdentifyDelete(entry) = &step {
                    bail!("Could not delete {}: {error}", entry.name);
                }
                if let Step::Change { change, .. } = &step {
                    bail!("{}", change.error(&error));
                }
                if let Step::Upload(name) = &step {
                    if !self.replace && self.entries.iter().any(|entry| &entry.name == name) {
                        bail!("Upload of {name} failed (replacing files is off): {error}");
                    }
                    bail!("Upload of {name} failed: {error}");
                }
                bail!("{error}");
            }
        };
        match step {
            Step::IdentifyDelete(entry) => {
                if self.open && may_open_dialog {
                    self.confirm_identified(entry, &reply["attrs"])?;
                } else {
                    self.status.clear();
                }
            }
            Step::Canonical => {
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
                    Step::Listing(path),
                    context,
                )?;
            }
            Step::Listing(path) => {
                let generation = self.generation;
                self.parsing = true;
                let rejected = Message::Files(FileAction::Listed(
                    id,
                    generation,
                    path.clone(),
                    Err("Unable to display the directory; refresh to retry".into()),
                ));
                context.spawn_background_with_rejection(
                    move |_| {
                        Message::Files(FileAction::Listed(
                            id,
                            generation,
                            path,
                            directory_entries(&reply).map_err(|e| e.to_string()),
                        ))
                    },
                    rejected,
                );
            }
            Step::Upload(name) => {
                self.status = format!("Uploaded {name} ({} bytes)", reply["bytes_transferred"]);
                if self.uploads.is_empty() {
                    let path = self.directory.clone().context("Missing upload directory")?;
                    self.job(
                        id,
                        session,
                        Operation::Ls { path: path.clone() },
                        Step::Listing(path),
                        context,
                    )?;
                } else {
                    self.upload_next(id, session, context)?;
                }
            }
            Step::Change { parent, change } => {
                self.status = change.completed();
                // The old name no longer identifies the entry the user selected.
                // If the follow-up listing fails, do not leave stale rows actionable.
                if matches!(change, EntryChange::Rename { .. } | EntryChange::Delete(_)) {
                    self.entries.clear();
                    self.selected = None;
                    self.page = 0;
                }
                self.reveal = Some(RevealTarget::Changed(change));
                self.job(
                    id,
                    session,
                    Operation::Ls {
                        path: parent.clone(),
                    },
                    Step::Listing(parent),
                    context,
                )?;
            }
            Step::Download(name) => {
                self.status = format!("Downloaded {name} ({} bytes)", reply["bytes_transferred"])
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
        self.files.observe_connection(frame.connection_generation);
        if let Some(path) = self
            .files
            .follow_target(&frame.reported_directory, &ssh.host)
        {
            self.files.cached.borrow_mut().take();
            if let Err(error) =
                self.files
                    .browse(self.session.pane_id, &self.session, path, context)
            {
                self.files.status = format!("{error:#}");
            }
        }
    }
}

impl TerminalApp {
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
        let files = &mut tab.files;
        files.observe_connection(generation);
        ensure!(
            !files.busy(),
            "The SFTP drawer is busy. Try again when the current operation finishes."
        );
        files.job(
            pane,
            &tab.session,
            Operation::ListMountedFolder { path: path.into() },
            Step::Listing(path.into()),
            context,
        )?;
        files.open = true;
        files.following = false;
        files.reveal = None;
        files.directory = None;
        files.entries.clear();
        files.selected = None;
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

    pub(super) fn cancel_inactive_name_editor(&mut self) {
        let inactive = self
            .tabs
            .entries()
            .iter()
            .find_map(|entry| match &entry.value.page {
                Page::Terminal(tab)
                    if Some(entry.id) != self.tabs.active_id()
                        && (tab.files.editing_name() || tab.files.identifying_delete()) =>
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
            tab.files.cancel_name_edit();
        }
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
        let width = (self.width * 0.45).clamp(1.0, 360.0);
        Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Pixel(width)])
            .keyed_children([
                KeyedView::new("terminal", Border::new().grid_column(0).content(content)),
                KeyedView::new(
                    "files",
                    Border::new().grid_column(1).content(tab.files.panel(
                        tab.session.pane_id,
                        ssh,
                        context,
                    )),
                ),
            ])
    }

    pub(super) fn file_action(&mut self, action: FileAction, context: &ComponentContext<Self>) {
        use FileAction::*;
        let pane = match &action {
            Toggle | OpenHere => match self.tabs.active().and_then(AppTab::pane_id) {
                Some(id) => id,
                None => return,
            },
            Close(id)
            | Follow(id, ..)
            | Reveal(id, ..)
            | Browse(id, ..)
            | Choose(id, ..)
            | Rename(id, ..)
            | Delete(id, ..)
            | NewFolder(id, ..)
            | EditName(id, ..)
            | FinishEdit(id, ..)
            | Page(id, _)
            | Replace(id, ..)
            | Pick(id, ..)
            | Picked(id, ..)
            | Drop(id, ..)
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
        let was_editing = files.editing_name();
        files.observe_connection(frame.connection_generation);
        if !matches!(action, Tick(_) | Picked(..) | Listed(..) | Drop(..)) && !visible {
            return;
        }
        if matches!(&action, NewFolder(_, g, ..) | Choose(_, g, ..) | Rename(_, g, ..) | Delete(_, g, ..) | EditName(_, g, ..) | FinishEdit(_, g, ..) | Follow(_, g, ..) | Reveal(_, g, ..) | Browse(_, g, ..) | Replace(_, g, ..) | Pick(_, g, ..) | Picked(_, g, ..) | Listed(_, g, ..) | Drop(_, g, ..) if *g != files.generation)
        {
            if was_editing && visible && !files.editing_name() {
                self.connection_ui(id);
            }
            return;
        }
        if !matches!(action, Tick(_) | EditName(..)) {
            files.cached.borrow_mut().take();
        }
        let result = (|| -> Result<()> {
            match action {
                Toggle => {
                    files.open = !files.open;
                    if !files.open {
                        files.cancel_name_edit();
                    }
                    self.initial_grid.set(None);
                    crate::input::clear_grid();
                    if files.open && files.directory.is_none() && !files.busy() {
                        files.browse(pane, session, ".".into(), context)?;
                    }
                }
                OpenHere => {
                    files.open = true;
                    self.initial_grid.set(None);
                    crate::input::clear_grid();
                    if !files.busy() {
                        let path = shell_directory(frame.reported_directory.as_deref(), &ssh.host)
                            .unwrap_or_else(|| ".".into());
                        files.browse(pane, session, path, context)?;
                    }
                }
                Close(_) => {
                    files.cancel_name_edit();
                    files.open = false;
                }
                Follow(_, _, enabled) => {
                    if files.open && files.following != enabled {
                        files.following = enabled;
                        files.last_report = None;
                    }
                }
                Reveal(_, _, directory, name) => {
                    if !files.busy() {
                        files.open = true;
                        files.following = false;
                        self.initial_grid.set(None);
                        crate::input::clear_grid();
                        files.browse(pane, session, directory, context)?;
                        files.reveal = name.map(RevealTarget::Existing);
                    }
                }
                Tick(_) => files.tick(pane, session, may_open_dialog, context)?,
                Listed(_, _, path, result) if files.parsing => {
                    files.parsing = false;
                    files.accept_listing(path, result.map_err(|e| anyhow!("{e}"))?);
                }
                Picked(_, _, remote, result) if files.picking => {
                    files.picking = false;
                    let Some(local) = result.map_err(|e| anyhow!("{e}"))? else {
                        return Ok(());
                    };
                    if let Some(remote) = remote {
                        files.status = format!("Downloading {remote}…");
                        files.job(
                            pane,
                            session,
                            Operation::Get {
                                local,
                                remote: remote.clone(),
                                overwrite: files.replace,
                                max_bytes: None,
                            },
                            Step::Download(remote),
                            context,
                        )?;
                    } else {
                        files.uploads.push_back(local);
                        files.upload_next(pane, session, context)?;
                    }
                }
                EditName(_, _, serial, name) => {
                    if let Some(editor) = files.editor.as_mut().filter(|e| {
                        e.serial == serial && !matches!(e.action, EditAction::Delete(..))
                    }) {
                        editor.name = name;
                    }
                }
                FinishEdit(_, _, serial, result) => {
                    if !may_open_dialog {
                        files.cancel_name_edit();
                        return Ok(());
                    }
                    if let Some((
                        NameEditor {
                            parent,
                            name,
                            action,
                            ..
                        },
                        path,
                    )) = files.finish_edit(serial, result)?
                    {
                        let (change, operation) = match action {
                            EditAction::Delete(entry, kind) => {
                                files.status = format!("Deleting {}…", entry.name);
                                (
                                    EntryChange::Delete(entry.name),
                                    Operation::RemoveEntry { path, kind },
                                )
                            }
                            EditAction::Rename(entry) => {
                                files.status = format!("Renaming {} to {name}…", entry.name);
                                let from = entry_path(&parent, &entry.name)?;
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
                _ if files.busy() || !files.open => {}
                NewFolder(_, _, parent)
                    if frame.prompt.is_none() && self.title_editor.is_none() =>
                {
                    files.begin_edit(parent, EditAction::Create)?;
                }
                Rename(_, _, entry) if frame.prompt.is_none() && self.title_editor.is_none() => {
                    files.begin_edit(entry.parent.clone(), EditAction::Rename(entry))?;
                }
                Delete(_, _, entry) if may_open_dialog => {
                    files.check_selected(&entry)?;
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
                    files.browse(pane, session, path, context)?;
                }
                Choose(_, _, entry) => {
                    if files.has_entry(&entry) {
                        files.selected = Some(entry.name);
                    }
                }
                Page(_, page) => {
                    files.page = page.min(files.entries.len().saturating_sub(1) / PAGE_SIZE);
                    files.selected = None;
                }
                Replace(_, _, replace) => files.replace = replace,
                Drop(_, _, directory, paths) => {
                    ensure!(
                        files.directory.as_ref() == Some(&directory),
                        "The destination directory changed. Drop the files again."
                    );
                    ensure!(!paths.is_empty(), "Drop local files onto this drawer");
                    ensure!(
                        paths.iter().all(|p| p.is_absolute()),
                        "Drop files with local paths"
                    );
                    files.uploads.extend(paths);
                    files.upload_next(pane, session, context)?;
                }
                Pick(_, _, directory, remote) => {
                    ensure!(
                        files.directory.as_ref() == Some(&directory),
                        "The directory changed. Select the file operation again."
                    );
                    let owner = crate::input::window().context("Window is not ready")?.0 as usize;
                    files.picking = true;
                    let generation = files.generation;
                    let reject = Message::Files(Picked(
                        pane,
                        generation,
                        remote.clone(),
                        Err("File picker unavailable".into()),
                    ));
                    context.spawn_background_with_rejection(
                        move |_| {
                            let purpose = remote
                                .as_deref()
                                .map_or(file_picker::Purpose::Open, |name| {
                                    file_picker::Purpose::Save(dialog_filename(name).to_owned())
                                });
                            let result = file_picker::pick(owner, purpose)
                                .map(|selection| selection.map(|file| file.path))
                                .map_err(|error| format!("{error:#}"));
                            Message::Files(Picked(pane, generation, remote, result))
                        },
                        reject,
                    );
                }
                Picked(..) | Listed(..) => {}
            }
            Ok(())
        })();
        if let Err(error) = result {
            files.uploads.clear();
            files.status = match files.reveal.take() {
                Some(RevealTarget::Changed(change)) => {
                    format!(
                        "{}, but refreshing the directory failed: {error:#}",
                        change.completed()
                    )
                }
                _ => format!("{error:#}"),
            };
            files.cached.borrow_mut().take();
        }
        if visible {
            tab.follow_shell_directory(&frame, context);
            if was_editing != tab.files.editing_name() {
                self.connection_ui(id);
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
        let upload = FileAction::Drop(
            pane,
            files.generation,
            files.directory.clone().unwrap_or_default(),
            vec![local.join("upload.bin")],
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
                    Message::History(super::command_history::Action::Return),
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
                let remote =
                    remote_child(files.directory.as_deref().unwrap(), "upload.bin").unwrap();
                let Page::Terminal(tab) = &mut self.tabs.get_mut(id).unwrap().page else {
                    unreachable!()
                };
                tab.files.picking = true;
                let generation = tab.files.generation;
                self.file_action(
                    FileAction::Picked(
                        pane,
                        generation,
                        Some(remote),
                        Ok(Some(local.join("download.bin"))),
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
    use serde_json::json;

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
        drawer.picking = true;
        assert!(
            drawer
                .follow_target(&report("file://peer/b"), "peer")
                .is_none()
        );
        drawer.picking = false;
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
    fn remote_paths_and_directory_types_use_sftp_semantics() {
        let listing = json!({"entries":[
            {"filename":"link", "attrs":{"permissions": 0o120777}}, {"filename":".", "attrs":{}},
            {"filename":"folder", "attrs":{"permissions":0o040755}}, {"filename":"file", "attrs":{"permissions":0o100644}},
        ]});
        let entries = directory_entries(&listing).unwrap();
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
        let first = drawer.editor_serial;
        let changed = Some(Arc::from("file://server/new"));
        assert!(drawer.follow_target(&changed, "server").is_none());
        drawer.cancel_name_edit();
        drawer
            .begin_edit("/work".into(), EditAction::Create)
            .unwrap();
        let current = drawer.editor_serial;
        drawer.editor.as_mut().unwrap().name = "prepared".into();
        assert!(
            drawer
                .finish_edit(first, ContentDialogResult::Primary)
                .unwrap()
                .is_none()
        );
        assert!(drawer.editing_name());
        let (editor, path) = drawer
            .finish_edit(current, ContentDialogResult::Primary)
            .unwrap()
            .unwrap();
        assert_eq!(
            (&*editor.parent, &*editor.name, &*path),
            ("/work", "prepared", "/work/prepared")
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
                .finish_edit(drawer.editor_serial, ContentDialogResult::Primary)
                .is_err()
        );
        drawer
            .begin_edit("/other".into(), EditAction::Create)
            .unwrap();
        drawer.observe_connection(5);
        assert!(
            drawer
                .finish_edit(drawer.editor_serial, ContentDialogResult::Primary)
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
            }],
            selected: Some(source.name.clone()),
            ..Default::default()
        };
        let begin = |drawer: &mut FileDrawer| {
            drawer.begin_edit(source.parent.clone(), EditAction::Rename(source.clone()))
        };
        begin(&mut drawer).unwrap();
        assert_eq!(drawer.editor.as_ref().unwrap().name, source.name);
        assert!(drawer.editor.as_ref().unwrap().destination().is_err()); // unchanged
        assert!(
            drawer
                .follow_target(&Some(Arc::from("file://host/elsewhere")), "host")
                .is_none()
        );
        drawer.editor.as_mut().unwrap().name = "new name 雪".into();
        let old = drawer.editor_serial;
        assert!(
            drawer
                .finish_edit(old, ContentDialogResult::None)
                .unwrap()
                .is_none()
        );
        begin(&mut drawer).unwrap();
        let current = drawer.editor_serial;
        assert!(
            drawer
                .finish_edit(old, ContentDialogResult::Primary)
                .unwrap()
                .is_none()
        );
        drawer.editor.as_mut().unwrap().name = "new name 雪".into();
        let (editor, path) = drawer
            .finish_edit(current, ContentDialogResult::Primary)
            .unwrap()
            .unwrap();
        assert_eq!(path, "/work/new name 雪");
        assert_eq!(editor.action.original().unwrap().name, "alias");
        assert!(
            drawer
                .finish_edit(current, ContentDialogResult::Primary)
                .unwrap()
                .is_none()
        );

        begin(&mut drawer).unwrap();
        drawer.editor.as_mut().unwrap().name = "new".into();
        drawer.listing_revision += 1; // same parent and filename in a replacement listing
        assert!(
            drawer
                .finish_edit(drawer.editor_serial, ContentDialogResult::Primary)
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
            }],
            ..Default::default()
        };
        for unsupported in [None, Some(0o644), Some(0o060640)] {
            assert!(
                drawer
                    .confirm_identified(entry.clone(), &attrs(unsupported))
                    .is_err()
            );
            assert!(!drawer.editing_name());
        }
        let link = attrs(Some(0o120777));
        drawer.listing_revision += 1;
        assert!(drawer.confirm_identified(entry.clone(), &link).is_err());
        drawer.listing_revision = entry.revision;
        drawer.selected = None;
        assert!(drawer.confirm_identified(entry.clone(), &link).is_err());
        drawer.selected = Some(entry.name.clone());
        drawer.confirm_identified(entry.clone(), &link).unwrap();
        let old = drawer.editor_serial;
        drawer.cancel_name_edit();
        drawer.confirm_identified(entry.clone(), &link).unwrap();
        assert!(
            drawer
                .finish_edit(old, ContentDialogResult::Primary)
                .unwrap()
                .is_none()
        );
        let serial = drawer.editor_serial;
        drawer.selected = None;
        assert!(
            drawer
                .finish_edit(serial, ContentDialogResult::Primary)
                .is_err()
        );
        drawer.selected = Some(entry.name.clone());
        drawer.confirm_identified(entry.clone(), &link).unwrap();
        let (editor, path) = drawer
            .finish_edit(drawer.editor_serial, ContentDialogResult::Primary)
            .unwrap()
            .unwrap();
        assert_eq!(path, "/work/unknown");
        assert!(matches!(
            editor.action,
            EditAction::Delete(_, EntryKind::Symlink)
        ));
        assert!(
            drawer
                .finish_edit(drawer.editor_serial, ContentDialogResult::Primary)
                .unwrap()
                .is_none()
        );

        drawer.confirm_identified(entry, &link).unwrap();
        let serial = drawer.editor_serial;
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
            "/work".into(),
            vec![Entry {
                name: "alias".into(),
                kind: ListedKind::Known(EntryKind::File),
            }],
        );
        assert_eq!(drawer.status, "Deleted alias");
        assert_eq!(drawer.selected, None);
        assert!(drawer.reveal.is_none());
        assert_eq!(drawer.page, 0);
        assert_eq!(drawer.listing_revision, 4);
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
    fn reconnect_discards_old_directory_picker_and_overwrite_consent() {
        let mut drawer = FileDrawer {
            open: true,
            generation: 1,
            directory: Some("/old".into()),
            picking: true,
            replace: true,
            uploads: VecDeque::from([PathBuf::from("queued")]),
            ..Default::default()
        };
        drawer.observe_connection(2);
        assert!(drawer.open);
        assert!(drawer.directory.is_none() && drawer.uploads.is_empty());
        assert!(!drawer.replace && !drawer.busy());
        assert_eq!(drawer.generation, 2);
    }

    #[test]
    fn duplicate_listing_entries_fail_before_creating_native_item_keys() {
        let listing = json!({"entries":[{"filename":"twice"},{"filename":"twice"}]});
        assert!(directory_entries(&listing).is_err());
    }
}
