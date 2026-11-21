use super::*;
use anyhow::{Result, anyhow, bail, ensure};
use serde_json::Value;
use std::{collections::VecDeque, path::PathBuf};
use tokio::sync::{oneshot, watch};
use w11term::sftp::{DEFAULT_TIMEOUT, Job, Operation, TransferProgress};

const PAGE_SIZE: usize = 32;

#[derive(Clone)]
pub enum FileAction {
    Toggle,
    OpenHere,
    Reveal(u64, u64, String, Option<String>),
    Close(u64),
    Browse(u64, u64, String),
    Choose(u64, String),
    Page(u64, usize),
    Replace(u64, u64, bool),
    Pick(u64, u64, Option<String>),
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
    entries: Vec<Entry>,
    page: usize,
    selected: Option<String>,
    reveal_name: Option<String>,
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
    directory: Option<bool>,
}

enum Step {
    Canonical,
    Listing(String),
    Upload(String),
    Download(String),
}

struct Pending {
    step: Step,
    reply: oneshot::Receiver<Result<Value, String>>,
    progress: Option<watch::Receiver<TransferProgress>>,
    _timer: crate::ui_timer::RetryTimer,
}

fn remote_child(directory: &str, name: &str) -> Result<String> {
    ensure!(
        !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0']),
        "Invalid remote filename"
    );
    Ok(format!("{}/{}", directory.trim_end_matches('/'), name))
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
        let directory = match item["attrs"]["permissions"].as_u64().map(|m| m & 0o170000) {
            Some(0o040000) => Some(true),
            Some(0o100000) => Some(false),
            _ => None,
        };
        entries.push(Entry {
            name: name.to_owned(),
            directory,
        });
    }
    entries.sort_by(|a, b| {
        (a.directory != Some(true), &a.name).cmp(&(b.directory != Some(true), &b.name))
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

fn pick_local(owner: usize, remote: Option<&str>) -> Result<Option<PathBuf>> {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};
    use windows::{
        Win32::{
            Foundation::{ERROR_CANCELLED, HWND},
            System::Com::*,
            UI::Shell::*,
        },
        core::{HRESULT, PCWSTR},
    };
    // A fresh STA owns the native modal dialog, independently of the Reactor UI thread.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        struct Apartment;
        impl Drop for Apartment {
            fn drop(&mut self) {
                unsafe { CoUninitialize() };
            }
        }
        let _apartment = Apartment;
        let clsid = if remote.is_some() {
            FileSaveDialog
        } else {
            FileOpenDialog
        };
        let dialog: IFileDialog = CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetOptions(dialog.GetOptions()? | FOS_FORCEFILESYSTEM | FOS_NOCHANGEDIR)?;
        if let Some(remote) = remote {
            let suggested: Vec<u16> = dialog_filename(remote)
                .encode_utf16()
                .chain(Some(0))
                .collect();
            dialog.SetFileName(PCWSTR(suggested.as_ptr()))?;
        }
        match dialog.Show(Some(HWND(owner as *mut _))) {
            Err(e) if e.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => return Ok(None),
            result => result?,
        }
        let wide = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = OsString::from_wide(wide.as_wide());
        CoTaskMemFree(Some(wide.0.cast()));
        Ok(Some(path.into()))
    }
}

impl FileDrawer {
    pub(super) fn busy(&self) -> bool {
        self.picking || self.parsing || self.pending.is_some()
    }

