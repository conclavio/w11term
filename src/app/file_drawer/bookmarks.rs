use super::*;
use w11term::profiles::{BookmarkPeer, bookmark_directory};

#[derive(Clone)]
pub enum Action {
    Show,
    Back,
    Save(String),
    Open(String),
    Remove(String),
}

pub(super) struct List {
    pub(super) profile: String,
    paths: Vec<String>,
}

impl List {
    pub(super) fn panel(
        &self,
        files: &FileDrawer,
        pane: u64,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let generation = files.generation;
        let button = |name: &str, action: Action| {
            Button::new()
                .automation_name(name)
                .on_click(ctx.callback(move |_| {
                    Message::Files(FileAction::Bookmarks(pane, generation, action.clone()))
                }))
        };
        let current = files.directory.as_deref().and_then(bookmark_directory);
        let mut header = vec![
            KeyedView::new(
                "heading",
                TextBlock::new()
                    .text(format!("Bookmarks · {}", self.profile))
                    .font_size(19.0),
            ),
            KeyedView::new(
                "back",
                button("Back to files", Action::Back).content("Back to files"),
            ),
        ];
        if let Some(path) = current {
            header.push(KeyedView::new(
                "path",
                TextBlock::new()
                    .text(path)
                    .text_wrapping(TextWrapping::Wrap),
            ));
            header.push(KeyedView::new(
                "save",
                button("Bookmark this folder", Action::Save(path.into()))
                    .is_enabled(!self.paths.iter().any(|saved| saved == path))
                    .content("Bookmark this folder"),
            ));
        }
        if !files.status.is_empty() {
            header.push(KeyedView::new(
                "status",
                TextBlock::new()
                    .text(&files.status)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        if self.paths.is_empty() {
            header.push(KeyedView::new(
                "empty",
                TextBlock::new().text("No bookmarked folders"),
            ));
        }
        let rows = self.paths.iter().map(|path| {
            KeyedView::new(
                path.as_str(),
                Grid::new()
                    .columns([GridLength::Star(1.0), GridLength::Auto])
                    .children((
                        button(&format!("Open bookmark {path}"), Action::Open(path.clone()))
                            .content(
                                TextBlock::new()
                                    .text(path)
                                    .text_wrapping(TextWrapping::Wrap),
                            ),
                        Border::new().grid_column(1).content(
                            button(
                                &format!("Remove bookmark {path}"),
                                Action::Remove(path.clone()),
                            )
                            .content("Remove"),
                        ),
                    )),
            )
        });
        Border::new()
            .padding(10.0)
            .background(Color::rgb(31, 32, 35))
            .content(
                Grid::new()
                    .rows([GridLength::Auto, GridLength::Star(1.0)])
                    .children((
                        StackPanel::new().spacing(6.0).keyed_children(header),
                        ScrollViewer::new()
                            .grid_row(1)
                            .content(StackPanel::new().spacing(4.0).keyed_children(rows)),
                    )),
            )
    }
}

impl FileDrawer {
    pub(super) fn bookmark_action(
        &mut self,
        pane: u64,
        action: Action,
        profile: Option<&str>,
        store: &ProfileStore,
        session: &Session,
        context: &ComponentContext<TerminalApp>,
    ) -> Result<()> {
        if matches!(action, Action::Back) {
            self.bookmarks = None;
            self.status.clear();
            return Ok(());
        }
        ensure!(
            !self.busy(),
            "Wait for the current file operation to finish"
        );
        let profile = profile.context("Bookmarks require a saved SSH profile")?;
        let connection = session
            .ssh_connection_at(self.generation)
            .context("The SSH connection has changed")?;
        let peer = BookmarkPeer::from_connection(&connection);
        if !matches!(action, Action::Show) {
            ensure!(
                self.bookmarks
                    .as_ref()
                    .is_some_and(|list| list.profile == profile),
                "Reopen bookmarks for the current profile"
            );
        }
        let paths = match action {
            Action::Show => store.list_bookmarks(profile, &peer)?,
            Action::Save(path) => {
                ensure!(
                    self.directory.as_deref().and_then(bookmark_directory) == Some(path.as_str()),
                    "The current folder has changed"
                );
                store.add_bookmark(profile, &peer, &path)?
            }
            Action::Remove(path) => store.remove_bookmark(profile, &peer, &path)?,
            Action::Open(path) => {
                let paths = store.list_bookmarks(profile, &peer)?;
                let found = paths.contains(&path);
                if let Some(list) = &mut self.bookmarks {
                    list.paths = paths;
                }
                ensure!(found, "This bookmark was removed");
                let description = format!("bookmark {path}");
                return self.browse_named(pane, session, path, description, context);
            }
            Action::Back => unreachable!(),
        };
        self.end_filter_edit();
        self.status.clear();
        self.bookmarks = Some(List {
            profile: profile.into(),
            paths,
        });
        Ok(())
    }
}
