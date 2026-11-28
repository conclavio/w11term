use super::*;
use anyhow::{Context, Result, ensure};
use catalog::{MAX_PAGE, Page, PageSource};
use std::{
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Write},
    os::windows::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
};

mod recovery;

pub enum Action {
    Open,
    Loaded(Arc<()>, Result<Document, String>),
    Mounted(Arc<()>),
    Text(Arc<()>, String),
    Import,
    Imported(Arc<()>, Result<Option<String>, String>),
    Export,
    Exported(Arc<()>, Result<Option<PathBuf>, String>),
    Save,
    Saved(Arc<()>, Result<String, String>),
    ReviewRemoval,
    CancelRemoval,
    Remove,
    Removed(Arc<()>, Result<Removal, String>),
    Recovery(recovery::Action),
    Cancel,
}

pub struct Removal {
    archive: PathBuf,
    library: Result<catalog::Loaded, String>,
}

pub struct Document {
    original: Option<String>,
    draft: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Work {
    Import,
    Export,
    Save,
    ReviewRemoval,
    Remove,
}

pub(super) struct Editor {
    key: Key,
    ticket: Arc<()>,
    document: Option<Document>,
    mounted: bool,
    work: Option<Work>,
    input: ElementRef<TextBox>,
    status: Option<String>,
    recovery: Option<recovery::Browser>,
}

fn message(action: Action) -> Message {
    Message::CommandReference(super::Action::Private(action))
}

impl Editor {
    fn ready(&self) -> bool {
        self.document.is_some() && self.mounted && self.work.is_none() && self.recovery.is_none()
    }

