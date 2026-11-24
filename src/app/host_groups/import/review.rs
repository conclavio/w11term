use super::*;
use std::collections::BTreeMap;

#[derive(Clone, PartialEq, Eq)]
pub struct Approval(BTreeMap<usize, Option<reuse::Connection>>);

pub(super) struct Review {
    pub(super) rows: Vec<list::Row>,
    pub(super) checked: BTreeSet<usize>,
    choices: BTreeMap<usize, Choice>,
}

struct Choice {
    candidates: Vec<reuse::Candidate>,
    selected: Option<reuse::Connection>,
}

#[derive(Default)]
struct Launch {
    reused: BTreeSet<u64>,
    fresh: Vec<(Option<String>, SshOptions)>,
}

impl Launch {
    fn feedback(&self) -> String {
        match (self.reused.len(), self.fresh.len()) {
            (0, 1) => "Opened 1 SSH pane from the host list".into(),
            (0, fresh) => format!("Opened {fresh} SSH panes from the host list"),
            (reused, fresh) => format!("Reused {reused} · Opened {fresh} from the host list"),
        }
    }
}

impl Review {
    fn new(
        rows: Vec<list::Row>,
        candidates: impl Fn(&list::Target) -> Vec<reuse::Candidate>,
    ) -> Self {
        let mut checked = BTreeSet::new();
        let mut choices = BTreeMap::new();
        for (index, row) in rows.iter().enumerate() {
            if let Ok(target) = &row.target {
                checked.insert(index);
                let candidates = candidates(target);
                if !candidates.is_empty() {
                    choices.insert(
                        index,
                        Choice {
                            candidates,
                            selected: None,
                        },
                    );
                }
            }
        }
        Self {
            rows,
            checked,
            choices,
        }
    }

    fn approval(&self) -> Approval {
        Approval(
            self.checked
                .iter()
                .map(|&index| {
                    (
                        index,
                        self.choices.get(&index).and_then(|choice| choice.selected),
                    )
                })
                .collect(),
        )
    }

    pub(super) fn choose(&mut self, index: usize, selected: Option<reuse::Connection>) {
        if self.checked.contains(&index)
            && let Some(choice) = self.choices.get_mut(&index)
            && selected.is_none_or(|connection| {
                choice
                    .candidates
                    .iter()
                    .any(|candidate| candidate.connection == connection)
            })
        {
            choice.selected = selected;
        }
    }

    pub(super) fn reuse_unique(&mut self) {
        for index in &self.checked {
            if let Some(choice) = self.choices.get_mut(index)
                && let [candidate] = &choice.candidates[..]
            {
                choice.selected = Some(candidate.connection);
            }
        }
    }

    pub(super) fn view(
        &self,
        action: impl Fn(Change) -> Message + Copy + 'static,
        cancel: View,
        error: View,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let rows = self.rows.iter().enumerate().map(|(index, row)| {
            let view = match &row.target {
                Ok(target) => {
                    let label = target.label();
                    let checked = self.checked.contains(&index);
                    let choice = self.choices.get(&index).map_or_else(View::empty, |choice| {
                        reuse::connection_picker(
                            &label,
                            &choice.candidates,
                            choice.selected,
                            checked,
                            move |connection| action(Change::Connection(index, connection)),
                            ctx,
                        )
                    });
                    StackPanel::new().spacing(4.0).children((
                        CheckBox::new()
                            .automation_name(&label)
                            .is_checked(checked)
                            .on_is_checked_changed(
                                ctx.callback(move |checked| action(Change::Check(index, checked))),
                            )
                            .content(label),
                        choice,
                    ))
                }
                Err(error) => TextBlock::new()
                    .text(format!("Line {} · {error}", row.line))
                    .text_wrapping(TextWrapping::Wrap)
                    .into(),
            };
            KeyedView::new(index, view)
        });
        let approval = self.approval();
        let reused = approval.0.values().flatten().count();
        let count = approval.0.len();
        let fresh = count - reused;
        let verb = if reused == 0 {
            "Open"
        } else if fresh == 0 {
            "Reuse"
        } else {
            "Use"
        };
        let unit = if count == 1 { "host" } else { "hosts" };
        let unique = if self.choices.is_empty() {
            View::empty()
        } else {
            Button::new()
                .is_enabled(self.checked.iter().any(|index| {
                    self.choices.get(index).is_some_and(|choice| matches!(
                        &choice.candidates[..], [candidate] if choice.selected != Some(candidate.connection)
                    ))
                }))
                .on_click(ctx.callback(move |_| action(Change::ReuseUnique)))
                .content("Reuse unique matches")
        };
        Grid::new()
            .margin(16.0)
            .row_spacing(8.0)
            .rows([GridLength::Auto, GridLength::STAR])
            .children((
                StackPanel::new().spacing(6.0).children((
                    TextBlock::new().font_size(24.0).text("Review SSH hosts"),
                    TextBlock::new().text(format!("{reused} to reuse · {fresh} to open")),
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            Button::new()
                                .style(ButtonStyle::Accent)
                                .is_enabled(count > 0)
                                .on_click(
                                    ctx.callback(move |_| action(Change::Open(approval.clone()))),
                                )
                                .content(format!("{verb} {count} checked {unit}")),
                            unique,
                            Button::new()
                                .on_click(ctx.callback(move |_| action(Change::Edit)))
                                .content("Edit host list"),
                            cancel,
                        )),
                    error,
                )),
                ScrollViewer::new()
                    .grid_row(1)
                    .content(StackPanel::new().spacing(6.0).keyed_children(rows)),
            ))
    }
}

