use super::*;
use std::path::PathBuf;
use w11term::sftp::{
    GroupUploadFile, TransferProgress, group_download_name, validate_group_upload_path,
};

mod run;
use run::{Batch, Outcome, Task};

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
        !matches!(state, TargetState::Authentication)
            && (self == Self::Download || !matches!(state, TargetState::ReadOnly))
    }
}

pub enum Local {
    Upload(GroupUploadFile),
    Download(PathBuf),
}

impl Local {
    fn path(&self) -> &std::path::Path {
        match self {
            Self::Upload(file) => file.path(),
            Self::Download(folder) => folder,
        }
    }

    fn size(&self) -> Option<u64> {
        match self {
            Self::Upload(file) => Some(file.size()),
            Self::Download(_) => None,
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
    local: PathBuf,
    remote: String,
    batch: Batch,
    rows: Vec<Row>,
}

struct Row {
    pane: u64,
    name: String,
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
    Start { local: PathBuf, remote: String },
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
        let path_ok = validate_group_upload_path(&self.remote).is_ok();
        let local = self.local.as_ref().map(|local| local.path().to_owned());
        let ready = local.is_some() && path_ok && !self.choosing && count > 0;
        let remote = self.remote.clone();
        let (title, picker, empty, help, destination) = match self.direction {
            Direction::Upload => (
                "Upload to selected SSH hosts",
                "Choose local file…",
                "Choose the local file to upload.",
                "Use an absolute path with a filename. Existing files are kept.",
                "Upload to",
            ),
            Direction::Download => (
                "Download from selected SSH hosts",
                "Choose local folder…",
                "Choose the local folder for the downloaded files.",
                "Use an absolute path with a filename. Each host gets a separate file in a new folder.",
                "Download from",
            ),
        };
        let hosts = if count == 1 { "host" } else { "hosts" };
        let label = format!("{destination} {count} {hosts}");
        let local_label = self.local.as_ref().map_or_else(
            || empty.into(),
            |local| match local.size() {
                Some(bytes) => format!("{} · {bytes} bytes", local.path().display()),
                None => local.path().display().to_string(),
            },
        );
        let rows = self.offers.iter().map(|offer| {
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
                    TextBlock::new().text("Remote file path"),
                    TextBox::new()
                        .automation_name("Remote file path")
                        .placeholder_text("/srv/incoming/report.txt")
                        .text(&self.remote)
                        .on_text_changed(ctx.callback(move |text| action(Form::Remote(text)))),
                    context_label(help),
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            Button::new()
                                .style(ButtonStyle::Accent)
                                .is_enabled(ready)
                                .on_click(ctx.callback(move |_| {
                                    local.as_ref().map_or(Message::ControlIdle, |local| {
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
                ScrollViewer::new()
                    .grid_row(1)
                    .content(StackPanel::new().spacing(6.0).keyed_children(rows)),
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
                "uploaded",
                "Cancel uploads",
                "Cancelling uploads…",
            ),
            Direction::Download => (
                "SSH group download results",
                "downloaded",
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
                if self.direction == Direction::Download && matches!(outcome, Outcome::Complete(_))
                {
                    let filename = group_download_name(row.pane, &self.remote);
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
                            .as_deref()
                            .unwrap_or(&self.local)
                            .display()
                            .to_string(),
                    ),
                    context_label(format!("Remote file · {}", self.remote)),
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
            Direction::Upload => "Upload file…",
            Direction::Download => "Download file…",
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
        let purpose = match direction {
            Direction::Upload => file_picker::Purpose::UploadFile,
            Direction::Download => file_picker::Purpose::DownloadFolder,
        };
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
                    file_picker::pick(owner, purpose)
                        .and_then(|selection| {
                            selection
                                .map(|selection| match direction {
                                    Direction::Upload => {
                                        GroupUploadFile::open(selection.path).map(Local::Upload)
                                    }
                                    Direction::Download => Ok(Local::Download(selection.path)),
                                })
                                .transpose()
                        })
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
        selected_path: PathBuf,
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
        if review.remote != remote
            || review.choosing
            || validate_group_upload_path(&remote).is_err()
            || review
                .local
                .as_ref()
                .is_none_or(|local| local.path() != selected_path)
        {
            return;
        }
        let Some(local) = review.local.take() else {
            return;
        };
        let direction = review.direction;
        let offers = review.offers.clone();
        let mut states = Vec::with_capacity(offers.len());
        let mut rows = Vec::with_capacity(offers.len());
        let mut tasks = Vec::new();
        for (index, offer) in offers.into_iter().enumerate() {
            let (progress, displayed) = watch::channel(TransferProgress {
                bytes: 0,
                total: local.size(),
                files: None,
            });
            rows.push(Row {
                pane: offer.pane,
                name: offer.name,
                progress: displayed,
            });
            let Some(expected) = offer.generation else {
                states.push(Outcome::Unavailable(
                    "Transfer was unavailable when reviewed.",
                ));
                continue;
            };
            if !selected.contains(&offer.pane) {
                states.push(Outcome::Unavailable(
                    "The selected hosts changed; transfer was not started.",
                ));
                continue;
            }
            match self.group_transfer_connection(offer.pane, direction) {
                Some((session, actual)) if actual == expected => {
                    tasks.push(Task {
                        index,
                        pane: offer.pane,
                        generation: expected,
                        input: session.input.clone(),
                        progress,
                    });
                    states.push(Outcome::Queued);
                }
                _ => states.push(Outcome::Unavailable(match direction {
                    Direction::Upload => "The SSH connection changed; upload was not started.",
                    Direction::Download => "The SSH connection changed; download was not started.",
                })),
            }
        }
        let batch = Batch::new(states);
        let Some(page) = self.host_group_mut(id) else {
            return;
        };
        page.transfer.view = Some(Current::Results(Report {
            direction,
            local: selected_path,
            remote: remote.clone(),
            batch: batch.clone(),
            rows,
        }));
        if !tasks.is_empty() {
            ctx.spawn_background_with_rejection(
                move |cancel| {
                    if cancel.is_cancelled() {
                        batch.cancel.cancel()
                    }
                    batch.run(tasks, local, remote);
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
                            Ok(Some(local)) => review.local = Some(local),
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