    pub(super) fn view(&self, ctx: &mut ViewContext<TerminalApp>) -> View {
        if let Some(recovery) = &self.recovery {
            return recovery.view(&self.key, ctx);
        }
        let mut fields = vec![KeyedView::new(
            "target",
            TextBlock::new().text(format!("{} · {}", self.key.name, self.key.platform.1)),
        )];
        if let Some(document) = &self.document {
            let ticket = self.ticket.clone();
            let sender = ctx.sender();
            let input = self.input.clone();
            let mounted = self.mounted;
            let ready = self.ready();
            ctx.use_effect("private manual text box", (mounted, ready), move || {
                if ready {
                    let _ = input.request_focus();
                } else if !mounted {
                    sender.send(message(Action::Mounted(ticket)));
                }
                None
            });
            let ticket = self.ticket.clone();
            fields.push(KeyedView::new(
                "markdown",
                TextBox::new()
                    .element_ref(&self.input)
                    .automation_name("Private page Markdown")
                    .accepts_return(true)
                    .text_wrapping(TextWrapping::Wrap)
                    .min_height(230.0)
                    .max_height(420.0)
                    .is_enabled(self.ready())
                    // Apply the seed after mounting, once WinUI has enabled multiline input.
                    .text_optional(self.mounted.then(|| document.draft.clone()))
                    .on_text_changed(
                        ctx.callback(move |text| message(Action::Text(ticket.clone(), text))),
                    ),
            ));
        }
        let status = match self.work {
            Some(Work::Import) => Some("Loading Markdown draft…"),
            Some(Work::Export) => Some("Exporting Markdown draft…"),
            Some(Work::Save) => Some("Saving private page…"),
            Some(Work::Remove) => Some("Archiving private page…"),
            Some(Work::ReviewRemoval) => Some(
                "The saved Markdown will be archived. Any unsaved changes will be discarded if you remove the page.",
            ),
            None if self.document.is_none() && self.status.is_none() => {
                Some("Loading private page…")
            }
            None => self.status.as_deref(),
        };
        if let Some(status) = status {
            fields.push(KeyedView::new(
                "status",
                TextBlock::new()
                    .text(status)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        if self.work == Some(Work::ReviewRemoval) {
            fields.push(KeyedView::new(
                "remove-review",
                StackPanel::new().spacing(4.0).children((
                    Button::new()
                        .on_click(ctx.callback(|_| message(Action::Remove)))
                        .content("Remove private page"),
                    Button::new()
                        .on_click(ctx.callback(|_| message(Action::CancelRemoval)))
                        .content("Back to private page"),
                )),
            ));
        } else {
            if let Some(document) = &self.document {
                fields.push(KeyedView::new(
                    "markdown-files",
                    StackPanel::new().spacing(4.0).children((
                        Button::new()
                            .is_enabled(self.ready())
                            .on_click(ctx.callback(|_| message(Action::Import)))
                            .content("Import Markdown…"),
                        Button::new()
                            .is_enabled(self.ready())
                            .on_click(ctx.callback(|_| message(Action::Export)))
                            .content("Export Markdown…"),
                        Button::new()
                            .is_enabled(self.ready())
                            .on_click(ctx.callback(|_| recovery::message(recovery::Action::Open)))
                            .content("Archived versions…"),
                    )),
                ));
                if document.original.is_some() {
                    fields.push(KeyedView::new(
                        "review-removal",
                        Button::new()
                            .style(ButtonStyle::Subtle)
                            .is_enabled(self.ready())
                            .on_click(ctx.callback(|_| message(Action::ReviewRemoval)))
                            .content("Review private page removal…"),
                    ));
                }
            }
            fields.push(KeyedView::new(
                "actions",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .is_enabled(self.ready())
                            .on_click(ctx.callback(|_| message(Action::Save)))
                            .content("Save private page"),
                        Button::new()
                            .is_enabled(self.work.is_none())
                            .on_click(ctx.callback(|_| message(Action::Cancel)))
                            .content("Cancel private page edit"),
                    )),
            ));
        }
        Border::new().padding(12.0).content(
            ScrollViewer::new().content(
                StackPanel::new().spacing(8.0).keyed_children(
                    std::iter::once(KeyedView::new(
                        "heading",
                        TextBlock::new()
                            .text("Private command page")
                            .font_size(18.0),
                    ))
                    .chain(fields),
                ),
            ),
        )
    }
}

impl Panel {
    pub(super) fn author_private_page(
        &mut self,
        action: Action,
        auto: Platform,
        owner: Option<usize>,
        ctx: &ComponentContext<TerminalApp>,
    ) {
        match action {
            Action::Open => {
                if !self.open || self.private_page.is_some() {
                    return;
                }
                let Some((key, _)) = self.private_page_target(self.platform.unwrap_or(auto)) else {
                    return;
                };
                let ticket = Arc::new(());
                let rejected = message(Action::Loaded(
                    ticket.clone(),
                    Err("Private page editor unavailable".into()),
                ));
                self.private_page = Some(Editor {
                    key: key.clone(),
                    ticket: ticket.clone(),
                    document: None,
                    mounted: false,
                    work: None,
                    input: ElementRef::default(),
                    status: None,
                    recovery: None,
                });
                self.draft = None;
                ctx.spawn_background_with_rejection(
                    move |_| {
                        let result = catalog::directory()
                            .and_then(|directory| load_document(&directory, &key));
                        message(Action::Loaded(
                            ticket,
                            result.map_err(|err| format!("{err:#}")),
                        ))
                    },
                    rejected,
                );
            }
            Action::Loaded(ticket, result) => {
                if let Some(editor) = &mut self.private_page
                    && Arc::ptr_eq(&editor.ticket, &ticket)
                {
                    match result {
                        Ok(document) => editor.document = Some(document),
                        Err(error) => editor.status = Some(error),
                    }
                }
            }
            Action::Mounted(ticket) => {
                if let Some(editor) = &mut self.private_page
                    && Arc::ptr_eq(&editor.ticket, &ticket)
                {
                    editor.mounted = true;
                }
            }
            Action::Text(ticket, text) => {
                if let Some(editor) = &mut self.private_page
                    && Arc::ptr_eq(&editor.ticket, &ticket)
                    && editor.ready()
                    && let Some(document) = &mut editor.document
                {
                    if text.len() > MAX_PAGE && normalize_lines(&text).len() > MAX_PAGE {
                        editor.status = Some("The private page exceeds 64 KiB".into());
                    } else {
                        document.draft = text;
                        editor.status = None;
                    }
                }
            }
            Action::Import => {
                let Some(owner) = owner else { return };
                let Some(editor) = self.private_page.as_mut().filter(|editor| editor.ready())
                else {
                    return;
                };
                let ticket = editor.ticket.clone();
                editor.work = Some(Work::Import);
                let rejected = message(Action::Imported(
                    ticket.clone(),
                    Err("Markdown file picker unavailable".into()),
                ));
                ctx.spawn_background_with_rejection(
                    move |scope| {
                        let result = if scope.is_cancelled() {
                            Ok(None)
                        } else {
                            file_picker::pick(owner, file_picker::Purpose::PrivatePageMarkdown)
                                .and_then(|file| {
                                    file.map(|file| read_imported_draft(&file.path)).transpose()
                                })
                        };
                        message(Action::Imported(
                            ticket,
                            result.map_err(|err| format!("{err:#}")),
                        ))
                    },
                    rejected,
                );
            }
            Action::Imported(ticket, result) => {
                if let Some(editor) = &mut self.private_page
                    && Arc::ptr_eq(&editor.ticket, &ticket)
                    && editor.work == Some(Work::Import)
                    && let Some(document) = &mut editor.document
                {
                    editor.work = None;
                    match result {
                        Ok(Some(draft)) => {
                            document.draft = draft;
                            editor.status = None;
                        }
                        Ok(None) => {}
                        Err(error) => editor.status = Some(error),
                    }
                }
            }
            Action::Export => {
                let Some(owner) = owner else { return };
                let Some(editor) = self.private_page.as_mut().filter(|editor| editor.ready())
                else {
                    return;
                };
                let Some(document) = &editor.document else {
                    return;
                };
                let draft = document.draft.clone();
                let filename = format!("{}.md", editor.key.name);
                let ticket = editor.ticket.clone();
                editor.work = Some(Work::Export);
                let rejected = message(Action::Exported(
                    ticket.clone(),
                    Err("Markdown file picker unavailable".into()),
                ));
                ctx.spawn_background_with_rejection(
                    move |scope| {
                        let export = || -> Result<Option<PathBuf>> {
                            if scope.is_cancelled() {
                                return Ok(None);
                            }
                            let Some(selected) = file_picker::pick(
                                owner,
                                file_picker::Purpose::SavePrivatePageMarkdown(filename),
                            )?
                            else {
                                return Ok(None);
                            };
                            if scope.is_cancelled() {
                                return Ok(None);
                            }
                            export_draft(
                                &catalog::directory()?,
                                &selected.path,
                                selected.overwrite,
                                &draft,
                            )?;
                            Ok(Some(selected.path))
                        };
                        message(Action::Exported(
                            ticket,
                            export().map_err(|err| format!("{err:#}")),
                        ))
                    },
                    rejected,
                );
            }
            Action::Exported(ticket, result) => {
                if let Some(editor) = &mut self.private_page
                    && Arc::ptr_eq(&editor.ticket, &ticket)
                    && editor.work == Some(Work::Export)
                {
                    editor.work = None;
                    match result {
                        Ok(Some(path)) => {
                            editor.status =
                                Some(format!("Exported Markdown to {}", path.display()));
                        }
                        Ok(None) => {}
                        Err(error) => editor.status = Some(error),
                    }
                }
            }
            Action::Save => {
                let Some(editor) = self.private_page.as_mut().filter(|editor| editor.ready())
                else {
                    return;
                };
                let Some(document) = &editor.document else {
                    return;
                };
                let key = editor.key.clone();
                let draft = document.draft.clone();
                let original = document.original.clone();
                let ticket = editor.ticket.clone();
                let rejected = message(Action::Saved(
                    ticket.clone(),
                    Err("Private page save unavailable".into()),
                ));
                editor.work = Some(Work::Save);
                editor.status = None;
                ctx.spawn_background_with_rejection(
                    move |_| {
                        let result = catalog::directory().and_then(|directory| {
                            save_document(&directory, &key, original.as_deref(), &draft)
                        });
                        message(Action::Saved(
                            ticket,
                            result.map_err(|err| format!("{err:#}")),
                        ))
                    },
                    rejected,
                );
            }
            Action::Saved(ticket, result) => {
                let Some(editor) = self.private_page.as_mut().filter(|editor| {
                    Arc::ptr_eq(&editor.ticket, &ticket) && editor.work == Some(Work::Save)
                }) else {
                    return;
                };
                match result {
                    Ok(text) => {
                        let key = editor.key.clone();
                        let page = parse_private_page(&text).expect("saved a valid private page");
                        if let Some(library) = &mut self.library {
                            library.catalog.set_private_page(key.clone(), page);
                        }
                        self.show_page(key, auto);
                        self.cached.replace(None);
                        self.private_page = None;
                    }
                    Err(error) => {
                        editor.work = None;
                        editor.status = Some(error);
                    }
                }
            }
            Action::ReviewRemoval => {
                if let Some(editor) = self.private_page.as_mut().filter(|editor| editor.ready())
                    && editor
                        .document
                        .as_ref()
                        .is_some_and(|document| document.original.is_some())
                {
                    editor.work = Some(Work::ReviewRemoval);
                }
            }
            Action::CancelRemoval => {
                if let Some(editor) = &mut self.private_page
                    && editor.work == Some(Work::ReviewRemoval)
                {
                    editor.work = None;
                }
            }
            Action::Remove => {
                let Some(editor) = self
                    .private_page
                    .as_mut()
                    .filter(|editor| editor.work == Some(Work::ReviewRemoval))
                else {
                    return;
                };
                let Some(original) = editor
                    .document
                    .as_ref()
                    .and_then(|document| document.original.clone())
                else {
                    return;
                };
                let key = editor.key.clone();
                let ticket = editor.ticket.clone();
                editor.work = Some(Work::Remove);
                let rejected = message(Action::Removed(
                    ticket.clone(),
                    Err("Private page archival unavailable".into()),
                ));
                ctx.spawn_background_with_rejection(
                    move |_| {
                        let result = catalog::directory()
                            .and_then(|directory| archive_document(&directory, &key, &original))
                            .map(|archive| Removal {
                                archive,
                                library: catalog::load().map_err(|error| format!("{error:#}")),
                            })
                            .map_err(|error| format!("{error:#}"));
                        message(Action::Removed(ticket, result))
                    },
                    rejected,
                );
            }
            Action::Removed(ticket, result) => {
                let Some(editor) = self.private_page.as_mut().filter(|editor| {
                    Arc::ptr_eq(&editor.ticket, &ticket) && editor.work == Some(Work::Remove)
                }) else {
                    return;
                };
                match result {
                    Ok(removal) => {
                        let key = editor.key.clone();
                        let mut status =
                            format!("Archived private Markdown to {}", removal.archive.display());
                        match removal.library {
                            Ok(library) => self.library = Some(library),
                            Err(error) => {
                                if let Some(library) = &mut self.library {
                                    library.catalog.remove_private_page(&key);
                                }
                                status.push_str(&format!(
                                    " Could not reload command reference pages. {error}"
                                ));
                            }
                        }
                        self.private_page = None;
                        self.cached.replace(None);
                        self.command_lookup = None;
                        self.related_history.clear();
                        self.set_query(key.name);
                        if self.platform.unwrap_or(auto) != key.platform {
                            self.platform = Some(key.platform);
                        }
                        self.mode = SearchMode::PreferredExact;
                        self.selected = 0;
                        self.focus = self.focus.wrapping_add(1);
                        self.error = Some(status);
                    }
                    Err(error) => {
                        editor.work = None;
                        editor.status = Some(error);
                    }
                }
            }
            Action::Recovery(action) => {
                if let Some(editor) = &mut self.private_page {
                    editor.recovery_action(action, ctx);
                }
            }
            Action::Cancel => {
                if self
                    .private_page
                    .as_ref()
                    .is_some_and(|editor| editor.work.is_none() && editor.recovery.is_none())
                {
                    self.private_page = None;
                    self.focus = self.focus.wrapping_add(1);
                }
            }
        }
    }
}

pub(super) fn editable_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name == catalog::query(name)
        && !name.starts_with('.')
        && w11term::sftp::validate_download_name(name).is_ok()
}

fn normalize_lines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn page_path(directory: &Path, key: &Key, create: bool) -> Result<Option<PathBuf>> {
    ensure!(editable_name(&key.name), "Choose a valid command name");
    if create {
        fs::create_dir_all(directory)?;
    }
    let mut parent = directory.to_path_buf();
    for name in ["pages", key.platform.0] {
        parent.push(name);
        if create && let Err(error) = fs::create_dir(&parent) {
            ensure!(
                error.kind() == ErrorKind::AlreadyExists,
                "Could not create the private page folder: {error}"
            );
        }
        let metadata = match fs::symlink_metadata(&parent) {
            Ok(metadata) => metadata,
            Err(error) if !create && error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("Opening the private page folder"),
        };
        ensure!(
            metadata.is_dir() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
            "Choose a regular private page folder"
        );
    }
    Ok(Some(parent.join(format!("{}.md", key.name))))
}

fn open_page(path: &Path) -> Result<Option<File>> {
    let file = match OpenOptions::new()
        .read(true)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_DELETE).0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Opening the private page"),
    };
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
        "Choose a regular private page file"
    );
    Ok(Some(file))
}

