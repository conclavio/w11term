use super::*;
use std::path::PathBuf;
use w11term::sftp::{
    GroupUploadFile, GroupUploadSet, TransferProgress, group_download_name, group_download_paths,
};

mod run;
use run::{Batch, Item, Outcome, Task};

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Direction {
    Upload,
    Download,
}

impl Direction {
    fn name(self) -> &'static str {
        match self {
            Self::Upload => "Upload",
            Self::Download => "Download",
        }
    }

    fn allows(self, state: TargetState) -> bool {
        !state.is_authenticating()
            && (self == Self::Download || !matches!(state, TargetState::ReadOnly))
    }
}

pub enum Local {
    Upload(GroupUploadSet),
    Download(PathBuf),
}

enum Plan {
    Upload(Vec<(GroupUploadFile, String)>),
    Download {
        parent: PathBuf,
        files: Vec<DownloadFile>,
    },
}

struct DownloadFile {
    remote: String,
    number: Option<usize>,
}

impl Local {
    fn paths(&self) -> Vec<PathBuf> {
        match self {
            Self::Upload(set) => set
                .files()
                .iter()
                .map(|file| file.path().to_owned())
                .collect(),
            Self::Download(folder) => vec![folder.clone()],
        }
    }

    fn upload_files(&self) -> &[GroupUploadFile] {
        match self {
            Self::Upload(set) => set.files(),
            Self::Download(_) => &[],
        }
    }

    fn valid_remote(&self, remote: &str) -> bool {
        match self {
            Self::Upload(set) => set.destinations(remote).is_ok(),
            Self::Download(_) => group_download_paths(remote).is_ok(),
        }
    }

    fn plan(&self, remote: &str) -> anyhow::Result<Plan> {
        Ok(match self {
            Self::Upload(set) => Plan::Upload(
                set.files()
                    .iter()
                    .cloned()
                    .zip(set.destinations(remote)?)
                    .collect(),
            ),
            Self::Download(parent) => {
                let paths = group_download_paths(remote)?;
                let multiple = paths.len() > 1;
                Plan::Download {
                    parent: parent.clone(),
                    files: paths
                        .into_iter()
                        .enumerate()
                        .map(|(index, path)| DownloadFile {
                            remote: path.to_owned(),
                            number: multiple.then_some(index + 1),
                        })
                        .collect(),
                }
            }
        })
    }

    fn label(&self) -> String {
        match self {
            Self::Upload(set) => match set.files() {
                [file] => format!("{} · {} bytes", file.path().display(), file.size()),
                files => format!("{} local files", files.len()),
            },
            Self::Download(folder) => folder.display().to_string(),
        }
    }
}

#[derive(Default)]
pub(super) struct State {
    revision: u64,
    view: Option<Current>,
}

enum Current {
    Review(Review),
    Results(Report),
}

#[derive(Clone)]
struct Offer {
    pane: u64,
    name: String,
    generation: Option<u64>,
}

struct Review {
    direction: Direction,
    offers: Vec<Offer>,
    local: Option<Local>,
    remote: String,
    choosing: bool,
    feedback: String,
}

struct Report {
    direction: Direction,
    local: String,
    remote: String,
    multiple: bool,
    batch: Batch,
    rows: Vec<Row>,
}

struct Row {
    name: String,
    filename: Option<String>,
    progress: watch::Receiver<TransferProgress>,
}

impl Drop for Report {
    fn drop(&mut self) {
        self.batch.cancel.cancel();
    }
}

pub enum Action {
    Open(Direction),
    Form(u64, Form),
}

pub enum Form {
    Back,
    Pick,
    Picked(Result<Option<Local>, String>),
    Remote(String),
    Start { local: Vec<PathBuf>, remote: String },
    Cancel,
    Unavailable,
}

impl State {
    pub(super) fn is_open(&self) -> bool {
        self.view.is_some()
    }

    fn review_mut(&mut self, revision: u64) -> Option<&mut Review> {
        match &mut self.view {
            Some(Current::Review(review)) if self.revision == revision => Some(review),
            _ => None,
        }
    }

