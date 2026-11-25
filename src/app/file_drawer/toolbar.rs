use super::*;

const ICON_SIZE: f64 = 30.0;
const ICON_GAP: f64 = 3.0;
const GROUP_GAP: f64 = 8.0;
const SINGLE_ROW_WIDTH: f64 = 310.0;

#[derive(Default)]
pub(super) struct ActionMenu {
    actions: Vec<(&'static str, FileAction)>,
}

impl ActionMenu {
    pub(super) fn item(
        &mut self,
        key: &'static str,
        label: &'static str,
        enabled: bool,
        action: FileAction,
    ) -> MenuItem {
        if enabled {
            self.actions.push((label, action));
        }
        command_menu_item(key, label, enabled)
    }

    pub(super) fn button(
        self,
        name: &str,
        glyph: &str,
        tooltip: String,
        enabled: bool,
        items: impl IntoIterator<Item = MenuItem>,
        context: &ViewContext<TerminalApp>,
    ) -> View {
        let icon = FontIcon::new().glyph(glyph);
        let button = title_tools::button(name)
            .width(ICON_SIZE)
            .height(ICON_SIZE)
            .is_enabled(enabled);
        if !enabled {
            return button
                .content(icon)
                .tooltip_with(title_tools::below(tooltip));
        }
        button
            .content(icon.tooltip_with(title_tools::below(tooltip)))
            .menu(Menu::new(
                items,
                context.callback(move |label: String| {
                    self.actions
                        .iter()
                        .find(|(name, _)| *name == label)
                        .map_or(Message::ControlIdle, |(_, action)| {
                            Message::Files(action.clone())
                        })
                }),
            ))
    }
}

fn icon_button(
    label: &str,
    glyph: &str,
    context: &ViewContext<TerminalApp>,
    enabled: bool,
    action: FileAction,
) -> View {
    title_tools::button(label)
        .width(ICON_SIZE)
        .height(ICON_SIZE)
        .is_enabled(enabled)
        .on_click(context.callback(move |_| Message::Files(action.clone())))
        .content(FontIcon::new().glyph(glyph))
        .tooltip_with(title_tools::below(label))
}

impl FileDrawer {
    pub(super) fn listing_toolbar(
        &self,
        pane: u64,
        ssh: &SshOptions,
        profile: Option<&str>,
        selected: Option<ListedEntry>,
        layout: DrawerLayout,
        context: &ViewContext<TerminalApp>,
    ) -> View {
        use FileAction::*;
        let folder = self.directory.as_deref().unwrap_or(".");
        let ready = !self.busy();
        let writable = ready && self.directory.is_some();
        let generation = self.generation;
        let revision = self.listing_revision;
        let button =
            |label, glyph, enabled, action| icon_button(label, glyph, context, enabled, action);
        let navigation = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(ICON_GAP)
            .children((
                button(
                    "Back",
                    "\u{e72b}",
                    writable && self.history.target(HistoryDirection::Back).is_some(),
                    History(pane, generation, revision, HistoryDirection::Back),
                ),
                button(
                    "Forward",
                    "\u{e72a}",
                    writable && self.history.target(HistoryDirection::Forward).is_some(),
                    History(pane, generation, revision, HistoryDirection::Forward),
                ),
                button(
                    "Up",
                    "\u{e74a}",
                    writable && folder != "/",
                    Browse(
                        pane,
                        generation,
                        format!("{}/..", folder.trim_end_matches('/')),
                    ),
                ),
                button(
                    "Refresh",
                    "\u{e72c}",
                    ready,
                    Browse(pane, generation, folder.into()),
                ),
            ));
        let download = match &selected {
            Some(entry) => {
                let label = if entry.kind == ListedKind::Known(EntryKind::Directory) {
                    "Download folder…"
                } else {
                    "Download…"
                };
                button(
                    label,
                    "\u{e896}",
                    ready,
                    Pick(
                        pane,
                        generation,
                        revision,
                        TransferPicker::Download(FileTarget::Listed(entry.clone()), None),
                    ),
                )
            }
            None => title_tools::button("Download…")
                .width(ICON_SIZE)
                .height(ICON_SIZE)
                .is_enabled(false)
                .content(FontIcon::new().glyph("\u{e896}"))
                .tooltip_with(title_tools::below("Select a file or folder to download")),
        };
        let tools = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(ICON_GAP)
            .children((
                button(
                    "Find files…",
                    "\u{e721}",
                    writable && generation != 0,
                    Search(pane, generation, find::Action::Show(revision)),
                ),
                button(
                    "Upload…",
                    "\u{e898}",
                    writable,
                    Pick(
                        pane,
                        generation,
                        revision,
                        TransferPicker::UploadFiles(folder.into()),
                    ),
                ),
                download,
                self.selected_entry_menu(pane, selected.as_ref(), context),
                self.folder_menu(pane, profile, folder, context),
            ));
        let commands = if layout.content_width() >= SINGLE_ROW_WIDTH {
            Grid::new()
                .columns([GridLength::Star(1.0), GridLength::Auto])
                .column_spacing(GROUP_GAP)
                .children((navigation, Border::new().grid_column(1).content(tools)))
        } else {
            StackPanel::new().spacing(3.0).children((navigation, tools))
        };
        let endpoint = format!("{}@{}:{}", ssh.user, ssh.host, ssh.port);
        StackPanel::new()
            .automation_name("SFTP file toolbar")
            .width(layout.content_width())
            .horizontal_alignment(HorizontalAlignment::Left)
            .spacing(4.0)
            .children((
                Grid::new()
                    .columns([GridLength::Star(1.0), GridLength::Auto])
                    .children((
                        StackPanel::new().spacing(1.0).children((
                            TextBlock::new().text("Files").font_size(16.0),
                            TextBlock::new()
                                .text(&endpoint)
                                .text_trimming(TextTrimming::CharacterEllipsis)
                                .tooltip_with(title_tools::below(endpoint)),
                        )),
                        Border::new().grid_column(1).content(button(
                            "Close files",
                            "\u{e711}",
                            true,
                            Close(pane),
                        )),
                    )),
                commands,
                Grid::new()
                    .columns([GridLength::Star(1.0), GridLength::Auto, GridLength::Auto])
                    .children((
                        TextBlock::new()
                            .vertical_alignment(VerticalAlignment::Center)
                            .text(folder)
                            .text_trimming(TextTrimming::CharacterEllipsis)
                            .tooltip_with(title_tools::below(folder)),
                        Border::new().grid_column(1).content(match selected {
                            Some(entry) => button(
                                "Open folder",
                                "\u{e838}",
                                ready && entry.kind != ListedKind::Known(EntryKind::File),
                                OpenEntry(pane, generation, entry),
                            ),
                            None => title_tools::button("Open folder")
                                .width(ICON_SIZE)
                                .height(ICON_SIZE)
                                .is_enabled(false)
                                .content(FontIcon::new().glyph("\u{e838}"))
                                .tooltip_with(title_tools::below("Select a folder to open")),
                        }),
                        Border::new().grid_column(2).content(button(
                            "Go to folder…",
                            "\u{e8b7}",
                            ready && generation != 0,
                            GoToFolder(pane, generation, revision),
                        )),
                    )),
            ))
    }