fn load_document(directory: &Path, key: &Key) -> Result<Document> {
    let original = match page_path(directory, key, false)? {
        Some(path) => open_page(&path)?.map(catalog::read_page).transpose()?,
        None => None,
    };
    let draft = original.as_ref().map_or_else(
        || {
            format!(
                "# {}\n\n> Describe the command.\n\n- Describe an example:\n\n` `\n",
                key.name
            )
        },
        |text| normalize_lines(text.trim_start_matches('\u{feff}')),
    );
    Ok(Document {
        original,
        draft: draft.replace('\n', "\r"),
    })
}

fn read_imported_draft(path: &Path) -> Result<String> {
    let file = File::open(path).context("Opening the selected Markdown file")?;
    ensure!(file.metadata()?.is_file(), "Choose a regular Markdown file");
    read_draft(file)
}

fn read_draft(file: File) -> Result<String> {
    let text = catalog::read_page(file).context("Reading the selected Markdown file")?;
    let text = normalize_lines(text.trim_start_matches('\u{feff}'));
    parse_private_page(&text)?;
    Ok(text.replace('\n', "\r"))
}

fn export_draft(directory: &Path, path: &Path, overwrite: bool, draft: &str) -> Result<()> {
    let filename = path.file_name().context("Choose a Markdown filename")?;
    let parent = path
        .parent()
        .context("Choose a folder for the Markdown file")?
        .canonicalize()
        .context("Opening the selected export folder")?;
    match directory.join("pages").canonicalize() {
        Ok(library) => ensure!(
            !parent.starts_with(library),
            "Choose a location outside the command reference library. Use Save private page to update a manual."
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("Checking the command reference library"),
    }
    w11term::transcript::save(&parent.join(filename), &normalize_lines(draft), overwrite)
}

fn parse_private_page(text: &str) -> Result<Page> {
    ensure!(text.len() <= MAX_PAGE, "The private page exceeds 64 KiB");
    ensure!(
        !text.contains('\0'),
        "The private page contains a NUL character"
    );
    Page::parse(text, PageSource::Custom)
        .context("Start the private page with a Markdown heading such as # command")
}

fn save_document(
    directory: &Path,
    key: &Key,
    original: Option<&str>,
    draft: &str,
) -> Result<String> {
    let text = normalize_lines(draft);
    parse_private_page(&text)?;
    let path = page_path(directory, key, true)?.expect("created private page folder");
    let parent = path.parent().expect("private page has a folder");
    let _lock = lock_private_pages(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(text.as_bytes())?;
    staged.as_file().sync_all()?;
    // Close the old file handle before replacing it. The sidecar lock still
    // serializes the compare and publish steps across w11term windows.
    let on_disk = open_page(&path)?.map(catalog::read_page).transpose()?;
    ensure!(
        on_disk.as_deref() == original,
        "The private page changed on disk. Copy your draft and reopen the editor before saving."
    );
    if on_disk.is_some() {
        staged.persist(path).context("Replacing the private page")?;
    } else {
        staged
            .persist_noclobber(path)
            .context("Creating the private page")?;
    }
    Ok(text)
}

fn lock_private_pages(parent: &Path) -> Result<File> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(parent.join(".private-pages.lock"))?;
    lock.try_lock()
        .context("Another window is editing private pages. Try again.")?;
    Ok(lock)
}

fn archive_document(directory: &Path, key: &Key, original: &str) -> Result<PathBuf> {
    let changed = "The private page changed on disk. Reopen the editor before removing it.";
    let source = page_path(directory, key, false)?.context(changed)?;
    let _lock = lock_private_pages(source.parent().expect("private page has a folder"))?;
    let mut open = open_page(&source)?.context(changed)?;
    ensure!(catalog::read_page(&mut open)? == original, "{changed}");

    let archive = directory.join("removed-pages");
    if let Err(error) = fs::create_dir(&archive) {
        ensure!(
            error.kind() == ErrorKind::AlreadyExists,
            "Could not create the private manual archive: {error}"
        );
    }
    let metadata = fs::symlink_metadata(&archive)?;
    ensure!(
        metadata.is_dir() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
        "Choose a regular private manual archive folder"
    );
    let backup_folder = tempfile::Builder::new()
        .prefix(&format!("{}-", key.platform.0))
        .tempdir_in(archive)?;
    let name = source.file_name().expect("private page has a filename");
    fs::rename(&source, backup_folder.path().join(name)).context("Archiving the private page")?;
    Ok(backup_folder.keep().join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> Key {
        Key {
            platform: LINUX,
            name: name.into(),
        }
    }

    #[test]
    fn creates_and_reopens_a_platform_specific_multiline_page() {
        let directory = tempfile::tempdir().unwrap();
        let key = key("crew-report");
        let new = load_document(directory.path(), &key).unwrap();
        assert!(new.original.is_none());
        assert!(new.draft.starts_with("# crew-report\r\r"));
        assert!(!directory.path().join("pages").exists());
        let markdown =
            "# Crew report\n\n> Report status.\n\n- Show one crew:\n\n`crew-report team`\n";
        save_document(directory.path(), &key, None, &markdown.replace('\n', "\r")).unwrap();
        let file = directory.path().join("pages/linux/crew-report.md");
        assert_eq!(fs::read_to_string(file).unwrap(), markdown);
        let existing = load_document(directory.path(), &key).unwrap();
        assert_eq!(existing.original.as_deref(), Some(markdown));
        assert_eq!(normalize_lines(&existing.draft), markdown);
        let windows = Key {
            platform: WINDOWS,
            ..key
        };
        assert!(
            load_document(directory.path(), &windows)
                .unwrap()
                .original
                .is_none()
        );
    }

    #[test]
    fn refuses_stale_saves_and_preserves_the_external_edit() {
        let directory = tempfile::tempdir().unwrap();
        let key = key("rg");
        save_document(directory.path(), &key, None, "# Original").unwrap();
        let file = directory.path().join("pages/linux/rg.md");
        let snapshot = load_document(directory.path(), &key).unwrap();
        fs::write(&file, "# Changed externally").unwrap();
        let error = save_document(
            directory.path(),
            &key,
            snapshot.original.as_deref(),
            "# Draft",
        )
        .unwrap_err();
        assert!(error.to_string().contains("changed on disk"));
        assert!(save_document(directory.path(), &key, None, "# Draft").is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "# Changed externally");
        save_document(
            directory.path(),
            &key,
            Some("# Changed externally"),
            "# Reopened draft",
        )
        .unwrap();
        assert_eq!(fs::read_to_string(file).unwrap(), "# Reopened draft");
    }

    #[test]
    fn archives_only_the_reviewed_version_and_keeps_each_backup() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = directory.path();
        let key = key("route-review");
        save_document(catalog, &key, None, "# Reviewed").unwrap();
        let path = page_path(catalog, &key, false).unwrap().unwrap();
        let external = "# Updated manually\r\n\r\n> Keep these bytes.\r\n";
        fs::write(&path, external).unwrap();
        assert!(archive_document(catalog, &key, "# Reviewed").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), external);
        assert!(!catalog.join("removed-pages").exists());

        let first = archive_document(catalog, &key, external).unwrap();
        assert!(!path.exists());
        assert_eq!(fs::read_to_string(&first).unwrap(), external);
        save_document(catalog, &key, None, "# A newer page").unwrap();
        let second = archive_document(catalog, &key, "# A newer page").unwrap();
        assert_ne!(first, second);
        assert_eq!(fs::read_to_string(first).unwrap(), external);
        assert_eq!(fs::read_to_string(second).unwrap(), "# A newer page");
    }

    #[test]
    fn imports_only_bounded_utf8_manuals_and_normalizes_the_draft() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("runbook.md");
        let source_text = "\u{feff}# Caf\u{e9} runbook\r\n\r\n> Report status.\r\n";
        fs::write(&source, source_text).unwrap();
        assert_eq!(
            read_imported_draft(&source).unwrap(),
            "# Caf\u{e9} runbook\r\r> Report status.\r"
        );
        assert_eq!(fs::read_to_string(&source).unwrap(), source_text);
        for contents in [
            b"missing heading".to_vec(),
            b"# Heading\n\0".to_vec(),
            b"# \xff".to_vec(),
            vec![b'x'; MAX_PAGE + 1],
        ] {
            fs::write(&source, contents).unwrap();
            assert!(read_imported_draft(&source).is_err());
        }
    }

    #[test]
    fn exports_an_importable_copy_without_writing_the_library() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = directory.path().join("catalog");
        let exported = directory.path().join("copy.md");
        let draft = "# Draft\r\r> Review the route.\r";
        export_draft(&catalog, &exported, false, draft).unwrap();
        assert_eq!(
            fs::read(&exported).unwrap(),
            b"# Draft\n\n> Review the route.\n"
        );
        assert_eq!(read_imported_draft(&exported).unwrap(), draft);
        assert!(!catalog.exists());

        let key = key("route-review");
        save_document(&catalog, &key, None, "# Saved").unwrap();
        let manual = page_path(&catalog, &key, false).unwrap().unwrap();
        assert!(export_draft(&catalog, &manual, true, draft).is_err());
        assert_eq!(fs::read_to_string(manual).unwrap(), "# Saved");
        assert!(export_draft(&catalog, &exported, false, "# Other").is_err());
        assert_eq!(read_imported_draft(&exported).unwrap(), draft);
    }

    #[test]
    fn rejects_invalid_page_names_and_documents_before_creating_files() {
        let directory = tempfile::tempdir().unwrap();
        for name in [
            "../crew-report",
            r"paths\tool",
            "con",
            "a:alternate",
            "tool.",
        ] {
            assert!(
                save_document(directory.path(), &key(name), None, "# Title").is_err(),
                "{name}"
            );
        }
        for draft in [
            "no title here".into(),
            format!("# Test\n{}", "x".repeat(MAX_PAGE)),
        ] {
            assert!(save_document(directory.path(), &key("test"), None, &draft).is_err());
        }
        assert!(!directory.path().join("pages").exists());
    }
}
