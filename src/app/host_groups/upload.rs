use super::*;
use std::path::PathBuf;
use w11term::sftp::{GroupUploadFile, TransferProgress, validate_group_upload_path};

mod run;
use run::{Batch, Outcome, Task};

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
    offers: Vec<Offer>,
    file: Option<GroupUploadFile>,
    remote: String,
    choosing: bool,
    feedback: String,
}

struct Report {
    source: PathBuf,
    remote: String,
    batch: Batch,
    rows: Vec<(String, watch::Receiver<TransferProgress>)>,
}

impl Drop for Report {
    fn drop(&mut self) {
        self.batch.cancel.cancel();
    }
}

pub enum Action {
    Open,
    Form(u64, Form),
}

pub enum Form {
    Back,
    Pick,
    Picked(Result<Option<GroupUploadFile>, String>),
    Remote(String),
    Start { source: PathBuf, remote: String },
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
        let action =
            move |form| Message::HostGroup(super::Action::Upload(id, Action::Form(revision, form)));
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
        let source = self.file.as_ref().map(|file| file.path().to_owned());
        let ready = source.is_some() && path_ok && !self.choosing && count > 0;
        let remote = self.remote.clone();
        let label = if count == 1 {
            "Upload to 1 host".into()
        } else {
            format!("Upload to {count} hosts")
        };
        let file_label = self.file.as_ref().map_or_else(
            || "Choose the local file to upload.".into(),
            |file| format!("{} · {} bytes", file.path().display(), file.size()),
        );
        let rows = self.offers.iter().map(|offer| {
            let label = if offer.generation.is_some() {
                offer.name.clone()
            } else {
                format!("{} · Upload unavailable", offer.name)
            };
            KeyedView::new(offer.pane.to_string(), context_label(label))
        });
        Grid::new()
            .margin(16.0)
            .row_spacing(8.0)
            .rows([GridLength::Auto, GridLength::STAR])
            .children((
                StackPanel::new().spacing(8.0).children((
                    TextBlock::new()
                        .font_size(24.0)
                        .text("Upload to selected SSH hosts"),
                    context_label(file_label),
                    Button::new()
                        .is_enabled(!self.choosing)
                        .on_click(ctx.callback(move |_| action(Form::Pick)))
                        .content("Choose local file…"),
                    TextBlock::new().text("Remote file path"),
                    TextBox::new()
                        .automation_name("Remote file path")
                        .placeholder_text("/srv/incoming/report.txt")
                        .text(&self.remote)
                        .on_text_changed(ctx.callback(move |text| action(Form::Remote(text)))),
                    context_label("Use an absolute path with a filename. Existing files are kept."),
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            Button::new()
                                .style(ButtonStyle::Accent)
                                .is_enabled(ready)
                                .on_click(ctx.callback(move |_| {
                                    source.as_ref().map_or(Message::ControlIdle, |source| {
                                        action(Form::Start {
                                            source: source.clone(),
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
        let status = format!(
            "{successes} uploaded · {pending} pending · {} skipped, failed or cancelled",
            outcomes.len() - successes - pending
        );
        let details = self.rows.iter().zip(outcomes).enumerate().map(
            |(index, ((name, progress), outcome))| {
                let caption = outcome.caption(*progress.borrow());
                KeyedView::new(
                    index.to_string(),
                    context_label(format!("{name} · {caption}")),
                )
            },
        );
        Grid::new()
            .margin(16.0)
            .row_spacing(8.0)
            .rows([GridLength::Auto, GridLength::STAR])
            .children((
                StackPanel::new().spacing(8.0).children((
                    TextBlock::new()
                        .font_size(24.0)
                        .text("SSH group upload results"),
                    context_label(self.source.display().to_string()),
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
                                    "Cancelling uploads…"
                                } else {
                                    "Cancel uploads"
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

pub(super) fn button(id: TabId, selected: &[Target], ctx: &ViewContext<TerminalApp>) -> View {
    Button::new()
        .is_enabled(selected.iter().any(|target| {
            target.connected
                && !matches!(
                    target.state,
                    TargetState::Authentication | TargetState::ReadOnly
                )
        }))
        .on_click(
            ctx.callback(move |_| Message::HostGroup(super::Action::Upload(id, Action::Open))),
        )
        .content("Upload file…")
}

impl TerminalApp {
    fn group_upload_connection(&self, pane: u64) -> Option<(&Session, u64)> {
        let Page::Terminal(live) = &self.tabs.get(self.tab_for_pane(pane)?)?.page else {
            return None;
        };
        let generation = live.session.frames.load().connection_generation;
        if generation == 0
            || !live.session.is_direct_ssh_shell()
            || live.read_only.get()
            || live.device_busy
            || live.session.prompts.current().is_some()
        {
            return None;
        }
        live.session.ssh_connection_at(generation)?;
        Some((&live.session, generation))
    }

    fn open_host_group_upload(&mut self, id: TabId) {
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
                    .group_upload_connection(target.pane)
                    .map(|(_, generation)| generation),
            })
            .collect();
        if offers.iter().any(|offer| offer.generation.is_some())
            && let Some(page) = self.host_group_mut(id)
        {
            page.upload.revision += 1;
            page.upload.view = Some(Current::Review(Review {
                offers,
                file: None,
                remote: String::new(),
                choosing: false,
                feedback: String::new(),
            }));
        }
    }

    fn pick_host_group_upload(&mut self, id: TabId, revision: u64, ctx: &ComponentContext<Self>) {
        if self.tabs.active_id() != Some(id) {
            return;
        }
        let owner = self.keyboard.hwnd();
        let Some(review) = self
            .host_group_mut(id)
            .and_then(|page| page.upload.review_mut(revision))
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
        let owner = owner.0 as usize;
        let picked = move |result| {
            Message::HostGroup(super::Action::Upload(
                id,
                Action::Form(revision, Form::Picked(result)),
            ))
        };
        ctx.spawn_background_with_rejection(
            move |cancel| {
                let result = if cancel.is_cancelled() {
                    Ok(None)
                } else {
                    file_picker::pick(owner, file_picker::Purpose::UploadFile)
                        .and_then(|selection| {
                            selection
                                .map(|selection| GroupUploadFile::open(selection.path))
                                .transpose()
                        })
                        .map_err(|error| format!("{error:#}"))
                };
                picked(result)
            },
            picked(Err("Could not open the file picker.".into())),
        );
    }

    fn start_host_group_upload(
        &mut self,
        id: TabId,
        revision: u64,
        source: PathBuf,
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
        let Some(review) = page.upload.review_mut(revision) else {
            return;
        };
        if review.remote != remote
            || review.choosing
            || validate_group_upload_path(&remote).is_err()
            || review
                .file
                .as_ref()
                .is_none_or(|file| file.path() != source)
        {
            return;
        }
        let Some(file) = review.file.take() else {
            return;
        };
        let offers = review.offers.clone();
        let mut states = Vec::with_capacity(offers.len());
        let mut rows = Vec::with_capacity(offers.len());
        let mut tasks = Vec::new();
        for (index, offer) in offers.into_iter().enumerate() {
            let (progress, displayed) = watch::channel(TransferProgress {
                bytes: 0,
                total: Some(file.size()),
                files: None,
            });
            rows.push((offer.name, displayed));
            let Some(expected) = offer.generation else {
                states.push(Outcome::Unavailable(
                    "Upload was unavailable when reviewed.",
                ));
                continue;
            };
            if !selected.contains(&offer.pane) {
                states.push(Outcome::Unavailable(
                    "The selected hosts changed; upload was not started.",
                ));
                continue;
            }
            match self.group_upload_connection(offer.pane) {
                Some((session, actual)) if actual == expected => {
                    tasks.push(Task {
                        index,
                        generation: expected,
                        input: session.input.clone(),
                        progress,
                    });
                    states.push(Outcome::Queued);
                }
                _ => states.push(Outcome::Unavailable(
                    "The SSH connection changed; upload was not started.",
                )),
            }
        }
        let batch = Batch::new(states);
        let Some(page) = self.host_group_mut(id) else {
            return;
        };
        page.upload.view = Some(Current::Results(Report {
            source,
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
                    batch.upload(tasks, file, remote);
                    Message::ControlIdle
                },
                Message::HostGroup(super::Action::Upload(
                    id,
                    Action::Form(revision, Form::Unavailable),
                )),
            );
        }
    }

    pub(super) fn handle_host_group_upload(
        &mut self,
        id: TabId,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        match action {
            Action::Open => self.open_host_group_upload(id),
            Action::Form(revision, Form::Pick) => self.pick_host_group_upload(id, revision, ctx),
            Action::Form(revision, Form::Start { source, remote }) => {
                self.start_host_group_upload(id, revision, source, remote, ctx)
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
                if page.upload.revision != revision {
                    return;
                }
                match (&mut page.upload.view, form) {
                    (Some(Current::Review(review)), Form::Picked(result)) if review.choosing => {
                        review.choosing = false;
                        match result {
                            Ok(Some(file)) => review.file = Some(file),
                            Ok(None) => {}
                            Err(error) => {
                                review.feedback =
                                    format!("Could not open the chosen file · {error}")
                            }
                        }
                    }
                    (Some(Current::Review(review)), Form::Remote(path)) => review.remote = path,
                    (Some(Current::Review(_)), Form::Back) => page.upload.view = None,
                    (Some(Current::Results(report)), Form::Back)
                        if !report.batch.snapshot().iter().any(Outcome::pending) =>
                    {
                        page.upload.view = None
                    }
                    (Some(Current::Results(report)), Form::Cancel) => report.batch.cancel.cancel(),
                    (Some(Current::Results(report)), Form::Unavailable) => report
                        .batch
                        .fail_pending("Could not start the group uploader."),
                    _ => {}
                }
            }
        }
    }
}