    fn folder_menu(
        &self,
        pane: u64,
        profile: Option<&str>,
        folder: &str,
        context: &ViewContext<TerminalApp>,
    ) -> View {
        use FileAction::*;
        use folder_report::Kind;
        let ready = !self.busy();
        let writable = ready && self.directory.is_some();
        let generation = self.generation;
        let revision = self.listing_revision;
        let can_report = writable && generation != 0;
        let has_reference = self.folder_reference.is_some();
        let mut menu = ActionMenu::default();
        let mut item = |key, label, enabled, action| menu.item(key, label, enabled, action);
        let reports = |kind| ShowFolderReport(pane, generation, revision, kind);
        let items = [
            item(
                "bookmarks",
                "Bookmarks…",
                ready && profile.is_some(),
                Bookmarks(pane, generation, bookmarks::Action::Show),
            ),
            item(
                "copy-path",
                "Copy directory path",
                writable,
                CopyPath(pane, generation, revision, None),
            ),
            MenuItem::separator("browse-end"),
            item(
                "new-folder",
                "New folder",
                writable,
                NewFolder(pane, generation, folder.into()),
            ),
            item(
                "new-text",
                "New text file…",
                self.can_start_new_editor(revision),
                NewTextFile(pane, generation, revision),
            ),
            item(
                "upload-folder",
                "Upload folder…",
                writable,
                Pick(
                    pane,
                    generation,
                    revision,
                    TransferPicker::UploadFolder(folder.into()),
                ),
            ),
            MenuItem::separator("write-end"),
            item(
                "space",
                "Filesystem space…",
                can_report,
                reports(Kind::Space),
            ),
            item(
                "contents",
                "Folder contents…",
                can_report,
                reports(Kind::Contents),
            ),
            MenuItem::submenu(
                "compare",
                "Compare folders",
                [
                    item(
                        "capture-remote",
                        "Set folder as comparison reference…",
                        can_report,
                        reports(Kind::Reference),
                    ),
                    item(
                        "capture-windows",
                        "Set Windows folder as reference…",
                        can_report,
                        Pick(
                            pane,
                            generation,
                            revision,
                            TransferPicker::LocalFolderReference(revision),
                        ),
                    ),
                    item(
                        "compare-current",
                        "Compare folder with reference…",
                        writable && has_reference,
                        reports(Kind::Comparison),
                    ),
                    item(
                        "clear-reference",
                        "Clear folder reference",
                        ready && has_reference,
                        ClearFolderReference(pane, generation, revision),
                    ),
                ],
            ),
        ];
        menu.button(
            "More folder actions",
            "\u{e712}",
            "More folder actions".into(),
            ready,
            items,
            context,
        )
    }
}