    pub(super) fn view(&self, id: TabId, ctx: &ViewContext<TerminalApp>) -> Option<View> {
        let revision = self.revision;
        let action = move |form| {
            Message::HostGroup(super::Action::Transfer(id, Action::Form(revision, form)))
        };
        match self.view.as_ref()? {
            Current::Review(review) => Some(review.view(action, ctx)),
            Current::Results(report) => Some(report.view(action, ctx)),
        }
    }
}

fn context_label(text: impl Into<String>) -> View {
    TextBlock::new()
        .text(text)
        .text_wrapping(TextWrapping::Wrap)
        .into()
}

impl Review {
    fn view(
        &self,
        action: impl Fn(Form) -> Message + Copy + 'static,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let count = self
            .offers
            .iter()
            .filter(|offer| offer.generation.is_some())
            .count();
        let local = self.local.as_ref();
        let ready = local.is_some_and(|local| local.valid_remote(&self.remote))
            && !self.choosing
            && count > 0;
        let files = local.map_or(&[][..], Local::upload_files);
        let multiple = files.len() > 1;
        let remote = self.remote.clone();
        let (title, picker, empty, help, destination) = match self.direction {
            Direction::Upload => (
                "Upload to selected SSH hosts",
                "Choose local files…",
                "Choose the local files to upload.",
                if multiple {
                    "Use an absolute directory. Files keep their names. Existing files are kept."
                } else {
                    "Use an absolute path with a filename. Existing files are kept."
                },
                "Upload to",
            ),
            Direction::Download => (
                "Download from selected SSH hosts",
                "Choose local folder…",
                "Choose the local folder for the downloaded files.",
                "Enter one absolute file path per line. The files are saved in a new local folder.",
                "Download from",
            ),
        };
        let hosts = if count == 1 { "host" } else { "hosts" };
        let label = format!("{destination} {count} {hosts}");
        let local_label = local.map_or_else(|| empty.into(), Local::label);
        let local_paths = local.map(Local::paths);
        let path_label = match self.direction {
            Direction::Download => "Remote file paths",
            Direction::Upload if multiple => "Remote directory",
            Direction::Upload => "Remote file path",
        };
        let path_editor = TextBox::new()
            .automation_name(path_label)
            .placeholder_text(if multiple {
                "/srv/incoming"
            } else {
                "/srv/incoming/report.txt"
            })
            .text(&self.remote)
            .on_text_changed(ctx.callback(move |text| action(Form::Remote(text))));
        let path_editor = if self.direction == Direction::Download {
            path_editor.height(88.0).accepts_return(true)
        } else {
            path_editor
        };
        let paths = files
            .iter()
            .enumerate()
            .filter(|_| multiple)
            .map(|(index, file)| {
                KeyedView::new(
                    format!("file-{index}"),
                    context_label(file.path().display().to_string()),
                )
            });
        let hosts = self.offers.iter().map(|offer| {
            let label = if offer.generation.is_some() {
                offer.name.clone()
            } else {
                format!("{} · {} unavailable", offer.name, self.direction.name())
            };
            KeyedView::new(offer.pane.to_string(), context_label(label))
        });
        Grid::new()
            .margin(16.0)
            .row_spacing(8.0)
            .rows([GridLength::Auto, GridLength::STAR])
            .children((
                StackPanel::new().spacing(8.0).children((
                    TextBlock::new().font_size(24.0).text(title),
                    context_label(local_label),
                    Button::new()
                        .is_enabled(!self.choosing)
                        .on_click(ctx.callback(move |_| action(Form::Pick)))
                        .content(picker),
                    TextBlock::new().text(path_label),
                    path_editor,
                    context_label(help),
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            Button::new()
                                .style(ButtonStyle::Accent)
                                .is_enabled(ready)
                                .on_click(ctx.callback(move |_| {
                                    local_paths.as_ref().map_or(Message::ControlIdle, |local| {
                                        action(Form::Start {
                                            local: local.clone(),
                                            remote: remote.clone(),
                                        })
                                    })
                                }))
                                .content(label),
                            Button::new()
                                .on_click(ctx.callback(move |_| action(Form::Back)))
                                .content("Back to host group"),
                        )),
                    if self.feedback.is_empty() {
                        View::empty()
                    } else {
                        context_label(&self.feedback)
                    },
                )),
                ScrollViewer::new().grid_row(1).content(
                    StackPanel::new()
                        .spacing(6.0)
                        .keyed_children(paths.chain(hosts)),
                ),
            ))
    }
}