fn launch_plan(
    rows: Vec<list::Row>,
    approval: Approval,
    matches: impl Fn(reuse::Connection, &list::Target) -> bool,
) -> Result<Launch> {
    let mut launch = Launch::default();
    for (index, row) in rows.into_iter().enumerate() {
        let (Some(&connection), Ok(target)) = (approval.0.get(&index), row.target) else {
            continue;
        };
        if let Some(connection) = connection {
            ensure!(
                matches(connection, &target),
                "The reviewed SSH connection for {} changed. Choose a new pane or preview the host list again.",
                target.label()
            );
            launch.reused.insert(connection.pane);
        } else {
            launch.fresh.push((target.profile, target.ssh));
        }
    }
    Ok(launch)
}

impl TerminalApp {
    fn import_draft(&self, id: TabId, revision: u64) -> Option<&Draft> {
        let Page::HostGroup(page) = &self.tabs.get(id)?.page else {
            return None;
        };
        page.import
            .draft
            .as_ref()
            .filter(|_| page.import.revision == revision)
    }

    pub(super) fn preview_host_import(&mut self, id: TabId, revision: u64) {
        let Some(draft) = self
            .import_draft(id, revision)
            .filter(|draft| draft.review.is_none() && !draft.loading_file)
        else {
            return;
        };
        let rows = self
            .store
            .catalog()
            .and_then(|catalog| draft.rows(&catalog, self.store.path()));
        let review = rows.map(|rows| {
            let targets = self.host_group_targets();
            Review::new(rows, |target| {
                self.group_reuse_candidates(&targets, target.profile.as_deref(), &target.ssh)
            })
        });
        if let Some(page) = self.host_group_mut(id)
            && let Some(draft) = &mut page.import.draft
        {
            match review {
                Ok(review) => {
                    draft.review = Some(review);
                    draft.error = None;
                    page.import.revision += 1;
                }
                Err(error) => draft.error = Some(format!("{error:#}")),
            }
        }
    }

    pub(super) fn open_host_import(
        &mut self,
        id: TabId,
        revision: u64,
        approval: Approval,
        ctx: &ComponentContext<Self>,
    ) {
        let Some(draft) = self.import_draft(id, revision) else {
            return;
        };
        let Some(review) = draft
            .review
            .as_ref()
            .filter(|review| !approval.0.is_empty() && review.approval() == approval)
        else {
            return;
        };
        let catalog = match self.store.catalog() {
            Ok(catalog) => catalog,
            Err(error) => {
                self.report_import_error(id, error);
                return;
            }
        };
        let rows = draft.rows(&catalog, self.store.path()).and_then(|rows| {
            ensure!(
                rows == review.rows,
                "SSH profiles changed. Preview the host list again."
            );
            Ok(rows)
        });
        let rows = match rows {
            Ok(rows) => rows,
            Err(error) => {
                if let Some(page) = self.host_group_mut(id)
                    && let Some(draft) = &mut page.import.draft
                {
                    draft.return_to_editor(Some(format!("{error:#}")));
                    page.import.revision += 1;
                }
                return;
            }
        };
        let launch = match launch_plan(rows, approval, |connection, target| {
            self.group_reuse_connection(connection.pane, target.profile.as_deref(), &target.ssh)
                == Some(connection)
        }) {
            Ok(launch) => launch,
            Err(error) => {
                self.report_import_error(id, error);
                return;
            }
        };
        let feedback = launch.feedback();
        if let Some(page) = self.host_group_mut(id) {
            page.import.draft = None;
            page.import.revision += 1;
        }
        self.use_group_catalog(catalog);
        self.start_host_group_panes(id, launch.reused, launch.fresh, ctx);
        if let Some(page) = self.host_group_mut(id) {
            page.import.feedback = feedback;
        }
    }

    fn report_import_error(&mut self, id: TabId, error: anyhow::Error) {
        if let Some(page) = self.host_group_mut(id)
            && let Some(draft) = &mut page.import.draft
        {
            draft.error = Some(format!("{error:#}"));
        }
    }
}