    pub(super) fn observe_connection(&mut self, generation: u64) {
        if self.generation == generation {
            return;
        }
        let status = if self
            .pending
            .as_ref()
            .is_some_and(|p| matches!(p.step, Step::Upload(_)))
        {
            "SSH reconnected during upload. The remote file may be incomplete. Refresh to browse."
        } else {
            "SSH connected. Refresh to browse."
        };
        *self = Self {
            open: self.open,
            generation,
            status: status.into(),
            ..Self::default()
        };
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
                    button("Upload…", writable, Pick(pane, generation, None)),
                )),
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .children((
                    button(
                        "Open folder",
                        ready && selected.is_some_and(|e| e.directory != Some(false)),
                        Browse(pane, generation, path.clone().unwrap_or_default()),
                    ),
                    button(
                        "Download…",
                        ready && selected.is_some_and(|e| e.directory != Some(true)),
                        Pick(pane, generation, path),
                    ),
                )),
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
                let label = if entry.directory == Some(true) {
                    format!("{name}/")
                } else {
                    name.clone()
                };
                let active = self.selected.as_ref() == Some(&name);
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
                        .on_click(
                            context.callback(move |_| Message::Files(Choose(pane, name.clone()))),
                        )
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
        let sender = context.sender();
        let timer = crate::ui_timer::RetryTimer::new(move || {
            sender.send(Message::Files(FileAction::Tick(id)));
        })?;
        session
            .input
            .send(Command::Sftp(Job {
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
        self.reveal_name = None;
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
            Err(oneshot::error::TryRecvError::Closed) => Err("SSH connection closed".into()),
            Ok(result) => result,
        };
        self.cached.borrow_mut().take();
        let step = self.pending.take().unwrap().step;
        let reply = match result {
            Ok(reply) => reply,
            Err(error) => {
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
            Step::Download(name) => {
                self.status = format!("Downloaded {name} ({} bytes)", reply["bytes_transferred"])
            }
        }
        Ok(())
    }
}

impl TerminalApp {
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
            | Reveal(id, ..)
            | Browse(id, ..)
            | Choose(id, _)
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
        files.observe_connection(frame.connection_generation);
        if !matches!(action, Tick(_) | Picked(..) | Listed(..) | Drop(..)) && !visible {
            return;
        }
        if matches!(&action, Reveal(_, g, ..) | Browse(_, g, ..) | Replace(_, g, ..) | Pick(_, g, ..) | Picked(_, g, ..) | Listed(_, g, ..) | Drop(_, g, ..) if *g != files.generation)
        {
            return;
        }
        if !matches!(action, Tick(_)) {
            files.cached.borrow_mut().take();
        }
        let result = (|| -> Result<()> {
            match action {
                Toggle => {
                    files.open = !files.open;
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
                    files.open = false;
                }
                Reveal(_, _, directory, name) => {
                    if !files.busy() {
                        files.open = true;
                        self.initial_grid.set(None);
                        crate::input::clear_grid();
                        files.browse(pane, session, directory, context)?;
                        files.reveal_name = name;
                    }
                }
                Tick(_) => files.tick(pane, session, context)?,
                Listed(_, _, path, result) if files.parsing => {
                    files.parsing = false;
                    files.entries = result.map_err(|e| anyhow!("{e}"))?;
                    files.directory = Some(path);
                    files.selected = None;
                    files.page = 0;
                    if files.status == "Reading directory…" {
                        files.status.clear();
                    }
                    if let Some(name) = files.reveal_name.take() {
                        if let Some(i) = files.entries.iter().position(|entry| entry.name == name) {
                            files.page = i / PAGE_SIZE;
                            files.selected = Some(name);
                        } else {
                            files.status = format!("No entry named {name} in this directory");
                        }
                    }
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
                _ if files.busy() || !files.open => {}
                Browse(_, _, path) => files.browse(pane, session, path, context)?,
                Choose(_, name) => files.selected = Some(name),
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
                Pick(_, _, remote) => {
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
                            let suggestion = remote.clone();
                            let result = std::thread::Builder::new()
                                .name("SFTP file picker".into())
                                .spawn(move || pick_local(owner, suggestion.as_deref()))
                                .map_err(|e| e.to_string())
                                .and_then(|handle| {
                                    handle.join().map_err(|_| "File picker stopped".to_owned())
                                })
                                .and_then(|result| result.map_err(|e| format!("{e:#}")));
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
            files.status = format!("{error:#}");
            files.cached.borrow_mut().take();
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
            (&*entries[0].name, entries[0].directory),
            ("folder", Some(true))
        );
        assert_eq!(entries[1].directory, Some(false));
        assert_eq!(entries[2].directory, None);
        assert_eq!(remote_child("/", "résumé.txt").unwrap(), "/résumé.txt");
        assert_eq!(remote_child("/home/user/", "a").unwrap(), "/home/user/a");
        for invalid in ["", ".", "..", "a/b", "bad\0name"] {
            assert!(remote_child("/", invalid).is_err());
        }
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
