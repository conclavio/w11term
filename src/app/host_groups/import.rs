use super::*;
use anyhow::{Result, ensure};
use std::{cell::OnceCell, path::Path};

mod file;
mod list;
mod ranges;

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
    loading_file: bool,
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
    FileLoaded(u64, Result<Option<String>, String>),
}

pub enum Change {
    Mounted,
    Text(String),
    LoadFile,
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

    fn finish_file(&mut self, revision: u64, result: Result<Option<String>, String>) {
        let Some(draft) = self
            .draft
            .as_mut()
            .filter(|draft| revision == self.revision && draft.loading_file)
        else {
            return;
        };
        draft.loading_file = false;
        draft.editor.take();
        self.revision += 1;
        match result {
            Ok(Some(text)) => {
                draft.text = text;
                draft.error = None;
            }
            Ok(None) => {}
            Err(error) => draft.error = Some(format!("Could not open the host list. {error}")),
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
                .is_enabled(draft.editor_ready && !draft.loading_file)
                .text_optional(draft.editor_ready.then_some(&draft.text))
                .on_text_changed(ctx.callback(move |text| action(Change::Text(text))))
                .into()
        });
        Some(ScrollViewer::new().content(StackPanel::new().margin(16.0).spacing(10.0).children((
            TextBlock::new().font_size(24.0).text("Import SSH host list"),
            TextBlock::new()
                .text("Enter one profile or address per line. Use # for comments and patterns such as web<01-03,07> for hostnames.")
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
                Button::new()
                    .is_enabled(draft.editor_ready && !draft.loading_file)
                    .on_click(ctx.callback(move |_| action(Change::LoadFile)))
                    .content("Open host list file…"),
                Button::new().style(ButtonStyle::Accent)
                    .is_enabled(!draft.loading_file && !draft.text.trim().is_empty())
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
        let (revision, change) = match action {
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
            Action::Form(revision, change) => (revision, change),
            Action::FileLoaded(revision, result) => {
                if let Some(page) = self.host_group_mut(id) {
                    page.import.finish_file(revision, result);
                }
                return;
            }
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
                if draft.review.is_none()
                    && draft.editor_ready
                    && !draft.loading_file
                    && draft.text != text =>
            {
                draft.text = text;
                draft.editor.take();
                draft.error = None;
            }
            Change::LoadFile
                if draft.review.is_none() && draft.editor_ready && !draft.loading_file =>
            {
                let Some(owner) = self.keyboard.hwnd() else {
                    draft.error = Some("The window is unavailable.".into());
                    return;
                };
                draft.loading_file = true;
                draft.editor.take();
                let owner = owner.0 as usize;
                let loaded = move |result| {
                    Message::HostGroup(super::Action::Import(
                        id,
                        Action::FileLoaded(revision, result),
                    ))
                };
                ctx.spawn_background_with_rejection(
                    move |cancel| {
                        let read = || -> Result<Option<String>> {
                            if cancel.is_cancelled() {
                                return Ok(None);
                            }
                            let Some(selected) =
                                file_picker::pick(owner, file_picker::Purpose::OpenSshHostList)?
                            else {
                                return Ok(None);
                            };
                            if cancel.is_cancelled() {
                                return Ok(None);
                            }
                            file::read(&selected.path).map(Some)
                        };
                        loaded(read().map_err(|error| format!("{error:#}")))
                    },
                    loaded(Err("File picker unavailable.".into())),
                );
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
            Change::Preview if draft.review.is_none() && !draft.loading_file => {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_results_belong_to_the_pending_editor_and_only_replace_its_text() {
        let mut state = State {
            draft: Some(Draft {
                text: "ops@typed".into(),
                default_user: "ops".into(),
                authentication: 1,
                loading_file: true,
                ..Default::default()
            }),
            revision: 9,
            ..Default::default()
        };
        state.finish_file(8, Ok(Some("wrong".into())));
        assert!(state.draft.as_ref().unwrap().loading_file);
        state.finish_file(9, Ok(None));
        assert_eq!(state.draft.as_ref().unwrap().text, "ops@typed");
        assert!(!state.draft.as_ref().unwrap().loading_file);
        state.draft.as_mut().unwrap().loading_file = true;
        state.finish_file(10, Err("read failed".into()));
        assert_eq!(state.draft.as_ref().unwrap().text, "ops@typed");
        assert!(state.draft.as_ref().unwrap().error.is_some());
        state.draft.as_mut().unwrap().loading_file = true;
        state.finish_file(11, Ok(Some("ops@loaded\n".into())));
        let draft = state.draft.unwrap();
        assert_eq!(draft.text, "ops@loaded\n");
        assert_eq!(draft.default_user, "ops");
        assert_eq!(draft.authentication, 1);
        assert!(draft.error.is_none());
        assert!(draft.review.is_none());
    }
}
