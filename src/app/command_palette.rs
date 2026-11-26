use super::*;
use host_groups::WindowRunScope;
use w11term::shortcuts::BINDINGS;
use w11term::terminal::BookmarkScreen;

pub(super) const FIND_TAB_LABEL: &str = "Find tab…";
pub(super) const FIND_COMMAND_HISTORY_LABEL: &str = "Find command history…";
pub(super) const FIND_RUNNING_COMMANDS_LABEL: &str = "Find running commands…";
pub(super) const FIND_BOOKMARK_LABEL: &str = "Find scrollback bookmark…";
pub(super) const FIND_PROFILE_LABEL: &str = "Find profile…";
pub(super) const SPLIT_RIGHT_PROFILE_LABEL: &str = "Find profile to split right…";
pub(super) const SPLIT_BELOW_PROFILE_LABEL: &str = "Find profile to split below…";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileDestination {
    Tab,
    Split { axis: Axis, origin: TabId },
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum Search {
    Window,
    Tabs,
    CommandHistory,
    RunningCommands,
    Profiles(ProfileDestination),
    GroupLibrary(TabId),
    GroupRecent(TabId),
    GroupRuns(TabId),
    WindowGroupRuns(WindowRunScope),
    Bookmarks(clipboard_paste::Origin, BookmarkScreen),
}

impl Search {
    pub(super) fn is_profiles(self) -> bool {
        matches!(self, Self::Profiles(_))
    }

    fn placeholder(self) -> &'static str {
        match self {
            Self::Window => "Search commands",
            Self::Tabs => "Search tabs",
            Self::CommandHistory => "Search commands across panes",
            Self::RunningCommands => "Search running commands",
            Self::Profiles(ProfileDestination::Tab) => "Search profiles",
            Self::Profiles(ProfileDestination::Split {
                axis: Axis::Columns,
                ..
            }) => "Search profiles to split right",
            Self::Profiles(ProfileDestination::Split {
                axis: Axis::Rows, ..
            }) => "Search profiles to split below",
            Self::Bookmarks(..) => "Search scrollback bookmarks",
            Self::GroupLibrary(_) => "Search saved commands",
            Self::GroupRecent(_) => "Search recent group commands",
            Self::GroupRuns(_) => "Search group runs",
            Self::WindowGroupRuns(WindowRunScope::All) => "Search group runs across tabs",
            Self::WindowGroupRuns(WindowRunScope::ArmedOutputAlerts) => {
                "Search armed SSH output alerts"
            }
        }
    }

    fn empty(self, query: &str) -> &'static str {
        match self {
            Self::GroupLibrary(_) if query.is_empty() => "Add saved commands in Settings.",
            Self::GroupRecent(_) if query.is_empty() => {
                "Commands submitted in this tab will appear here."
            }
            Self::GroupRecent(_) => "No matching recent commands",
            Self::WindowGroupRuns(WindowRunScope::ArmedOutputAlerts) if query.is_empty() => {
                "No armed SSH output alerts"
            }
            Self::WindowGroupRuns(WindowRunScope::ArmedOutputAlerts) => {
                "No matching armed SSH output alerts"
            }
            Self::GroupRuns(_) | Self::WindowGroupRuns(_) => "No matching group runs",
            Self::Tabs => "No matching tabs",
            Self::CommandHistory if query.trim().is_empty() => {
                "No retained commands in this window"
            }
            Self::CommandHistory => "No matching commands in this window",
            Self::RunningCommands if query.trim().is_empty() => {
                "No running shell commands in this window"
            }
            Self::RunningCommands => "No matching running commands",
            Self::Profiles(_) => "No matching profiles",
            Self::Bookmarks(..) if query.is_empty() => "No retained scrollback bookmarks",
            Self::Bookmarks(..) => "No matching scrollback bookmarks",
            _ => "No matching commands",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum CommandId {
    Menu(String),
    MoveTabToExistingWindow(Option<TabId>, u64),
    BroadcastTyping(pane_broadcast::Action),
    RenamePane(Option<(u64, TabId)>),
    SaveLiveProfile(Option<clipboard_paste::Origin>),
    FocusPreviousPane(Option<previous_pane::Target>),
    SaveScrollback(Option<u64>),
    CopyPaneText(Option<u64>),
    PaneImage(Option<u64>, text_export::ImageDestination),
    Recording(Option<recordings::Choice>),
    ClearScrollback(Option<u64>),
    ScrollFailedCommand(Option<clipboard_paste::Origin>, bool),
    ToggleScrollbackBookmark(Option<clipboard_paste::Origin>),
    ScrollToBookmark(Option<clipboard_paste::Origin>, bool),
    FindScrollbackBookmark(clipboard_paste::Origin, BookmarkScreen),
    OpenScrollbackBookmark(clipboard_paste::Origin, BookmarkScreen, isize),
    SelectAll(Option<clipboard_paste::Origin>),
    PaneFind(Option<clipboard_paste::Origin>, Shortcut),
    CopySelection(Option<clipboard_paste::Origin>),
    PaneColorScheme(u64, ColorScheme),
    SharedFolders(Option<u64>),
    LocalForward(Option<local_forwards::Origin>),
    RestartExited(Option<ExitedPane>),
    StopSshAttempt(PendingPaneAttempt),
    RetrySshNow(PendingPaneRetry),
    DisconnectSsh(disconnect_ssh::Target),
    Clipboard(Option<clipboard_paste::Origin>, bool),
    LongCommands(Option<clipboard_paste::Origin>, bool),
    SavedCommand(
        Option<clipboard_paste::Origin>,
        w11term::saved_commands::Entry,
    ),
    GroupSavedCommand(TabId, w11term::saved_commands::Entry),
    SearchGroupRecentCommands(TabId),
    GroupRecentCommand(TabId, u64),
    SearchGroupRuns(TabId),
    GroupRun(TabId, u64),
    SearchWindowGroupRuns(WindowRunScope),
    WindowGroupRun(WindowRunScope, TabId, u64),
    FocusTab(u64, TabId),
    FocusRunningCommand(u64, w11term::events::RunningCommandTarget),
    OpenWindowCommand(
        u64,
        w11term::events::CommandLink,
        Option<std::ops::Range<usize>>,
    ),
    OpenSavedHostGroup(String, Vec<String>),
    ReviewSavedHostGroup(host_groups::LaunchReviewOffer),
    ManageRecipientSets(TabId, String),
    ApplyRecipientSet(TabId, host_groups::RecipientSetOffer),
    SshRoster(TabId, host_groups::RosterCommand),
    TabAction(Shortcut, Option<TabId>),
    SetTabPinned(Option<TabId>, bool),
    SetTabColor(TabId, Option<TabColor>),
    CloseTabs(Option<bulk_tab_close::Offer>),
    ReopenClosedTab(Option<TabId>),
    ReopenClosedPane(Option<TabId>),
    SwapPanes(PaneSwap),
    RotateSplit(PaneRotation),
    BalancePanes(PaneBalance),
    OpenInDirectory(
        directory_split::Destination,
        Option<directory_split::Origin>,
    ),
    OpenExplorer(Option<directory_split::Origin>),
    CopyCurrentDirectory(Option<directory_split::Origin>),
    Profile(String),
    LaunchProfile(ProfileDestination, String, Option<String>),
    Shortcut(Shortcut),
}

pub(super) struct Row {
    pub id: CommandId,
    pub(super) label: String,
    pub(super) chord: String,
    pub enabled: bool,
}

fn menu_shortcut(message: Message) -> Option<Shortcut> {
    match message {
        Message::Shortcut(shortcut) => Some(shortcut),
        Message::PaneNeighbor(direction) => Some(Shortcut::FocusPane(direction)),
        Message::SplitPane(Axis::Columns, None) => Some(Shortcut::SplitRight),
        Message::SplitPane(Axis::Rows, None) => Some(Shortcut::SplitBelow),
        Message::ZoomPane => Some(Shortcut::ZoomPane),
        Message::ClosePane => Some(Shortcut::ClosePane),
        Message::MovePaneToTab => Some(Shortcut::MovePane),
        Message::Workspace(workspace_view::WorkspaceAction::Show) => Some(Shortcut::Workspaces),
        Message::Paste => Some(Shortcut::Paste),
        Message::PasteRaw => Some(Shortcut::PasteRaw),
        _ => None,
    }
}

// Use the same profile and caret menu entries (including nested menus) and their enabled state.
// Preserve the complete callback label for names containing spaces, colons or other punctuation.
fn menu_rows(items: Vec<MenuItem>, profiles: bool, rows: &mut Vec<Row>) {
    for item in items {
        match item {
            // Color choices are bound below to the native pane ID and get a searchable verb.
            MenuItem::Submenu { key, .. } if !profiles && key == pane_colors::MENU_KEY.into() => {}
            MenuItem::Submenu { items, .. } => menu_rows(items, profiles, rows),
            MenuItem::Separator { .. } => {}
            MenuItem::Item {
                key,
                label,
                enabled,
            } => {
                if (!enabled && key == "panes".into())
                    || (!profiles
                        && (key == "save-scrollback".into()
                            || key == "copy-selection".into()
                            || key == "restart-exited".into()
                            || key == "stop-ssh-attempt".into()
                            || key == "retry-ssh-now".into()
                            || key == "disconnect-ssh-shell".into()
                            || key == "shared-folders".into()
                            || key == "local-forward".into()))
                {
                    continue;
                }
                let id = if profiles {
                    CommandId::Profile(label.clone())
                } else {
                    menu_shortcut(tab_menu_message(label.clone()))
                        .map_or_else(|| CommandId::Menu(label.clone()), CommandId::Shortcut)
                };
                if id == CommandId::Shortcut(Shortcut::CommandPalette) {
                    continue;
                }
                let (label, chord) = match &id {
                    CommandId::Profile(_)
                        if key != "new-profile".into()
                            && key != "quick-connect".into()
                            && key != "find-profile".into()
                            && key != "lab".into() =>
                    {
                        (format!("New tab with {label}"), String::new())
                    }
                    CommandId::Shortcut(Shortcut::Fullscreen) => {
                        (label.replace(" (F11)", ""), "F11".into())
                    }
                    CommandId::Shortcut(Shortcut::Paste) => (label, "Ctrl+V".into()),
                    CommandId::Shortcut(Shortcut::PasteRaw) => (label, "Ctrl+Alt+V".into()),
                    CommandId::Shortcut(_) => match label.rsplit_once("    ") {
                        Some((title, chord)) => (title.to_owned(), chord.to_owned()),
                        None => (label, String::new()),
                    },
                    _ => (label, String::new()),
                };
                rows.push(Row {
                    id,
                    label,
                    chord,
                    enabled,
                });
            }
        }
    }
}

fn filter(mut rows: Vec<Row>, query: &str) -> Vec<Row> {
    let query = query.trim().to_lowercase();
    let words: Vec<_> = query.split_whitespace().collect();
    rows.retain(|row| {
        let text = format!("{} {}", row.label, row.chord).to_lowercase();
        words.iter().all(|word| text.contains(word))
    });
    rows.sort_by_cached_key(|row| {
        let label = row.label.to_lowercase();
        // Stable sorting keeps profiles within each action in the Settings order.
        let group = [
            "new tab with ",
            "split right with ",
            "split below with ",
            "find sessions on ",
            "browse herdr terminals on ",
        ]
        .into_iter()
        .find(|prefix| label.starts_with(prefix));
        (
            !row.enabled,
            label != query,
            !label.starts_with(&query),
            group.map_or(label, str::to_owned),
        )
    });
    rows
}

pub(super) fn profile_search_rows(
    catalog: &Profiles,
    destination: ProfileDestination,
    query: &str,
) -> Vec<Row> {
    let query = profile_query::Query::new(query);
    let mut matches = Vec::new();
    for name in &catalog.profile_order {
        let endpoint = catalog.profiles.get(name).map(profile_query::address);
        let details = endpoint.as_deref().unwrap_or("Local");
        if let Some(rank) = query.rank(name, details) {
            matches.push((
                rank,
                Row {
                    label: format!("{name} · {details}"),
                    id: CommandId::LaunchProfile(destination, name.clone(), endpoint),
                    chord: String::new(),
                    enabled: true,
                },
            ));
        }
    }
    matches.sort_by_key(|(rank, _)| *rank);
    matches.into_iter().map(|(_, row)| row).collect()
}

#[derive(PartialEq, Eq)]
pub(super) struct SearchTab {
    group: TabId,
    title: String,
}

fn search_tabs(
    query: &str,
    source: u64,
    active: Option<TabId>,
    windows: impl IntoIterator<Item = (u64, Rc<[SearchTab]>)>,
) -> Vec<Row> {
    let Some((window_filter, query)) = tab_windows::window_query(query) else {
        return Vec::new();
    };
    let mut matches = Vec::new();
    for (window, entries) in windows {
        if window_filter.is_some_and(|id| id != window) {
            continue;
        }
        let sibling = window != source;
        for (index, tab) in entries.iter().enumerate() {
            let title = tab.title.to_lowercase();
            let position = format!("Tab {}", index + 1);
            let position_query = position.to_lowercase();
            let terms = format!("{title} {position_query}");
            if query.split_whitespace().all(|word| terms.contains(word)) {
                let rank = (
                    title != query,
                    position_query != query,
                    !title.starts_with(&query),
                    sibling,
                );
                matches.push((
                    rank,
                    Row {
                        id: CommandId::FocusTab(window, tab.group),
                        label: tab.title.clone(),
                        chord: if sibling {
                            format!("Window {window} · {position}")
                        } else if Some(tab.group) == active {
                            format!("{position} · Current")
                        } else {
                            position
                        },
                        enabled: true,
                    },
                ));
            }
        }
    }
    matches.sort_by_key(|(rank, _)| *rank);
    matches.into_iter().map(|(_, row)| row).collect()
}

impl TerminalApp {
    pub(super) fn tab_search_entries(&self, follow_titles: bool) -> Vec<SearchTab> {
        self.tabs
            .headers()
            .into_iter()
            .map(|tab| SearchTab {
                group: tab.group_id(),
                title: self.tab_label(tab.id, follow_titles).into_owned(),
            })
            .collect()
    }

    pub(super) fn tab_search_rows(&self, query: &str) -> Vec<Row> {
        let follow_titles = self
            .appearance
            .read()
            .unwrap()
            .interaction
            .application_tab_titles;
        let source = self.registry.window_id();
        let local = (source, self.tab_search_entries(follow_titles).into());
        search_tabs(
            query,
            source,
            self.tabs.active_group_id(),
            std::iter::once(local).chain(self.sibling_tab_search_entries()),
        )
    }

    pub(super) fn command_rows(&self, query: &str) -> Vec<Row> {
        let mut rows = vec![];
        let exited = self.tabs.active().and_then(AppTab::exited_pane);
        let arrangement = self.pane_arrangement();
        let directory_split = self.directory_split_origin();
        let previous = previous_pane::target(&self.tabs);
        let recordings = self.recording_choices();
        menu_rows(
            self.caret_items(
                exited.is_some(),
                &arrangement,
                directory_split.as_ref(),
                previous,
                recordings,
            ),
            false,
            &mut rows,
        );
        let origin = self.paste_origin();
        let broadcast = self.broadcast_offer();
        let live = self.tabs.active().and_then(|tab| match &tab.page {
            Page::Terminal(live) => Some(live),
            _ => None,
        });
        let pane = live.map(|tab| tab.session.pane_id);
        let rename_target = self
            .tabs
            .active()
            .and_then(AppTab::pane_id)
            .zip(self.tabs.active_group_id());
        for row in &mut rows {
            if matches!(&row.id, CommandId::Menu(label) if label == text_export::COPY_VIEWPORT_LABEL)
            {
                row.id = CommandId::CopyPaneText(pane);
            }
            if let CommandId::Menu(label) = &row.id
                && let Some(destination) = text_export::ImageDestination::from_label(label)
            {
                row.id = CommandId::PaneImage(pane, destination);
            }
            if let CommandId::Menu(label) = &row.id
                && let Some(enabled) = attention::long_command_toggle(label)
            {
                row.id = CommandId::LongCommands(origin, enabled);
            }
            if let CommandId::Menu(label) = &row.id
                && let Some(window) = tab_windows::menu_window(label)
            {
                let title = label.strip_prefix("Window ").unwrap_or(label);
                row.label = format!("Move tab to existing window {title}");
                row.id = CommandId::MoveTabToExistingWindow(self.tabs.active_group_id(), window);
            }
            if matches!(&row.id, CommandId::Menu(label) if pane_broadcast::is_label(label))
                && let Some(action) = broadcast
                    .as_ref()
                    .filter(|action| action.label() == row.label)
            {
                row.id = CommandId::BroadcastTyping(action.clone());
            }
            if let CommandId::Shortcut(shortcut @ (Shortcut::Paste | Shortcut::PasteRaw)) = row.id {
                row.id = CommandId::Clipboard(origin, shortcut == Shortcut::PasteRaw);
            }
            if let CommandId::Menu(label) = &row.id
                && recordings::is_command(label)
            {
                row.id = CommandId::Recording(recordings.target(label));
            }
            if matches!(&row.id, CommandId::Menu(label) if label == tab_title::RENAME_PANE) {
                row.id = CommandId::RenamePane(rename_target);
            }
            if matches!(&row.id, CommandId::Menu(label) if label == live_profile::SAVE) {
                row.id = CommandId::SaveLiveProfile(self.saveable_ssh_origin());
            }
            if matches!(&row.id, CommandId::Menu(label) if label == directory_split::OPEN_EXPLORER)
            {
                row.id = CommandId::OpenExplorer(directory_split.clone());
            }
            if let CommandId::Menu(label) = &row.id
                && let Some(pinned) = tab_pin_command(label)
            {
                row.id = CommandId::SetTabPinned(self.active_terminal_group(), pinned);
            }
            if let CommandId::Menu(label) = &row.id
                && let Some(color) = tab_colors::from_menu(label)
                && let Some(group) = self.active_terminal_group()
            {
                row.id = CommandId::SetTabColor(group, color);
            }
            if let CommandId::Menu(label) = &row.id
                && let Some(scope) = bulk_tab_close::Scope::from_label(label)
            {
                row.id = CommandId::CloseTabs(self.bulk_close_offer(scope));
            }
        }
        menu_rows(self.profile_items(), true, &mut rows);
        let can_review_group = self.check_saved_group_launch().is_ok();
        for (name, members) in &self.catalog.ssh_host_groups {
            rows.push(Row {
                id: CommandId::OpenSavedHostGroup(name.clone(), members.clone()),
                label: format!("Open SSH group · {name}"),
                chord: String::new(),
                enabled: !members.is_empty(),
            });
            rows.push(Row {
                id: CommandId::ReviewSavedHostGroup(host_groups::LaunchReviewOffer {
                    group: name.clone(),
                    profiles: members.clone(),
                    recipient_set: None,
                }),
                label: format!("Review SSH group · {name}"),
                chord: String::new(),
                enabled: can_review_group && !members.is_empty(),
            });
            if let Some(sets) = self.catalog.ssh_recipient_sets.get(name) {
                rows.extend(sets.iter().map(|(set, profiles)| Row {
                    id: CommandId::ReviewSavedHostGroup(host_groups::LaunchReviewOffer {
                        group: name.clone(),
                        profiles: members.clone(),
                        recipient_set: Some((set.clone(), profiles.clone())),
                    }),
                    label: format!("Review SSH recipient set · {name} · {set}"),
                    chord: String::new(),
                    enabled: can_review_group
                        && !profiles.is_empty()
                        && profiles.iter().all(|profile| {
                            members.contains(profile)
                                && self.catalog.ssh_host_group_member(profile).is_some()
                        }),
                }));
            }
        }
        let tab_count = self.tabs.headers().len();
        if let Some(live) = live.filter(|tab| pane_colors::available(tab)) {
            rows.extend(
                pane_colors::choices(live.color_scheme).map(|(scheme, label)| Row {
                    id: CommandId::PaneColorScheme(live.session.pane_id, scheme),
                    label: format!("Set pane color scheme to {label}"),
                    chord: String::new(),
                    enabled: true,
                }),
            );
        }
        let forward = self.forward_origin();
        rows.push(Row {
            id: CommandId::LocalForward(forward),
            label: local_forwards::LABEL.into(),
            chord: String::new(),
            enabled: forward.is_some(),
        });
        rows.push(Row {
            id: CommandId::SharedFolders(pane),
            label: folder_shares::LABEL.into(),
            chord: String::new(),
            enabled: pane.and_then(|p| self.folder_authority(p)).is_some(),
        });
        rows.push(Row {
            id: CommandId::RestartExited(exited),
            label: RESTART_EXITED_PANE.into(),
            chord: String::new(),
            enabled: exited.is_some(),
        });
        if let Some(attempt) = self.tabs.active().and_then(AppTab::pending_pane_attempt) {
            rows.push(Row {
                id: CommandId::StopSshAttempt(attempt),
                label: STOP_SSH_ATTEMPT.into(),
                chord: String::new(),
                enabled: true,
            });
        }
        if let Some(retry) = self.tabs.active().and_then(AppTab::pending_pane_retry) {
            rows.push(Row {
                id: CommandId::RetrySshNow(retry),
                label: RETRY_SSH_NOW.into(),
                chord: String::new(),
                enabled: true,
            });
        }
        if let Some(target) = self.disconnect_target() {
            rows.push(Row {
                id: CommandId::DisconnectSsh(target),
                label: disconnect_ssh::LABEL.into(),
                chord: String::new(),
                enabled: true,
            });
        }
        rows.push(Row {
            id: CommandId::SaveScrollback(pane),
            label: text_export::SCROLLBACK_LABEL.into(),
            chord: String::new(),
            enabled: pane.is_some() && self.export.pending.is_none(),
        });
        let frame = live.map(|tab| tab.session.frames.load());
        if let Some(origin) = origin
            && let Some(frame) = &frame
            && let Some(screen) = frame.bookmark_screen()
        {
            rows.push(Row {
                id: CommandId::FindScrollbackBookmark(origin, screen),
                label: FIND_BOOKMARK_LABEL.into(),
                chord: String::new(),
                enabled: !frame.scrollback_bookmarks.is_empty(),
            });
        }
        rows.push(Row {
            id: CommandId::ToggleScrollbackBookmark(origin),
            label: "Toggle scrollback bookmark".into(),
            chord: String::new(),
            enabled: frame
                .as_ref()
                .is_some_and(|frame| frame.bookmark_target().is_some()),
        });
        for (older, label) in [(true, "Previous bookmark"), (false, "Next bookmark")] {
            rows.push(Row {
                id: CommandId::ScrollToBookmark(origin, older),
                label: label.into(),
                chord: String::new(),
                enabled: frame
                    .as_ref()
                    .is_some_and(|frame| frame.adjacent_bookmark(older).is_some()),
            });
        }
        for (older, label) in [
            (true, "Previous failed command"),
            (false, "Next failed command"),
        ] {
            rows.push(Row {
                id: CommandId::ScrollFailedCommand(origin, older),
                label: label.into(),
                chord: String::new(),
                enabled: frame
                    .as_ref()
                    .is_some_and(|frame| frame.adjacent_failed_command(older).is_some()),
            });
        }
        let selection = self.active_selection();
        let appearance = self.appearance.read().unwrap();
        for binding in BINDINGS
            .iter()
            .filter(|b| b.action != KeyAction::CommandPalette)
        {
            let shortcut = Shortcut::from(binding.action);
            let id = CommandId::Shortcut(shortcut);
            let row = match rows.iter_mut().find(|r| r.id == id) {
                Some(row) => row,
                None => {
                    // Pane commands absent from the native menu require a different layout or source.
                    let enabled = match shortcut {
                        Shortcut::NewTab => true,
                        Shortcut::CloseTab => tab_count > 0 && !self.active_tab_is_pinned(),
                        Shortcut::ClosePane => self.can_close_active_pane(),
                        Shortcut::Find | Shortcut::FindNext | Shortcut::FindPrevious => {
                            live.is_some_and(LiveTab::find_available)
                        }
                        Shortcut::NextTab | Shortcut::PreviousTab => tab_count > 1,
                        Shortcut::Select(index) => index < tab_count,
                        _ => false,
                    };
                    rows.push(Row {
                        id,
                        label: binding.label.into(),
                        chord: String::new(),
                        enabled,
                    });
                    rows.last_mut().unwrap()
                }
            };
            row.chord = appearance
                .shortcuts
                .get(binding)
                .map(|chord| chord.to_string())
                .unwrap_or_default();
            match shortcut {
                Shortcut::ReopenClosedTab => {
                    let closed = self.closed_tabs.back().map(|tab| tab.group);
                    row.enabled = closed.is_some();
                    row.id = CommandId::ReopenClosedTab(closed);
                }
                Shortcut::ReopenClosedPane => {
                    let closed = self.closed_pane.as_ref().map(|pane| pane.id);
                    row.enabled = closed.is_some();
                    row.id = CommandId::ReopenClosedPane(closed);
                }
                Shortcut::FocusPreviousPane => {
                    row.enabled = previous.is_some();
                    row.id = CommandId::FocusPreviousPane(previous);
                }
                shortcut if let Some(destination) = directory_split::destination(shortcut) => {
                    row.id = CommandId::OpenInDirectory(destination, directory_split.clone());
                }
                Shortcut::CopyCurrentDirectory => {
                    row.id = CommandId::CopyCurrentDirectory(directory_split.clone());
                }
                Shortcut::ClearScrollback => {
                    row.id = CommandId::ClearScrollback(pane);
                }
                Shortcut::SelectAll => {
                    row.id = CommandId::SelectAll(origin);
                    row.enabled = self.copy_mode_target().is_some();
                }
                Shortcut::Find | Shortcut::FindNext | Shortcut::FindPrevious => {
                    row.id = CommandId::PaneFind(origin, shortcut);
                }
                Shortcut::DuplicateTabLayout | Shortcut::MoveTabLeft | Shortcut::MoveTabRight => {
                    // A click queued on this row must keep its original tab even
                    // when a control client switches tabs before we receive it.
                    row.id = CommandId::TabAction(shortcut, self.tabs.active_group_id());
                }
                Shortcut::SwapPane(direction) => {
                    if let Some(target) = arrangement.get(direction) {
                        row.id = CommandId::SwapPanes(target);
                    }
                }
                Shortcut::RotateSplit => {
                    if let Some(target) = &arrangement.rotation {
                        row.id = CommandId::RotateSplit(target.clone());
                    } else {
                        row.enabled = false;
                    }
                }
                Shortcut::BalancePanes => {
                    if let Some(target) = &arrangement.balance {
                        row.id = CommandId::BalancePanes(target.clone());
                    } else {
                        row.enabled = false;
                    }
                }
                Shortcut::FocusPane(dir) => {
                    row.enabled = self
                        .tabs
                        .active_id()
                        .zip(self.tabs.active_layout())
                        .is_some_and(|(id, layout)| layout.neighbor(id, dir).is_some());
                }
                Shortcut::KeyboardHints | Shortcut::CopyMode => {
                    row.enabled = live.is_some_and(|live| {
                        !live.find.open
                            && !live.device_busy
                            && !live.browsing_sessions()
                            && live.session.frames.load().prompt.is_none()
                    });
                }
                _ => {}
            }
        }
        for (shortcut, label, chord, enabled) in [
            (Shortcut::Copy, "Copy selection", "Ctrl+Shift+C", selection),
            (
                Shortcut::Zoom(1),
                "Increase font size",
                "Ctrl++",
                appearance.font_size < 72.0,
            ),
            (
                Shortcut::Zoom(-1),
                "Decrease font size",
                "Ctrl+-",
                appearance.font_size > 6.0,
            ),
            (Shortcut::ResetZoom, "Reset font size", "Ctrl+0", true),
        ] {
            rows.push(Row {
                id: if shortcut == Shortcut::Copy {
                    CommandId::CopySelection(origin)
                } else {
                    CommandId::Shortcut(shortcut)
                },
                label: label.into(),
                chord: chord.into(),
                enabled,
            });
        }
        if self.has_window_group_runs() {
            rows.extend(
                [
                    (WindowRunScope::All, host_groups::WINDOW_RUN_SEARCH_LABEL),
                    (
                        WindowRunScope::ArmedOutputAlerts,
                        host_groups::WINDOW_OUTPUT_ALERT_SEARCH_LABEL,
                    ),
                ]
                .into_iter()
                .map(|(scope, label)| Row {
                    id: CommandId::SearchWindowGroupRuns(scope),
                    label: label.into(),
                    chord: String::new(),
                    enabled: true,
                }),
            );
        }
        if let Some(group) = self.host_group_command_target() {
            rows.extend(self.group_saved_rows(group));
            rows.extend(self.roster_commands(group).into_iter().map(|command| Row {
                label: command.label().into(),
                id: CommandId::SshRoster(group, command),
                chord: String::new(),
                enabled: true,
            }));
            rows.push(Row {
                id: CommandId::SearchGroupRecentCommands(group),
                label: host_groups::RECENT_COMMANDS_LABEL.into(),
                chord: String::new(),
                enabled: true,
            });
            if self.group_run_choices(group, "").next().is_some() {
                rows.push(Row {
                    id: CommandId::SearchGroupRuns(group),
                    label: host_groups::FIND_RUN_LABEL.into(),
                    chord: String::new(),
                    enabled: true,
                });
            }
            if let Some((name, sets)) = self.recipient_sets_for_tab(group) {
                rows.push(Row {
                    id: CommandId::ManageRecipientSets(group, name.into()),
                    label: "Manage recipient sets…".into(),
                    chord: String::new(),
                    enabled: true,
                });
                rows.extend(sets.into_iter().flatten().map(|(set, members)| Row {
                    id: CommandId::ApplyRecipientSet(
                        group,
                        host_groups::RecipientSetOffer {
                            group: name.into(),
                            name: set.clone(),
                            members: members.clone(),
                        },
                    ),
                    label: format!("Apply recipient set · {set}"),
                    chord: String::new(),
                    enabled: true,
                }));
            }
        } else {
            let origin = self.saved_command_origin();
            rows.extend(self.saved_commands.entries().iter().map(|entry| Row {
                id: CommandId::SavedCommand(origin, entry.clone()),
                label: format!("Insert command · {}", entry.name),
                chord: String::new(),
                enabled: origin.is_some(),
            }));
        }
        filter(rows, query)
    }

    fn group_saved_rows(&self, group: TabId) -> impl Iterator<Item = Row> + '_ {
        self.saved_commands.entries().iter().map(move |entry| Row {
            id: CommandId::GroupSavedCommand(group, entry.clone()),
            label: format!("Load group command · {}", entry.name),
            chord: String::new(),
            enabled: true,
        })
    }

    pub(super) fn group_saved_command_rows(&self, group: TabId, query: &str) -> Vec<Row> {
        if self.host_group_command_target() != Some(group) {
            return Vec::new();
        }
        filter(self.group_saved_rows(group).collect(), query)
    }

    pub(super) fn group_recent_command_rows(&self, group: TabId, query: &str) -> Vec<Row> {
        self.recent_group_commands(group, query)
            .map(|(submission, command)| Row {
                id: CommandId::GroupRecentCommand(group, submission),
                label: command.into(),
                chord: String::new(),
                enabled: true,
            })
            .collect()
    }

    pub(super) fn group_run_rows(&self, group: TabId, query: &str) -> Vec<Row> {
        self.group_run_choices(group, query)
            .map(|(run, label)| Row {
                id: CommandId::GroupRun(group, run),
                label: format!("Run {run} · {label}"),
                chord: String::new(),
                enabled: true,
            })
            .collect()
    }

    pub(super) fn run_palette_command(&mut self, id: CommandId, ctx: &ComponentContext<Self>) {
        let history_search = match &id {
            CommandId::OpenWindowCommand(pane, link, _) => {
                self.window_history_return((*pane, *link))
            }
            _ => None,
        };
        self.close_picker(ctx);
        let message = match id {
            CommandId::RenamePane(target) => tab_title::show_pane(target),
            CommandId::SaveLiveProfile(origin) => live_profile::open(origin),
            CommandId::ReopenClosedTab(group) => Message::ReopenClosedTab(group),
            CommandId::ReopenClosedPane(id) => Message::ReopenClosedPane(id),
            CommandId::FocusPreviousPane(target) => previous_pane::message(target),
            CommandId::PaneColorScheme(pane, scheme) => Message::PaneColorScheme(pane, scheme),
            CommandId::LocalForward(origin) => local_forwards::open(origin),
            CommandId::OpenInDirectory(destination, target) => {
                directory_split::message(destination, target.as_ref())
            }
            CommandId::OpenExplorer(target) => directory_split::explorer_message(target.as_ref()),
            CommandId::CopyCurrentDirectory(target) => {
                directory_split::copy_message(target.as_ref())
            }
            CommandId::Clipboard(origin, raw) => {
                Message::Clipboard(clipboard_paste::Action::Start(origin, raw))
            }
            CommandId::SavedCommand(Some(origin), entry) => {
                return self.insert_saved_command(origin, entry, ctx);
            }
            CommandId::SavedCommand(None, _) => return,
            CommandId::GroupSavedCommand(group, entry) => {
                Message::HostGroup(host_groups::Action::LoadSavedCommand(group, entry))
            }
            CommandId::SearchGroupRecentCommands(group) => {
                Message::HostGroup(host_groups::Action::OpenRecentCommands(group))
            }
            CommandId::GroupRecentCommand(group, submission) => {
                Message::HostGroup(host_groups::Action::LoadRecentCommand(group, submission))
            }
            CommandId::SearchGroupRuns(group) => {
                Message::HostGroup(host_groups::Action::OpenRunPicker(group))
            }
            CommandId::GroupRun(group, run) => {
                Message::HostGroup(host_groups::Action::ViewSubmission(group, run))
            }
            CommandId::SearchWindowGroupRuns(scope) => {
                self.open_window_group_run_picker(scope, ctx);
                return;
            }
            CommandId::WindowGroupRun(scope, group, run) => {
                self.open_window_group_run(scope, group, run, ctx);
                return;
            }
            CommandId::FocusTab(window, group) => {
                return self.request_window_tab(window, group, ctx);
            }
            CommandId::FocusRunningCommand(pane, target) => {
                self.focus_running_command(pane, target, ctx);
                return;
            }
            CommandId::OpenWindowCommand(pane, link, hit) => {
                if !self.open_command_history_preview(pane, Some(link), hit, history_search, ctx) {
                    self.window_error =
                        Some("That command is no longer retained in its source pane.".into());
                }
                return;
            }
            CommandId::OpenSavedHostGroup(name, members) => {
                Message::HostGroup(host_groups::Action::OpenNamedGroup { name, members })
            }
            CommandId::ReviewSavedHostGroup(offer) => {
                Message::HostGroup(host_groups::Action::ReviewNamedGroup(offer))
            }
            CommandId::ManageRecipientSets(group, name) => {
                Message::HostGroup(host_groups::Action::RecipientSets(
                    group,
                    host_groups::RecipientSetAction::OpenFromPalette(name),
                ))
            }
            CommandId::ApplyRecipientSet(group, offer) => {
                Message::HostGroup(host_groups::Action::RecipientSets(
                    group,
                    host_groups::RecipientSetAction::ApplyFromPalette(offer),
                ))
            }
            CommandId::SshRoster(group, command) => Message::HostGroup(command.into_action(group)),
            CommandId::TabAction(shortcut, group) => {
                bind_tab_action(Message::Shortcut(shortcut), group)
            }
            CommandId::SetTabPinned(group, pinned) => group.map_or(Message::ControlIdle, |group| {
                Message::SetTabPinned(group, pinned)
            }),
            CommandId::SetTabColor(group, color) => Message::SetTabColor(group, color),
            CommandId::CloseTabs(offer) => offer.map_or(Message::ControlIdle, Message::CloseTabs),
            CommandId::SwapPanes(target) => Message::SwapPanes(target),
            CommandId::RotateSplit(target) => Message::RotateSplit(target),
            CommandId::BalancePanes(target) => Message::BalancePanes(target),
            CommandId::SharedFolders(Some(pane)) => {
                Message::FolderShare(folder_shares::Action::Open(pane))
            }
            CommandId::SharedFolders(None) => return,
            CommandId::RestartExited(Some(exit)) => Message::RestartExited(exit),
            CommandId::RestartExited(None) => return,
            CommandId::StopSshAttempt(attempt) => Message::StopSshAttempt(attempt),
            CommandId::RetrySshNow(retry) => Message::RetrySshNow(retry),
            CommandId::DisconnectSsh(target) => {
                Message::DisconnectSsh(disconnect_ssh::Action::Review(target))
            }
            CommandId::SaveScrollback(pane) => {
                Message::Export(text_export::Action::StartScrollback(pane))
            }
            CommandId::CopyPaneText(pane) => {
                Message::Export(text_export::Action::CopyPaneText(pane))
            }
            CommandId::PaneImage(pane, destination) => {
                Message::Export(text_export::Action::StartPaneImage(pane, destination))
            }
            CommandId::ClearScrollback(pane) => {
                pane.map_or(Message::ControlIdle, Message::ClearScrollback)
            }
            CommandId::ScrollFailedCommand(origin, older) => {
                Message::ScrollFailedCommand(origin, older)
            }
            CommandId::ToggleScrollbackBookmark(origin) => {
                Message::ToggleScrollbackBookmark(origin)
            }
            CommandId::ScrollToBookmark(origin, older) => Message::ScrollToBookmark(origin, older),
            CommandId::FindScrollbackBookmark(origin, screen) => {
                return self.open_bookmark_search(origin, screen, ctx);
            }
            CommandId::OpenScrollbackBookmark(origin, screen, row) => {
                return self.reveal_bookmark(origin, screen, row);
            }
            CommandId::SelectAll(origin) => Message::SelectAll(origin),
            CommandId::PaneFind(origin, shortcut) => Message::PaneFind(origin, shortcut),
            CommandId::CopySelection(origin) => Message::CopySelection(origin),
            CommandId::LongCommands(origin, enabled) => {
                attention::set_long_commands(origin, enabled)
            }
            CommandId::Profile(label) => Message::ProfileMenu(label),
            CommandId::LaunchProfile(destination, name, _) => {
                match destination {
                    ProfileDestination::Tab => {
                        if let Some(source) = self.catalog.source(&name) {
                            self.open(name.clone(), source, Some(name), ctx);
                        }
                    }
                    ProfileDestination::Split { axis, origin }
                        if self.move_source() == Some(origin) =>
                    {
                        self.split_pane(axis, Some(name), ctx);
                    }
                    ProfileDestination::Split { .. } => {}
                }
                return;
            }
            CommandId::Menu(label) => tab_menu_message(label),
            CommandId::MoveTabToExistingWindow(group, window) => {
                Message::MoveTabToExistingWindow(group, window)
            }
            CommandId::BroadcastTyping(action) => Message::BroadcastTyping(action),
            CommandId::Recording(choice) => recordings::invoke(choice),
            CommandId::Shortcut(shortcut) => Message::Shortcut(shortcut),
        };
        let tab_switch = matches!(
            &message,
            Message::Shortcut(Shortcut::NextTab | Shortcut::PreviousTab)
        );
        self.update(message, ctx);
        // Invoking from the palette has no switcher modifier to release.
        if tab_switch {
            self.finish_switcher(true, ctx);
        }
    }
}

pub(super) fn view(
    rows: &[Row],
    selected: usize,
    query: &str,
    input: &ElementRef<TextBox>,
    mode: Search,
    app: &TerminalApp,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    let narrow_history = app.width < 520.0;
    let running = app
        .picker
        .as_ref()
        .filter(|_| mode == Search::RunningCommands);
    let search_height = if mode == Search::CommandHistory {
        if narrow_history { 264.0 } else { 232.0 }
    } else if mode == Search::RunningCommands {
        running.map_or(296.0, pane_picker::Picker::running_controls_height)
    } else {
        156.0
    };
    let page = ((app.window_height - search_height) / 40.0).clamp(1.0, 10.0) as usize;
    let first = selected / page * page;
    let search_label = mode.placeholder();
    let trimming = if matches!(
        mode,
        Search::Tabs
            | Search::CommandHistory
            | Search::RunningCommands
            | Search::Profiles(_)
            | Search::GroupRecent(_)
            | Search::GroupRuns(_)
            | Search::WindowGroupRuns(_)
            | Search::Bookmarks(..)
    ) {
        TextTrimming::CharacterEllipsis
    } else {
        TextTrimming::None
    };
    let search = Grid::new()
        .columns([GridLength::STAR, GridLength::Auto])
        .children((
            TextBox::new()
                .element_ref(input)
                .text(query)
                .placeholder_text(if mode.is_profiles() {
                    "Search by profile name or SSH address"
                } else {
                    search_label
                })
                .automation_name(search_label)
                .on_text_changed(ctx.callback(Message::PickerQuery)),
            Button::new()
                .grid_column(1)
                .style(ButtonStyle::Subtle)
                .automation_name(match mode {
                    Search::Tabs => "Close tab search",
                    Search::CommandHistory => "Close command history search",
                    Search::RunningCommands
                        if running
                            .is_some_and(pane_picker::Picker::reviewing_running_interrupt) =>
                    {
                        "Cancel command interrupt"
                    }
                    Search::RunningCommands => "Close running command search",
                    Search::Profiles(_) => "Close profile search",
                    Search::Bookmarks(..) => "Close bookmark search",
                    _ => "Close command palette",
                })
                .on_click(ctx.callback(|_| Message::Shortcut(Shortcut::PickerCancel)))
                .content(TextBlock::new().text("×")),
        ));
    let mut results = Vec::new();
    for (index, row) in rows.iter().enumerate().skip(first).take(page) {
        let id = row.id.clone();
        let caption = if row.chord.is_empty() {
            row.label.clone()
        } else {
            format!("{}    {}", row.label, row.chord)
        };
        let button = Button::new()
            .automation_name(&caption)
            .is_enabled(row.enabled)
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .horizontal_content_alignment(HorizontalAlignment::Stretch)
            .style(if index == selected {
                ButtonStyle::Accent
            } else {
                ButtonStyle::Subtle
            })
            .on_click(ctx.callback(move |_| Message::PaletteChoose(id.clone())))
            .content(
                Grid::new()
                    .columns([GridLength::STAR, GridLength::Auto])
                    .children((
                        TextBlock::new()
                            .text(&row.label)
                            .text_trimming(trimming)
                            .horizontal_alignment(HorizontalAlignment::Left),
                        TextBlock::new().grid_column(1).text(&row.chord),
                    )),
            );
        results.push(KeyedView::new(index, button));
    }
    if rows.is_empty() {
        results.push(KeyedView::new(
            "empty",
            TextBlock::new().text(
                app.picker
                    .as_ref()
                    .and_then(|picker| {
                        picker
                            .window_history_status()
                            .or_else(|| picker.bookmark_status(app))
                    })
                    .unwrap_or_else(|| {
                        if mode.is_profiles() && app.profile_error.is_some() {
                            "Could not load profiles"
                        } else {
                            mode.empty(query)
                        }
                    }),
            ),
        ));
    } else if rows.len() > page {
        let last = (first + page).min(rows.len());
        results.push(KeyedView::new(
            "pages",
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    Button::new()
                        .automation_name(match mode {
                            Search::Tabs => "Previous tabs",
                            Search::Profiles(_) => "Previous profiles",
                            _ => "Previous commands",
                        })
                        .is_enabled(first > 0)
                        .on_click(ctx.callback(move |_| {
                            Message::Shortcut(Shortcut::PickerMove(-(page as isize)))
                        }))
                        .content(TextBlock::new().text("‹")),
                    TextBlock::new()
                        .vertical_alignment(VerticalAlignment::Center)
                        .text(format!("{}–{last} of {}", first + 1, rows.len())),
                    Button::new()
                        .automation_name(match mode {
                            Search::Tabs => "Next tabs",
                            Search::Profiles(_) => "Next profiles",
                            _ => "Next commands",
                        })
                        .is_enabled(last < rows.len())
                        .on_click(ctx.callback(move |_| {
                            Message::Shortcut(Shortcut::PickerMove(page as isize))
                        }))
                        .content(TextBlock::new().text("›")),
                )),
        ));
    }
    let history = app
        .picker
        .as_ref()
        .filter(|_| mode == Search::CommandHistory);
    let history_controls = history.map_or_else(View::empty, |picker| {
        picker.window_history_controls(narrow_history, ctx)
    });
    let history_export =
        history.map_or_else(View::empty, |picker| picker.history_export_button(app, ctx));
    let running_controls = running.map_or_else(View::empty, |picker| {
        picker.running_command_controls(app, ctx)
    });
    pane_picker::Picker::overlay(
        StackPanel::new().spacing(6.0).children((
            search,
            history_controls,
            StackPanel::new().spacing(4.0).keyed_children(results),
            history_export,
            running_controls,
        )),
        (app.width - 32.0).clamp(1.0, 660.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_search_disambiguates_duplicate_titles_and_exact_window_numbers() {
        let tab = |group, title: &str| SearchTab {
            group,
            title: title.into(),
        };
        let windows: [(u64, Rc<[SearchTab]>); 3] = [
            (1, [tab(8, "Build")].into()),
            (2, [tab(9, "Other"), tab(10, "Build")].into()),
            (12, [tab(11, "Build")].into()),
        ];

        let all = search_tabs("build", 1, Some(8), windows.clone());
        assert_eq!(
            all.iter().map(|row| &row.id).collect::<Vec<_>>(),
            [
                &CommandId::FocusTab(1, 8),
                &CommandId::FocusTab(2, 10),
                &CommandId::FocusTab(12, 11),
            ]
        );
        assert_eq!(all[0].chord, "Tab 1 · Current");
        assert_eq!(all[1].chord, "Window 2 · Tab 2");

        let selected = search_tabs("  BUILD  window 2  tab 2  ", 1, Some(8), windows);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, CommandId::FocusTab(2, 10));
    }

    #[test]
    fn profile_search_keeps_catalog_order_and_matches_names_and_addresses() {
        let mut catalog = Profiles::default();
        catalog.local.insert(
            "Terminal lab".into(),
            serde_json::from_value(serde_json::json!({"executable": "powershell.exe"})).unwrap(),
        );
        let ssh = serde_json::from_value::<SshOptions>(serde_json::json!({
            "host": "2001:db8::1", "port": 6789, "user": "ops", "auth": "none"
        }))
        .unwrap();
        catalog.profiles.insert("West lab".into(), ssh.clone());
        catalog
            .profiles
            .insert("West    lab".into(), SshOptions { port: 6792, ..ssh });
        catalog.profile_order = ["Terminal lab", "West    lab", "West lab"]
            .map(String::from)
            .into();

        let labels = |query| {
            profile_search_rows(&catalog, ProfileDestination::Tab, query)
                .into_iter()
                .map(|row| row.label)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            labels(""),
            [
                "Terminal lab · Local",
                "West    lab · ops@[2001:db8::1]:6792",
                "West lab · ops@[2001:db8::1]:6789",
            ]
        );
        assert_eq!(labels("LOCAL LAB"), ["Terminal lab · Local"]);
        assert_eq!(
            labels("6789 WEST OPS"),
            ["West lab · ops@[2001:db8::1]:6789"]
        );
        assert_eq!(labels("west lab")[0], "West lab · ops@[2001:db8::1]:6789");
        assert!(labels("west east").is_empty());
        assert_eq!(
            profile_search_rows(&catalog, ProfileDestination::Tab, "6792")[0].id,
            CommandId::LaunchProfile(
                ProfileDestination::Tab,
                "West    lab".into(),
                Some("ops@[2001:db8::1]:6792".into())
            )
        );
    }

    #[test]
    fn select_all_menu_and_palette_share_the_configured_chord() {
        let mut rows = Vec::new();
        menu_rows(
            vec![
                MenuItem::item("select-all", "Select all terminal text    F10"),
                MenuItem::item("copy-selection", "Copy selection    Ctrl+Shift+C"),
            ],
            false,
            &mut rows,
        );
        assert_eq!(rows.len(), 1, "Copy has a separate pane-bound palette row");
        assert_eq!(rows[0].id, CommandId::Shortcut(Shortcut::SelectAll));
        assert_eq!(rows[0].chord, "F10");
        assert_eq!(rows[0].label, KeyAction::SelectAll.binding().label);
    }

    #[test]
    fn profile_results_preserve_menu_order_within_each_action() {
        let mut rows = Vec::new();
        menu_rows(
            vec![
                MenuItem::item("local:z", "Zulu · Local"),
                MenuItem::item("profile:a", "Alpha    user@host:22"),
            ],
            true,
            &mut rows,
        );
        menu_rows(
            vec![
                MenuItem::item("z", "Split right with Zulu"),
                MenuItem::item("a", "Split right with Alpha"),
            ],
            false,
            &mut rows,
        );
        let labels = filter(rows, "with")
            .into_iter()
            .map(|r| r.label)
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            [
                "New tab with Zulu · Local",
                "New tab with Alpha    user@host:22",
                "Split right with Zulu",
                "Split right with Alpha",
            ]
        );
    }

    #[test]
    fn nested_profile_names_survive_filtering_and_dispatch() {
        let profile = "ops    west: blue";
        let label = format!("Split right with {profile}");
        let mut rows = Vec::new();
        menu_rows(
            vec![MenuItem::submenu(
                "right-profile",
                "Split",
                [MenuItem::item("command-palette", &label)],
            )],
            false,
            &mut rows,
        );
        let found = filter(rows, "WEST ops");
        assert_eq!(found.len(), 1);
        let CommandId::Menu(callback) = &found[0].id else {
            panic!("expected menu command")
        };
        assert_eq!(found[0].label, label);
        assert!(matches!(tab_menu_message(callback.clone()),
            Message::SplitPane(Axis::Columns, Some(name)) if name == profile));

        let display = format!("{profile}    me@example:23");
        let mut rows = Vec::new();
        menu_rows(
            vec![MenuItem::item("profile:ops", &display)],
            true,
            &mut rows,
        );
        assert_eq!(rows[0].id, CommandId::Profile(display));
    }

    #[test]
    fn unavailable_tab_duplication_is_searchable_with_its_reason() {
        let mut rows = Vec::new();
        menu_rows(
            vec![MenuItem::disabled(
                "duplicate_tab_layout",
                "Duplicate tab layout: Select a terminal tab",
            )],
            false,
            &mut rows,
        );
        let found = filter(rows, "duplicate layout");
        assert_eq!(found.len(), 1);
        assert!(!found[0].enabled);
        assert!(found[0].label.contains("Select a terminal tab"));
        assert_eq!(
            found[0].id,
            CommandId::Shortcut(Shortcut::DuplicateTabLayout)
        );
    }

    #[test]
    fn search_includes_chords_and_orders_disabled_matches_after_enabled_commands() {
        let mut rows = Vec::new();
        menu_rows(
            vec![
                MenuItem::disabled("focus-left", "Focus left    F9"),
                MenuItem::item("focus-right", "Focus right    F8"),
                MenuItem::item("command-palette", "Command palette…    F7"),
            ],
            false,
            &mut rows,
        );
        let found = filter(rows, " FOCUS ");
        assert_eq!(found.len(), 2);
        assert_eq!(
            (found[0].label.as_str(), found[0].chord.as_str()),
            ("Focus right", "F8")
        );
        assert!(!found[1].enabled);
        let found = filter(found, "f9  left");
        assert_eq!(
            found[0].id,
            CommandId::Shortcut(Shortcut::FocusPane(Direction::Left))
        );
        assert_eq!(found.len(), 1);
    }
}
