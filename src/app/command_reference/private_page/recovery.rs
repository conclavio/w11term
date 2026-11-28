use super::*;
use std::time::SystemTime;

pub enum Action {
    Open,
    Listed(Arc<()>, Result<Vec<Version>, String>),
    Select(Arc<()>, usize),
    Previewed(Arc<()>, Result<String, String>),
    Use(Arc<()>),
    Back(Arc<()>),
}

pub struct Version {
    folder: std::ffi::OsString,
    time: Option<SystemTime>,
}

impl Version {
    fn label(&self) -> String {
        self.time.map_or_else(
            || self.folder.to_string_lossy().into_owned(),
            |time| {
                chrono::DateTime::<chrono::Utc>::from(time)
                    .format("%Y-%m-%d %H:%M:%S%.3f UTC")
                    .to_string()
            },
        )
    }
}

#[derive(Default)]
enum Preview {
    #[default]
    None,
    Reading,
    Ready(String),
    Failed(String),
}

#[derive(Default)]
pub(super) struct Browser {
    ticket: Arc<()>,
    versions: Option<Result<Vec<Version>, String>>,
    selected: Option<usize>,
    preview: Preview,
}

pub(super) fn message(action: Action) -> Message {
    super::message(super::Action::Recovery(action))
}

impl Browser {
    pub(super) fn view(&self, key: &Key, ctx: &ViewContext<TerminalApp>) -> View {
        let mut rows = vec![
            KeyedView::new(
                "heading",
                TextBlock::new().text("Archived versions").font_size(18.0),
            ),
            KeyedView::new(
                "target",
                TextBlock::new().text(format!("{} · {}", key.name, key.platform.1)),
            ),
        ];
        match &self.versions {
            None => rows.push(label("Finding archived versions…")),
            Some(Err(error)) => rows.push(label(error)),
            Some(Ok(versions)) if versions.is_empty() => {
                rows.push(label("No archived versions for this command and platform."));
            }
            Some(Ok(versions)) => {
                let ticket = self.ticket.clone();
                rows.push(KeyedView::new(
                    "versions",
                    ComboBox::new()
                        .automation_name("Archived private page versions")
                        .items_source(versions.iter().map(Version::label))
                        .is_enabled(!matches!(self.preview, Preview::Reading))
                        .selected_index(self.selected)
                        .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                            index.map_or(Message::ControlIdle, |index| {
                                message(Action::Select(ticket.clone(), index))
                            })
                        })),
                ));
                match &self.preview {
                    Preview::Reading => rows.push(label("Reading the archived Markdown…")),
                    Preview::Failed(error) => rows.push(label(error)),
                    Preview::Ready(text) => rows.push(KeyedView::new(
                        "preview",
                        TextBlock::new()
                            .text(normalize_lines(text))
                            .text_wrapping(TextWrapping::Wrap)
                            .is_text_selection_enabled(true),
                    )),
                    Preview::None => {}
                }
            }
        }
        let ticket = self.ticket.clone();
        let back = ticket.clone();
        rows.push(KeyedView::new(
            "actions",
            StackPanel::new().spacing(4.0).children((
                Button::new()
                    .is_enabled(matches!(self.preview, Preview::Ready(_)))
                    .on_click(ctx.callback(move |_| message(Action::Use(ticket.clone()))))
                    .content("Use archived draft"),
                Button::new()
                    .on_click(ctx.callback(move |_| message(Action::Back(back.clone()))))
                    .content("Back to private page"),
            )),
        ));
        Border::new().padding(12.0).content(
            ScrollViewer::new().content(StackPanel::new().spacing(8.0).keyed_children(rows)),
        )
    }

    fn start_preview(&mut self, key: &Key, index: usize, ctx: &ComponentContext<TerminalApp>) {
        if self.selected == Some(index) || matches!(self.preview, Preview::Reading) {
            return;
        }
        let Some(version) = self
            .versions
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .and_then(|versions| versions.get(index))
        else {
            return;
        };
        self.selected = Some(index);
        self.preview = Preview::Reading;
        let folder = version.folder.clone();
        let key = key.clone();
        let ticket = self.ticket.clone();
        let rejected = message(Action::Previewed(
            ticket.clone(),
            Err("Archived Markdown reader unavailable".into()),
        ));
        ctx.spawn_background_with_rejection(
            move |_| {
                let result = catalog::directory()
                    .and_then(|directory| archived_draft(&directory, &key, &folder));
                message(Action::Previewed(
                    ticket,
                    result.map_err(|error| format!("{error:#}")),
                ))
            },
            rejected,
        );
    }
}