impl Report {
    fn view(
        &self,
        action: impl Fn(Form) -> Message + Copy + 'static,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let outcomes = self.batch.snapshot();
        let pending = outcomes.iter().filter(|outcome| outcome.pending()).count();
        let successes = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, Outcome::Complete(_)))
            .count();
        let cancelling = self.batch.cancel.is_cancelled();
        let (title, complete, cancel, cancelling_text) = match self.direction {
            Direction::Upload => (
                "SSH group upload results",
                if self.multiple {
                    "files uploaded"
                } else {
                    "uploaded"
                },
                "Cancel uploads",
                "Cancelling uploads…",
            ),
            Direction::Download => (
                "SSH group download results",
                if self.multiple {
                    "files downloaded"
                } else {
                    "downloaded"
                },
                "Cancel downloads",
                "Cancelling downloads…",
            ),
        };
        let status = format!(
            "{successes} {complete} · {pending} pending · {} skipped, failed or cancelled",
            outcomes.len() - successes - pending
        );
        let details = self
            .rows
            .iter()
            .zip(outcomes)
            .enumerate()
            .map(|(index, (row, outcome))| {
                let mut caption = outcome.caption(self.direction, *row.progress.borrow());
                if let Some(filename) = &row.filename
                    && matches!(outcome, Outcome::Complete(_))
                {
                    caption.push_str(&format!(" · {filename}"));
                }
                KeyedView::new(
                    index.to_string(),
                    context_label(format!("{} · {caption}", row.name)),
                )
            });
        Grid::new()
            .margin(16.0)
            .row_spacing(8.0)
            .rows([GridLength::Auto, GridLength::STAR])
            .children((
                StackPanel::new().spacing(8.0).children((
                    TextBlock::new().font_size(24.0).text(title),
                    context_label(
                        self.batch
                            .folder()
                            .map_or_else(|| self.local.clone(), |path| path.display().to_string()),
                    ),
                    context_label(&self.remote),
                    TextBlock::new().text(status),
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            Button::new()
                                .is_enabled(pending > 0 && !cancelling)
                                .on_click(ctx.callback(move |_| action(Form::Cancel)))
                                .content(if cancelling && pending > 0 {
                                    cancelling_text
                                } else {
                                    cancel
                                }),
                            Button::new()
                                .is_enabled(pending == 0)
                                .on_click(ctx.callback(move |_| action(Form::Back)))
                                .content("Back to host group"),
                        )),
                )),
                ScrollViewer::new()
                    .grid_row(1)
                    .content(StackPanel::new().spacing(8.0).keyed_children(details)),
            ))
    }
}

pub(super) fn button(
    id: TabId,
    selected: &[Target],
    direction: Direction,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    Button::new()
        .is_enabled(
            selected
                .iter()
                .any(|target| target.connected && direction.allows(target.state)),
        )
        .on_click(ctx.callback(move |_| {
            Message::HostGroup(super::Action::Transfer(id, Action::Open(direction)))
        }))
        .content(match direction {
            Direction::Upload => "Upload files…",
            Direction::Download => "Download files…",
        })
}

impl TerminalApp {
    fn group_transfer_connection(
        &self,
        pane: u64,
        direction: Direction,
    ) -> Option<(&Session, u64)> {
        let Page::Terminal(live) = &self.tabs.get(self.tab_for_pane(pane)?)?.page else {
            return None;
        };
        let generation = live.session.frames.load().connection_generation;
        if generation == 0
            || !live.session.is_direct_ssh_shell()
            || (direction == Direction::Upload && live.read_only.get())
            || live.device_busy
            || live.session.prompts.current().is_some()
        {
            return None;
        }
        live.session.ssh_connection_at(generation)?;
        Some((&live.session, generation))
    }

