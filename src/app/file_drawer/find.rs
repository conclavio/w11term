use super::*;
use w11term::sftp::{FilenameMatch, FilenameSearch, valid_filename_query};

const RESULTS_PER_PAGE: usize = 25;

#[derive(Clone)]
pub enum Action {
    Show(u64),
    Query(u64, String),
    Run(u64),
    Cancel(u64),
    Page(u64, usize),
    Reveal(u64, FilenameMatch),
    Back(u64),
}

pub(super) struct Panel {
    serial: u64,
    root: String,
    query: String,
    result: Option<FilenameSearch>,
    page: usize,
    pub(super) cancel: Option<TransferCancel>,
}

impl Panel {
    fn page_count(&self) -> usize {
        self.result
            .as_ref()
            .map_or(0, |r| r.matches.len().div_ceil(RESULTS_PER_PAGE))
    }

    pub(super) fn view(
        &self,
        files: &FileDrawer,
        pane: u64,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let generation = files.generation;
        let serial = self.serial;
        let button = |label: &str, action: Action, enabled: bool| {
            Button::new()
                .automation_name(label)
                .is_enabled(enabled)
                .on_click(ctx.callback(move |_| {
                    Message::Files(FileAction::Search(pane, generation, action.clone()))
                }))
                .content(label)
        };
        let running = self.cancel.as_ref();
        let mut rows = vec![
            KeyedView::new("back", button("Back to files", Action::Back(serial), true)),
            KeyedView::new(
                "path",
                TextBlock::new()
                    .text(format!("Find files in {}", self.root))
                    .text_wrapping(TextWrapping::Wrap),
            ),
            KeyedView::new(
                "query",
                TextBox::new()
                    .automation_name("Filename contains")
                    .placeholder_text("Filename contains")
                    .is_enabled(running.is_none())
                    .text(&self.query)
                    .on_text_changed(ctx.callback(move |text| {
                        Message::Files(FileAction::Search(
                            pane,
                            generation,
                            Action::Query(serial, text),
                        ))
                    })),
            ),
        ];
        rows.push(KeyedView::new(
            "run",
            if let Some(cancel) = running {
                button(
                    "Cancel search",
                    Action::Cancel(serial),
                    !cancel.is_cancelled(),
                )
            } else {
                button(
                    "Search",
                    Action::Run(serial),
                    !files.busy() && valid_filename_query(&self.query),
                )
            },
        ));
        if !files.status.is_empty() {
            rows.push(KeyedView::new(
                "status",
                TextBlock::new()
                    .text(&files.status)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        if self.page_count() > 1 {
            rows.push(KeyedView::new(
                "page",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(6.0)
                    .children((
                        button(
                            "Previous results",
                            Action::Page(serial, self.page.saturating_sub(1)),
                            self.page > 0,
                        ),
                        button(
                            "Next results",
                            Action::Page(serial, self.page + 1),
                            self.page + 1 < self.page_count(),
                        ),
                    )),
            ));
        }
        let matches = self
            .result
            .iter()
            .flat_map(|result| &result.matches)
            .enumerate()
            .skip(self.page * RESULTS_PER_PAGE)
            .take(RESULTS_PER_PAGE)
            .map(|(index, hit)| {
                let path = format!("{}/{}", hit.parent.trim_end_matches('/'), hit.name);
                KeyedView::new(
                    index,
                    Button::new()
                        .automation_name(format!("Reveal {path}"))
                        .horizontal_alignment(HorizontalAlignment::Stretch)
                        .on_click({
                            let hit = hit.clone();
                            ctx.callback(move |_| {
                                Message::Files(FileAction::Search(
                                    pane,
                                    generation,
                                    Action::Reveal(serial, hit.clone()),
                                ))
                            })
                        })
                        .content(
                            TextBlock::new()
                                .text(path)
                                .text_wrapping(TextWrapping::Wrap),
                        ),
                )
            });
        Border::new()
            .padding(10.0)
            .background(Color::rgb(31, 32, 35))
            .content(
                Grid::new()
                    .rows([GridLength::Auto, GridLength::Star(1.0)])
                    .children((
                        StackPanel::new().spacing(6.0).keyed_children(rows),
                        ScrollViewer::new()
                            .grid_row(1)
                            .automation_name("SFTP search results")
                            .content(StackPanel::new().spacing(4.0).keyed_children(matches)),
                    )),
            )
    }
}

impl FileDrawer {
    pub(super) fn close_search(&mut self) {
        if let Some(panel) = self.search.take() {
            if let Some(cancel) = panel.cancel {
                cancel.cancel();
            }
            self.status.clear();
        }
    }

    pub(super) fn finish_search(&mut self, serial: u64, result: Result<Value, String>) {
        let Some(panel) = self.search.as_mut().filter(|panel| panel.serial == serial) else {
            return;
        };
        panel.cancel = None;
        self.status = match result.and_then(|value| {
            serde_json::from_value::<FilenameSearch>(value).map_err(|error| error.to_string())
        }) {
            Ok(result) => {
                let mut status = format!(
                    "{} matches in {} folders",
                    result.matches.len(),
                    result.scanned_directories,
                );
                if result.cancelled {
                    status.push_str(". Search cancelled");
                } else if result.truncated {
                    status.push_str(". Search limit reached");
                }
                if result.skipped_directories > 0 {
                    use std::fmt::Write;
                    let _ = write!(
                        status,
                        ". {} unavailable folders skipped",
                        result.skipped_directories
                    );
                }
                panel.result = Some(result);
                status
            }
            Err(error) => error,
        };
    }

    pub(super) fn filename_search(
        &mut self,
        pane: u64,
        session: &Session,
        action: Action,
        ctx: &ComponentContext<TerminalApp>,
    ) -> Result<()> {
        if let Action::Show(revision) = action {
            if self.busy() || !self.open || self.listing_revision != revision {
                return Ok(());
            }
            let Some(root) = self.directory.clone() else {
                return Ok(());
            };
            self.end_filter_edit();
            self.dialog_serial = self.dialog_serial.wrapping_add(1);
            self.status.clear();
            self.search = Some(Panel {
                serial: self.dialog_serial,
                root,
                query: String::new(),
                result: None,
                page: 0,
                cancel: None,
            });
            return Ok(());
        }
        let busy = self.busy();
        let Some(panel) = self.search.as_mut() else {
            return Ok(());
        };
        match action {
            Action::Query(serial, query) if serial == panel.serial && panel.cancel.is_none() => {
                if panel.query != query {
                    panel.query = query;
                    panel.result = None;
                    self.status.clear();
                }
            }
            Action::Run(serial)
                if serial == panel.serial && !busy && valid_filename_query(&panel.query) =>
            {
                let op = Operation::Find {
                    path: panel.root.clone(),
                    query: panel.query.clone(),
                };
                panel.result = None;
                panel.page = 0;
                panel.cancel = Some(TransferCancel::default());
                self.status = "Searching folders…".into();
                if let Err(error) = self.job(pane, session, op, Step::Search(serial), ctx) {
                    self.finish_search(serial, Err(format!("{error:#}")));
                }
            }
            Action::Cancel(serial) if serial == panel.serial => {
                if let Some(cancel) = &panel.cancel {
                    cancel.cancel();
                    self.status = "Cancelling search…".into();
                }
            }
            Action::Page(serial, page) if serial == panel.serial && page < panel.page_count() => {
                panel.page = page;
            }
            Action::Reveal(serial, hit)
                if serial == panel.serial
                    && !busy
                    && panel.result.as_ref().is_some_and(|result| {
                        result
                            .matches
                            .iter()
                            .any(|item| item.parent == hit.parent && item.name == hit.name)
                    }) =>
            {
                self.following = false;
                self.browse(pane, session, hit.parent, None, ctx)?;
                self.reveal = Some(RevealTarget::Existing(hit.name));
            }
            Action::Back(serial) if serial == panel.serial => {
                self.close_search();
                self.status.clear();
            }
            _ => {}
        }
        Ok(())
    }
}
