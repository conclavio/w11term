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

pub enum Action {
    Open,
    Loaded(Arc<()>, Result<Document, String>),
    Mounted(Arc<()>),
    Text(Arc<()>, String),
    Save,
    Saved(Arc<()>, Result<String, String>),
    Cancel,
}

pub struct Document {
    original: Option<String>,
    draft: String,
}

pub(super) struct Editor {
    key: Key,
    ticket: Arc<()>,
    document: Option<Document>,
    mounted: bool,
    saving: bool,
    input: ElementRef<TextBox>,
    error: Option<String>,
}

fn message(action: Action) -> Message {
    Message::CommandReference(super::Action::Private(action))
}

impl Editor {
    pub(super) fn view(&self, ctx: &mut ViewContext<TerminalApp>) -> View {
        let mut fields = vec![KeyedView::new(
            "target",
            TextBlock::new().text(format!("{} · {}", self.key.name, self.key.platform.1)),
        )];
        if let Some(document) = &self.document {
            let ticket = self.ticket.clone();
            let sender = ctx.sender();
            let input = self.input.clone();
            let mounted = self.mounted;
            ctx.use_effect("private manual text box", mounted, move || {
                if mounted {
                    let _ = input.request_focus();
                } else {
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
                    .is_enabled(self.mounted && !self.saving)
                    // Apply the seed after mounting, once WinUI has enabled multiline input.
                    .text_optional(self.mounted.then(|| document.draft.clone()))
                    .on_text_changed(
                        ctx.callback(move |text| message(Action::Text(ticket.clone(), text))),
                    ),
            ));
        }
        if let Some(error) = &self.error {
            fields.push(KeyedView::new(
                "error",
                TextBlock::new()
                    .text(error)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        } else if self.saving || self.document.is_none() {
            fields.push(KeyedView::new(
                "progress",
                TextBlock::new().text(if self.saving {
                    "Saving private page…"
                } else {
                    "Loading private page…"
                }),
            ));
        }
        fields.push(KeyedView::new(
            "actions",
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    Button::new()
                        .is_enabled(self.mounted && !self.saving)
                        .on_click(ctx.callback(|_| message(Action::Save)))
                        .content("Save private page"),
                    Button::new()
                        .is_enabled(!self.saving)
                        .on_click(ctx.callback(|_| message(Action::Cancel)))
                        .content("Cancel private page edit"),
                )),
        ));
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
                    saving: false,
                    input: ElementRef::default(),
                    error: None,
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
                        Err(error) => editor.error = Some(error),
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
                    && editor.mounted
                    && !editor.saving
                    && let Some(document) = &mut editor.document
                {
                    if text.len() > MAX_PAGE && normalize_lines(&text).len() > MAX_PAGE {
                        editor.error = Some("The private page exceeds 64 KiB".into());
                    } else {
                        document.draft = text;
                        editor.error = None;
                    }
                }
            }
            Action::Save => {
                let Some(editor) = self
                    .private_page
                    .as_mut()
                    .filter(|editor| editor.mounted && !editor.saving)
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
                editor.saving = true;
                editor.error = None;
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
                let Some(editor) = self
                    .private_page
                    .as_mut()
                    .filter(|editor| Arc::ptr_eq(&editor.ticket, &ticket))
                else {
                    return;
                };
                match result {
                    Ok(text) => {
                        let key = editor.key.clone();
                        let page = parse_private_page(&text).expect("saved a valid private page");
                        if let Some(library) = &mut self.library {
                            library.catalog.set_private_page(key.clone(), page);
                        }
                        self.set_query(key.name);
                        self.exact = true;
                        if self.platform.unwrap_or(auto) != key.platform {
                            self.platform = Some(key.platform);
                        }
                        self.selected = 0;
                        self.cached.replace(None);
                        self.private_page = None;
                        self.focus = self.focus.wrapping_add(1);
                    }
                    Err(error) => {
                        editor.saving = false;
                        editor.error = Some(error);
                    }
                }
            }
            Action::Cancel => {
                if self
                    .private_page
                    .as_ref()
                    .is_some_and(|editor| !editor.saving)
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
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(parent.join(".private-pages.lock"))?;
    lock.try_lock()
        .context("Another window is saving private pages. Try again.")?;
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