    fn open_host_group_transfer(&mut self, id: TabId, direction: Direction) {
        if self.host_group_command_target() != Some(id) {
            return;
        }
        let roster = self.host_group_targets();
        let Some(page) = self.host_group_mut(id) else {
            return;
        };
        let selected = page.selected_shown(&roster);
        let offers: Vec<_> = roster
            .into_iter()
            .filter(|target| selected.contains(&target.pane))
            .map(|target| Offer {
                pane: target.pane,
                name: target.name,
                generation: self
                    .group_transfer_connection(target.pane, direction)
                    .map(|(_, generation)| generation),
            })
            .collect();
        if offers.iter().any(|offer| offer.generation.is_some())
            && let Some(page) = self.host_group_mut(id)
        {
            page.transfer.revision += 1;
            page.transfer.view = Some(Current::Review(Review {
                direction,
                offers,
                local: None,
                remote: String::new(),
                choosing: false,
                feedback: String::new(),
            }));
        }
    }

    fn pick_host_group_transfer(&mut self, id: TabId, revision: u64, ctx: &ComponentContext<Self>) {
        if self.tabs.active_id() != Some(id) {
            return;
        }
        let owner = self.keyboard.hwnd();
        let Some(review) = self
            .host_group_mut(id)
            .and_then(|page| page.transfer.review_mut(revision))
        else {
            return;
        };
        if review.choosing {
            return;
        }
        let Some(owner) = owner else {
            review.feedback = "The window is unavailable.".into();
            return;
        };
        review.choosing = true;
        review.feedback.clear();
        let direction = review.direction;
        let owner = owner.0 as usize;
        let picked = move |result| {
            Message::HostGroup(super::Action::Transfer(
                id,
                Action::Form(revision, Form::Picked(result)),
            ))
        };
        ctx.spawn_background_with_rejection(
            move |cancel| {
                let result = if cancel.is_cancelled() {
                    Ok(None)
                } else {
                    match direction {
                        Direction::Upload => {
                            file_picker::pick_paths(owner, file_picker::Purpose::UploadFiles)
                                .and_then(|paths| {
                                    if paths.is_empty() {
                                        Ok(None)
                                    } else {
                                        GroupUploadSet::open(paths)
                                            .map(|set| Some(Local::Upload(set)))
                                    }
                                })
                        }
                        Direction::Download => {
                            file_picker::pick(owner, file_picker::Purpose::DownloadFolder)
                                .map(|found| found.map(|found| Local::Download(found.path)))
                        }
                    }
                    .map_err(|error| format!("{error:#}"))
                };
                picked(result)
            },
            picked(Err("Could not open the file picker.".into())),
        );
    }

