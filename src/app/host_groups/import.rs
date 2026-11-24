use super::*;
use anyhow::{Result, ensure};
use std::{cell::OnceCell, path::Path};

mod list;

const LABEL: &str = "Import host list…";

#[derive(Default)]
pub(super) struct State {
    draft: Option<Draft>,
    revision: u64,
    pub(super) feedback: String,
}

#[derive(Default)]
struct Draft {
    text: String,
    editor_ready: bool,
    editor: OnceCell<View>,
    default_user: String,
    authentication: usize,
    identity: String,
    review: Option<Review>,
    error: Option<String>,
}

struct Review {
    rows: Vec<list::Row>,
    checked: BTreeSet<usize>,
}

pub enum Action {
    Begin,
    Form(u64, Change),
}

pub enum Change {
    Mounted,
    Text(String),
    DefaultUser(String),
    Authentication(usize),
    Identity(String),
    Preview,
    Check(usize, bool),
    Edit,
    Cancel,
    Open(BTreeSet<usize>),
}

impl Draft {
    fn rows(&self, catalog: &Profiles, path: &Path) -> Result<Vec<list::Row>> {
        let auth =
            ssh_authentication(self.authentication, &self.identity).map_err(|e| e.to_string());
        list::build(&self.text, &self.default_user, catalog, path, auth)
    }

    fn return_to_editor(&mut self, error: Option<String>) {
        self.review = None;
        self.editor_unmounted();
        self.error = error;
    }

    fn editor_unmounted(&mut self) {
        self.editor_ready = false;
        self.editor.take();
    }
}

impl State {
    pub(super) fn is_open(&self) -> bool {
        self.draft.is_some()
    }

    pub(super) fn editor_unmounted(&mut self) {
        if let Some(draft) = &mut self.draft {
            draft.editor_unmounted();
        }
    }

    pub(super) fn button(id: TabId, ctx: &ViewContext<TerminalApp>) -> View {
        Button::new()
            .on_click(
                ctx.callback(move |_| Message::HostGroup(super::Action::Import(id, Action::Begin))),
            )
            .content(LABEL)
    }

    pub(super) fn view(&self, id: TabId, ctx: &ViewContext<TerminalApp>) -> Option<View> {
        let draft = self.draft.as_ref()?;
        let revision = self.revision;
        let action = move |change| {
            Message::HostGroup(super::Action::Import(id, Action::Form(revision, change)))
        };
        let cancel = Button::new()
            .on_click(ctx.callback(move |_| action(Change::Cancel)))
            .content("Cancel import");
        if let Some(review) = &draft.review {
            let rows = review.rows.iter().enumerate().map(|(index, row)| {
                let view = match &row.target {
                    Ok(target) => {
                        let label = target.label();
                        CheckBox::new()
                            .automation_name(&label)
                            .is_checked(review.checked.contains(&index))
                            .on_is_checked_changed(
                                ctx.callback(move |checked| action(Change::Check(index, checked))),
                            )
                            .content(label)
                    }
                    Err(error) => TextBlock::new()
                        .text(format!("Line {} · {error}", row.line))
                        .text_wrapping(TextWrapping::Wrap)
                        .into(),
                };
                KeyedView::new(index.to_string(), view)
            });
            let checked = review.checked.clone();
            let label = match checked.len() {
                1 => "Open 1 checked host".to_owned(),
                count => format!("Open {count} checked hosts"),
            };
            return Some(
                Grid::new()
                    .margin(16.0)
                    .row_spacing(8.0)
                    .rows([GridLength::Auto, GridLength::STAR])
                    .children((
                        StackPanel::new().spacing(6.0).children((
                            TextBlock::new().font_size(24.0).text("Review SSH hosts"),
                            TextBlock::new().text("Each checked host opens in a new SSH tab."),
                            StackPanel::new()
                                .orientation(Orientation::Horizontal)
                                .spacing(8.0)
                                .children((
                                    Button::new()
                                        .style(ButtonStyle::Accent)
                                        .is_enabled(!checked.is_empty())
                                        .on_click(ctx.callback(move |_| {
                                            action(Change::Open(checked.clone()))
                                        }))
                                        .content(label),
                                    Button::new()
                                        .on_click(ctx.callback(move |_| action(Change::Edit)))
                                        .content("Edit host list"),
                                    cancel,
                                )),
                            error_text(draft),
                        )),
                        ScrollViewer::new()
                            .grid_row(1)
                            .content(StackPanel::new().spacing(4.0).keyed_children(rows)),
                    )),
            );
        }
        let key = if draft.authentication == PRIVATE_KEY_AUTH {
            TextBox::new()
                .automation_name("Private key file for new hosts")
                .placeholder_text("Private key file path")
                .text(&draft.identity)
                .on_text_changed(ctx.callback(move |text| action(Change::Identity(text))))
                .into()
        } else {
            View::empty()
        };
        let editor = draft.editor.get_or_init(|| {
            // Enable multiline input before writing its saved text. WinUI
            // applies the text property first when mounting a new control.
            if !draft.editor_ready {
                ctx.sender().send(action(Change::Mounted));
            }
            TextBox::new()
                .automation_name("SSH host list")
                .height(134.0)
                .accepts_return(true)
                .text_wrapping(TextWrapping::Wrap)
                .is_enabled(draft.editor_ready)
                .text_optional(draft.editor_ready.then_some(&draft.text))
                .on_text_changed(ctx.callback(move |text| action(Change::Text(text))))
                .into()
        });
        Some(ScrollViewer::new().content(StackPanel::new().margin(16.0).spacing(10.0).children((
            TextBlock::new().font_size(24.0).text("Import SSH host list"),
            TextBlock::new()
                .text("Enter one profile or address per line. Use # for comments and <01-03> for a numeric range.")
                .text_wrapping(TextWrapping::Wrap),
            editor.clone(),
            TextBox::new()
                .automation_name("Default SSH user")
                .placeholder_text("Default SSH user for host-only entries")
                .text(&draft.default_user)
                .on_text_changed(ctx.callback(move |text| action(Change::DefaultUser(text)))),
            StackPanel::new().spacing(4.0).children((
                TextBlock::new().text("Authentication for new hosts"),
                ComboBox::new()
                    .automation_name("Authentication for new hosts")
                    .items_source(SSH_AUTH_LABELS)
                    .selected_index(draft.authentication)
                    .on_selection_changed(ctx.callback(move |value: Option<usize>| {
                        value.map_or(Message::ControlIdle, |index| action(Change::Authentication(index)))
                    })),
            )),
            key,
            StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children((
                Button::new().style(ButtonStyle::Accent)
                    .is_enabled(!draft.text.trim().is_empty())
                    .on_click(ctx.callback(move |_| action(Change::Preview))).content("Preview hosts"),
                cancel,
            )),
            error_text(draft),
        ))))
    }
}