fn label(text: &str) -> KeyedView {
    KeyedView::new(
        "status",
        TextBlock::new()
            .text(text)
            .text_wrapping(TextWrapping::Wrap),
    )
}

impl Editor {
    pub(super) fn recovery_action(&mut self, action: Action, ctx: &ComponentContext<TerminalApp>) {
        if matches!(action, Action::Open) {
            if !self.ready() {
                return;
            }
            let browser = Browser::default();
            let ticket = browser.ticket.clone();
            let key = self.key.clone();
            self.recovery = Some(browser);
            let rejected = message(Action::Listed(
                ticket.clone(),
                Err("Archive listing unavailable".into()),
            ));
            ctx.spawn_background_with_rejection(
                move |_| {
                    let result =
                        catalog::directory().and_then(|directory| list_versions(&directory, &key));
                    message(Action::Listed(
                        ticket,
                        result.map_err(|error| format!("{error:#}")),
                    ))
                },
                rejected,
            );
            return;
        }
        let Some(browser) = &mut self.recovery else {
            return;
        };
        match action {
            Action::Listed(ticket, result) if Arc::ptr_eq(&browser.ticket, &ticket) => {
                browser.versions = Some(result);
                browser.start_preview(&self.key, 0, ctx);
            }
            Action::Select(ticket, index) if Arc::ptr_eq(&browser.ticket, &ticket) => {
                browser.start_preview(&self.key, index, ctx);
            }
            Action::Previewed(ticket, result) if Arc::ptr_eq(&browser.ticket, &ticket) => {
                browser.preview = match result {
                    Ok(text) => Preview::Ready(text),
                    Err(error) => Preview::Failed(error),
                };
            }
            Action::Use(ticket) if Arc::ptr_eq(&browser.ticket, &ticket) => {
                if let Preview::Ready(draft) = &browser.preview
                    && let Some(document) = &mut self.document
                {
                    document.draft = draft.clone();
                    self.status = None;
                    self.recovery = None;
                    self.mounted = false;
                }
            }
            Action::Back(ticket) if Arc::ptr_eq(&browser.ticket, &ticket) => {
                self.recovery = None;
                self.mounted = false;
            }
            _ => {}
        }
    }
}

fn list_versions(directory: &Path, key: &Key) -> Result<Vec<Version>> {
    let mut versions = Vec::new();
    let archive = directory.join("removed-pages");
    let Some(_) = regular_archive_folder(&archive)? else {
        return Ok(versions);
    };
    let prefix = format!("{}-", key.platform.0);
    let file = format!("{}.md", key.name);
    for entry in fs::read_dir(archive)? {
        let entry = entry?;
        let folder = entry.file_name();
        if !folder
            .to_str()
            .is_some_and(|name| name.starts_with(&prefix))
        {
            continue;
        }
        let Ok(Some(metadata)) = regular_archive_folder(&entry.path()) else {
            continue;
        };
        let Ok(archived) = fs::symlink_metadata(entry.path().join(&file)) else {
            continue;
        };
        if archived.is_file() && archived.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0 {
            versions.push(Version {
                folder,
                time: metadata.created().or_else(|_| metadata.modified()).ok(),
            });
        }
    }
    versions.sort_unstable_by(|left, right| {
        (right.time, &right.folder).cmp(&(left.time, &left.folder))
    });
    Ok(versions)
}

fn archived_draft(directory: &Path, key: &Key, folder: &std::ffi::OsStr) -> Result<String> {
    let archive = directory.join("removed-pages");
    regular_archive_folder(&archive)?.context("The private manual archive is unavailable")?;
    let version = archive.join(folder);
    regular_archive_folder(&version)?.context("This archived version is unavailable")?;
    let path = version.join(format!("{}.md", key.name));
    read_draft(open_page(&path)?.context("This archived Markdown file is unavailable")?)
}

fn regular_archive_folder(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                metadata.is_dir()
                    && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
                "The private manual archive must be a regular folder"
            );
            Ok(Some(metadata))
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("Reading the private manual archive"),
    }
}