    fn start_host_group_transfer(
        &mut self,
        id: TabId,
        revision: u64,
        selected_paths: Vec<PathBuf>,
        remote: String,
        ctx: &ComponentContext<Self>,
    ) {
        if self.tabs.active_id() != Some(id) {
            return;
        }
        let roster = self.host_group_targets();
        let Some(page) = self.host_group_mut(id) else {
            return;
        };
        let selected = page.selected_shown(&roster);
        let Some(review) = page.transfer.review_mut(revision) else {
            return;
        };
        let Some(local) = review.local.as_ref() else {
            return;
        };
        if review.remote != remote || review.choosing || local.paths() != selected_paths {
            return;
        }
        let Ok(plan) = local.plan(&remote) else {
            return;
        };
        let direction = review.direction;
        let offers = review.offers.clone();
        let local = local.label();
        let (file_count, remote) = match &plan {
            Plan::Upload(files) if files.len() > 1 => {
                (files.len(), format!("Remote directory · {remote}"))
            }
            Plan::Upload(_) => (1, format!("Remote file · {remote}")),
            Plan::Download { files, .. } if files.len() > 1 => {
                (files.len(), format!("{} remote files", files.len()))
            }
            Plan::Download { files, .. } => (1, format!("Remote file · {}", files[0].remote)),
        };
        let multiple = file_count > 1;
        let mut states = Vec::new();
        let mut rows = Vec::new();
        let mut tasks = Vec::new();
        for offer in offers {
            let connection = match offer.generation {
                None => Err("Transfer was unavailable when reviewed."),
                Some(_) if !selected.contains(&offer.pane) => {
                    Err("The selected hosts changed; transfer was not started.")
                }
                Some(expected) => self
                    .group_transfer_connection(offer.pane, direction)
                    .filter(|(_, actual)| *actual == expected)
                    .ok_or(match direction {
                        Direction::Upload => "The SSH connection changed; upload was not started.",
                        Direction::Download => {
                            "The SSH connection changed; download was not started."
                        }
                    }),
            };
            let mut items = Vec::new();
            for file in 0..file_count {
                let (label, filename, total) = match &plan {
                    Plan::Upload(files) => {
                        let source = &files[file].0;
                        let label = multiple.then(|| {
                            source
                                .path()
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                        });
                        (label, None, Some(source.size()))
                    }
                    Plan::Download { files, .. } => {
                        let file = &files[file];
                        (
                            multiple.then(|| file.remote.as_str().into()),
                            Some(group_download_name(offer.pane, &file.remote, file.number)),
                            None,
                        )
                    }
                };
                let (progress, displayed) = watch::channel(TransferProgress {
                    bytes: 0,
                    total,
                    files: None,
                });
                rows.push(Row {
                    name: label.map_or_else(
                        || offer.name.clone(),
                        |label| format!("{} · {label}", offer.name),
                    ),
                    filename,
                    progress: displayed,
                });
                if connection.is_ok() {
                    items.push(Item {
                        index: states.len(),
                        progress,
                    });
                }
                states.push(
                    connection
                        .err()
                        .map_or(Outcome::Queued, Outcome::Unavailable),
                );
            }
            if let Ok((session, generation)) = connection {
                tasks.push(Task {
                    pane: offer.pane,
                    generation,
                    input: session.input.clone(),
                    items,
                });
            }
        }
        let batch = Batch::new(states);
        let Some(page) = self.host_group_mut(id) else {
            return;
        };
        page.transfer.view = Some(Current::Results(Report {
            direction,
            local,
            remote,
            multiple,
            batch: batch.clone(),
            rows,
        }));
        if !tasks.is_empty() {
            ctx.spawn_background_with_rejection(
                move |cancel| {
                    if cancel.is_cancelled() {
                        batch.cancel.cancel()
                    }
                    batch.run(tasks, plan);
                    Message::ControlIdle
                },
                Message::HostGroup(super::Action::Transfer(
                    id,
                    Action::Form(revision, Form::Unavailable),
                )),
            );
        }
    }

    pub(super) fn handle_host_group_transfer(
        &mut self,
        id: TabId,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        match action {
            Action::Open(direction) => self.open_host_group_transfer(id, direction),
            Action::Form(revision, Form::Pick) => self.pick_host_group_transfer(id, revision, ctx),
            Action::Form(revision, Form::Start { local, remote }) => {
                self.start_host_group_transfer(id, revision, local, remote, ctx)
            }
            Action::Form(revision, form) => {
                if !matches!(form, Form::Picked(_) | Form::Unavailable)
                    && self.tabs.active_id() != Some(id)
                {
                    return;
                }
                let Some(Page::HostGroup(page)) = self.tabs.get_mut(id).map(|tab| &mut tab.page)
                else {
                    return;
                };
                if page.transfer.revision != revision {
                    return;
                }
                match (&mut page.transfer.view, form) {
                    (Some(Current::Review(review)), Form::Picked(result)) if review.choosing => {
                        review.choosing = false;
                        match result {
                            Ok(Some(local)) => {
                                let was_directory = review
                                    .local
                                    .as_ref()
                                    .is_some_and(|previous| previous.upload_files().len() > 1);
                                if was_directory != (local.upload_files().len() > 1) {
                                    review.remote.clear();
                                }
                                review.local = Some(local);
                            }
                            Ok(None) => {}
                            Err(error) => {
                                review.feedback =
                                    format!("Could not open the local selection · {error}")
                            }
                        }
                    }
                    (Some(Current::Review(review)), Form::Remote(path)) => review.remote = path,
                    (Some(Current::Review(_)), Form::Back) => page.transfer.view = None,
                    (Some(Current::Results(report)), Form::Back)
                        if !report.batch.snapshot().iter().any(Outcome::pending) =>
                    {
                        page.transfer.view = None
                    }
                    (Some(Current::Results(report)), Form::Cancel) => report.batch.cancel.cancel(),
                    (Some(Current::Results(report)), Form::Unavailable) => report
                        .batch
                        .fail_pending("Could not start the group transfer."),
                    _ => {}
                }
            }
        }
    }
}