fn error_text(draft: &Draft) -> View {
    draft.error.as_ref().map_or_else(View::empty, |error| {
        TextBlock::new()
            .text(error)
            .text_wrapping(TextWrapping::Wrap)
            .foreground(Color::rgb(255, 160, 150))
            .into()
    })
}

impl TerminalApp {
    pub(super) fn handle_host_import(
        &mut self,
        id: TabId,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        let revision = match action {
            Action::Begin => {
                if self.host_group_command_target() == Some(id)
                    && let Some(page) = self.host_group_mut(id)
                {
                    page.import.revision += 1;
                    page.import.feedback.clear();
                    page.import.draft = Some(Draft::default());
                }
                return;
            }
            Action::Form(revision, _) => revision,
        };
        let Action::Form(_, change) = action else {
            return;
        };
        if self.tabs.active_id() != Some(id) {
            return;
        }
        let Some(Page::HostGroup(page)) = self.tabs.get_mut(id).map(|tab| &mut tab.page) else {
            return;
        };
        if page.import.revision != revision {
            return;
        }
        let catalog_store = &self.store;
        let state = &mut page.import;
        let Some(draft) = &mut state.draft else {
            return;
        };
        match change {
            Change::Cancel => {
                state.draft = None;
                state.revision += 1;
            }
            Change::Mounted if draft.review.is_none() && !draft.editor_ready => {
                draft.editor_ready = true;
                draft.editor.take();
            }
            Change::Text(text)
                if draft.review.is_none() && draft.editor_ready && draft.text != text =>
            {
                draft.text = text;
                draft.editor.take();
                draft.error = None;
            }
            Change::DefaultUser(user) if draft.review.is_none() && draft.default_user != user => {
                draft.default_user = user;
                draft.error = None;
            }
            Change::Authentication(index)
                if draft.review.is_none() && draft.authentication != index =>
            {
                draft.authentication = index;
                draft.error = None;
            }
            Change::Identity(identity) if draft.review.is_none() && draft.identity != identity => {
                draft.identity = identity;
                draft.error = None;
            }
            Change::Preview if draft.review.is_none() => {
                let rows = catalog_store
                    .catalog()
                    .and_then(|catalog| draft.rows(&catalog, catalog_store.path()));
                match rows {
                    Ok(rows) => {
                        let checked = rows
                            .iter()
                            .enumerate()
                            .filter_map(|(index, row)| row.target.is_ok().then_some(index))
                            .collect();
                        draft.review = Some(Review { rows, checked });
                        draft.error = None;
                        state.revision += 1;
                    }
                    Err(error) => draft.error = Some(format!("{error:#}")),
                }
            }
            Change::Check(index, checked) => {
                if let Some(review) = &mut draft.review
                    && review.rows.get(index).is_some_and(|row| row.target.is_ok())
                {
                    if checked {
                        review.checked.insert(index);
                    } else {
                        review.checked.remove(&index);
                    }
                    draft.error = None;
                }
            }
            Change::Edit if draft.review.is_some() => {
                draft.return_to_editor(None);
                state.revision += 1;
            }
            Change::Open(checked) if !checked.is_empty() => {
                let Some(review) = draft
                    .review
                    .as_ref()
                    .filter(|review| review.checked == checked)
                else {
                    return;
                };
                let catalog = match catalog_store.catalog() {
                    Ok(catalog) => catalog,
                    Err(error) => {
                        draft.error = Some(format!("{error:#}"));
                        return;
                    }
                };
                let launch = draft.rows(&catalog, catalog_store.path()).and_then(|rows| {
                    ensure!(
                        rows == review.rows,
                        "SSH profiles changed. Preview the host list again."
                    );
                    Ok(rows
                        .into_iter()
                        .enumerate()
                        .filter(|(index, _)| checked.contains(index))
                        .filter_map(|(_, row)| {
                            row.target.ok().map(|target| (target.profile, target.ssh))
                        })
                        .collect::<Vec<_>>())
                });
                match launch {
                    Ok(launch) => {
                        let count = launch.len();
                        state.draft = None;
                        state.revision += 1;
                        self.use_group_catalog(catalog);
                        self.start_host_group_panes(id, BTreeSet::new(), launch, ctx);
                        if let Some(page) = self.host_group_mut(id) {
                            page.import.feedback = if count == 1 {
                                "Opened 1 SSH pane from the host list".into()
                            } else {
                                format!("Opened {count} SSH panes from the host list")
                            };
                        }
                    }
                    Err(error) => {
                        draft.return_to_editor(Some(format!("{error:#}")));
                        state.revision += 1;
                    }
                }
            }
            _ => {}
        }
    }
}
