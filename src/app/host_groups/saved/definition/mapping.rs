use super::*;
use crate::app::host_groups::filter::{Filter, PROFILE_PAGE_SIZE};

pub enum Change {
    Filter(String),
    Page(usize),
    Choose(usize),
    Cancel,
}

struct Candidate {
    name: String,
    address: String,
    options: SshOptions,
}

pub(super) struct Picker {
    pub(super) index: usize,
    pub(super) revision: u64,
    candidates: Vec<Candidate>,
    filter: Filter,
    page: usize,
}

impl Picker {
    pub(super) fn new(index: usize, revision: u64, catalog: &Profiles) -> Self {
        let candidates = catalog
            .profile_order
            .iter()
            .filter_map(|name| {
                let options = catalog.ssh_host_group_member(name)?;
                Some(Candidate {
                    name: name.clone(),
                    address: address::display(&options.user, &options.host, options.port),
                    options: options.clone(),
                })
            })
            .collect();
        Self {
            index,
            revision,
            candidates,
            filter: Filter::default(),
            page: 0,
        }
    }

    fn matching(&self) -> impl Iterator<Item = (usize, &Candidate)> {
        self.candidates.iter().enumerate().filter(|(_, candidate)| {
            self.filter
                .matches_profile(&candidate.name, Some(&candidate.address))
        })
    }

    pub(super) fn change(&mut self, change: Change) -> Option<LocalProfile> {
        match change {
            Change::Filter(query) => {
                if self.filter.query() != query {
                    self.filter.set_query(query);
                    self.page = 0;
                }
            }
            Change::Page(page) => {
                if page < self.matching().count().div_ceil(PROFILE_PAGE_SIZE) {
                    self.page = page;
                }
            }
            Change::Choose(index) => {
                return self
                    .matching()
                    .skip(self.page * PROFILE_PAGE_SIZE)
                    .take(PROFILE_PAGE_SIZE)
                    .find(|(position, _)| *position == index)
                    .map(|(_, candidate)| LocalProfile {
                        name: candidate.name.clone(),
                        options: Some(candidate.options.clone()),
                    });
            }
            Change::Cancel => {}
        }
        None
    }

    pub(super) fn view(
        &self,
        source: &str,
        selected: &str,
        send: impl Fn(Edit) -> Message + Copy + 'static,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let revision = self.revision;
        let action = move |change| send(Edit::Pick(revision, change));
        let count = self.matching().count();
        let pages = count.div_ceil(PROFILE_PAGE_SIZE).max(1);
        let previous = self.page.saturating_sub(1);
        let next = self.page + 1;
        let rows = self
            .matching()
            .skip(self.page * PROFILE_PAGE_SIZE)
            .take(PROFILE_PAGE_SIZE)
            .map(|(index, candidate)| {
                KeyedView::new(
                    candidate.name.clone(),
                    Button::new()
                        .automation_name(format!("Use local profile {}", candidate.name))
                        .on_click(ctx.callback(move |_| action(Change::Choose(index))))
                        .content(format!("{} · {}", candidate.name, candidate.address)),
                )
            });
        Grid::new()
            .margin(16.0)
            .row_spacing(8.0)
            .rows([GridLength::Auto, GridLength::STAR])
            .children((
                StackPanel::new().spacing(8.0).children((
                    TextBlock::new()
                        .font_size(24.0)
                        .text(format!("Choose local profile for {source}")),
                    TextBlock::new().text(format!("Current match · {selected}")),
                    TextBox::new()
                        .automation_name("Find local SSH profiles")
                        .placeholder_text("Find by profile name or SSH address")
                        .text(self.filter.query())
                        .on_text_changed(ctx.callback(move |query| action(Change::Filter(query)))),
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            Button::new()
                                .is_enabled(self.page != 0)
                                .on_click(ctx.callback(move |_| action(Change::Page(previous))))
                                .content("Previous profiles"),
                            Button::new()
                                .is_enabled(next < pages)
                                .on_click(ctx.callback(move |_| action(Change::Page(next))))
                                .content("Next profiles"),
                            Button::new()
                                .on_click(ctx.callback(move |_| action(Change::Cancel)))
                                .content("Back to import review"),
                        )),
                    if count == 0 {
                        View::from(TextBlock::new().text("No matching local SSH profiles"))
                    } else {
                        TextBlock::new()
                            .text(format!("Page {next} of {pages}"))
                            .into()
                    },
                )),
                ScrollViewer::new()
                    .grid_row(1)
                    .content(StackPanel::new().spacing(4.0).keyed_children(rows)),
            ))
    }
}
