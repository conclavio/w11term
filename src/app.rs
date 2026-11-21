use crate::{
    input::{KeyboardBridge, PickerStage, Shortcut},
    renderer::{Metrics, PADDING, Renderer, STATUS_HEIGHT},
};
use anyhow::Context as _;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};
use w11term::{
    appearance::{Appearance, installed_fonts},
    control::{Pane, Registry, Server},
    interaction::{
        Link, Point, Selection, TabSwitcherMode, link_at, paste_text, safe_url, word_range,
    },
    options::{Authentication, LocalOptions, Profiles, Source, SshOptions, TerminalType},
    pane_layout::{Axis, Direction},
    profiles::{ProfileSnapshot, ProfileStore},
    session::{Command, Session},
    shortcuts::Action as KeyAction,
    tabs::{TabId, Tabs},
};
use wezterm_term::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind, TerminalSize};
use windows_canvas::{Canvas, Invalidator};
use windows_reactor::*;

const NEW_PROFILE: &str = "New Profile…";
const LAB: &str = "Terminal lab";
const RESTART_EXITED_PANE: &str = "Restart exited pane";

mod attention;
mod command_history;
mod config_import;
mod file_drawer;
mod file_picker;
mod folder_revoke;
mod folder_shares;
mod folder_sharing_state;
mod keyboard_settings;
mod local_profile;
mod pattern_settings;
use file_drawer::{FileAction, FileDrawer};
#[cfg(feature = "ui-smoke")]
mod accessibility_smoke;
mod command_palette;
mod durable_view;
#[cfg(feature = "ui-smoke")]
mod ime_smoke;
mod inspector;
mod keyboard_copy;
mod keyboard_hints;
#[cfg(feature = "ui-smoke")]
mod local_smoke;
mod pane_picker;
mod pane_view;
mod scrollback_export;
mod session_browser;
#[cfg(feature = "ui-smoke")]
mod split_smoke;
mod tab_title;
mod tab_windows;
mod taskbar;
pub use tab_windows::WindowLaunch;
mod workspace_view;
use pane_view::PaneSurface;

struct LiveTab {
    folder_sharing: Option<w11term::folder_grants::PaneShares>,
    read_only: Rc<Cell<bool>>,
    startup: Option<Box<w11term::workspace::SshStartup>>,
    observed_frame: u64,
    files: Box<FileDrawer>,
    discovery_page: Option<usize>,
    copy_output_unavailable: Option<&'static str>,
    confirm_end: bool,
    durable_phase: Option<w11term::ssh::DurablePhase>,
    source: Source,
    profile_name: Option<String>,
    surface: PaneSurface,
    device_busy: bool,
    device_message: Option<String>,
    session: Box<Session>,
    last_size: Rc<Cell<Option<TerminalSize>>>,
    error: Option<String>,
    selection: Rc<RefCell<Option<Selection>>>,
    hover: Rc<RefCell<Option<Link>>>,
    remote_mouse: RemoteMouse,
    clicked_link: Option<String>,
    last_click: Option<(Instant, Point, u8)>,
    scroll_direction: isize,
    scroll_tick: bool,
    gesture: u64,
    find: Find,
    prompt_id: Option<u64>,
    answers: Vec<zeroize::Zeroizing<String>>,
    connection_generation: u64,
}
struct Find {
    open: bool,
    query: String,
    sensitive: bool,
    regex: bool,
    pending: bool,
    epoch: u64,
    input: ElementRef<TextBox>,
    focused: bool,
}
#[derive(Clone, Copy)]
pub enum FindOption {
    Case,
    Regex,
}
impl LiveTab {
    fn reset_transport_input(&mut self, keyboard: &KeyboardBridge) {
        self.remote_mouse = RemoteMouse::default();
        self.end_pointer_gesture();
        keyboard.reset_keys(&self.session.input);
    }

    fn end_pointer_gesture(&mut self) {
        if let Some(release) = self.remote_mouse.release() {
            let _ = self.session.input.send(Command::Mouse(release));
        }
        if let Some(selection) = self.selection.borrow_mut().as_mut() {
            selection.dragging = false;
        }
        self.clicked_link = None;
        self.scroll_direction = 0;
        self.scroll_tick = false;
        self.gesture += 1;
    }

    fn browsing_sessions(&self) -> bool {
        self.discovery_page.is_some()
            && self
                .session
                .durable_state()
                .is_some_and(|state| state.phase.is_browser())
    }
}

// The window that received a press also owns its release, even if capture is
// interrupted or the pane's live session moves to another window.
#[derive(Default)]
struct RemoteMouse {
    held: Option<MouseEvent>,
}

impl RemoteMouse {
    fn event(&mut self, mut event: MouseEvent) -> Option<MouseEvent> {
        match event.kind {
            MouseEventKind::Press if event.button == MouseButton::None => return None,
            MouseEventKind::Press => self.held = Some(event),
            MouseEventKind::Release => event.button = self.held.take()?.button,
            MouseEventKind::Move => {
                if let Some(held) = &mut self.held {
                    *held = MouseEvent {
                        button: held.button,
                        ..event
                    };
                } else if event.button != MouseButton::None {
                    // A drag that began in another pane or transport is not this stream's input.
                    return None;
                }
            }
        }
        Some(event)
    }

    fn release(&mut self) -> Option<MouseEvent> {
        self.held.take().map(|event| MouseEvent {
            kind: MouseEventKind::Release,
            ..event
        })
    }
}

impl Default for Find {
    fn default() -> Self {
        Self {
            open: false,
            query: String::new(),
            sensitive: false,
            regex: false,
            pending: false,
            epoch: 0,
            input: ElementRef::new(),
            focused: false,
        }
    }
}
impl Find {
    fn matches(&self, result: &w11term::search::SearchSnapshot) -> bool {
        result.query == self.query
            && result.sensitive == self.sensitive
            && result.regex == self.regex
    }

    fn apply(&mut self, session: &Session) {
        self.pending = session
            .input
            .send(Command::Search {
                query: self.query.clone(),
                sensitive: self.sensitive,
                regex: self.regex,
            })
            .is_err();
    }

    fn limit_query(text: &mut String, regex: bool) {
        let limit = w11term::search::MAX_QUERY_BYTES;
        if text.len() > limit {
            // Keep one overflowing scalar in regex mode so an oversized paste
            // stays invalid until corrected. Literal Find retains its old cap.
            let end = if regex {
                text.ceil_char_boundary(limit + 1)
            } else {
                text.floor_char_boundary(limit)
            };
            text.truncate(end);
        }
    }

    fn status(&self, result: Option<&w11term::search::SearchSnapshot>) -> String {
        if self.query.is_empty() {
            return "Find in scrollback".into();
        }
        let Some(result) = result.filter(|result| self.matches(result)) else {
            return "Searching…".into();
        };
        if let Some(error) = &result.error {
            return if self.regex {
                format!("Invalid regex — {error}. Edit the pattern or turn off Regex.")
            } else {
                error.clone()
            };
        }
        let mut count = if result.total == 0 {
            "No matches".into()
        } else {
            format!(
                "{} / {}{}",
                result.selected.map_or(0, |i| i + 1),
                result.total,
                if result.truncated { "+" } else { "" }
            )
        };
        if result.incomplete {
            count.push_str(" · Some long lines were skipped");
        }
        count
    }
}
struct TabSwitcher {
    ids: Vec<TabId>,
    index: usize,
}
#[derive(Clone, Copy)]
pub enum InteractionField {
    CopyOnSelect,
    TrimBlock,
    TrimPaste,
    Snap,
    ScrollZoom,
    DetectUrls,
    Osc52,
}
enum SettingsEditor {
    Keyboard(keyboard_settings::KeyboardPage),
    Patterns(pattern_settings::PatternPage),
}
enum Page {
    SharedFolders(folder_shares::Panel),
    Attention,
    CommandHistory(command_history::HistoryPage),
    Terminal(LiveTab),
    LocalProfile(local_profile::Draft),
    ImportConfig(config_import::ImportPage),
    Workspaces(workspace_view::WorkspacePage),
    Settings(Option<SettingsEditor>),
    NewProfile(ProfileDraft),
}
struct AppTab {
    label: String,
    page: Page,
}

impl AppTab {
    fn exited_pane(&self) -> Option<ExitedPane> {
        let Page::Terminal(live) = &self.page else {
            return None;
        };
        Some(ExitedPane {
            pane: live.session.pane_id,
            token: live.session.events.restart_token()?,
        })
    }

    fn profile_name(&self) -> Option<&str> {
        match &self.page {
            Page::Terminal(live) => live.profile_name.as_deref(),
            _ => None,
        }
    }

    fn with_profile(mut self, name: Option<String>) -> Self {
        if let Page::Terminal(live) = &mut self.page {
            live.profile_name = name;
        }
        self
    }

    fn ssh_profile_name(&self) -> Option<&str> {
        match &self.page {
            Page::Terminal(live) if matches!(live.source, Source::Ssh(_)) => {
                live.profile_name.as_deref()
            }
            _ => None,
        }
    }

    fn pane_id(&self) -> Option<u64> {
        match &self.page {
            Page::Terminal(tab) => Some(tab.session.pane_id),
            _ => None,
        }
    }
}

#[derive(Clone, Default)]
enum DraftOrigin {
    #[default]
    New,
    Edit(ProfileSnapshot),
    Copy(ProfileSnapshot),
}
impl DraftOrigin {
    fn editing(&self) -> Option<&ProfileSnapshot> {
        match self {
            Self::Edit(base) => Some(base),
            _ => None,
        }
    }
}

#[derive(Clone)]
struct ProfileDraft {
    origin: DraftOrigin,
    folder_revoke: folder_revoke::Editor,
    confirm_delete: bool,
    proxy_jump: Vec<w11term::options::JumpHost>,
    durable: bool,
    saved: bool,
    name: String,
    host: String,
    user: String,
    port: String,
    terminal_preset: usize,
    custom_terminal: String,
    identity: String,
    fingerprint: String,
    authentication: usize,
    error: Option<String>,
    player: w11term::options::PlayerOptions,
    device: Option<w11term::keys::DeviceKey>,
    generating: bool,
}

impl Default for ProfileDraft {
    fn default() -> Self {
        Self {
            origin: DraftOrigin::New,
            folder_revoke: Default::default(),
            confirm_delete: false,
            proxy_jump: Vec::new(),
            durable: false,
            saved: false,
            name: String::new(),
            host: String::new(),
            user: String::new(),
            port: "22".into(),
            terminal_preset: 0,
            custom_terminal: String::new(),
            identity: String::new(),
            fingerprint: String::new(),
            authentication: 0,
            error: None,
            player: Default::default(),
            device: None,
            generating: false,
        }
    }
}

impl ProfileDraft {
    fn terminal_type(&self) -> anyhow::Result<TerminalType> {
        let name = match self.terminal_preset {
            0 => TerminalType::DEFAULT,
            1 => TerminalType::SIXEL,
            _ => &self.custom_terminal,
        };
        TerminalType::try_from(name.to_owned())
    }

    fn from_options(name: String, options: &SshOptions) -> Self {
        let (authentication, identity) = match &options.authentication {
            Authentication::Agent => (0, String::new()),
            Authentication::Identity { identity } => (1, identity.to_string_lossy().into_owned()),
            Authentication::None => (2, String::new()),
            Authentication::Password => (3, String::new()),
            Authentication::KeyboardInteractive => (4, String::new()),
        };
        Self {
            proxy_jump: options.proxy_jump.clone(),
            name,
            durable: options.durable,
            host: options.host.clone(),
            user: options.user.clone(),
            port: options.port.to_string(),
            terminal_preset: match options.terminal_type.as_str() {
                TerminalType::DEFAULT => 0,
                TerminalType::SIXEL => 1,
                _ => 2,
            },
            custom_terminal: options.terminal_type.as_str().into(),
            identity,
            fingerprint: options.host_key.clone().unwrap_or_default(),
            authentication,
            player: options.player.clone(),
            ..Default::default()
        }
    }
}
fn command_menu_item(key: &str, label: &str, enabled: bool) -> MenuItem {
    if enabled {
        MenuItem::item(key, label)
    } else {
        MenuItem::disabled(key, label)
    }
}
fn state_menu_item(key: &str, label: &str, state: Option<bool>) -> MenuItem {
    match state {
        Some(state) => MenuItem::item(
            key,
            format!("{label}: {}", if state { "On" } else { "Off" }),
        ),
        None => MenuItem::disabled(key, format!("{label} (select a terminal tab)")),
    }
}
fn tab_menu_message(label: String) -> Message {
    if let Some(name) = label.strip_prefix("Browse Herdr terminals on ") {
        return Message::Discovery(session_browser::Action::Discover {
            name: name.into(),
            herdr: true,
        });
    }
    if let Some(name) = label.strip_prefix("Find sessions on ") {
        return Message::Discovery(session_browser::Action::Discover {
            name: name.into(),
            herdr: false,
        });
    }
    if let Some(action) = durable_view::menu_action(&label) {
        return Message::Durability(action);
    }
    for (prefix, axis) in [
        ("Split right with ", Axis::Columns),
        ("Split below with ", Axis::Rows),
    ] {
        if let Some(name) = label.strip_prefix(prefix) {
            return Message::SplitPane(axis, Some(name.to_owned()));
        }
    }
    // Menu callbacks return the displayed label, including the current shortcut.
    let command = label
        .split_once("    ")
        .map_or(label.as_str(), |(name, _)| name);
    match command.split(": ").next().unwrap_or(command) {
        "Terminal inspector" => Message::ToggleInspector,
        "Attention inbox" => Message::Attention(attention::Action::Open),
        "Rename tab…" => Message::TabTitle(tab_title::Action::Show),
        "Duplicate tab layout" => Message::Shortcut(Shortcut::DuplicateTabLayout),
        "SFTP files" => Message::Files(FileAction::Toggle),
        "Workspace startup…" => {
            Message::Workspace(workspace_view::WorkspaceAction::ConfigureStartup)
        }
        "Open files here" => Message::Files(FileAction::OpenHere),
        "Copy last command output" => Message::CopyLastCommand,
        "Command history" => Message::History(command_history::Action::Open),
        "Notify when command finishes" => Message::Attention(attention::Action::WatchCommand(true)),
        "Cancel command notification" => Message::Attention(attention::Action::WatchCommand(false)),
        "Split right" => Message::SplitPane(Axis::Columns, None),
        "Split below" => Message::SplitPane(Axis::Rows, None),
        "Focus left" => Message::PaneNeighbor(Direction::Left),
        "Focus right" => Message::PaneNeighbor(Direction::Right),
        "Focus above" => Message::PaneNeighbor(Direction::Up),
        "Focus below" => Message::PaneNeighbor(Direction::Down),
        "Resize left" => Message::Shortcut(Shortcut::ResizePane(Direction::Left)),
        "Resize right" => Message::Shortcut(Shortcut::ResizePane(Direction::Right)),
        "Resize up" => Message::Shortcut(Shortcut::ResizePane(Direction::Up)),
        "Resize down" => Message::Shortcut(Shortcut::ResizePane(Direction::Down)),
        "Zoom pane" | "Restore panes" => Message::ZoomPane,
        "Close pane" => Message::ClosePane,
        "Enable read-only pane input" => Message::SetPaneReadOnly(true),
        "Disable read-only pane input" => Message::SetPaneReadOnly(false),
        "Move pane to new tab" => Message::MovePaneToTab,
        "Move tab to new window" => Message::MoveTabToWindow,
        "Move pane to existing tab…" => Message::Shortcut(Shortcut::MovePaneToExistingTab),
        "Paste" => Message::Paste,
        "Paste raw (no bracketed paste)" => Message::PasteRaw,
        "Fullscreen (F11)" => Message::Shortcut(Shortcut::Fullscreen),
        "Mute" => Message::TogglePlayer("mute".into()),
        "Game mode (suppress key repeat)" => Message::TogglePlayer("game_mode".into()),
        "Alternate-screen wheel keys" => Message::TogglePlayer("alternate_scroll".into()),
        "Remote named sounds" => Message::TogglePlayer("remote_sounds".into()),
        "Remote asset synchronization" => Message::TogglePlayer("sync_assets".into()),
        "Edit current profile…" => Message::EditActiveProfile,
        "Enroll this device (at game prompt)" => Message::DeviceAction(false),
        "Forget this device (revoke game key)" => Message::DeviceAction(true),
        "Settings" => Message::SettingsMenu,
        "Workspaces…" => Message::Workspace(workspace_view::WorkspaceAction::Show),
        "Command palette…" => Message::Shortcut(Shortcut::CommandPalette),
        "Find pane…" => Message::Shortcut(Shortcut::PanePicker),
        "Keyboard copy mode" => Message::Shortcut(Shortcut::CopyMode),
        "Keyboard hints" => Message::Shortcut(Shortcut::KeyboardHints),
        _ => Message::ControlIdle,
    }
}

fn bind_tab_action(message: Message, group: Option<TabId>) -> Message {
    match message {
        Message::Shortcut(Shortcut::DuplicateTabLayout) => {
            group.map_or(Message::ControlIdle, Message::DuplicateTabLayout)
        }
        message => message,
    }
}

#[derive(Clone, Copy)]
pub enum Field {
    Name,
    Host,
    User,
    Port,
    CustomTerminal,
    Identity,
    Fingerprint,
}

enum UiWork {
    FolderAuthority,
    Control(w11term::control::UiRequest),
    AttentionChanged,
}

struct CaretMenuBinding {
    pane: Option<u64>,
    group: Option<TabId>,
    exited: Option<ExitedPane>,
    on_click: Callback<String>,
}

pub struct TerminalApp {
    folder_wake: Arc<dyn Fn() + Send + Sync>,
    folder_panel_timer: Option<(TabId, crate::ui_timer::RetryTimer)>,
    taskbar: RefCell<taskbar::Taskbar>,
    inbox: w11term::events::Inbox,
    show_inspector: bool,
    inspection: Option<inspector::Inspection>,
    accessible: crate::accessibility::Bridge,
    active_group: Rc<Cell<Option<TabId>>>,
    windows: Rc<RefCell<crate::window_state::Windows>>,
    window_profile: Option<String>,
    window_restored: bool,
    // Unhook before dropping tabs and shutting down their independent sessions.
    keyboard: KeyboardBridge,
    _control: Arc<Server>,
    window_launch: w11term::options::Options,
    opening: Option<Rc<crate::workspace_window::Opening>>,
    incoming: Option<Rc<tab_windows::Transfer>>,
    outgoing: Option<Rc<tab_windows::Transfer>>,
    window_timer: Option<crate::ui_timer::RetryTimer>,
    native_bound: bool,
    window_error: Option<String>,
    export: scrollback_export::Export,
    registry: Registry,
    control_rx: Arc<std::sync::Mutex<std::sync::mpsc::Receiver<UiWork>>>,
    control_task: Option<ComponentTask>,
    tabs: Tabs<AppTab>,
    metrics: Rc<Cell<Metrics>>,
    invalidator: Invalidator,
    header: RefCell<Option<View>>,
    caret_menu_binding: RefCell<Option<CaretMenuBinding>>,
    frame_tasks: BTreeMap<TabId, ComponentTask>,
    watcher_retry: Option<crate::ui_timer::RetryTimer>,
    watch_epoch: u64,
    default_source: Source,
    default_label: String,
    default_profile: Option<String>,
    catalog: Profiles,
    store: ProfileStore,
    profile_error: Option<String>,
    profile_move_error: Option<String>,
    width: f64,
    selection_pending: bool,
    selection_epoch: u64,
    client_size: (f64, f64),
    window_height: f64,
    initial_grid: Rc<crate::window_state::RequestedGrid>,
    appearance: Arc<RwLock<Appearance>>,
    fonts: Vec<String>,
    appearance_error: Option<String>,
    divider_drag: Rc<RefCell<Option<pane_view::DividerDrag>>>,
    measured_layout: Option<Rc<pane_view::PaneGeometry>>,
    mru_groups: Vec<TabId>,
    switcher: Option<TabSwitcher>,
    picker: Option<pane_picker::Picker>,
    title_editor: Option<tab_title::Editor>,
    hints: Rc<RefCell<Option<keyboard_hints::Hints>>>,
    copy_mode: Rc<RefCell<Option<keyboard_copy::ActiveCopy>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExitedPane {
    pane: u64,
    token: u64,
}

pub enum Message {
    FolderAuthority,
    FolderShare(folder_shares::Action),
    FolderRevoke(TabId, u64, folder_revoke::Action),
    RestartExited(ExitedPane),
    Export(scrollback_export::Action),
    TabTitle(tab_title::Action),
    Attention(attention::Action),
    AttentionChanged,
    ToggleInspector,
    PaletteChoose(command_palette::CommandId),
    PickerQuery(String),
    PickerHover(u64),
    PickerFrame(u64, u64),
    PickerChoose(u64),
    PlacePane(TabId, TabId, Direction),
    PickerFocus(u8),
    LocalProfile(local_profile::Action),
    Files(FileAction),
    AccessibleSelection(crate::accessibility::SelectionRequest),
    SplitPane(Axis, Option<String>),
    PaneNeighbor(Direction),
    ZoomPane,
    ClosePane,
    MovePaneToTab,
    MoveTabToWindow,
    DuplicateTabLayout(TabId),
    WindowTick,
    DismissWindowError,
    TransferReady(u64),
    TransferClaim(u64),
    TransferDone(u64),
    TransferCanceled(u64),
    PaneFocus(TabId),
    SetPaneReadOnly(bool),
    PaneRatio(u64, Vec<bool>, f64),
    HoverPane(TabId),
    DividerPress(u64, Vec<bool>, f64),
    DividerEnd(u64, Vec<bool>),
    DeviceAction(bool),
    DeviceDone(TabId, bool, Result<String, String>),
    DraftBell(TabId, String),
    RestoreWindow,
    TogglePlayer(String),
    ControlIdle,
    WatchRejected,
    RetryWatchers,
    Control(w11term::control::UiRequest),
    ControlResizeDone {
        pane: u64,
        cols: usize,
        rows: usize,
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>,
    },
    PromptEdit(TabId, u64, usize, String),
    PromptReply(TabId, u64, ContentDialogResult),
    GenerateKey(TabId),
    KeyGenerated(TabId, Result<w11term::keys::DeviceKey, String>),
    CopyPublicKey(TabId),
    CopyLastCommand,
    History(command_history::Action),
    DraftPlayer(TabId, String, bool),
    Durability(durable_view::Action),
    Discovery(session_browser::Action),
    FrameReady(TabId, u64),
    CloseWindow,
    Select(Option<usize>, u64, bool),
    CloseTab(String),
    Reorder(Vec<String>, Option<TabId>),
    SyncSelection(u64),
    Shortcut(Shortcut),
    ProfileMenu(String),
    MoveProfile(String, isize, Arc<[String]>),
    RefreshProfiles,
    SettingsMenu,
    Keyboard(keyboard_settings::Edit),
    Patterns(pattern_settings::Edit),
    Workspace(workspace_view::WorkspaceAction),
    ImportConfig(config_import::Action),
    Font(Option<usize>),
    Ligatures(bool),
    Interaction(InteractionField, bool),
    TabMode(Option<usize>),
    PickTab(TabId),
    CaptureLost(TabId),
    FinalizeCapture(TabId, u64),
    PointerExit(TabId),
    DragScroll(TabId),
    PasteRaw,
    Paste,
    RawPaste(Result<(), String>),
    CopyResult(TabId, u64, Result<(), String>),
    Pointer(TabId, PointerEventInfo, MouseEventKind),
    RenderError(TabId, String),
    RetryRender(TabId),
    Edit(TabId, Field, String),
    Auth(TabId, Option<usize>),
    TerminalPreset(TabId, Option<usize>),
    SaveProfile(TabId),
    SaveProfileOnly(TabId),
    EditProfile(String),
    EditActiveProfile,
    DuplicateProfile(TabId),
    DeleteProfile(TabId),
    ConfirmDeleteProfile(TabId, bool),
    WindowSize(WindowSize),
    Geometry(f64, f64, u64, bool),
    GridFallback(u64),
    FindEdit(TabId, String),
    FindOption(TabId, FindOption, bool),
    FindApply(TabId, u64),
    FindFocus(TabId, u64, u8),
    FindFocusResult(TabId, u64, bool),
    #[cfg(feature = "ui-smoke")]
    Smoke(u8),
}

fn live_tab(label: String, source: Source, osc52: bool) -> AppTab {
    live_tab_sized(label, source, TerminalSize::default(), osc52, None, None)
}
fn live_tab_sized(
    label: String,
    source: Source,
    size: TerminalSize,
    osc52: bool,
    connection: Option<w11term::ssh::ConnectionShare>,
    saved: Option<w11term::durable::DurableReference>,
) -> AppTab {
    let session = if matches!(&source, Source::Ssh(ssh) if ssh.durable) {
        Session::start_durable(source.clone(), size, osc52, connection, saved)
    } else {
        Session::start_configured_with_connection(source.clone(), size, osc52, connection)
    };
    live_tab_for_session(label, source, session)
}

fn live_tab_for_session(label: String, source: Source, session: Session) -> AppTab {
    AppTab {
        label,
        page: Page::Terminal(LiveTab {
            folder_sharing: None,
            startup: None,
            observed_frame: 0,
            files: Box::default(),
            discovery_page: None,
            copy_output_unavailable: Some("waiting for shell markers"),
            source,
            confirm_end: false,
            durable_phase: None,
            profile_name: None,
            surface: PaneSurface::default(),
            device_busy: false,
            read_only: Rc::default(),
            device_message: None,
            session: Box::new(session),
            last_size: Rc::new(Cell::new(None)),
            error: None,
            selection: Rc::new(RefCell::new(None)),
            hover: Rc::new(RefCell::new(None)),
            remote_mouse: Default::default(),
            clicked_link: None,
            last_click: None,
            scroll_direction: 0,
            scroll_tick: false,
            gesture: 0,
            find: Find::default(),
            prompt_id: None,
            answers: Vec::new(),
            connection_generation: 0,
        }),
    }
}

fn source_label(source: &Source) -> String {
    match source {
        Source::Local(shell) => shell
            .executable
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        Source::Demo => LAB.into(),
        Source::Replay(path) => format!("Replay · {}", path.display()),
        Source::Ssh(ssh) => format!("{}@{}:{}", ssh.user, ssh.host, ssh.port),
    }
}

fn profile_label(name: &str, ssh: &SshOptions) -> String {
    format!("{name}    {}@{}:{}", ssh.user, ssh.host, ssh.port)
}

impl TerminalApp {
    fn watch_control(&mut self, context: &ComponentContext<Self>) {
        let receiver = self.control_rx.clone();
        self.control_task = Some(context.spawn_background_with_rejection(
            move |cancel| {
                loop {
                    if cancel.is_cancelled() {
                        return Message::ControlIdle;
                    }
                    match receiver
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_millis(200))
                    {
                        Ok(UiWork::Control(request)) => return Message::Control(request),
                        Ok(UiWork::AttentionChanged) => return Message::AttentionChanged,
                        Ok(UiWork::FolderAuthority) => return Message::FolderAuthority,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(_) => return Message::ControlIdle,
                    }
                }
            },
            Message::WatchRejected,
        ));
    }
    fn connection_ui(&mut self, id: TabId) {
        let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.get_mut(id)
        else {
            return;
        };
        let frame = tab.session.frames.load();
        tab.files.observe_connection(frame.connection_generation);
        let prompt_id = frame.prompt.as_ref().map(|p| p.id);
        if self.hints.borrow().as_ref().is_some_and(|h| {
            h.pane == id
                && (prompt_id.is_some()
                    || h.frame.connection_generation != frame.connection_generation
                    || h.frame.cols != frame.cols
                    || h.frame.rows.len() != frame.rows.len())
        }) {
            self.hints.borrow_mut().take();
            self.keyboard.set_hints(false);
            tab.surface.invalidator.invalidate();
        }
        if prompt_id.is_some() && self.picker.take().is_some() {
            self.keyboard.set_picker(None);
        }
        let changed = prompt_id != tab.prompt_id;
        self.keyboard
            .set_game_mode(tab.session.player.read().unwrap().game_mode);
        if changed {
            tab.answers = match frame.prompt.as_ref().map(|p| &p.question) {
                Some(w11term::prompts::Question::Authentication { challenges, .. }) => (0
                    ..challenges.len())
                    .map(|_| zeroize::Zeroizing::new(String::new()))
                    .collect(),
                _ => Vec::new(),
            };
            tab.prompt_id = prompt_id;
        }
        if tab.connection_generation != frame.connection_generation {
            self.header.borrow_mut().take();
            tab.connection_generation = frame.connection_generation;
            tab.reset_transport_input(&self.keyboard);
        }
        if prompt_id.is_some() {
            self.title_editor = None;
            tab.files.cancel_name_edit();
        }
        self.keyboard.set_modal(
            prompt_id.is_some() || self.title_editor.is_some() || tab.files.editing_name(),
        );
        let choosing = tab.browsing_sessions();
        if changed {
            self.keyboard
                .set_target(if prompt_id.is_none() && !tab.device_busy && !choosing {
                    Some((
                        tab.session.input.clone(),
                        tab.session.frames.clone(),
                        tab.read_only.clone(),
                    ))
                } else {
                    None
                });
            self.keyboard
                .set_search(prompt_id.is_none() && tab.find.open);
        } else if prompt_id.is_some() || tab.device_busy || choosing {
            self.keyboard.set_target(None);
        }
    }
    fn prompt_view(
        &self,
        id: TabId,
        tab: &LiveTab,
        prompt: &w11term::prompts::Prompt,
        context: &ViewContext<Self>,
    ) -> View {
        use w11term::prompts::Question;
        let prompt_id = prompt.id;
        let (title,primary,children):(String,&str,Vec<View>)=match &prompt.question {
            Question::HostKey{host,port,algorithm,fingerprint}=>("Trust this SSH host?".into(),"Accept and save",vec![
                TextBlock::new().text(format!("First connection to {host}:{port}")).text_wrapping(TextWrapping::Wrap).into(),
                TextBlock::new().text(format!("{algorithm}\n{fingerprint}")).text_wrapping(TextWrapping::Wrap).into(),
                TextBlock::new().text("Verify this fingerprint with the server owner. Accepting saves it in your OpenSSH known_hosts file.").text_wrapping(TextWrapping::Wrap).into(),
            ]),
            Question::Authentication{name,instructions,challenges}=> {
                let mut children:Vec<View>=vec![TextBlock::new().text(instructions).text_wrapping(TextWrapping::Wrap).into()];
                for (index,challenge) in challenges.iter().enumerate(){
                    children.push(TextBlock::new().text(&challenge.label).text_wrapping(TextWrapping::Wrap).into());
                    let value=tab.answers.get(index).map_or("",|v|v.as_str());
                    children.push(if challenge.echo {
                        TextBox::new().text(value).automation_name(&challenge.label).on_text_changed(context.callback(move |value|Message::PromptEdit(id,prompt_id,index,value))).into()
                    }else{
                        PasswordBox::new().password(value).automation_name(&challenge.label).password_reveal_mode(PasswordRevealMode::Hidden).on_password_changed(context.callback(move |value|Message::PromptEdit(id,prompt_id,index,value))).into()
                    });
                }
                (if name.is_empty(){"SSH authentication".into()}else{name.clone()},"Sign in",children)
            }
        };
        ContentDialog::new()
            .is_open(true)
            .title(title)
            .primary_button_text(primary)
            .close_button_text("Cancel")
            .on_closed(context.callback(move |result| Message::PromptReply(id, prompt_id, result)))
            .content(
                StackPanel::new()
                    .max_width(500.0)
                    .spacing(12.0)
                    .keyed_children(
                        children
                            .into_iter()
                            .enumerate()
                            .map(|(i, v)| KeyedView::new(i.to_string(), v)),
                    ),
            )
    }
    fn find(&mut self, close: bool, direction: Option<isize>, context: &ComponentContext<Self>) {
        self.close_copy_mode();
        let Some(id) = self.tabs.active_id() else {
            return;
        };
        let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.get_mut(id)
        else {
            return;
        };
        let configure = !tab.find.open
            || tab.find.pending
            || tab
                .session
                .frames
                .load()
                .search
                .as_ref()
                .is_none_or(|s| !tab.find.matches(s));
        tab.find.open = !close;
        if close {
            tab.find.focused = false;
        }
        tab.find.epoch += 1;
        self.keyboard.set_search(!close);
        if close {
            let _ = tab.session.input.send(Command::SearchClose);
        } else {
            if configure {
                tab.find.apply(&tab.session);
            }
            if let Some(direction) = direction {
                let _ = tab.session.input.send(Command::SearchNavigate(direction));
            }
            let epoch = tab.find.epoch;
            context.spawn_background(move |_| {
                std::thread::sleep(Duration::from_millis(20));
                Message::FindFocus(id, epoch, 0)
            });
        }
        self.invalidator.invalidate();
    }
    fn apply_find(&mut self, id: TabId, epoch: u64) {
        if let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.get_mut(id)
            && tab.find.open
            && tab.find.epoch == epoch
        {
            tab.find.apply(&tab.session);
        }
    }
    fn find_view(&self, id: TabId, tab: &LiveTab, context: &ViewContext<Self>) -> View {
        let frame = tab.session.frames.load();
        let status = tab.find.status(frame.search.as_ref());
        Border::new()
            .grid_row(1)
            .horizontal_alignment(HorizontalAlignment::Right)
            .vertical_alignment(VerticalAlignment::Top)
            .margin(8.0)
            .padding(8.0)
            .corner_radius(6.0)
            .background(Color::rgb(40, 42, 48))
            .width((self.width - 16.0).clamp(300.0, 600.0))
            .content(
                Grid::new()
                    .rows([GridLength::Auto, GridLength::Auto])
                    .row_spacing(4.0)
                    .columns([
                        GridLength::STAR,
                        GridLength::Auto,
                        GridLength::Auto,
                        GridLength::Auto,
                    ])
                    .children((
                        TextBox::new()
                            .element_ref(&tab.find.input)
                            .text(&tab.find.query)
                            .placeholder_text("Find · Enter next · Esc close")
                            .automation_name("Find in scrollback")
                            .on_text_changed(
                                context.callback(move |text| Message::FindEdit(id, text)),
                            ),
                        Button::new()
                            .grid_column(1)
                            .style(ButtonStyle::Subtle)
                            .automation_name("Previous match (Shift+Enter)")
                            .on_click(
                                context.callback(|_| Message::Shortcut(Shortcut::FindPrevious)),
                            )
                            .content(TextBlock::new().text("↑")),
                        Button::new()
                            .grid_column(2)
                            .style(ButtonStyle::Subtle)
                            .automation_name("Next match (Enter)")
                            .on_click(context.callback(|_| Message::Shortcut(Shortcut::FindNext)))
                            .content(TextBlock::new().text("↓")),
                        Button::new()
                            .grid_column(3)
                            .style(ButtonStyle::Subtle)
                            .automation_name("Close find (Escape)")
                            .on_click(context.callback(|_| Message::Shortcut(Shortcut::FindClose)))
                            .content(TextBlock::new().text("×")),
                        Grid::new()
                            .grid_row(1)
                            .grid_column_span(4)
                            .columns([GridLength::Auto, GridLength::Auto, GridLength::STAR])
                            .column_spacing(12.0)
                            .children((
                                CheckBox::new()
                                    .min_width(0.0)
                                    .automation_name("Match case")
                                    .is_checked(tab.find.sensitive)
                                    .on_is_checked_changed(context.callback(move |v| {
                                        Message::FindOption(id, FindOption::Case, v)
                                    }))
                                    .content("Match case"),
                                CheckBox::new()
                                    .grid_column(1)
                                    .min_width(0.0)
                                    .automation_name("Regular expression")
                                    .is_checked(tab.find.regex)
                                    .on_is_checked_changed(context.callback(move |v| {
                                        Message::FindOption(id, FindOption::Regex, v)
                                    }))
                                    .content("Regex"),
                                TextBlock::new()
                                    .grid_column(2)
                                    .vertical_alignment(VerticalAlignment::Center)
                                    .automation_id("find-status")
                                    .text(status)
                                    .text_wrapping(TextWrapping::Wrap),
                            )),
                    )),
            )
    }
    fn active_selection(&self) -> bool {
        matches!(self.tabs.active(),Some(AppTab{page:Page::Terminal(tab),..}) if tab.selection.borrow().as_ref().is_some_and(|s|{let frame=tab.session.frames.load();s.active && s.alternate==frame.alternate && s.cols==frame.cols}))
    }
    fn clear_selection(&mut self) {
        if let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.active_id().and_then(|id| self.tabs.get_mut(id))
        {
            tab.selection.borrow_mut().take();
            tab.scroll_direction = 0;
        }
        self.keyboard.set_selection(false);
        self.invalidator.invalidate();
    }
    fn copy(&self) {
        if let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.active()
            && let Some(selection) = tab.selection.borrow().as_ref()
        {
            let text = selection.text(
                &tab.session.frames.load(),
                self.appearance
                    .read()
                    .unwrap()
                    .interaction
                    .trim_block_selection,
            );
            if !text.is_empty()
                && let Err(e) = w11term::clipboard::write(&text)
            {
                log::warn!("Copy: {e:#}");
            }
        }
    }
    fn paste(&mut self, raw: bool, context: &ComponentContext<Self>) {
        if let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.active()
        {
            if tab.read_only.get() {
                return;
            }
            match w11term::clipboard::read() {
                Ok(text) => {
                    if raw {
                        if !text.is_empty() {
                            let (tx, rx) = tokio::sync::oneshot::channel();
                            if tab
                                .session
                                .input
                                .send(Command::Raw(text.into_bytes(), tx))
                                .is_ok()
                            {
                                context.spawn_background(move |_| {
                                    Message::RawPaste(rx.blocking_recv().unwrap_or_else(|_| {
                                        Err("SSH session closed before acknowledging paste".into())
                                    }))
                                });
                            }
                        }
                    } else {
                        let preferences = self.appearance.read().unwrap();
                        let text = paste_text(&text, preferences.interaction.trim_paste);
                        if !text.is_empty() {
                            let _ = tab.session.input.send(Command::Paste(text.into()));
                        }
                    }
                }
                Err(e) => log::warn!("Paste: {e:#}"),
            }
        }
        self.clear_selection();
    }
    fn switch_tabs(&mut self, delta: isize, context: &ComponentContext<Self>) {
        let mode = self
            .appearance
            .read()
            .unwrap()
            .interaction
            .tab_switcher_mode;
        if mode == TabSwitcherMode::Disabled {
            self.tabs.cycle(delta);
            self.activate(context);
            return;
        }
        if self.switcher.is_none() {
            let mut ids: Vec<_> = self.tabs.headers().iter().map(|tab| tab.id).collect();
            if mode == TabSwitcherMode::Mru {
                // A tab's focused pane can move to another tab. The group keeps its history.
                ids.sort_by_cached_key(|&id| {
                    self.tabs
                        .group_id(id)
                        .and_then(|group| self.mru_groups.iter().position(|&g| g == group))
                        .unwrap_or(usize::MAX)
                });
            }
            let index = ids
                .iter()
                .position(|id| Some(*id) == self.tabs.active_id())
                .unwrap_or(0);
            self.switcher = Some(TabSwitcher { ids, index });
        }
        if let Some(switcher) = &mut self.switcher
            && !switcher.ids.is_empty()
        {
            switcher.index =
                (switcher.index as isize + delta).rem_euclid(switcher.ids.len() as isize) as usize;
        }
        self.keyboard.set_switcher(true);
    }
    fn finish_switcher(&mut self, commit: bool, context: &ComponentContext<Self>) {
        if let Some(switcher) = self.switcher.take()
            && commit
            && let Some(id) = switcher.ids.get(switcher.index)
        {
            self.tabs.select(*id);
            self.activate(context);
        }
        self.keyboard.set_switcher(false);
    }
    fn pointer(
        &mut self,
        id: TabId,
        pointer: PointerEventInfo,
        kind: MouseEventKind,
        context: &ComponentContext<Self>,
    ) {
        if !self.tabs.visible(id) {
            return;
        }
        if self
            .copy_mode
            .borrow()
            .as_ref()
            .is_some_and(|m| m.pane == id)
            || self.hints.borrow().as_ref().is_some_and(|h| h.pane == id)
        {
            return;
        }
        if kind == MouseEventKind::Press && self.tabs.active_id() != Some(id) {
            self.focus_pane(id, context);
        }
        self.keyboard.hover_pane(id);
        let preferences = self.appearance.read().unwrap().interaction.clone();
        let modifiers = crate::input::modifiers();
        let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.get_mut(id)
        else {
            return;
        };
        let metrics = tab.surface.metrics.get();
        let frame = tab.session.frames.load();
        let hit = metrics.hit(pointer.x, pointer.y);
        let (x, y) = (hit.col, hit.row);
        let point = hit.point(&frame);
        let link = link_at(&frame, point, preferences.detect_urls);
        if let Some(inspector) = &mut self.inspection {
            if inspector.pane == tab.session.pane_id {
                inspector.observe_pointer((pointer.x, pointer.y), &hit, &frame, link.as_ref());
            } else {
                inspector.clear_pointer();
            }
        }
        if *tab.hover.borrow() != link {
            *tab.hover.borrow_mut() = link.clone();
            tab.surface.invalidator.invalidate();
        }
        let button = if pointer.is_left_button_pressed {
            MouseButton::Left
        } else if pointer.is_right_button_pressed {
            MouseButton::Right
        } else if pointer.is_middle_button_pressed {
            MouseButton::Middle
        } else {
            MouseButton::None
        };
        if kind == MouseEventKind::Press {
            tab.gesture += 1;
        }
        let local_drag = tab.selection.borrow().as_ref().is_some_and(|s| s.dragging)
            || tab.clicked_link.is_some();
        if frame.mouse_grabbed
            && !tab.read_only.get()
            && !local_drag
            && !modifiers.contains(KeyModifiers::SHIFT)
            && !(modifiers.contains(KeyModifiers::CTRL) && link.is_some())
        {
            if let Some(event) = tab.remote_mouse.event(MouseEvent {
                kind,
                x,
                y: y as i64,
                x_pixel_offset: crate::input::cell_pixel_offset(
                    pointer.x as f32 - PADDING,
                    metrics.cell_width,
                    metrics.scale,
                ),
                y_pixel_offset: crate::input::cell_pixel_offset(
                    pointer.y as f32 - PADDING,
                    metrics.cell_height,
                    metrics.scale,
                ),
                button,
                modifiers,
            }) {
                let _ = tab.session.input.send(Command::Mouse(event));
            }
            return;
        }
        let mut should_copy = false;
        let mut should_paste = false;
        let mut open = None;
        match kind {
            MouseEventKind::Press if button == MouseButton::Middle => {}
            MouseEventKind::Press if button == MouseButton::Right => {
                if !preferences.copy_on_select
                    && tab.selection.borrow().as_ref().is_some_and(|s| s.active)
                {
                    should_copy = true;
                } else {
                    should_paste = true;
                }
            }
            MouseEventKind::Press if button == MouseButton::Left => {
                if modifiers.contains(KeyModifiers::CTRL)
                    && let Some(link) = link
                {
                    tab.clicked_link = Some(link.uri);
                } else {
                    let clicks = tab
                        .last_click
                        .filter(|(time, p, _)| {
                            time.elapsed() < Duration::from_millis(500) && *p == point
                        })
                        .map_or(1, |(_, _, count)| count % 3 + 1);
                    tab.last_click = Some((Instant::now(), point, clicks));
                    let mut selection = if modifiers.contains(KeyModifiers::SHIFT) {
                        tab.selection.borrow().clone().unwrap_or_else(|| {
                            Selection::new(
                                point,
                                modifiers.contains(KeyModifiers::ALT),
                                frame.alternate,
                            )
                        })
                    } else {
                        Selection::new(
                            point,
                            modifiers.contains(KeyModifiers::ALT),
                            frame.alternate,
                        )
                    };
                    selection.dragging = true;
                    selection.update(point);
                    if clicks == 2 {
                        let range = word_range(&frame, point, &preferences.word_delimiters);
                        selection.anchor.col = range.start;
                        selection.end.col = range.end.saturating_sub(1);
                        selection.active = true;
                    } else if clicks == 3 {
                        selection.anchor.col = 0;
                        selection.end.col = frame.cols.saturating_sub(1);
                        selection.active = true;
                    }
                    selection.observe(&frame);
                    *tab.selection.borrow_mut() = Some(selection);
                    tab.surface.invalidator.invalidate();
                }
            }
            MouseEventKind::Move if pointer.is_left_button_pressed => {
                if let Some(selection) = tab.selection.borrow_mut().as_mut().filter(|s| s.dragging)
                {
                    selection.update(point);
                    selection.observe(&frame);
                }
                tab.scroll_direction = if pointer.y < PADDING as f64 {
                    2
                } else if pointer.y > (PADDING + metrics.rows as f32 * metrics.cell_height) as f64 {
                    -2
                } else {
                    0
                };
                if tab.scroll_direction != 0 && !tab.scroll_tick {
                    tab.scroll_tick = true;
                    context.spawn_background(move |_| {
                        std::thread::sleep(Duration::from_millis(40));
                        Message::DragScroll(id)
                    });
                }
                tab.surface.invalidator.invalidate();
            }
            MouseEventKind::Release => {
                tab.scroll_direction = 0;
                if let Some(uri) = tab.clicked_link.take()
                    && link.as_ref().is_some_and(|l| l.uri == uri)
                {
                    open = Some(uri);
                }
                if let Some(selection) = tab.selection.borrow_mut().as_mut().filter(|s| s.dragging)
                {
                    selection.dragging = false;
                    selection.observe(&frame);
                    should_copy = preferences.copy_on_select && selection.active;
                }
                tab.surface.invalidator.invalidate();
            }
            _ => {}
        }
        let selected = tab.selection.borrow().as_ref().is_some_and(|s| s.active);
        if self.tabs.active_id() == Some(id) {
            self.keyboard.set_selection(selected);
        }
        if should_copy {
            self.copy();
            if kind == MouseEventKind::Press
                && button == MouseButton::Right
                && !preferences.copy_on_select
            {
                self.clear_selection();
            }
        }
        if should_paste {
            self.paste(false, context);
        }
        if let Some(uri) = open {
            Self::open_link(&uri);
        }
    }

    fn open_link(uri: &str) {
        if safe_url(uri) {
            use windows::{
                Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
                core::PCWSTR,
            };
            let uri: Vec<_> = uri.encode_utf16().chain(Some(0)).collect();
            let result = unsafe {
                ShellExecuteW(
                    None,
                    windows::core::w!("open"),
                    PCWSTR(uri.as_ptr()),
                    None,
                    None,
                    SW_SHOWNORMAL,
                )
            };
            if result.0 as isize <= 32 {
                log::warn!("Could not open URL");
            }
        }
    }
    fn save_preferences(&mut self, appearance: Appearance) {
        self.keyboard
            .set_preferences(appearance.interaction.clone());
        for entry in self.tabs.entries() {
            if let Page::Terminal(tab) = &entry.value.page {
                tab.session.clipboard_enabled.store(
                    appearance.interaction.osc52,
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
        }
        *self.appearance.write().unwrap() = appearance.clone();
        self.appearance_error = Appearance::path()
            .and_then(|path| appearance.save(&path))
            .err()
            .map(|e| format!("Settings could not be saved: {e:#}"));
    }
    fn apply_appearance(&mut self, appearance: Appearance) {
        let mut renderer = match Renderer::with_size(
            &appearance.font_family,
            appearance.ligatures,
            appearance.font_size,
        ) {
            Ok(renderer) => renderer,
            Err(e) => {
                self.appearance_error = Some(format!("Unable to use font: {e}"));
                return;
            }
        };
        for entry in self.tabs.entries() {
            if let Page::Terminal(tab) = &entry.value.page {
                tab.selection.borrow_mut().take();
                tab.hover.borrow_mut().take();
                if let Some(size) = tab.surface.remeasure(&mut renderer) {
                    tab.last_size.set(Some(size));
                    let _ = tab.session.input.send(Command::Resize(size));
                }
                tab.surface.release();
                tab.surface.invalidator.invalidate();
            }
        }
        self.keyboard.set_selection(false);
        self.save_preferences(appearance);
        self.invalidator.invalidate();
    }

    fn settings_view(&self, context: &ViewContext<Self>) -> View {
        let appearance = self.appearance.read().unwrap().clone();
        let mut children:Vec<View>=vec![
            TextBlock::new().text("Settings").font_size(26.0).into(),
            TextBlock::new().text("Terminal font").font_size(18.0).into(),
            ComboBox::new().width(360.0).automation_name("Terminal font")
                .items_source(self.fonts.iter().cloned())
                .selected_index(self.fonts.iter().position(|font|font==&appearance.font_family))
                .on_selection_changed(context.callback(Message::Font)).into(),
            TextBlock::new().text("Installed Cascadia Code, Cascadia Mono, Consolas, JetBrains Mono, Monaspace and Fira Code fonts.").text_wrapping(TextWrapping::Wrap).into(),
            TextBlock::new().text("Programming ligatures").into(),
            ToggleSwitch::new().automation_name("Programming ligatures").is_on(appearance.ligatures).on_toggled(context.callback(Message::Ligatures)).into(),
            TextBlock::new().text("Used by fonts with ligature support, such as Cascadia Code, JetBrains Mono and Fira Code.").text_wrapping(TextWrapping::Wrap).into(),
            TextBlock::new().text("Applies to all tabs. Changes are saved automatically.").into(),
        ];
        let mut profiles: Vec<View> =
            vec![TextBlock::new().text("Profiles").font_size(20.0).into()];
        let order: Arc<[String]> = self.catalog.profile_order.as_slice().into();
        let mut rows = Vec::with_capacity(order.len());
        for (index, name) in order.iter().enumerate() {
            let edit_name = name.clone();
            let (label, edit) = if let Some(ssh) = self.catalog.profiles.get(name) {
                (
                    profile_label(name, ssh),
                    Button::new()
                        .automation_name(format!("Edit profile {name}"))
                        .on_click(
                            context.callback(move |_| Message::EditProfile(edit_name.clone())),
                        ),
                )
            } else {
                (
                    format!("{name} · Local"),
                    Button::new()
                        .automation_name(format!("Edit local profile {name}"))
                        .on_click(context.callback(move |_| {
                            Message::LocalProfile(local_profile::Action::Open(Some(
                                edit_name.clone(),
                            )))
                        })),
                )
            };
            let movement = |direction, delta, enabled| {
                let order = order.clone();
                let name = name.clone();
                Button::new()
                    .automation_name(format!("Move {name} {direction}"))
                    .is_enabled(enabled)
                    .on_click(context.callback(move |_| {
                        Message::MoveProfile(name.clone(), delta, order.clone())
                    }))
                    .content(format!("Move {direction}"))
            };
            rows.push(KeyedView::new(
                name.clone(),
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        edit.content("Edit…"),
                        movement("up", -1, index > 0),
                        movement("down", 1, index + 1 < order.len()),
                        TextBlock::new()
                            .text(label)
                            .vertical_alignment(VerticalAlignment::Center),
                    )),
            ));
        }
        if rows.is_empty() {
            profiles.push(TextBlock::new().text("No saved profiles yet.").into());
        } else {
            profiles.push(StackPanel::new().spacing(8.0).keyed_children(rows));
        }
        profiles.push(
            Button::new()
                .on_click(
                    context.callback(|_| Message::LocalProfile(local_profile::Action::Open(None))),
                )
                .content("New local shell profile…"),
        );
        for error in self.profile_error.iter().chain(&self.profile_move_error) {
            profiles.push(
                TextBlock::new()
                    .text(error)
                    .text_wrapping(TextWrapping::Wrap)
                    .foreground(Color::rgb(255, 150, 130))
                    .into(),
            );
        }
        profiles.push(
            Button::new()
                .on_click(context.callback(|_| Message::ProfileMenu(NEW_PROFILE.into())))
                .content("New Profile…"),
        );
        profiles.push(
            Button::new()
                .on_click(context.callback(|_| Message::ImportConfig(config_import::Action::Show)))
                .content("Import OpenSSH config…"),
        );
        children.splice(1..1, profiles);
        children.insert(
            1,
            Button::new()
                .on_click(context.callback(|_| Message::Patterns(pattern_settings::Edit::Show)))
                .content("Hint patterns"),
        );
        children.insert(
            1,
            Button::new()
                .automation_name("Keyboard")
                .on_click(context.callback(|_| Message::Keyboard(keyboard_settings::Edit::Show)))
                .content("Keyboard"),
        );
        children.push(
            TextBlock::new()
                .text(format!(
                    "Font size: {}  ·  Ctrl++ / Ctrl+- / Ctrl+0",
                    appearance.font_size
                ))
                .into(),
        );
        children.push(TextBlock::new().text("Interaction").font_size(20.0).into());
        for (field, label, value) in [
            (
                InteractionField::CopyOnSelect,
                "Automatically copy selection",
                appearance.interaction.copy_on_select,
            ),
            (
                InteractionField::TrimBlock,
                "Remove trailing whitespace in block selections",
                appearance.interaction.trim_block_selection,
            ),
            (
                InteractionField::TrimPaste,
                "Trim trailing whitespace on paste",
                appearance.interaction.trim_paste,
            ),
            (
                InteractionField::Snap,
                "Snap window resizing to the character grid",
                appearance.interaction.snap_to_grid_on_resize,
            ),
            (
                InteractionField::ScrollZoom,
                "Ctrl+scroll to zoom",
                appearance.interaction.scroll_to_zoom,
            ),
            (
                InteractionField::DetectUrls,
                "Detect URLs (Ctrl+click to open)",
                appearance.interaction.detect_urls,
            ),
            (
                InteractionField::Osc52,
                "Allow remote clipboard copy (OSC 52)",
                appearance.interaction.osc52,
            ),
        ] {
            children.push(
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new().text(label),
                    ToggleSwitch::new()
                        .automation_name(label)
                        .is_on(value)
                        .on_toggled(
                            context.callback(move |value| Message::Interaction(field, value)),
                        ),
                )),
            );
        }
        children.push(TextBlock::new().text("Clipboard copies contain plain text only. Alt+drag selects a block. Hold Shift to select or paste over a mouse-reporting TUI.").text_wrapping(TextWrapping::Wrap).into());
        children.push(TextBlock::new().text("Tab switcher").into());
        children.push(
            ComboBox::new()
                .width(360.0)
                .items_source(["In tab order", "Most recently used", "Disabled"])
                .selected_index(Some(match appearance.interaction.tab_switcher_mode {
                    TabSwitcherMode::InOrder => 0,
                    TabSwitcherMode::Mru => 1,
                    TabSwitcherMode::Disabled => 2,
                }))
                .on_selection_changed(context.callback(Message::TabMode))
                .into(),
        );
        if let Some(error) = &self.appearance_error {
            children.push(
                TextBlock::new()
                    .text(error.clone())
                    .foreground(Color::rgb(255, 150, 130))
                    .text_wrapping(TextWrapping::Wrap)
                    .into(),
            );
        }
        ScrollViewer::new().content(
            StackPanel::new()
                .spacing(16.0)
                .margin(32.0)
                .max_width(720.0)
                .horizontal_alignment(HorizontalAlignment::Left)
                .keyed_children(
                    children
                        .into_iter()
                        .enumerate()
                        .map(|(index, view)| KeyedView::new(index.to_string(), view)),
                ),
        )
    }
    fn publish_panes(&self) {
        self.registry
            .replace(self.tabs.entries().iter().filter_map(|entry| {
                let Page::Terminal(tab) = &entry.value.page else {
                    return None;
                };
                tab.session.events.connect_inbox(
                    tab.session.pane_id,
                    &entry.value.label,
                    &self.inbox,
                );
                Some(Pane {
                    id: tab.session.pane_id,
                    tab_id: self.tabs.group_id(entry.id)?,
                    tab_title: self
                        .tabs
                        .title_for(entry.id)
                        .map(|title| title.as_str().to_owned()),
                    visible: self.tabs.visible(entry.id),
                    read_only: tab.read_only.get(),
                    label: entry.value.label.clone(),
                    active: self.tabs.active_id() == Some(entry.id),
                    input: tab.session.input.clone(),
                })
            }));
    }
    fn tab_for_pane(&self, pane: u64) -> Option<TabId> {
        self.tabs
            .entries()
            .iter()
            .find(|tab| tab.value.pane_id() == Some(pane))
            .map(|tab| tab.id)
    }
    fn sync_selection(&mut self, context: &ComponentContext<Self>) {
        // Native collection resets select the first item after Reactor's property pass.
        // Reapply our selected index after mounting, and ignore intermediate reset events.
        self.selection_pending = true;
        self.selection_epoch += 1;
        let epoch = self.selection_epoch;
        context.spawn_background(move |_| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            Message::SyncSelection(epoch)
        });
    }
    fn cancel_frame_tasks(&mut self) {
        self.watch_epoch += 1;
        for (_, task) in std::mem::take(&mut self.frame_tasks) {
            task.cancel();
        }
    }

    fn watch_frames(&mut self, context: &ComponentContext<Self>) {
        for entry in self.tabs.entries() {
            let id = entry.id;
            if !self.tabs.visible(id) || self.frame_tasks.contains_key(&id) {
                continue;
            }
            let Page::Terminal(tab) = &entry.value.page else {
                continue;
            };
            let frames = tab.session.frames.clone();
            let generation = tab.observed_frame;
            let inspected = self
                .inspection
                .as_ref()
                .filter(|inspector| inspector.pane == tab.session.pane_id)
                .map(|inspector| inspector.modes);
            let epoch = self.watch_epoch;
            self.frame_tasks.insert(
                id,
                context.spawn_background_with_rejection(
                    move |cancel| {
                        frames.wait_for_view(generation, inspected, || cancel.is_cancelled());
                        Message::FrameReady(id, epoch)
                    },
                    Message::WatchRejected,
                ),
            );
        }
    }

    fn activate(&mut self, context: &ComponentContext<Self>) {
        self.bind_folder_shares();
        self.activate_folder_panel(context);
        self.title_editor = None;
        self.cancel_inactive_name_editor();
        self.close_copy_mode();
        self.close_hints();
        self.sync_inspector();
        if self.picker.take().is_some() {
            self.keyboard.set_picker(None);
        }
        let single = matches!(
            self.tabs.active_layout(),
            Some(w11term::pane_layout::Layout::Leaf(_))
        );
        let profile = self
            .tabs
            .active()
            .and_then(AppTab::profile_name)
            .filter(|name| {
                single
                    && (self.catalog.profiles.contains_key(*name)
                        || self.catalog.local.contains_key(*name))
            })
            .map(str::to_owned);
        let group = self.tabs.active_id().and_then(|id| self.tabs.group_id(id));
        let previous_group = self.active_group.replace(group);
        let group_changed = previous_group != group;
        if group_changed
            && let Some(previous) = previous_group
            && let Some(AppTab {
                page: Page::NewProfile(draft),
                ..
            }) = self.tabs.get_mut(previous)
        {
            draft.folder_revoke.cancel_confirmation();
        }
        if profile != self.window_profile {
            if let Some(previous) = self.window_profile.take() {
                let _ = self.windows.borrow_mut().remember(&previous);
            }
            self.window_profile = profile;
            if group_changed {
                self.restore_window();
            }
        }
        let windows = self.windows.clone();
        let profile = self.window_profile.clone();
        self.keyboard.on_close(move || {
            if let Some(name) = &profile {
                let _ = windows.borrow_mut().remember(name);
            }
        });
        if let Some(group) = group {
            self.mru_groups.retain(|g| *g != group);
            self.mru_groups.insert(0, group);
        }
        self.cancel_frame_tasks();
        self.divider_drag.borrow_mut().take();
        if !single
            || !matches!(
                self.tabs.active(),
                Some(AppTab {
                    page: Page::Terminal(_),
                    ..
                })
            )
        {
            crate::input::clear_grid();
            self.initial_grid.set(None);
        }
        for entry in self.tabs.entries() {
            if let Page::Terminal(tab) = &entry.value.page {
                if self.tabs.active_id() == Some(entry.id) {
                    self.metrics = tab.surface.metrics.clone();
                    self.invalidator = tab.surface.invalidator.clone();
                }
                if self.tabs.visible(entry.id) {
                    tab.surface.invalidator.invalidate();
                } else {
                    tab.surface.window_hidden();
                }
            }
        }
        let target = match self.tabs.active() {
            Some(AppTab {
                page: Page::Terminal(tab),
                ..
            }) => Some((
                tab.session.input.clone(),
                tab.session.frames.clone(),
                tab.read_only.clone(),
            )),
            _ => None,
        };
        if target.is_none() {
            self.keyboard.set_modal(false);
        }
        self.keyboard.set_target(target);
        self.sync_ime_surface();
        self.keyboard
            .set_wheel_panes(self.tabs.entries().iter().filter_map(|entry| {
                let Page::Terminal(tab) = &entry.value.page else {
                    return None;
                };
                (self.tabs.visible(entry.id) && !tab.browsing_sessions()).then(|| {
                    (
                        entry.id,
                        tab.session.input.clone(),
                        tab.session.frames.clone(),
                    )
                })
            }));
        if let Some(id) = self.tabs.active_id() {
            self.connection_ui(id);
            if let Some(AppTab {
                page: Page::Terminal(tab),
                ..
            }) = self.tabs.get_mut(id)
            {
                tab.follow_shell_directory(&tab.session.frames.load(), context);
            }
        }
        self.keyboard.set_search(
            matches!(self.tabs.active(),Some(AppTab{page:Page::Terminal(tab),..}) if tab.find.open && tab.prompt_id.is_none()),
        );
        if let Some(id) = self.tabs.active_id()
            && let Some(AppTab {
                page: Page::Terminal(tab),
                ..
            }) = self.tabs.get_mut(id)
            && tab.find.open
        {
            if tab.find.pending {
                tab.find.apply(&tab.session);
            }
            tab.find.focused = false;
            let epoch = tab.find.epoch;
            context.spawn_background(move |_| {
                std::thread::sleep(Duration::from_millis(20));
                Message::FindFocus(id, epoch, 0)
            });
        }
        self.keyboard
            .set_preferences(self.appearance.read().unwrap().interaction.clone());
        self.keyboard.set_selection(self.active_selection());
        self.invalidator.invalidate();
        self.watch_frames(context);
        self.publish_panes();
    }

    fn restore_window(&self) {
        if self.incoming.is_some() || !self.native_bound || self.initial_grid.get().is_some() {
            return;
        }
        if let Some(name) = &self.window_profile {
            let windows = self.windows.borrow();
            if windows.fullscreen() {
                return;
            }
            if windows.restore(name).is_ok()
                && let Some(placement) = windows.profiles.get(name)
                && let Some((cols, rows)) = placement.cols.zip(placement.rows)
                && (1..=1000).contains(&cols)
                && (1..=500).contains(&rows)
            {
                self.initial_grid.set(Some((cols, rows)));
            }
        }
    }

    fn close_tab(&mut self, id: TabId, context: &ComponentContext<Self>) {
        let Some(group) = self.tabs.group_id(id) else {
            return;
        };
        let active = self.active_group.get() == Some(group);
        if active {
            self.keyboard.set_target(None);
            self.cancel_frame_tasks();
        }
        self.tabs.remove_group(id);
        self.mru_groups.retain(|g| *g != group);
        self.publish_panes();
        if self.tabs.entries().is_empty() {
            let _ = context.window().request_close();
        } else {
            self.sync_selection(context);
            if active {
                self.activate(context);
            }
        }
    }

    fn open(
        &mut self,
        label: String,
        source: Source,
        profile: Option<String>,
        context: &ComponentContext<Self>,
    ) {
        self.tabs.insert(
            live_tab(
                label,
                source,
                self.appearance.read().unwrap().interaction.osc52,
            )
            .with_profile(profile),
        );
        self.sync_selection(context);
        self.activate(context);
    }

    fn focus_pane(&mut self, id: TabId, context: &ComponentContext<Self>) {
        if self.tabs.visible(id) && self.tabs.active_id() != Some(id) && self.tabs.select(id) {
            self.activate(context);
        }
    }

    fn split_pane(
        &mut self,
        axis: Axis,
        profile: Option<String>,
        context: &ComponentContext<Self>,
    ) {
        let Some(id) = self.tabs.active_id() else {
            return;
        };
        let Some(AppTab {
            label,
            page: Page::Terminal(tab),
        }) = self.tabs.get(id)
        else {
            return;
        };
        let connection = if profile.is_none() {
            tab.session.ssh_connection()
        } else {
            None
        };
        let explicit_profile = profile.is_some();
        let profile_name = profile.or_else(|| tab.profile_name.clone());
        let (label, source) = match &profile_name {
            Some(name) => {
                let source = match self.catalog.source(name) {
                    Some(source) => source,
                    None if !explicit_profile => tab.source.clone(),
                    None => return,
                };
                (name.clone(), source)
            }
            None => (label.clone(), tab.source.clone()),
        };
        let leaf = live_tab_sized(
            label,
            source,
            TerminalSize::default(),
            self.appearance.read().unwrap().interaction.osc52,
            connection,
            None,
        )
        .with_profile(profile_name);
        if self.tabs.split(id, leaf, axis).is_some() {
            self.initial_grid.set(None);
            self.sync_selection(context);
            self.activate(context);
        }
    }

    fn resize_split(&mut self, id: TabId, cols: usize, rows: usize) -> anyhow::Result<()> {
        use w11term::pane_layout::Layout;
        let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.get(id)
        else {
            anyhow::bail!("pane is closed");
        };
        let layout = self.tabs.active_layout().context("no visible layout")?;
        let (width, height, _) = tab
            .surface
            .area
            .get()
            .context("pane has not been drawn yet")?;
        let metrics = tab.surface.metrics.get();
        let mut edits = Vec::new();
        for (axis, current, requested, extent, cell, padding) in [
            (
                Axis::Columns,
                metrics.cols,
                cols,
                width,
                metrics.cell_width,
                2.0 * PADDING,
            ),
            (
                Axis::Rows,
                metrics.rows,
                rows,
                height,
                metrics.cell_height,
                2.0 * PADDING + STATUS_HEIGHT,
            ),
        ] {
            if current == requested {
                continue;
            }
            let (path, node, second_side) = layout.divider_for(id, axis).context(
                "requested grid cannot fit on this axis within the visible split layout",
            )?;
            let Layout::Split { first, second, .. } = node else {
                unreachable!()
            };
            let first_size = self
                .layout_size(first)
                .context("split is still being measured")?;
            let second_size = self
                .layout_size(second)
                .context("split is still being measured")?;
            let (a, b) = match axis {
                Axis::Columns => (first_size.0, second_size.0),
                Axis::Rows => (first_size.1, second_size.1),
            };
            // Aim inside the requested cell to absorb WinUI's physical-pixel rounding.
            let delta = f64::from(padding + (requested as f32 + 0.25) * cell - extent);
            let first_extent = if second_side { a - delta } else { a + delta };
            let available = a + b;
            anyhow::ensure!(
                first_extent >= f64::from(padding + cell)
                    && available - first_extent >= f64::from(padding + cell),
                "requested grid does not fit while keeping both panes visible"
            );
            edits.push((path, first_extent / available));
        }
        for (path, ratio) in edits {
            self.tabs.set_ratio(&path, ratio);
        }
        Ok(())
    }

    fn refresh_profiles(&mut self) {
        match self.store.catalog() {
            Ok(catalog) => {
                if self.catalog != catalog || self.profile_error.is_some() {
                    self.catalog = catalog;
                    self.profile_error = None;
                    self.header.borrow_mut().take();
                    self.bind_folder_shares();
                }
            }
            Err(error) => {
                let error = format!("{error:#}");
                if self.profile_error.as_ref() != Some(&error) {
                    self.profile_error = Some(error);
                    self.header.borrow_mut().take();
                }
            }
        }
    }

    fn utility(&mut self, settings: bool, context: &ComponentContext<Self>) {
        let existing = self
            .tabs
            .entries()
            .iter()
            .find(|t| {
                if settings {
                    matches!(t.value.page, Page::Settings(_))
                } else {
                    matches!(&t.value.page, Page::NewProfile(draft) if matches!(draft.origin, DraftOrigin::New))
                }
            })
            .map(|t| t.id);
        if let Some(id) = existing {
            self.tabs.select(id);
        } else {
            self.tabs.insert(AppTab {
                label: if settings {
                    "Settings".into()
                } else {
                    "New Profile".into()
                },
                page: if settings {
                    Page::Settings(None)
                } else {
                    Page::NewProfile(ProfileDraft::default())
                },
            });
            self.sync_selection(context);
        }
        self.activate(context);
    }

    fn edit_profile(&mut self, name: String, context: &ComponentContext<Self>) {
        if let Some(id) = self
            .tabs
            .entries()
            .iter()
            .find(|tab| {
                matches!(&tab.value.page,
            Page::NewProfile(draft) if draft.origin.editing().is_some_and(|base| base.name==name))
            })
            .map(|tab| tab.id)
        {
            self.tabs.select(id);
            self.activate(context);
            return;
        }
        match self.store.inspect_catalog(&name) {
            Ok((catalog, base)) => {
                self.catalog = catalog;
                self.profile_error = None;
                self.tabs.insert(AppTab {
                    label: format!("Edit {name}"),
                    page: Page::NewProfile(ProfileDraft {
                        origin: DraftOrigin::Edit(base.clone()),
                        ..ProfileDraft::from_options(name, &base.options)
                    }),
                });
                self.sync_selection(context);
                self.activate(context);
            }
            Err(error) => {
                self.profile_error = Some(format!("{error:#}"));
                self.utility(true, context);
            }
        }
    }
    fn duplicate_profile(&mut self, id: TabId, context: &ComponentContext<Self>) {
        let Some(AppTab {
            page: Page::NewProfile(draft),
            ..
        }) = self.tabs.get(id)
        else {
            return;
        };
        let Some(base) = draft.origin.editing() else {
            return;
        };
        let mut name = format!("{} copy", base.name);
        let mut index = 2;
        while self.catalog.profiles.contains_key(&name) || self.catalog.local.contains_key(&name) {
            name = format!("{} copy {index}", base.name);
            index += 1;
        }
        let copy = ProfileDraft {
            origin: DraftOrigin::Copy(base.clone()),
            ..ProfileDraft::from_options(name, &base.options)
        };
        self.tabs.insert(AppTab {
            label: format!("Duplicate {}", base.name),
            page: Page::NewProfile(copy),
        });
        self.sync_selection(context);
        self.activate(context);
    }

    fn confirm_delete_profile(
        &mut self,
        id: TabId,
        accept: bool,
        context: &ComponentContext<Self>,
    ) {
        let Some(AppTab {
            page: Page::NewProfile(draft),
            ..
        }) = self.tabs.get_mut(id)
        else {
            return;
        };
        if !std::mem::take(&mut draft.confirm_delete) || !accept {
            return;
        }
        let Some(base) = draft.origin.editing() else {
            return;
        };
        let catalog = match self.store.delete(base).and_then(|_| self.store.catalog()) {
            Ok(catalog) => catalog,
            Err(error) => {
                draft.error = Some(format!("{error:#}"));
                return;
            }
        };
        let name = base.name.clone();
        self.catalog = catalog;
        let linked: Vec<_> = self
            .tabs
            .entries()
            .iter()
            .filter(|tab| tab.value.ssh_profile_name() == Some(&name))
            .map(|tab| tab.id)
            .collect();
        for linked_id in linked {
            if let Some(AppTab {
                page: Page::Terminal(live),
                ..
            }) = self.tabs.get_mut(linked_id)
            {
                live.profile_name = None;
                if let Source::Ssh(options) = &mut live.source {
                    options.player = live.session.player.read().unwrap().clone();
                }
            }
        }
        if self.default_profile.as_ref() == Some(&name)
            && matches!(self.default_source, Source::Ssh(_))
        {
            self.default_profile = None;
            self.default_source = Source::Demo;
            self.default_label = LAB.into();
        }
        self.utility(true, context);
        self.close_tab(id, context);
    }

    fn save_profile(&mut self, id: TabId, context: &ComponentContext<Self>) {
        self.save_profile_action(id, true, context);
    }
    fn save_profile_action(&mut self, id: TabId, connect: bool, context: &ComponentContext<Self>) {
        let Some(AppTab {
            page: Page::NewProfile(draft),
            ..
        }) = self.tabs.get(id)
        else {
            return;
        };
        let draft = draft.clone();
        let result = (|| -> anyhow::Result<ProfileSnapshot> {
            let name = draft.name.trim();
            anyhow::ensure!(!name.is_empty(), "Enter a profile name");
            let host = draft.host.trim().trim_matches(['[', ']']);
            let user = draft.user.trim();
            anyhow::ensure!(
                !host.is_empty() && !user.is_empty(),
                "Enter a host and user"
            );
            anyhow::ensure!(
                !host.chars().any(char::is_whitespace) && !user.chars().any(char::is_whitespace),
                "Host and user cannot contain spaces"
            );
            let authentication = match draft.authentication {
                1 => {
                    anyhow::ensure!(
                        !draft.identity.trim().is_empty(),
                        "Enter the private key file path"
                    );
                    Authentication::Identity {
                        identity: draft.identity.trim().into(),
                    }
                }
                2 => Authentication::None,
                3 => Authentication::Password,
                4 => Authentication::KeyboardInteractive,
                _ => Authentication::Agent,
            };
            let options = SshOptions {
                host: host.into(),
                terminal_type: draft.terminal_type()?,
                user: user.into(),
                port: draft
                    .port
                    .trim()
                    .parse()
                    .map_err(|_| anyhow::anyhow!("Port must be between 1 and 65535"))?,
                authentication,
                host_key: (!draft.fingerprint.trim().is_empty())
                    .then(|| draft.fingerprint.trim().into()),
                player: draft.player.clone(),
                proxy_jump: draft.proxy_jump.clone(),
                durable: draft.durable,
            };
            match &draft.origin {
                DraftOrigin::New => self.store.add(name, &options)?,
                DraftOrigin::Edit(base) => {
                    self.store.edit(&base.name, name, &options, &base.options)?
                }
                DraftOrigin::Copy(base) => self.store.duplicate(base, name, &options)?,
            }
            let (catalog, saved) = self.store.inspect_catalog(name)?;
            self.catalog = catalog;
            self.profile_error = None;
            Ok(saved)
        })();
        match result {
            Ok(saved) => {
                let options = &saved.options;
                let name = draft.name.trim().to_owned();
                if let Some(base) = draft.origin.editing() {
                    let original = &base.name;
                    if self.default_profile.as_ref() == Some(original)
                        && matches!(self.default_source, Source::Ssh(_))
                    {
                        self.default_label = name.clone();
                        self.default_profile = Some(name.clone());
                        self.default_source = Source::Ssh(options.clone());
                    }
                    let ids: Vec<_> = self
                        .tabs
                        .entries()
                        .iter()
                        .filter(|entry| entry.value.ssh_profile_name() == Some(original.as_str()))
                        .map(|entry| entry.id)
                        .collect();
                    for id in ids {
                        if let Some(tab) = self.tabs.get_mut(id) {
                            tab.label = name.clone();
                            if let Page::Terminal(live) = &mut tab.page {
                                live.profile_name = Some(name.clone());
                                *live.session.player.write().unwrap() = options.player.clone();
                            }
                        }
                    }
                    if original != &name {
                        let mut windows = self.windows.borrow_mut();
                        if let Some(placement) = windows.profiles.remove(original) {
                            windows.profiles.entry(name.clone()).or_insert(placement);
                        }
                    }
                }
                if let Some(tab) = self.tabs.get_mut(id) {
                    if connect {
                        *tab = live_tab(
                            name.clone(),
                            Source::Ssh(options.clone()),
                            self.appearance.read().unwrap().interaction.osc52,
                        )
                        .with_profile(Some(name));
                    } else if let Page::NewProfile(editor) = &mut tab.page {
                        editor.origin = DraftOrigin::Edit(saved);
                        editor.folder_revoke = Default::default();
                        editor.confirm_delete = false;
                        editor.name = name.clone();
                        editor.error = None;
                        editor.saved = true;
                        tab.label = format!("Edit {name}");
                    }
                }
                self.sync_selection(context);
                self.activate(context);
            }
            Err(error) => {
                if let Some(AppTab {
                    page: Page::NewProfile(draft),
                    ..
                }) = self.tabs.get_mut(id)
                {
                    draft.error = Some(format!("{error:#}"));
                }
            }
        }
    }

    fn caret_menu_callback(
        &self,
        context: &ViewContext<Self>,
        exited: Option<ExitedPane>,
    ) -> Callback<String> {
        let pane = self.tabs.active().and_then(AppTab::pane_id);
        let group = self.tabs.active_group_id();
        let mut cached = self.caret_menu_binding.borrow_mut();
        if let Some(binding) = cached.as_ref()
            && binding.pane == pane
            && binding.group == group
            && binding.exited == exited
        {
            return binding.on_click.clone();
        }
        // Reactor replaces a flyout when its callback changes. Keep the same
        // callback while repainting the title bar for the same action target.
        let on_click = context.callback(move |label: String| match label.as_str() {
            folder_shares::LABEL => pane.map_or(Message::ControlIdle, |id| {
                Message::FolderShare(folder_shares::Action::Open(id))
            }),
            scrollback_export::LABEL => Message::Export(scrollback_export::Action::Start(pane)),
            RESTART_EXITED_PANE => exited.map_or(Message::ControlIdle, Message::RestartExited),
            _ => bind_tab_action(tab_menu_message(label), group),
        });
        *cached = Some(CaretMenuBinding {
            pane,
            group,
            exited,
            on_click: on_click.clone(),
        });
        on_click
    }

    fn header_view(&self, context: &ViewContext<Self>) -> View {
        let mut progress = BTreeMap::new();
        for entry in self.tabs.entries() {
            if let Page::Terminal(pane) = &entry.value.page {
                let next = pane.session.events.progress();
                if next != wezterm_term::Progress::None {
                    let group = progress.entry(entry.group_id()).or_default();
                    *group = taskbar::most_important(*group, next);
                }
            }
        }
        if let Some(hwnd) = self.keyboard.hwnd() {
            let state = progress
                .values()
                .copied()
                .fold(wezterm_term::Progress::None, taskbar::most_important);
            self.taskbar.borrow_mut().show(hwnd, state);
        }
        let tab_strip = TabView::new()
            .height(44.0)
            .width(
                (self.tabs.headers().len() as f64 * 110.0 + 16.0)
                    .min((self.width - 390.0).max(140.0)),
            )
            .is_add_tab_button_visible(false)
            .can_reorder_tabs(true)
            .selected_index(if self.selection_pending {
                None
            } else {
                self.tabs.active_index()
            })
            .on_selection_changed({
                let epoch = self.selection_epoch;
                let pending = self.selection_pending;
                context.callback(move |index| Message::Select(index, epoch, pending))
            })
            .on_close_requested(context.callback(Message::CloseTab))
            .on_reordered({
                let selected = self.tabs.active_id();
                context.callback(move |order| Message::Reorder(order, selected))
            })
            .collection_slot(
                TabViewSlot::TabItems,
                self.tabs.headers().into_iter().map(|tab| {
                    KeyedView::new(
                        self.tabs.group_id(tab.id).unwrap().to_string(),
                        TabViewItem::new()
                            .header(taskbar::tab_header(
                                self.tab_label(tab.id),
                                progress.get(&tab.group_id()).copied().unwrap_or_default(),
                            ))
                            .tag(tab.id.to_string())
                            .is_closable(true),
                    )
                }),
            );
        let plus = Button::new()
            .style(ButtonStyle::Subtle)
            .width(40.0)
            .height(36.0)
            .automation_name("New tab")
            .content(TextBlock::new().text("+").font_size(22.0))
            // Reactor 0.100 cannot currently nest a Menu owner under a Tooltip wrapper.
            .menu(Menu::new(
                self.profile_items(),
                context.callback(Message::ProfileMenu),
            ));
        let exited = self.tabs.active().and_then(AppTab::exited_pane);
        let caret = Button::new()
            .style(ButtonStyle::Subtle)
            .width(44.0)
            .height(36.0)
            .automation_name("Tab menu")
            .content(FontIcon::new().glyph("\u{e70d}"))
            .menu(Menu::new(
                self.caret_items(exited.is_some()),
                self.caret_menu_callback(context, exited),
            ));
        TitleBar::new()
            .preferred_height(WindowTitleBarHeight::Tall)
            .slot(
                TitleBarSlot::Content,
                Grid::new()
                    // TitleBar centers its Content by default. Occupy its available width so
                    // tabs start at the left rather than sitting in the middle of the caption.
                    .width((self.width - 180.0).max(400.0))
                    .columns([
                        GridLength::Auto,
                        GridLength::Auto,
                        GridLength::Auto,
                        GridLength::Auto,
                        GridLength::STAR,
                    ])
                    .children((
                        tab_strip,
                        Border::new().grid_column(1).content(plus),
                        Border::new().grid_column(2).content(caret),
                        Border::new()
                            .grid_column(3)
                            .content(self.inbox_button(context)),
                    )),
            )
    }

    fn profile_items(&self) -> Vec<MenuItem> {
        let mut menu: Vec<_> = self
            .catalog
            .profile_order
            .iter()
            .map(|name| {
                if let Some(ssh) = self.catalog.profiles.get(name) {
                    MenuItem::item(format!("profile:{name}"), profile_label(name, ssh))
                } else {
                    MenuItem::item(format!("local:{name}"), format!("{name} · Local"))
                }
            })
            .collect();
        if self.profile_error.is_some() {
            menu.push(MenuItem::disabled("error", "Could not load profiles"));
        }
        menu.extend([
            MenuItem::separator("profiles-end"),
            MenuItem::item("new-profile", NEW_PROFILE),
            MenuItem::item("lab", LAB),
        ]);
        menu
    }

    fn caret_items(&self, can_restart: bool) -> Vec<MenuItem> {
        let can_paste = matches!(self.tabs.active().map(|tab| &tab.page), Some(Page::Terminal(live)) if !live.read_only.get());
        let player = self.tabs.active().and_then(|tab| match &tab.page {
            Page::Terminal(live) => Some(live.session.player.read().unwrap().clone()),
            _ => None,
        });
        let can_edit = self.tabs.active().is_some_and(|tab| {
            tab.profile_name().is_some_and(|name| {
                self.catalog.profiles.contains_key(name) || self.catalog.local.contains_key(name)
            })
        });
        let device_ready = matches!(self.tabs.active().map(|tab| &tab.page), Some(Page::Terminal(live))
            if !live.device_busy && live.session.prompts.current().is_none()
            && live.session.frames.load().connection_generation > 0
            && live.profile_name.as_ref().and_then(|name| self.catalog.profiles.get(name))
                .is_some_and(|profile| profile.player.rustmush && profile.player.device_key.is_some()));
        let workspace_startup = match self.tabs.active().map(|tab| &tab.page) {
            Some(Page::Terminal(live)) if matches!(&live.source, Source::Ssh(ssh) if !ssh.durable && !ssh.player.rustmush) => {
                MenuItem::item("workspace-startup", "Workspace startup…")
            }
            _ => MenuItem::disabled("workspace-startup", "Workspace startup…"),
        };
        vec![
            self.binding_menu(
                "command-palette",
                "Command palette…",
                KeyAction::CommandPalette,
            ),
            self.pane_menu(can_restart),
            command_menu_item("rename-tab", "Rename tab…", self.move_source().is_some()),
            self.duplicate_tab_item(),
            self.binding_menu("pane-picker", "Find pane…", KeyAction::PanePicker),
            MenuItem::item("inbox", "Attention inbox"),
            command_menu_item("paste", "Paste", can_paste),
            command_menu_item("paste-raw", "Paste raw (no bracketed paste)", can_paste),
            MenuItem::separator("paste-end"),
            MenuItem::item("settings", "Settings"),
            self.binding_menu("workspaces", "Workspaces…", KeyAction::Workspaces),
            workspace_startup,
            self.discovery_menu(),
            state_menu_item(
                "inspector",
                "Terminal inspector",
                self.tabs
                    .active()
                    .and_then(AppTab::pane_id)
                    .map(|_| self.show_inspector),
            ),
            state_menu_item(
                "fullscreen",
                "Fullscreen (F11)",
                Some(self.windows.borrow().fullscreen()),
            ),
            state_menu_item("mute", "Mute", player.as_ref().map(|p| p.mute)),
            state_menu_item(
                "game-mode",
                "Game mode (suppress key repeat)",
                player.as_ref().map(|p| p.game_mode),
            ),
            state_menu_item(
                "alternate-scroll",
                "Alternate-screen wheel keys",
                player.as_ref().map(|p| p.alternate_scroll_enabled()),
            ),
            state_menu_item(
                "remote-sounds",
                "Remote named sounds",
                player.as_ref().map(|p| p.remote_sounds_enabled()),
            ),
            state_menu_item(
                "sync-assets",
                "Remote asset synchronization",
                player.as_ref().map(|p| p.sync_assets_enabled()),
            ),
            MenuItem::separator("profile-actions"),
            command_menu_item(
                "shared-folders",
                folder_shares::LABEL,
                self.tabs
                    .active()
                    .and_then(AppTab::pane_id)
                    .and_then(|pane| self.folder_authority(pane))
                    .is_some(),
            ),
            if can_edit {
                MenuItem::item("edit-profile", "Edit current profile…")
            } else {
                MenuItem::disabled("edit-profile", "Edit current profile…")
            },
            command_menu_item(
                "enroll",
                "Enroll this device (at game prompt)",
                device_ready,
            ),
            command_menu_item(
                "forget-device",
                "Forget this device (revoke game key)",
                device_ready,
            ),
        ]
    }

    fn binding_menu(&self, key: &str, label: &str, action: KeyAction) -> MenuItem {
        let description = self
            .appearance
            .read()
            .unwrap()
            .shortcuts
            .get(action.binding())
            .map_or_else(|| label.to_owned(), |chord| format!("{label}    {chord}"));
        MenuItem::item(key, description)
    }
    fn duplicate_tab_item(&self) -> MenuItem {
        let action = KeyAction::DuplicateTabLayout;
        let binding = action.binding();
        match self
            .tabs
            .active_group_id()
            .context("Select a terminal tab")
            .and_then(|group| self.can_duplicate_tab_layout(group))
        {
            Ok(()) => self.binding_menu(binding.id, binding.label, action),
            Err(reason) => MenuItem::disabled(binding.id, format!("{}: {reason}", binding.label)),
        }
    }
    fn pane_menu(&self, can_restart: bool) -> MenuItem {
        if self.tabs.active().and_then(AppTab::pane_id).is_none() {
            return MenuItem::disabled("panes", "Panes");
        }
        let action = KeyAction::MovePaneToExistingTab;
        let binding = action.binding();
        let move_item = if self.move_source().is_some_and(|source| {
            self.tabs
                .headers()
                .iter()
                .any(|tab| tab.id != source && tab.value.pane_id().is_some())
        }) {
            self.binding_menu(binding.id, binding.label, action)
        } else {
            MenuItem::disabled(binding.id, binding.label)
        };
        let mut items = vec![
            self.binding_menu("split-right", "Split right", KeyAction::SplitRight),
            self.binding_menu("split-below", "Split below", KeyAction::SplitBelow),
            command_menu_item("restart-exited", RESTART_EXITED_PANE, can_restart),
            move_item,
            if self.outgoing.is_none() && self.incoming.is_none() {
                MenuItem::item("move-tab-window", "Move tab to new window")
            } else {
                MenuItem::disabled("move-tab-window", "Move tab to new window")
            },
            self.binding_menu("copy-mode", "Keyboard copy mode", KeyAction::CopyMode),
            self.binding_menu("keyboard-hints", "Keyboard hints", KeyAction::KeyboardHints),
        ];
        if matches!(self.tabs.active(), Some(AppTab { page: Page::Terminal(tab), .. }) if matches!(tab.source, Source::Ssh(_)))
        {
            items.push(MenuItem::item("files", "SFTP files"));
            items.push(MenuItem::item("files-here", "Open files here"));
        }
        if let Some(AppTab {
            page: Page::Terminal(live),
            ..
        }) = self.tabs.active()
        {
            if matches!(live.source, Source::Ssh(_) | Source::Local(_)) {
                items.push(MenuItem::item(
                    "pane-read-only",
                    if live.read_only.get() {
                        "Disable read-only pane input"
                    } else {
                        "Enable read-only pane input"
                    },
                ));
            }
            items.push(match live.session.frames.load().copy_output_unavailable() {
                Some(why) => MenuItem::disabled(
                    "copy-last-output",
                    format!("Copy last command output ({why})"),
                ),
                None => MenuItem::item("copy-last-output", "Copy last command output"),
            });
            items.push(MenuItem::item("command-history", "Command history"));
            items.push(command_menu_item(
                "save-scrollback",
                scrollback_export::LABEL,
                self.export.pending.is_none(),
            ));
            items.push(match live.session.events.command_watch() {
                Some(true) => MenuItem::item("command-watch", "Cancel command notification"),
                Some(false) => MenuItem::item("command-watch", "Notify when command finishes"),
                None => MenuItem::disabled("command-watch", "Notify when command finishes"),
            });
            if let Some(menu) = durable_view::pane_menu(live) {
                items.push(menu);
            }
        }
        if !self.catalog.profile_order.is_empty() {
            for (key, label) in [
                ("right-profile", "Split right with"),
                ("below-profile", "Split below with"),
            ] {
                items.push(MenuItem::submenu(
                    key,
                    format!("{label} profile"),
                    self.catalog
                        .profile_order
                        .iter()
                        .map(|name| MenuItem::item(name.clone(), format!("{label} {name}"))),
                ));
            }
        }
        if self
            .tabs
            .active_layout()
            .is_some_and(|layout| matches!(layout, w11term::pane_layout::Layout::Split { .. }))
        {
            items.push(MenuItem::separator("pane-actions"));
            for (key, action) in [
                ("left", KeyAction::FocusLeft),
                ("right", KeyAction::FocusRight),
                ("up", KeyAction::FocusUp),
                ("down", KeyAction::FocusDown),
            ] {
                items.push(self.binding_menu(key, action.binding().label, action));
            }
            if !self.tabs.zoomed()
                && let Some(id) = self.tabs.active_id()
                && let Some(layout) = self.tabs.active_layout()
            {
                for (axis, actions) in [
                    (
                        Axis::Columns,
                        [KeyAction::ResizeLeft, KeyAction::ResizeRight],
                    ),
                    (Axis::Rows, [KeyAction::ResizeUp, KeyAction::ResizeDown]),
                ] {
                    if layout.divider_for(id, axis).is_some() {
                        for action in actions {
                            let binding = action.binding();
                            items.push(self.binding_menu(binding.id, binding.label, action));
                        }
                    }
                }
            }
            items.push(self.binding_menu(
                "zoom-pane",
                if self.tabs.zoomed() {
                    "Restore panes"
                } else {
                    "Zoom pane"
                },
                KeyAction::ZoomPane,
            ));
            items.push(self.binding_menu("close-pane", "Close pane", KeyAction::ClosePane));
            items.push(self.binding_menu("move-pane", "Move pane to new tab", KeyAction::MovePane));
        }
        MenuItem::submenu("panes", "Panes", items)
    }

    fn profile_form(&self, id: TabId, draft: &ProfileDraft, context: &ViewContext<Self>) -> View {
        let mut fields: Vec<View> = vec![
            TextBlock::new()
                .text(match draft.origin {
                    DraftOrigin::Edit(_) => "Edit Profile",
                    DraftOrigin::Copy(_) => "Duplicate Profile",
                    DraftOrigin::New => "New Profile",
                })
                .font_size(26.0)
                .into(),
        ];
        if let Some(base) = draft.origin.editing() {
            fields.push(TextBlock::new().text("Connection changes apply to new sessions. Sound and game options update open tabs.").text_wrapping(TextWrapping::Wrap).into());
            if let Some(pane) = self.folder_editor_target(draft) {
                fields.push(
                    Button::new()
                        .horizontal_alignment(HorizontalAlignment::Left)
                        .on_click(context.callback(move |_| {
                            Message::FolderShare(folder_shares::Action::Open(pane))
                        }))
                        .content(folder_shares::LABEL),
                );
            }
            fields.push(self.folder_revoke_view(id, base, &draft.folder_revoke, context));
        }
        // Keep the feedback slot mounted so save/error messages do not remount inputs.
        if !draft.proxy_jump.is_empty() {
            fields.push(
                TextBlock::new()
                    .text(config_import::jump_summary(&draft.proxy_jump))
                    .text_wrapping(TextWrapping::Wrap)
                    .into(),
            );
        }
        fields.push(
            TextBlock::new()
                .text(draft.error.as_deref().unwrap_or(if draft.saved {
                    "Changes saved."
                } else {
                    ""
                }))
                .text_wrapping(TextWrapping::Wrap)
                .foreground(if draft.error.is_some() {
                    Color::rgb(255, 160, 150)
                } else {
                    Color::rgb(130, 220, 160)
                })
                .into(),
        );
        for (field, label, text, placeholder) in [
            (Field::Name, "Profile name", &draft.name, "My server"),
            (Field::Host, "Host", &draft.host, "example.com"),
            (Field::User, "User", &draft.user, "root"),
            (Field::Port, "Port", &draft.port, "22"),
        ] {
            fields.push(
                StackPanel::new().spacing(5.0).children((
                    TextBlock::new().text(label),
                    TextBox::new()
                        .text(text)
                        .placeholder_text(placeholder)
                        .automation_name(label)
                        .on_text_changed(context.callback(move |v| Message::Edit(id, field, v))),
                )),
            );
        }
        fields.push(
            StackPanel::new().spacing(5.0).children((
                TextBlock::new().text("SSH terminal type (TERM)"),
                ComboBox::new()
                    .items_source([
                        "xterm-256color (default)",
                        "xterm-sixel (Sixel graphics)",
                        "Custom…",
                    ])
                    .selected_index(draft.terminal_preset)
                    .automation_name("SSH terminal type")
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .on_selection_changed(
                        context.callback(move |v| Message::TerminalPreset(id, v)),
                    ),
            )),
        );
        if draft.terminal_preset == 2 {
            fields.push(
                TextBox::new()
                    .text(&draft.custom_terminal)
                    .placeholder_text("Terminfo name on the remote host")
                    .automation_name("Custom SSH terminal type")
                    .on_text_changed(
                        context.callback(move |v| Message::Edit(id, Field::CustomTerminal, v)),
                    )
                    .into(),
            );
        }
        fields.push(
            StackPanel::new().spacing(5.0).children((
                TextBlock::new().text("Authentication"),
                ComboBox::new()
                    .items_source([
                        "Windows SSH agent",
                        "Private key file",
                        "None (keyless server)",
                        "Password",
                        "Keyboard-interactive (password / MFA)",
                    ])
                    .selected_index(draft.authentication)
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .on_selection_changed(context.callback(move |v| Message::Auth(id, v))),
            )),
        );
        if draft.authentication == 1 {
            fields.push(
                StackPanel::new().spacing(5.0).children((
                    TextBlock::new().text("Private key file"),
                    TextBox::new()
                        .text(&draft.identity)
                        .placeholder_text("C:\\path\\to\\key or a path beside profiles.json")
                        .automation_name("Private key file")
                        .on_text_changed(
                            context.callback(move |v| Message::Edit(id, Field::Identity, v)),
                        ),
                )),
            );
        }
        fields.push(
            StackPanel::new().spacing(5.0).children((
                TextBlock::new().text("Host fingerprint (optional)"),
                TextBox::new()
                    .text(&draft.fingerprint)
                    .placeholder_text("SHA256:… — otherwise use OpenSSH known_hosts")
                    .automation_name("Host fingerprint")
                    .on_text_changed(
                        context.callback(move |v| Message::Edit(id, Field::Fingerprint, v)),
                    ),
            )),
        );
        fields.push(
            Button::new()
                .is_enabled(!draft.generating)
                .on_click(context.callback(move |_| Message::GenerateKey(id)))
                .content(if draft.generating {
                    "Generating device key…"
                } else {
                    "Generate device key (Ed25519)"
                }),
        );
        if let Some(device) = &draft.device {
            fields.push(
                TextBlock::new()
                    .text(format!(
                        "{} · {}\n{}",
                        device.device,
                        device.fingerprint,
                        device.path.display()
                    ))
                    .text_wrapping(TextWrapping::Wrap)
                    .into(),
            );
            fields.push(
                Button::new()
                    .on_click(context.callback(move |_| Message::CopyPublicKey(id)))
                    .content("Copy public key"),
            );
            fields.push(TextBlock::new().text("Private key: Windows-user DPAPI encryption and a restricted ACL. For another host, append the copied public line to ~/.ssh/authorized_keys. OpenSSH cannot read the DPAPI file directly.").text_wrapping(TextWrapping::Wrap).into());
        }
        fields.push(
            CheckBox::new()
                .is_checked(draft.durable)
                .on_is_checked_changed(
                    context.callback(move |v| {
                        Message::Durability(durable_view::Action::Profile(id, v))
                    }),
                )
                .content("Keep the remote shell running with zmx (Linux)"),
        );
        for (name, label, value) in [
            (
                "rustmush",
                "RustMUSH server (device keys and session resume)",
                draft.player.rustmush,
            ),
            (
                "game_mode",
                "Game mode (suppress held-key repeats)",
                draft.player.game_mode,
            ),
            (
                "reconnect",
                "Reconnect after a network interruption",
                draft.player.reconnect,
            ),
            ("mute", "Mute this profile", draft.player.mute),
            (
                "remote_sounds",
                "Allow remote named sounds (OSC 440)",
                draft.player.remote_sounds_enabled(),
            ),
            (
                "sync_assets",
                "Allow remote asset synchronization over SFTP",
                draft.player.sync_assets_enabled(),
            ),
            (
                "alternate_scroll",
                "Send wheel as arrow keys on the alternate screen",
                draft.player.alternate_scroll_enabled(),
            ),
        ] {
            fields.push(
                CheckBox::new()
                    .is_checked(value)
                    .on_is_checked_changed(
                        context.callback(move |value| Message::DraftPlayer(id, name.into(), value)),
                    )
                    .content(label),
            );
        }
        fields.push(
            TextBlock::new()
                .text("Bell sound name (system, off, or a named WAV)")
                .into(),
        );
        fields.push(
            TextBox::new()
                .text(&draft.player.bell)
                .on_text_changed(context.callback(move |v| Message::DraftBell(id, v)))
                .into(),
        );
        if !matches!(draft.origin, DraftOrigin::New) {
            fields.push(
                Button::new()
                    .style(ButtonStyle::Accent)
                    .automation_name("Save profile changes")
                    .is_enabled(!draft.generating)
                    .on_click(context.callback(move |_| Message::SaveProfileOnly(id)))
                    .content(if matches!(draft.origin, DraftOrigin::Copy(_)) {
                        "Save copy"
                    } else {
                        "Save changes"
                    }),
            );
        }
        fields.push(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(10.0)
                .children((
                    Button::new()
                        .style(
                            matches!(draft.origin, DraftOrigin::New).then_some(ButtonStyle::Accent),
                        )
                        .is_enabled(!draft.generating)
                        .on_click(context.callback(move |_| Message::SaveProfile(id)))
                        .content("Save and connect"),
                    Button::new()
                        .on_click(context.callback(move |_| Message::CloseTab(id.to_string())))
                        .content(if draft.saved { "Close" } else { "Cancel" }),
                )),
        );
        if let Some(base) = draft.origin.editing() {
            if draft.confirm_delete {
                fields.push(TextBlock::new().text(format!("Delete saved profile “{}”? Open sessions stay connected and key files stay on disk.", base.name)).text_wrapping(TextWrapping::Wrap).into());
                fields.push(
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(10.0)
                        .children((
                            Button::new()
                                .on_click(
                                    context.callback(move |_| {
                                        Message::ConfirmDeleteProfile(id, false)
                                    }),
                                )
                                .content("Keep profile"),
                            Button::new()
                                .automation_name(format!("Confirm delete {}", base.name))
                                .on_click(
                                    context
                                        .callback(move |_| Message::ConfirmDeleteProfile(id, true)),
                                )
                                .content("Delete saved profile"),
                        )),
                );
            } else {
                fields.push(
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(10.0)
                        .children((
                            Button::new()
                                .is_enabled(!draft.generating)
                                .on_click(context.callback(move |_| Message::DuplicateProfile(id)))
                                .content("Duplicate saved profile…"),
                            Button::new()
                                .is_enabled(!draft.generating)
                                .on_click(context.callback(move |_| Message::DeleteProfile(id)))
                                .content("Delete profile…"),
                        )),
                );
            }
        }
        ScrollViewer::new().content(
            StackPanel::new()
                .spacing(14.0)
                .width(520.0)
                .horizontal_alignment(HorizontalAlignment::Left)
                .margin(32.0)
                .keyed_children(
                    fields
                        .into_iter()
                        .enumerate()
                        .map(|(index, field)| KeyedView::new(index.to_string(), field)),
                ),
        )
    }
}

impl Component for TerminalApp {
    type Input = WindowLaunch;
    type Message = Message;

    fn create(input: &Self::Input, context: &ComponentContext<Self>) -> Self {
        let options = &input.options;
        let opening = &input.opening;
        let moving = input.transfer.is_some();
        let sender = context.sender();
        let active_group = Rc::new(Cell::new(None));
        let shortcut_group = active_group.clone();
        let keyboard = KeyboardBridge::install(move |shortcut| {
            sender.send(bind_tab_action(
                Message::Shortcut(shortcut),
                shortcut_group.get(),
            ));
        })
        .expect("installing terminal input adapter");
        let _native = keyboard.scope().enter();
        let store = ProfileStore::new(options.profiles_path.clone())
            .expect("resolving profile configuration path");
        let (catalog, profile_error) = match store.catalog() {
            Ok(catalog) => (catalog, None),
            Err(error) => (Profiles::default(), Some(format!("{error:#}"))),
        };
        let label = options
            .initial_profile
            .clone()
            .unwrap_or_else(|| source_label(&options.source));
        let mut tabs = Tabs::default();
        let (mut appearance, mut appearance_error) =
            match Appearance::path().and_then(|path| Appearance::load(&path)) {
                Ok(a) => (a, None),
                Err(e) => (
                    Appearance::default(),
                    Some(format!("Unable to load settings: {e:#}")),
                ),
            };
        let fonts = match installed_fonts() {
            Ok(f) => f,
            Err(e) => {
                appearance_error = Some(format!("Unable to enumerate fonts: {e:#}"));
                Vec::new()
            }
        };
        if !fonts.contains(&appearance.font_family) {
            appearance.font_family = fonts
                .iter()
                .find(|font| font.as_str() == "Cascadia Mono")
                .or_else(|| fonts.iter().find(|font| font.as_str() == "Consolas"))
                .or_else(|| fonts.first())
                .cloned()
                .unwrap_or_else(|| "Cascadia Mono".into());
        }
        if !appearance.font_size.is_finite() {
            appearance.font_size = 16.0;
        }
        appearance.font_size = appearance.font_size.clamp(6.0, 72.0);
        let renderer = Renderer::with_size(
            &appearance.font_family,
            appearance.ligatures,
            appearance.font_size,
        )
        .expect("creating DirectWrite formats");
        keyboard.set_shortcuts(
            appearance
                .shortcuts
                .keymap()
                .expect("validated keyboard settings"),
        );
        let appearance = Arc::new(RwLock::new(appearance));
        let grid = if !moving && (options.cols.is_some() || options.rows.is_some()) {
            Some((options.cols.unwrap_or(120), options.rows.unwrap_or(32)))
        } else {
            None
        };
        let size = grid
            .map(|(cols, rows)| TerminalSize {
                cols,
                rows,
                pixel_width: cols * renderer.metrics.cell_width.ceil() as usize,
                pixel_height: rows * renderer.metrics.cell_height.ceil() as usize,
                ..TerminalSize::default()
            })
            .unwrap_or_default();
        let client_size = grid
            .map(|(cols, rows)| {
                (
                    (cols as f32 * renderer.metrics.cell_width.ceil() + PADDING * 2.0) as f64,
                    (rows as f32 * renderer.metrics.cell_height.ceil()
                        + PADDING * 2.0
                        + STATUS_HEIGHT
                        + 48.0) as f64,
                )
            })
            .unwrap_or((1120.0, 760.0));
        let osc52 = appearance.read().unwrap().interaction.osc52;
        if let Some(workspace) = opening.as_ref().map(|window| &window.layout) {
            workspace_view::append_resolved_workspace(
                &mut tabs,
                &workspace.layout,
                workspace.sources.clone(),
                osc52,
            );
        } else if !moving {
            tabs.insert(
                live_tab_sized(
                    label.clone(),
                    options.source.clone(),
                    size,
                    osc52,
                    None,
                    None,
                )
                .with_profile(options.initial_profile.clone()),
            );
            for name in &options.extra_profiles {
                if let Some(source) = catalog.source(name) {
                    tabs.insert(
                        live_tab_sized(name.clone(), source, size, osc52, None, None)
                            .with_profile(Some(name.clone())),
                    );
                }
            }
        }
        // The initial cell target sizes the whole window. A split layout derives
        // its individual PTY grids from the measured rectangle of each canvas.
        let grid = grid.filter(|_| {
            matches!(
                tabs.active_layout(),
                Some(w11term::pane_layout::Layout::Leaf(_))
            )
        });
        let registry = input.transfer.as_ref().map_or_else(
            || Registry::window(1, opening.as_ref().map(|window| window.key.clone())),
            |transfer| transfer.registry.clone(),
        );
        let (control_sender, control_rx) = std::sync::mpsc::sync_channel::<UiWork>(8);
        let wake = control_sender.clone();
        let folder_messages = control_sender.clone();
        let folder_wake = Arc::new(move || {
            let _ = folder_messages.try_send(UiWork::FolderAuthority);
        });
        let inbox = w11term::events::Inbox::new(move || {
            // If full, a control message will already wake the UI to read the latest count.
            let _ = wake.try_send(UiWork::AttentionChanged);
        });
        registry.set_ui(move |request| {
            if let Err(
                std::sync::mpsc::TrySendError::Full(UiWork::Control(request))
                | std::sync::mpsc::TrySendError::Disconnected(UiWork::Control(request)),
            ) = control_sender.try_send(UiWork::Control(request))
            {
                request.fail("GUI control queue is unavailable or full");
            }
        });
        let capture_appearance = appearance.clone();
        let capture: w11term::control::CapturePng = Arc::new(move |frame, size, path| {
            let appearance = capture_appearance.read().unwrap().clone();
            crate::renderer::screenshot_with_font(frame, size, path, &appearance)
        });
        registry.set_capture(capture.clone());
        let control = input.transfer.as_ref().map_or_else(
            || {
                Arc::new(
                    Server::start(registry.clone(), move |frame, size, path| {
                        capture(frame, size, path)
                    })
                    .expect("starting local control pipe"),
                )
            },
            |transfer| transfer.server.clone(),
        );
        let active = registry.clone();
        let sender = context.sender();
        keyboard.on_activation(move |focused| {
            if focused {
                active.activate();
                sender.send(Message::RefreshProfiles);
            }
        });
        let mut app = Self {
            folder_wake,
            inbox,
            taskbar: RefCell::default(),
            accessible: Default::default(),
            active_group,
            windows: Rc::new(RefCell::new(crate::window_state::Windows::load())),
            window_profile: None,
            window_restored: moving || options.cols.is_some() || options.rows.is_some(),
            keyboard,
            _control: control,
            window_launch: options.clone(),
            opening: opening.clone(),
            incoming: input.transfer.clone(),
            outgoing: None,
            window_timer: None,
            native_bound: false,
            window_error: None,
            export: Default::default(),
            registry,
            control_rx: Arc::new(std::sync::Mutex::new(control_rx)),
            control_task: None,
            tabs,
            metrics: Rc::new(Cell::new(Metrics::default())),
            invalidator: Invalidator::new(),
            header: RefCell::new(None),
            caret_menu_binding: RefCell::new(None),
            folder_panel_timer: None,
            frame_tasks: BTreeMap::new(),
            show_inspector: false,
            inspection: None,
            watcher_retry: None,
            watch_epoch: 0,
            default_source: options.source.clone(),
            default_label: label,
            default_profile: options.initial_profile.clone(),
            catalog,
            store,
            profile_error,
            profile_move_error: None,
            width: client_size.0,
            window_height: client_size.1,
            client_size,
            initial_grid: Rc::new(crate::window_state::RequestedGrid::new(grid)),
            appearance,
            fonts,
            appearance_error,
            divider_drag: Rc::new(RefCell::new(None)),
            measured_layout: None,
            mru_groups: Vec::new(),
            switcher: None,
            picker: None,
            title_editor: None,
            hints: Default::default(),
            copy_mode: Default::default(),
            selection_pending: true,
            selection_epoch: 0,
        };
        if let Some(transfer) = &app.incoming {
            transfer.target.replace(Some(context.sender()));
        }
        app.arm_window_tick(context);
        context.sender().send(Message::WindowTick);
        app.activate(context);
        app.watch_control(context);
        app.sync_selection(context);
        #[cfg(feature = "ui-smoke")]
        if !moving && std::env::var_os("W11TERM_UI_SMOKE").is_some() {
            if let Some(fixture) = std::env::var_os("W11TERM_IMPORT_SMOKE") {
                let expected = std::path::PathBuf::from(fixture).with_file_name("profiles.json");
                assert_eq!(
                    options.profiles_path.canonicalize().unwrap(),
                    expected.canonicalize().unwrap(),
                    "import smoke requires isolated profiles beside the fixture"
                );
            }
            let loopback = matches!(&options.source, Source::Ssh(ssh)
                if ssh.host == "127.0.0.1" && ssh.host_key.is_some() &&
                    ((ssh.user == "test" && (std::env::var_os("W11TERM_SSH_SPLIT_SMOKE").is_some()
                         || std::env::var_os("W11TERM_LOCAL_SMOKE").is_some()
                         || std::env::var_os("W11TERM_FILE_SMOKE").is_some()))
                     || (ssh.user == "pilot" && ssh.durable && std::env::var_os("W11TERM_DURABLE_SMOKE").is_some())
                     || (ssh.user == "pilot" && std::env::var_os("W11TERM_DISCOVERY_SMOKE").is_some())
                     || (ssh.user == "bench" && std::env::var_os("W11TERM_PROFILE_SMOKE").is_some())));
            assert!(
                (matches!(options.source, Source::Demo) || loopback)
                    && options.extra_profiles.is_empty(),
                "UI smoke requires a local lab or an explicit loopback fixture"
            );
            if std::env::var_os("W11TERM_PROFILE_SMOKE").is_some() {
                let fixture = std::path::PathBuf::from(
                    std::env::var_os("W11TERM_PROFILE_SMOKE_CONFIG")
                        .expect("profile smoke requires an isolated config"),
                );
                assert_eq!(
                    options.profiles_path.canonicalize().unwrap(),
                    fixture.canonicalize().unwrap(),
                    "profile smoke must not edit user profiles"
                );
            }
            context.spawn_background(|_| {
                std::thread::sleep(std::time::Duration::from_secs(1));
                Message::Smoke(0)
            });
        }
        if !moving && let Some(seconds) = options.smoke_seconds {
            context.spawn_background(move |_| {
                std::thread::sleep(std::time::Duration::from_secs(seconds.min(30)));
                Message::CloseWindow
            });
        }
        app
    }

    fn update(&mut self, message: Message, context: &ComponentContext<Self>) {
        let _native = self.keyboard.scope().enter();
        if !matches!(
            &message,
            Message::FrameReady(..)
                | Message::FolderShare(folder_shares::Action::Panel(..))
                | Message::RefreshProfiles
                | Message::TabTitle(tab_title::Action::Edit(..))
                | Message::Files(..)
                | Message::Keyboard(keyboard_settings::Edit::Text(..))
                | Message::Patterns(
                    pattern_settings::Edit::Name(_) | pattern_settings::Edit::Expression(_)
                )
                | Message::PickerQuery(..)
                | Message::PickerHover(..)
                | Message::PickerFrame(..)
                | Message::PickerFocus(..)
                | Message::PaneRatio(..)
                | Message::DividerPress(..)
                | Message::DividerEnd(..)
                | Message::HoverPane(..)
                | Message::PointerExit(..)
                | Message::Pointer(_, _, MouseEventKind::Move)
        ) {
            self.header.borrow_mut().take();
        }
        if matches!(
            &message,
            Message::Control(_) | Message::AttentionChanged | Message::FolderAuthority
        ) {
            if self.inbox.acknowledge_wake()
                && let Some(hwnd) = self.keyboard.hwnd()
            {
                attention::flash_background_window(hwnd);
            }
            self.refresh_open_commands();
            self.watch_control(context);
        }
        let message = match message {
            Message::Control(request) => match self.registry.for_this_window(request) {
                Some(request) => Message::Control(request),
                None => return,
            },
            other => other,
        };
        match message {
            Message::FolderShare(action) => self.folder_action(action, context),
            Message::FolderRevoke(id, ticket, action) => {
                self.folder_revoke(id, ticket, action, context)
            }
            Message::RestartExited(exit) => {
                // A native menu may still be delivering a callback for a previous exit or pane.
                if let Some(id) = self.tab_for_pane(exit.pane)
                    && let Some(AppTab {
                        page: Page::Terminal(live),
                        ..
                    }) = self.tabs.get_mut(id)
                    && live.session.events.restart_token() == Some(exit.token)
                {
                    match live.session.restart_exited(exit.token) {
                        Ok(()) => live.reset_transport_input(&self.keyboard),
                        Err(error) => {
                            self.window_error =
                                Some(format!("Could not restart pane {}: {error:#}", exit.pane));
                        }
                    }
                }
                self.refresh_open_commands();
            }
            Message::Export(action) => {
                self.export_scrollback(action, context);
                self.refresh_open_commands();
            }
            Message::DismissWindowError => self.window_error = None,
            Message::MoveTabToWindow => self.move_tab_window(context),
            Message::DuplicateTabLayout(group) => {
                self.window_error = self
                    .duplicate_tab_layout(group, context)
                    .err()
                    .map(|error| format!("Couldn’t duplicate tab layout. {error:#}"));
            }
            Message::WindowTick => self.window_tick(context),
            Message::TransferReady(id) => self.offer_group(id, context),
            Message::TransferClaim(id) => self.claim_group(id, context),
            Message::TransferDone(id) => self.finish_transfer(id, context),
            Message::TransferCanceled(id) => self.cancel_transfer(id, context),
            Message::TabTitle(action) => self.edit_tab_title(action),
            Message::Attention(action) => self.handle_attention(action, context),
            Message::AttentionChanged | Message::FolderAuthority => {}
            Message::ToggleInspector => {
                self.show_inspector = !self.show_inspector;
                self.sync_inspector();
                self.cancel_frame_tasks();
                self.watch_frames(context);
            }
            Message::CopyLastCommand => {
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.active()
                    && let Some(command) = tab.session.frames.load().command_history.back()
                    && let Ok(text) = &command.output
                    && let Err(error) = w11term::clipboard::write(text)
                {
                    log::warn!("Copy last command output: {error:#}");
                }
            }
            Message::History(action) => self.handle_history(action, context),
            Message::PaletteChoose(id) => self.choose_command(id, context),
            Message::PickerQuery(query) => self.filter_picker(query, context),
            Message::PickerHover(pane) => self.hover_picker(pane, context),
            Message::PickerFrame(pane, generation) => self.picker_frame(pane, generation, context),
            Message::PickerChoose(pane) => self.choose_picker_pane(pane, context),
            Message::PlacePane(source, target, side) => {
                self.place_pane(source, target, side, context)
            }
            Message::PickerFocus(attempt) => self.focus_picker_input(attempt, context),
            Message::Files(action) => self.file_action(action, context),
            Message::AccessibleSelection(request) => {
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.active_id().and_then(|id| self.tabs.get_mut(id))
                    && tab.session.pane_id == request.pane
                    && tab.prompt_id.is_none()
                    && !tab.find.open
                    && self.switcher.is_none()
                    && self.picker.is_none()
                    && self.hints.borrow().is_none()
                    && self.copy_mode.borrow().is_none()
                {
                    let frame = tab.session.frames.load();
                    if frame.generation == request.generation {
                        *tab.selection.borrow_mut() = request.span.map(|(anchor, end)| {
                            let mut selection = Selection::new(anchor, false, frame.alternate);
                            selection.update(end);
                            selection.active = true;
                            selection.dragging = false;
                            selection.observe(&frame);
                            selection
                        });
                        self.keyboard.set_selection(request.span.is_some());
                        tab.surface.invalidator.invalidate();
                    }
                }
            }
            Message::DraftBell(id, value) => {
                if let Some(AppTab {
                    page: Page::NewProfile(draft),
                    ..
                }) = self.tabs.get_mut(id)
                    && value.len() <= 128
                {
                    if draft.player.bell != value {
                        draft.saved = false;
                    }
                    draft.player.bell = value;
                }
            }
            Message::DeviceAction(remove) => {
                if let Some(id) = self.tabs.active_id()
                    && let Some(AppTab {
                        page: Page::Terminal(tab),
                        ..
                    }) = self.tabs.get_mut(id)
                    && !tab.device_busy
                {
                    let result = (|| -> anyhow::Result<_> {
                        let profile = tab
                            .profile_name
                            .as_ref()
                            .and_then(|name| self.catalog.profiles.get(name))
                            .context("Use a saved Rustmush profile")?;
                        anyhow::ensure!(
                            profile.player.rustmush,
                            "Enable Rustmush in this profile first"
                        );
                        let path = profile
                            .player
                            .device_key
                            .clone()
                            .context("Generate a device key in the connect wizard first")?;
                        anyhow::ensure!(
                            tab.session.frames.load().connection_generation > 0
                                && tab.session.prompts.current().is_none(),
                            "Sign in and reach the game prompt first"
                        );
                        Ok((path, profile.user.clone()))
                    })();
                    match result {
                        Ok((path, user)) => {
                            tab.device_busy = true;
                            tab.device_message = Some(
                                if remove {
                                    "Revoking this device and verifying the server listing…"
                                } else {
                                    "Enrolling this device and verifying its fingerprint…"
                                }
                                .into(),
                            );
                            let input = tab.session.input.clone();
                            self.keyboard.set_target(None);
                            context.spawn_background(move |_| {
                                let result = (|| -> anyhow::Result<String> {
                                    let key = w11term::keys::describe(&path)?;
                                    let fingerprint = key.fingerprint.clone();
                                    tokio::runtime::Builder::new_current_thread()
                                        .enable_all()
                                        .build()?
                                        .block_on(w11term::enroll::run(input, key, user, remove))?;
                                    Ok(fingerprint)
                                })();
                                Message::DeviceDone(
                                    id,
                                    remove,
                                    result.map_err(|e| format!("{e:#}")),
                                )
                            });
                        }
                        Err(error) => tab.device_message = Some(format!("{error:#}")),
                    }
                }
            }
            Message::DeviceDone(id, remove, result) => {
                let active = self.tabs.active_id() == Some(id);
                let mut auth_update = None;
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get_mut(id)
                {
                    tab.device_busy = false;
                    tab.device_message = Some(match result {
                        Ok(fingerprint) => {
                            let result = (|| -> anyhow::Result<()> {
                                let label =
                                    tab.profile_name.as_ref().context("profile was removed")?;
                                let mut profile = self
                                    .catalog
                                    .profiles
                                    .get(label)
                                    .context("profile was removed")?
                                    .clone();
                                let auth = if remove {
                                    Authentication::Password
                                } else {
                                    Authentication::Identity {
                                        identity: profile
                                            .player
                                            .device_key
                                            .clone()
                                            .context("device key path missing")?,
                                    }
                                };
                                profile.authentication = auth.clone();
                                self.store.update(label, &profile)?;
                                self.catalog.profiles.insert(label.clone(), profile);
                                auth_update = Some((label.clone(), auth));
                                Ok(())
                            })();
                            match result {
                                Ok(()) => format!(
                                    "{} · verified {fingerprint}",
                                    if remove {
                                        "Device revoked; future connections use a password"
                                    } else {
                                        "Device enrolled; future connections use its key"
                                    }
                                ),
                                Err(error) => format!(
                                    "Server operation succeeded ({fingerprint}), but saving the profile failed: {error:#}"
                                ),
                            }
                        }
                        Err(error) => error,
                    });
                    if active {
                        self.keyboard.set_target(Some((
                            tab.session.input.clone(),
                            tab.session.frames.clone(),
                            tab.read_only.clone(),
                        )));
                    }
                }
                if let Some((label, auth)) = auth_update {
                    for entry in self.tabs.entries() {
                        if entry.value.ssh_profile_name() == Some(label.as_str())
                            && let Page::Terminal(tab) = &entry.value.page
                        {
                            let _ = tab.session.input.send(Command::Authenticate(auth.clone()));
                        }
                    }
                }
            }
            Message::RestoreWindow => {
                self.restore_window();
            }
            Message::TogglePlayer(field) => {
                if let Some(id) = self.tabs.active_id()
                    && let Some(AppTab {
                        page: Page::Terminal(tab),
                        ..
                    }) = self.tabs.get_mut(id)
                {
                    let mut player = tab.session.player.write().unwrap();
                    match field.as_str() {
                        "mute" => player.mute = !player.mute,
                        "game_mode" => player.game_mode = !player.game_mode,
                        "remote_sounds" => {
                            player.remote_sounds = Some(!player.remote_sounds_enabled())
                        }
                        "sync_assets" => player.sync_assets = Some(!player.sync_assets_enabled()),
                        "alternate_scroll" => {
                            player.alternate_scroll = Some(!player.alternate_scroll_enabled())
                        }
                        _ => {}
                    }
                    let preferences = player.clone();
                    drop(player);
                    if let Source::Ssh(options) = &mut tab.source {
                        options.player = preferences.clone();
                    }
                    if let Some(name) = tab
                        .profile_name
                        .clone()
                        .filter(|_| matches!(tab.source, Source::Ssh(_)))
                    {
                        if let Some(profile) = self.catalog.profiles.get_mut(&name) {
                            profile.player = preferences.clone();
                            if let Err(error) = self.store.update(&name, profile) {
                                log::warn!("Saving player preferences: {error:#}");
                            }
                        }
                        for entry in self.tabs.entries() {
                            if entry.id != id
                                && entry.value.ssh_profile_name() == Some(name.as_str())
                                && let Page::Terminal(other) = &entry.value.page
                            {
                                *other.session.player.write().unwrap() = preferences.clone();
                            }
                        }
                    }
                }
                if let Some(id) = self.tabs.active_id() {
                    self.connection_ui(id);
                }
            }
            Message::ControlIdle => {}
            Message::WatchRejected => {
                if self.watcher_retry.is_none() {
                    let sender = context.sender();
                    match crate::ui_timer::RetryTimer::new(move || {
                        sender.send(Message::RetryWatchers);
                    }) {
                        Ok(timer) => self.watcher_retry = Some(timer),
                        Err(error) => log::error!("Cannot schedule watcher recovery: {error}"),
                    }
                }
            }
            Message::RetryWatchers => {
                if self
                    .control_task
                    .as_ref()
                    .is_none_or(|task| task.status() == ComponentTaskStatus::Rejected)
                {
                    self.watch_control(context);
                }
                self.frame_tasks
                    .retain(|_, task| task.status() != ComponentTaskStatus::Rejected);
                self.watch_frames(context);
                let preview_ready = self.watch_picker_preview(context);
                if preview_ready
                    && self
                        .control_task
                        .as_ref()
                        .is_some_and(|task| task.status() != ComponentTaskStatus::Rejected)
                    && self
                        .frame_tasks
                        .values()
                        .all(|task| task.status() != ComponentTaskStatus::Rejected)
                {
                    self.watcher_retry = None;
                }
            }
            Message::Control(w11term::control::UiRequest::Open {
                profile,
                cols,
                rows,
                activate,
                reply,
            }) => {
                let result = (|| -> anyhow::Result<serde_json::Value> {
                    let source = self
                        .catalog
                        .source(&profile)
                        .with_context(|| format!("unknown profile {profile}"))?;
                    let metrics = self.metrics.get();
                    let explicit_grid = cols.is_some() || rows.is_some();
                    anyhow::ensure!(
                        !activate || !explicit_grid || !self.windows.borrow().fullscreen(),
                        "Exit fullscreen before opening an active tab with a different grid; background open is available"
                    );
                    let cols = cols.unwrap_or(metrics.cols.max(1));
                    let rows = rows.unwrap_or(metrics.rows.max(1));
                    let size = TerminalSize {
                        cols,
                        rows,
                        pixel_width: (cols as f32 * metrics.cell_width * metrics.scale).round()
                            as usize,
                        pixel_height: (rows as f32 * metrics.cell_height * metrics.scale).round()
                            as usize,
                        dpi: (96.0 * metrics.scale).round() as u32,
                    };
                    let previous = self.tabs.active_id();
                    let tab = live_tab_sized(
                        profile.clone(),
                        source,
                        size,
                        self.appearance.read().unwrap().interaction.osc52,
                        None,
                        None,
                    )
                    .with_profile(Some(profile));
                    let pane = tab.pane_id().expect("new terminal has a pane ID");
                    self.tabs.insert(tab);
                    if !activate && let Some(previous) = previous {
                        self.tabs.select(previous);
                    }
                    self.sync_selection(context);
                    if activate || previous.is_none() {
                        if explicit_grid {
                            self.initial_grid.set(Some((cols, rows)));
                            self.windows.borrow_mut().grid = Some((cols, rows));
                        }
                        self.activate(context);
                    } else {
                        self.publish_panes();
                    }
                    Ok(
                        serde_json::json!({"pane":pane,"cols":cols,"rows":rows,"active":activate||previous.is_none()}),
                    )
                })();
                let _ = reply.send(result.map_err(|e| format!("{e:#}")));
            }
            Message::Control(w11term::control::UiRequest::Resize {
                pane,
                cols,
                rows,
                reply,
            }) => {
                let Some(tab_id) = self.tab_for_pane(pane) else {
                    let _ = reply.send(Err(format!("unknown terminal pane {pane}")));
                    return;
                };
                let split = self.tabs.visible(tab_id)
                    && matches!(
                        self.tabs.active_layout(),
                        Some(w11term::pane_layout::Layout::Split { .. })
                    );
                if split && self.tabs.zoomed() {
                    let _ =
                        reply.send(Err("Unzoom the pane before resizing its split grid".into()));
                    return;
                }
                if !split
                    && self.tabs.active_id() == Some(tab_id)
                    && self.windows.borrow().fullscreen()
                {
                    let _ = reply.send(Err(
                        "Exit fullscreen before resizing the active window grid".into(),
                    ));
                    return;
                }
                if split {
                    if let Err(error) = self.resize_split(tab_id, cols, rows) {
                        let _ = reply.send(Err(error.to_string()));
                        return;
                    }
                } else if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get(tab_id)
                {
                    let metrics = self.metrics.get();
                    let size = TerminalSize {
                        cols,
                        rows,
                        pixel_width: (cols as f32 * metrics.cell_width * metrics.scale).round()
                            as usize,
                        pixel_height: (rows as f32 * metrics.cell_height * metrics.scale).round()
                            as usize,
                        dpi: (96.0 * metrics.scale).round() as u32,
                    };
                    let _ = tab.session.input.send(Command::Resize(size));
                    if self.tabs.active_id() == Some(tab_id) {
                        self.initial_grid.set(Some((cols, rows)));
                        self.windows.borrow_mut().grid = Some((cols, rows));
                        self.client_size.0 +=
                            (cols as f64 - metrics.cols as f64) * metrics.cell_width as f64;
                        self.client_size.1 +=
                            (rows as f64 - metrics.rows as f64) * metrics.cell_height as f64;
                        self.invalidator.invalidate();
                    }
                } else {
                    let _ = reply.send(Err(format!("unknown terminal pane {pane}")));
                    return;
                }
                context.spawn_background(move |_| {
                    std::thread::sleep(Duration::from_millis(300));
                    Message::ControlResizeDone {
                        pane,
                        cols,
                        rows,
                        reply,
                    }
                });
            }
            Message::ControlResizeDone {
                pane,
                cols,
                rows,
                reply,
            } => {
                let result = if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tab_for_pane(pane).and_then(|id| self.tabs.get(id))
                {
                    let frame = tab.session.frames.load();
                    if frame.cols == cols && frame.rows.len() == rows {
                        Ok(serde_json::json!({"pane":pane,"cols":cols,"rows":rows}))
                    } else {
                        self.initial_grid.set(None);
                        Err(format!(
                            "requested grid does not fit the current window/display; actual {}x{}",
                            frame.cols,
                            frame.rows.len()
                        ))
                    }
                } else {
                    Err("pane was closed".into())
                };
                let _ = reply.send(result);
            }
            Message::PromptEdit(id, prompt, index, value) => {
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get_mut(id)
                    && tab.prompt_id == Some(prompt)
                    && value.len() <= 4096
                    && let Some(answer) = tab.answers.get_mut(index)
                {
                    *answer = zeroize::Zeroizing::new(value);
                }
            }
            Message::PromptReply(id, prompt, result) => {
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get_mut(id)
                    && tab.prompt_id == Some(prompt)
                {
                    let question = tab.session.prompts.current();
                    let answer = if result == ContentDialogResult::Primary {
                        match question.map(|p| p.question) {
                            Some(w11term::prompts::Question::HostKey { .. }) => {
                                w11term::prompts::Answer::Trust(true)
                            }
                            _ => w11term::prompts::Answer::Authentication(std::mem::take(
                                &mut tab.answers,
                            )),
                        }
                    } else {
                        w11term::prompts::Answer::Cancel
                    };
                    let _ = tab.session.prompts.answer(prompt, answer);
                    tab.answers.clear();
                }
            }
            Message::GenerateKey(id) => {
                if let Some(AppTab {
                    page: Page::NewProfile(draft),
                    ..
                }) = self.tabs.get_mut(id)
                    && !draft.generating
                {
                    draft.generating = true;
                    draft.error = None;
                    let name = draft.name.clone();
                    context.spawn_background(move |_| {
                        Message::KeyGenerated(
                            id,
                            w11term::keys::generate(&name).map_err(|e| format!("{e:#}")),
                        )
                    });
                }
            }
            Message::KeyGenerated(id, result) => {
                if let Some(AppTab {
                    page: Page::NewProfile(draft),
                    ..
                }) = self.tabs.get_mut(id)
                {
                    draft.generating = false;
                    match result {
                        Ok(key) => {
                            draft.identity = key.path.to_string_lossy().into_owned();
                            draft.player.device_key = Some(key.path.clone());
                            draft.saved = false;
                            draft.authentication = if draft.player.rustmush { 3 } else { 1 };
                            if let Err(e) = w11term::clipboard::write(&key.public_line) {
                                draft.error =
                                    Some(format!("Public key generated; clipboard: {e:#}"));
                            }
                            draft.device = Some(key);
                        }
                        Err(e) => draft.error = Some(e),
                    }
                }
            }
            Message::CopyPublicKey(id) => {
                if let Some(AppTab {
                    page: Page::NewProfile(draft),
                    ..
                }) = self.tabs.get(id)
                    && let Some(key) = &draft.device
                    && let Err(e) = w11term::clipboard::write(&key.public_line)
                {
                    log::warn!("Copying public key: {e:#}");
                }
            }
            Message::DraftPlayer(id, field, value) => {
                if let Some(AppTab {
                    page: Page::NewProfile(draft),
                    ..
                }) = self.tabs.get_mut(id)
                {
                    let before = draft.player.clone();
                    match field.as_str() {
                        "rustmush" => {
                            let changed = draft.player.rustmush != value;
                            draft.player.rustmush = value;
                            if value && changed {
                                draft.player.game_mode = true;
                                draft.authentication = 3;
                            }
                        }
                        "game_mode" => draft.player.game_mode = value,
                        "alternate_scroll" => {
                            // Native property reconciliation can emit the same
                            // value; that must not turn automatic policy into an override.
                            if draft.player.alternate_scroll_enabled() != value {
                                draft.player.alternate_scroll = Some(value);
                            }
                        }
                        "remote_sounds" => {
                            if draft.player.remote_sounds_enabled() != value {
                                draft.player.remote_sounds = Some(value);
                            }
                        }
                        "sync_assets" => {
                            if draft.player.sync_assets_enabled() != value {
                                draft.player.sync_assets = Some(value);
                            }
                        }
                        "reconnect" => draft.player.reconnect = value,
                        "mute" => draft.player.mute = value,
                        _ => {}
                    }
                    if draft.player != before {
                        draft.saved = false;
                    }
                }
            }
            Message::Durability(action) => self.durability_action(action),
            Message::Discovery(action) => self.discovery_action(action, context),
            #[cfg(feature = "ui-smoke")]
            Message::Smoke(step) => self.smoke_step(step, context),
            Message::FrameReady(id, epoch) => {
                if self.tabs.visible(id) && self.watch_epoch == epoch {
                    self.frame_tasks.remove(&id);
                    let active = self.tabs.active_id() == Some(id);
                    if let Some(AppTab {
                        page: Page::Terminal(tab),
                        ..
                    }) = self.tabs.get_mut(id)
                    {
                        if let Some(inspector) = &mut self.inspection
                            && inspector.pane == tab.session.pane_id
                        {
                            inspector.refresh(tab.session.frames.inspection());
                        }
                        let frame = tab.session.frames.load();
                        if frame.generation == tab.observed_frame {
                            self.watch_frames(context);
                            return;
                        }
                        tab.observed_frame = frame.generation;
                        if let Some(inspector) = &mut self.inspection
                            && inspector.pane == tab.session.pane_id
                        {
                            inspector.refresh_pointer(
                                tab.surface.metrics.get(),
                                &frame,
                                self.appearance.read().unwrap().interaction.detect_urls,
                            );
                        }
                        tab.error = None;
                        tab.files.observe_connection(frame.connection_generation);
                        if active {
                            tab.follow_shell_directory(&frame, context);
                        }
                        let unavailable = frame.copy_output_unavailable();
                        if tab.copy_output_unavailable != unavailable {
                            tab.copy_output_unavailable = unavailable;
                            self.header.borrow_mut().take();
                        }
                        let phase = tab.session.durable_state().map(|s| s.phase);
                        if tab.durable_phase != phase {
                            tab.durable_phase = phase;
                            tab.confirm_end = false;
                            self.header.borrow_mut().take();
                        }
                    }
                    if active {
                        self.refresh_copy_mode(id);
                        self.connection_ui(id);
                    }
                    if let Some(AppTab {
                        page: Page::Terminal(tab),
                        ..
                    }) = self.tabs.get(id)
                    {
                        let frame = tab.session.frames.load();
                        if tab
                            .selection
                            .borrow()
                            .as_ref()
                            .is_some_and(|s| s.alternate != frame.alternate || s.cols != frame.cols)
                        {
                            tab.selection.borrow_mut().take();
                            if self.tabs.active_id() == Some(id) {
                                self.keyboard.set_selection(false);
                            }
                        }
                        if let Some(selection) =
                            tab.selection.borrow_mut().as_mut().filter(|s| s.dragging)
                        {
                            selection.observe(&frame);
                            if tab.scroll_direction != 0 {
                                let row = if tab.scroll_direction > 0 {
                                    frame.first_stable_row
                                } else {
                                    frame.first_stable_row
                                        + frame.rows.len().saturating_sub(1) as isize
                                };
                                selection.update(Point {
                                    row,
                                    col: selection.end.col,
                                });
                            }
                        }
                        if self.hints.borrow().as_ref().is_none_or(|h| h.pane != id) {
                            tab.surface.invalidator.invalidate();
                        }
                    }
                    self.watch_frames(context);
                }
            }
            Message::CloseWindow => {
                if let Some(name) = &self.window_profile {
                    let _ = self.windows.borrow_mut().remember(name);
                }
                let _ = context.window().request_close();
            }
            Message::PaneFocus(id) => self.focus_pane(id, context),
            Message::SetPaneReadOnly(enabled) => {
                if let Some(id) = self.tabs.active_id()
                    && let Some(AppTab {
                        page: Page::Terminal(live),
                        ..
                    }) = self.tabs.get_mut(id)
                    && matches!(live.source, Source::Ssh(_) | Source::Local(_))
                {
                    KeyboardBridge::set_read_only(&live.session.input, &live.read_only, enabled);
                    live.remote_mouse = RemoteMouse::default();
                    live.surface.invalidator.invalidate();
                    self.publish_panes();
                }
            }
            Message::PaneRatio(epoch, path, ratio) => {
                if epoch == self.watch_epoch && self.dragging_divider(epoch, &path) {
                    self.tabs.set_ratio(&path, ratio);
                }
            }
            Message::PaneNeighbor(direction) => {
                if self.tabs.focus_neighbor(direction) {
                    self.activate(context);
                }
            }
            Message::ZoomPane => {
                if self.tabs.toggle_zoom() {
                    self.activate(context);
                }
            }
            Message::MovePaneToTab => {
                if self.tabs.move_active_to_new_tab() {
                    self.sync_selection(context);
                    self.activate(context);
                }
            }
            Message::ClosePane => {
                if let Some(id) = self.tabs.active_id() {
                    if self.tabs.active_layout().is_some_and(|layout| {
                        matches!(layout, w11term::pane_layout::Layout::Split { .. })
                    }) {
                        self.keyboard.set_target(None);
                        self.tabs.remove(id);
                        self.sync_selection(context);
                        self.activate(context);
                    } else {
                        self.close_tab(id, context);
                    }
                }
            }
            Message::SplitPane(axis, profile) => self.split_pane(axis, profile, context),
            Message::SyncSelection(epoch) => {
                if self.selection_epoch == epoch {
                    self.selection_pending = false;
                }
            }
            Message::Select(Some(index), epoch, pending) => {
                if pending || self.selection_pending || epoch != self.selection_epoch {
                    return;
                }
                if let Some(id) = self.tabs.headers().get(index).map(|t| t.id)
                    && self.tabs.active_id() != Some(id)
                {
                    self.tabs.select(id);
                    self.activate(context);
                }
            }
            Message::Select(None, _, _) => {}
            Message::CloseTab(id) => {
                if let Ok(id) = id.parse() {
                    self.close_tab(id, context);
                }
            }
            Message::Reorder(order, selected) => {
                if let Some(ids) = order
                    .iter()
                    .map(|id| id.parse().ok())
                    .collect::<Option<Vec<_>>>()
                    && self.tabs.reorder(&ids)
                {
                    let changed = self.tabs.active_id() != selected;
                    if let Some(id) = selected {
                        self.tabs.select(id);
                    }
                    self.sync_selection(context);
                    if changed {
                        self.activate(context);
                    }
                }
            }
            Message::Shortcut(shortcut) => match shortcut {
                Shortcut::CommandPalette => self.open_command_palette(context),
                Shortcut::PanePicker => self.open_picker(context),
                Shortcut::MovePaneToExistingTab => self.open_move_picker(context),
                Shortcut::PickerPlace(side) => {
                    if let Some((source, target)) =
                        self.picker.as_ref().and_then(|picker| picker.placement())
                    {
                        self.place_pane(source, target, side, context);
                    }
                }
                Shortcut::CopyMode => self.open_copy_mode(),
                Shortcut::CopyKey(key) => self.copy_key(key, context),
                Shortcut::KeyboardHints => self.open_hints(),
                Shortcut::HintKey(key) => self.hint_key(key, context),
                Shortcut::PickerAccept => self.accept_picker(context),
                Shortcut::PickerCancel => self.close_picker(context),
                Shortcut::PickerMove(delta) => {
                    if let Some(picker) = &mut self.picker {
                        picker.move_by(delta);
                        self.sync_picker_preview(context);
                    }
                }
                Shortcut::SplitRight => self.split_pane(Axis::Columns, None, context),
                Shortcut::ResizePane(direction) => self.keyboard_resize(direction),
                Shortcut::SplitBelow => self.split_pane(Axis::Rows, None, context),
                Shortcut::FocusPane(direction) => {
                    self.update(Message::PaneNeighbor(direction), context)
                }
                Shortcut::ZoomPane => self.update(Message::ZoomPane, context),
                Shortcut::ClosePane => self.update(Message::ClosePane, context),
                Shortcut::MovePane => self.update(Message::MovePaneToTab, context),
                Shortcut::Workspaces => self.update(
                    Message::Workspace(workspace_view::WorkspaceAction::Show),
                    context,
                ),
                Shortcut::Fullscreen => {
                    if let Err(error) = self.windows.borrow_mut().toggle() {
                        log::warn!("Fullscreen: {error:#}");
                    }
                    self.invalidator.invalidate();
                }
                Shortcut::BeginResize => {
                    self.initial_grid.set(None);
                }
                Shortcut::TaskbarCreated => {
                    self.taskbar.borrow_mut().shell_restarted();
                }
                Shortcut::RememberWindow => {
                    if let Some(name) = &self.window_profile {
                        let _ = self.windows.borrow_mut().remember(name);
                    }
                }
                Shortcut::Find => self.find(false, None, context),
                Shortcut::FindNext => self.find(false, Some(1), context),
                Shortcut::FindPrevious => self.find(false, Some(-1), context),
                Shortcut::FindClose => self.find(true, None, context),
                Shortcut::NextTab | Shortcut::PreviousTab => {
                    self.switch_tabs(
                        if matches!(shortcut, Shortcut::NextTab) {
                            1
                        } else {
                            -1
                        },
                        context,
                    );
                }
                Shortcut::Copy => self.copy(),
                Shortcut::Paste => self.paste(false, context),
                Shortcut::PasteRaw => self.paste(true, context),
                Shortcut::ClearSelection => self.clear_selection(),
                Shortcut::Zoom(delta) => {
                    let mut appearance = self.appearance.read().unwrap().clone();
                    appearance.font_size = (appearance.font_size + delta as f32).clamp(6.0, 72.0);
                    self.apply_appearance(appearance);
                }
                Shortcut::ResetZoom => {
                    let mut appearance = self.appearance.read().unwrap().clone();
                    appearance.font_size = 16.0;
                    self.apply_appearance(appearance);
                }
                Shortcut::CommitTab => self.finish_switcher(true, context),
                Shortcut::CancelTab => self.finish_switcher(false, context),
                Shortcut::NewTab => self.open(
                    self.default_label.clone(),
                    self.default_profile
                        .as_ref()
                        .and_then(|name| self.catalog.source(name))
                        .unwrap_or_else(|| self.default_source.clone()),
                    self.default_profile.clone(),
                    context,
                ),
                Shortcut::DuplicateTabLayout => {
                    if let Some(group) = self.tabs.active_group_id() {
                        self.update(Message::DuplicateTabLayout(group), context);
                    }
                }
                Shortcut::CloseTab => {
                    if let Some(id) = self.tabs.active_id() {
                        self.close_tab(id, context);
                    }
                }
                Shortcut::Select(index) => {
                    if let Some(id) = self.tabs.headers().get(index).map(|t| t.id) {
                        self.tabs.select(id);
                        self.activate(context);
                    }
                }
            },
            Message::ProfileMenu(label) => {
                if label == NEW_PROFILE {
                    self.utility(false, context);
                } else if label == LAB {
                    self.open(LAB.into(), Source::Demo, None, context);
                } else if let Some((name, profile)) = self
                    .catalog
                    .profiles
                    .iter()
                    .find(|(name, ssh)| profile_label(name, ssh) == label)
                    .map(|(n, p)| (n.clone(), p.clone()))
                {
                    self.open(name.clone(), Source::Ssh(profile), Some(name), context);
                } else if let Some(name) = label.strip_suffix(" · Local")
                    && let Some(shell) = self.catalog.local.get(name)
                {
                    self.open(
                        name.into(),
                        Source::Local(shell.clone()),
                        Some(name.into()),
                        context,
                    );
                }
            }
            Message::EditProfile(name) => self.edit_profile(name, context),
            Message::DuplicateProfile(id) => self.duplicate_profile(id, context),
            Message::DeleteProfile(id) => {
                if let Some(AppTab {
                    page: Page::NewProfile(draft),
                    ..
                }) = self.tabs.get_mut(id)
                    && draft.origin.editing().is_some()
                {
                    draft.confirm_delete = true;
                }
            }
            Message::ConfirmDeleteProfile(id, accept) => {
                self.confirm_delete_profile(id, accept, context)
            }
            Message::EditActiveProfile => {
                if let Some(name) = self
                    .tabs
                    .active()
                    .and_then(AppTab::profile_name)
                    .map(str::to_owned)
                {
                    if self.catalog.profiles.contains_key(&name) {
                        self.edit_profile(name, context);
                    } else if self.catalog.local.contains_key(&name) {
                        self.local_profile_action(local_profile::Action::Open(Some(name)), context);
                    }
                }
            }
            Message::Keyboard(edit) => self.edit_keyboard(edit),
            Message::Patterns(edit) => self.edit_patterns(edit),
            Message::RefreshProfiles => {
                self.refresh_profiles();
                for entry in self.tabs.entries() {
                    if let Page::Terminal(live) = &entry.value.page
                        && let Some(shares) = &live.folder_sharing
                    {
                        shares.refresh_profile();
                    }
                }
            }
            Message::MoveProfile(name, delta, order) => {
                match self.store.move_profile(&name, delta, &order) {
                    Ok(catalog) => {
                        self.catalog = catalog;
                        self.profile_error = None;
                        self.profile_move_error = None;
                    }
                    Err(error) => {
                        self.refresh_profiles();
                        self.profile_move_error = Some(format!(
                            "Could not move {name}: {error:#}. Review the current order and try again."
                        ));
                    }
                }
            }
            Message::SettingsMenu => {
                self.refresh_profiles();
                self.utility(true, context);
            }
            Message::LocalProfile(action) => self.local_profile_action(action, context),
            Message::Workspace(action) => self.workspace_action(action, context),
            Message::ImportConfig(action) => self.import_action(action, context),
            Message::Paste => self.paste(false, context),
            Message::PasteRaw => self.paste(true, context),
            Message::CopyResult(pane, request, result) => {
                self.finish_keyboard_copy(pane, request, result)
            }
            Message::RawPaste(result) => {
                if let Err(error) = result {
                    log::warn!("Raw paste: {error}");
                }
            }
            Message::Interaction(field, value) => {
                let mut appearance = self.appearance.read().unwrap().clone();
                let setting = match field {
                    InteractionField::CopyOnSelect => &mut appearance.interaction.copy_on_select,
                    InteractionField::TrimBlock => &mut appearance.interaction.trim_block_selection,
                    InteractionField::TrimPaste => &mut appearance.interaction.trim_paste,
                    InteractionField::Snap => &mut appearance.interaction.snap_to_grid_on_resize,
                    InteractionField::ScrollZoom => &mut appearance.interaction.scroll_to_zoom,
                    InteractionField::DetectUrls => &mut appearance.interaction.detect_urls,
                    InteractionField::Osc52 => &mut appearance.interaction.osc52,
                };
                if *setting != value {
                    *setting = value;
                    self.save_preferences(appearance);
                    self.invalidator.invalidate();
                }
            }
            Message::TabMode(Some(index)) if index < 3 => {
                let mut appearance = self.appearance.read().unwrap().clone();
                let mode = [
                    TabSwitcherMode::InOrder,
                    TabSwitcherMode::Mru,
                    TabSwitcherMode::Disabled,
                ][index];
                if mode != appearance.interaction.tab_switcher_mode {
                    appearance.interaction.tab_switcher_mode = mode;
                    self.save_preferences(appearance);
                    self.finish_switcher(false, context);
                }
            }
            Message::TabMode(_) => {}
            Message::PickTab(id) => {
                self.finish_switcher(false, context);
                self.tabs.select(id);
                self.activate(context);
            }
            Message::CaptureLost(id) => {
                // Reactor releases capture before queuing PointerReleased. Defer cancellation
                // so that normal releases still copy the selection or open a clicked URL.
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get(id)
                {
                    let gesture = tab.gesture;
                    context.spawn_background(move |_| {
                        std::thread::sleep(Duration::from_millis(10));
                        Message::FinalizeCapture(id, gesture)
                    });
                }
            }
            Message::FinalizeCapture(id, gesture) => {
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get_mut(id)
                    && tab.gesture == gesture
                {
                    tab.end_pointer_gesture();
                }
            }
            Message::DividerPress(epoch, path, coordinate) => {
                if epoch == self.watch_epoch {
                    self.begin_divider(&path, coordinate);
                }
            }
            Message::DividerEnd(epoch, path) => {
                if self.dragging_divider(epoch, &path) {
                    self.divider_drag.borrow_mut().take();
                }
            }
            Message::HoverPane(id) => {
                self.keyboard.hover_pane(id);
                if self.tabs.active_id() != Some(id)
                    && let Some(inspector) = &mut self.inspection
                {
                    inspector.clear_pointer();
                }
            }
            Message::PointerExit(id) => {
                self.keyboard.leave_pane(id);
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get(id)
                {
                    if let Some(inspector) = &mut self.inspection
                        && inspector.pane == tab.session.pane_id
                    {
                        inspector.clear_pointer();
                    }
                    tab.hover.borrow_mut().take();
                    tab.surface.invalidator.invalidate();
                }
            }
            Message::DragScroll(id) => {
                let active = self.tabs.active_id() == Some(id);
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get_mut(id)
                {
                    tab.scroll_tick = false;
                    if active
                        && tab.scroll_direction != 0
                        && tab.selection.borrow().as_ref().is_some_and(|s| s.dragging)
                    {
                        let _ = tab
                            .session
                            .input
                            .send(Command::Scroll(tab.scroll_direction));
                        tab.scroll_tick = true;
                        context.spawn_background(move |_| {
                            std::thread::sleep(Duration::from_millis(40));
                            Message::DragScroll(id)
                        });
                    }
                }
            }
            Message::Font(Some(index)) => {
                if let Some(font) = self.fonts.get(index) {
                    let mut appearance = self.appearance.read().unwrap().clone();
                    if appearance.font_family != *font {
                        appearance.font_family = font.clone();
                        self.apply_appearance(appearance);
                    }
                }
            }
            Message::Font(None) => {}
            Message::Ligatures(enabled) => {
                let mut appearance = self.appearance.read().unwrap().clone();
                if appearance.ligatures != enabled {
                    appearance.ligatures = enabled;
                    self.apply_appearance(appearance);
                }
            }
            Message::WindowSize(size) => {
                self.width = size.width;
                self.window_height = size.height;
                if !self.window_restored {
                    self.window_restored = true;
                    context.spawn_background(|_| {
                        std::thread::sleep(Duration::from_millis(200));
                        Message::RestoreWindow
                    });
                }
            }
            Message::Geometry(width, height, epoch, first) => {
                if self.initial_grid.epoch() == epoch {
                    self.client_size = (width, height);
                    if first {
                        context.spawn_background_with_rejection(
                            move |_| {
                                std::thread::sleep(Duration::from_millis(300));
                                Message::GridFallback(epoch)
                            },
                            Message::GridFallback(epoch),
                        );
                    }
                }
            }
            Message::GridFallback(epoch) => {
                if self.initial_grid.epoch() == epoch && self.initial_grid.get().is_some() {
                    self.initial_grid.set(None);
                    self.invalidator.invalidate();
                }
            }
            Message::RenderError(id, error) => {
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get_mut(id)
                {
                    log::error!("Renderer: {error}");
                    tab.error = Some(error);
                }
            }
            Message::RetryRender(id) => {
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get_mut(id)
                {
                    tab.error = None;
                    self.invalidator.invalidate();
                }
            }
            Message::Edit(id, field, text) => {
                if let Some(AppTab {
                    page: Page::NewProfile(draft),
                    ..
                }) = self.tabs.get_mut(id)
                {
                    let target = match field {
                        Field::Name => &mut draft.name,
                        Field::Host => &mut draft.host,
                        Field::User => &mut draft.user,
                        Field::Port => &mut draft.port,
                        Field::CustomTerminal => &mut draft.custom_terminal,
                        Field::Identity => &mut draft.identity,
                        Field::Fingerprint => &mut draft.fingerprint,
                    };
                    let changed = *target != text;
                    *target = text;
                    if changed {
                        draft.saved = false;
                        draft.error = None;
                    }
                }
            }
            Message::Auth(id, Some(index)) => {
                if let Some(AppTab {
                    page: Page::NewProfile(draft),
                    ..
                }) = self.tabs.get_mut(id)
                {
                    if draft.authentication != index {
                        draft.saved = false;
                    }
                    draft.authentication = index;
                }
            }
            Message::Auth(_, None) => {}
            Message::TerminalPreset(id, selected) => {
                if let Some(AppTab {
                    page: Page::NewProfile(draft),
                    ..
                }) = self.tabs.get_mut(id)
                    && let Some(index @ 0..=2) = selected
                    && index != draft.terminal_preset
                {
                    draft.terminal_preset = index;
                    draft.saved = false;
                    draft.error = None;
                }
            }
            Message::SaveProfile(id) => self.save_profile(id, context),
            Message::SaveProfileOnly(id) => self.save_profile_action(id, false, context),
            Message::Pointer(id, pointer, kind) => self.pointer(id, pointer, kind, context),
            Message::FindEdit(id, mut text) => {
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get_mut(id)
                    && tab.find.open
                {
                    Find::limit_query(&mut text, tab.find.regex);
                    if tab.find.query == text {
                        return;
                    }
                    tab.find.query = text;
                    tab.find.pending = true;
                    tab.find.epoch += 1;
                    let epoch = tab.find.epoch;
                    context.spawn_background(move |_| {
                        std::thread::sleep(Duration::from_millis(120));
                        Message::FindApply(id, epoch)
                    });
                }
            }
            Message::FindOption(id, option, enabled) => {
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get_mut(id)
                    && tab.find.open
                {
                    let flag = match option {
                        FindOption::Case => &mut tab.find.sensitive,
                        FindOption::Regex => &mut tab.find.regex,
                    };
                    if *flag == enabled {
                        return;
                    }
                    *flag = enabled;
                    Find::limit_query(&mut tab.find.query, tab.find.regex);
                    tab.find.epoch += 1;
                    tab.find.apply(&tab.session);
                }
            }
            Message::FindApply(id, epoch) => self.apply_find(id, epoch),
            Message::FindFocus(id, epoch, attempt) => {
                if self.picker.is_some() {
                    return;
                }
                let sender = context.sender();
                if self.tabs.active_id() == Some(id)
                    && let Some(AppTab {
                        page: Page::Terminal(tab),
                        ..
                    }) = self.tabs.get(id)
                    && tab.find.open
                    && tab.find.epoch == epoch
                    && !tab.find.input.request_focus_result(move |result| {
                        sender.send(Message::FindFocusResult(id, epoch, result.unwrap_or(false)));
                    })
                    && attempt < 3
                {
                    context.spawn_background(move |_| {
                        std::thread::sleep(Duration::from_millis(30));
                        Message::FindFocus(id, epoch, attempt + 1)
                    });
                }
            }
            Message::FindFocusResult(id, epoch, focused) => {
                if self.tabs.active_id() == Some(id)
                    && let Some(AppTab {
                        page: Page::Terminal(tab),
                        ..
                    }) = self.tabs.get_mut(id)
                    && tab.find.epoch == epoch
                    && tab.find.open
                {
                    tab.find.focused = focused;
                }
            }
        }
    }

    fn view(&self, _input: &Self::Input, context: &mut ViewContext<Self>) -> View {
        let _native = self.keyboard.scope().enter();
        if let Some(AppTab {
            label,
            page: Page::Terminal(tab),
        }) = self.tabs.active()
            && tab.prompt_id.is_none()
            && !tab.find.open
            && !tab.browsing_sessions()
            && self.switcher.is_none()
            && self.picker.is_none()
            && self.hints.borrow().is_none()
            && self.title_editor.is_none()
            && let (Some(parent), Some(bounds)) = (crate::input::window(), self.focused_pane_rect())
        {
            let marked = self
                .copy_mode
                .borrow()
                .as_ref()
                .and_then(|m| m.state.selection());
            let metrics = tab.surface.metrics.get();
            let sender = context.sender();
            self.accessible.update(
                parent,
                crate::accessibility::Pane {
                    id: tab.session.pane_id,
                    label,
                    frames: &tab.session.frames,
                    geometry: crate::accessibility::Geometry {
                        bounds,
                        cell_width: f64::from(metrics.cell_width * metrics.scale),
                        cell_height: f64::from(metrics.cell_height * metrics.scale),
                        padding: f64::from(PADDING * metrics.scale),
                    },
                    selection: marked.as_ref().or(tab.selection.borrow().as_ref()),
                },
                move |request| {
                    sender.send(Message::AccessibleSelection(request));
                },
            );
        } else {
            self.accessible.clear();
        }
        let title = self
            .tabs
            .active_id()
            .map_or("Terminal", |id| self.tab_label(id));
        if self.keyboard.hwnd().is_some() {
            context.window_title(format!("w11term · {title}"));
        } else {
            context.window_title(self.keyboard.bootstrap_title());
        }
        context.window_visuals(
            WindowVisuals::new()
                .client_size(self.client_size.0, self.client_size.1)
                .theme(WindowTheme::Dark)
                .constraints(WindowConstraints {
                    min_width: Some(640.0_f64.min(self.client_size.0)),
                    min_height: Some(360.0_f64.min(self.client_size.1)),
                    ..Default::default()
                }),
        );
        context.on_window_size(context.callback(Message::WindowSize));
        let cached_header = self.header.borrow().clone();
        let header = cached_header.unwrap_or_else(|| {
            let header = self.header_view(context);
            *self.header.borrow_mut() = Some(header.clone());
            header
        });
        let content = if let Some(id) = self.tabs.active_id() {
            match &self.tabs.get(id).unwrap().page {
                Page::Terminal(tab) => match self.tabs.active_layout() {
                    Some(layout) if !self.tabs.zoomed() => {
                        self.layout_view(layout, &mut Vec::new(), context)
                    }
                    _ => self.pane_view(id, tab, false, context),
                },
                Page::Settings(None) => self.settings_view(context),
                Page::Attention => self.attention_view(context),
                Page::CommandHistory(page) => self.history_view(page, context),
                Page::SharedFolders(page) => self.folder_panel_view(page, context),
                Page::Settings(Some(SettingsEditor::Keyboard(page))) => {
                    keyboard_settings::keyboard_view(
                        page,
                        &self.appearance.read().unwrap().shortcuts,
                        context,
                    )
                }
                Page::Settings(Some(SettingsEditor::Patterns(page))) => pattern_settings::view(
                    page,
                    &self.appearance.read().unwrap().hint_patterns,
                    context,
                ),
                Page::LocalProfile(draft) => self.local_profile_view(id, draft, context),
                Page::Workspaces(page) => self.workspace_view(id, page, context),
                Page::ImportConfig(page) => self.import_view(id, page, context),
                Page::NewProfile(draft) => self.profile_form(id, draft, context),
            }
        } else if self.incoming.is_some() || self.outgoing.is_some() {
            TextBlock::new().text("Moving tab…").margin(16.0).into()
        } else {
            Border::new().into()
        };
        let content = self.with_file_drawer(content, context);
        let mut views = vec![
            KeyedView::new(
                "header",
                Border::new()
                    .background(Color::rgb(35, 35, 35))
                    .content(header),
            ),
            KeyedView::new("content", Border::new().grid_row(1).content(content)),
        ];
        if let Some(message) = &self.window_error {
            views.push(KeyedView::new(
                "window-error",
                Button::new()
                    .grid_row(1)
                    .vertical_alignment(VerticalAlignment::Top)
                    .on_click(context.callback(|_| Message::DismissWindowError))
                    .content(format!("{message}  ·  Dismiss")),
            ));
        }
        if let Some(message) = &self.export.status {
            views.push(KeyedView::new(
                "scrollback-export-status",
                Button::new()
                    .grid_row(1)
                    .automation_name(message)
                    .horizontal_alignment(HorizontalAlignment::Right)
                    .vertical_alignment(VerticalAlignment::Bottom)
                    .on_click(
                        context.callback(|_| Message::Export(scrollback_export::Action::Dismiss)),
                    )
                    .content(format!("{message}  ·  Dismiss")),
            ));
        }
        if let Some(inspection) = &self.inspection
            && self.hints.borrow().is_none()
            && self.copy_mode.borrow().is_none()
        {
            views.push(KeyedView::new(
                "terminal-inspector",
                inspection.view(context),
            ));
        }
        if let Some(id) = self.tabs.active_id()
            && let Some(AppTab {
                page: Page::Terminal(tab),
                ..
            }) = self.tabs.get(id)
            && tab.find.open
        {
            views.push(KeyedView::new("find", self.find_view(id, tab, context)));
        }
        if let Some(switcher) = &self.switcher {
            let choices: Vec<_> = switcher
                .ids
                .iter()
                .enumerate()
                .filter_map(|(index, id)| {
                    self.tabs
                        .get(*id)
                        .map(|_| (index, *id, self.tab_label(*id).to_owned()))
                })
                .map(|(index, id, label)| {
                    KeyedView::new(
                        id.to_string(),
                        Button::new()
                            .style(if index == switcher.index {
                                ButtonStyle::Accent
                            } else {
                                ButtonStyle::Subtle
                            })
                            .horizontal_alignment(HorizontalAlignment::Stretch)
                            .on_click(context.callback(move |_| Message::PickTab(id)))
                            .content(TextBlock::new().text(label)),
                    )
                })
                .collect();
            views.push(KeyedView::new(
                "switcher",
                Border::new()
                    .grid_row(1)
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .vertical_alignment(VerticalAlignment::Top)
                    .margin(24.0)
                    .padding(12.0)
                    .corner_radius(8.0)
                    .background(Color::rgb(40, 42, 48))
                    .content(
                        StackPanel::new()
                            .min_width(320.0)
                            .spacing(6.0)
                            .keyed_children(choices),
                    ),
            ));
        }
        if let Some(picker) = &self.picker {
            views.push(KeyedView::new("pane-picker", picker.view(self, context)));
        }
        if let Some(editor) = &self.title_editor {
            views.push(KeyedView::new("rename-tab", editor.view(context)));
        }
        if let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.active()
            && self.title_editor.is_none()
            && tab.session.frames.load().prompt.is_none()
            && let Some(dialog) = tab.files.name_dialog(tab.session.pane_id, context)
        {
            views.push(KeyedView::new("sftp-name-editor", dialog));
        }
        if let Some(id) = self.tabs.active_id()
            && let Some(AppTab {
                page: Page::Terminal(tab),
                ..
            }) = self.tabs.get(id)
            && let Some(prompt) = &tab.session.frames.load().prompt
        {
            views.push(KeyedView::new(
                format!("prompt-{}", prompt.id),
                self.prompt_view(id, tab, prompt, context),
            ));
        }
        if let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.active()
            && let Some(message) = &tab.device_message
        {
            views.push(KeyedView::new(
                "device-message",
                Border::new()
                    .grid_row(1)
                    .vertical_alignment(VerticalAlignment::Top)
                    .background(Color::rgb(40, 42, 48))
                    .padding(10.0)
                    .content(
                        TextBlock::new()
                            .text(message)
                            .text_wrapping(TextWrapping::Wrap),
                    ),
            ));
        }
        Grid::new()
            .rows([GridLength::Auto, GridLength::STAR])
            .background(Color::rgb(14, 16, 20))
            .keyed_children(views)
    }
}

#[cfg(feature = "ui-smoke")]
impl TerminalApp {
    fn smoke_step(&mut self, step: u8, context: &ComponentContext<Self>) {
        if std::env::var_os("W11TERM_DISCOVERY_SMOKE").is_some() {
            self.discovery_smoke_step(step, context);
            return;
        }
        if std::env::var_os("W11TERM_DURABLE_SMOKE").is_some() {
            self.durable_smoke_step(step, context);
            return;
        }
        if let Some(dir) = std::env::var_os("W11TERM_LOCAL_SMOKE") {
            self.local_smoke_step(step, std::path::Path::new(&dir), context);
            return;
        }
        if std::env::var_os("W11TERM_FILE_SMOKE").is_some() {
            self.file_smoke_step(step, context);
            return;
        }
        if std::env::var_os("W11TERM_IMPORT_SMOKE").is_some() {
            self.import_smoke_step(step, context);
            return;
        }
        if let Some(path) = std::env::var_os("W11TERM_UIA_SMOKE") {
            self.accessibility_smoke(step, std::path::Path::new(&path), context);
            return;
        }
        if std::env::var_os("W11TERM_IME_SMOKE").is_some() {
            self.ime_smoke_step(step, context);
            return;
        }
        if std::env::var_os("W11TERM_WORKSPACE_SMOKE").is_some() {
            self.workspace_smoke_step(step, context);
            return;
        }
        if std::env::var_os("W11TERM_SPLIT_SMOKE").is_some() {
            self.split_smoke_step(step, context);
            return;
        }
        if std::env::var_os("W11TERM_PROFILE_SMOKE").is_some() {
            self.profile_smoke_step(step, context);
            return;
        }
        if std::env::var_os("W11TERM_PLAYER_SMOKE").is_some() {
            self.player_smoke_step(step, context);
            return;
        }
        if std::env::var_os("W11TERM_SEARCH_SMOKE").is_some() {
            self.search_smoke_step(step, context);
            return;
        }
        let text = |tab: &AppTab| match &tab.page {
            Page::Terminal(tab) => tab
                .session
                .frames
                .load()
                .rows
                .iter()
                .map(|l| l.as_str().to_string())
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        match step {
            0 => {
                if let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page {
                    // Seed lab geometry before typing: native first-paint timing varies when
                    // the smoke window starts behind another window or a release build.
                    tab.session
                        .input
                        .send(Command::Resize(TerminalSize {
                            pixel_width: 800,
                            pixel_height: 600,
                            ..Default::default()
                        }))
                        .unwrap();
                    tab.session
                        .input
                        .send(Command::Text("tab-one".into()))
                        .unwrap();
                }
            }
            1 => self.open("Second lab".into(), Source::Demo, None, context),
            2 => {
                if let Page::Terminal(tab) = &self.tabs.get(2).unwrap().page {
                    tab.session
                        .input
                        .send(Command::Resize(TerminalSize {
                            pixel_width: 800,
                            pixel_height: 600,
                            ..Default::default()
                        }))
                        .unwrap();
                    tab.session
                        .input
                        .send(Command::Text("tab-two".into()))
                        .unwrap();
                }
            }
            3 => {
                assert!(text(self.tabs.get(1).unwrap()).contains("tab-one"));
                assert!(text(self.tabs.get(2).unwrap()).contains("tab-two"));
                self.tabs.select(1);
                self.activate(context);
                if std::env::var_os("W11TERM_INTERACTION_SMOKE").is_some() {
                    assert!(std::env::var_os("W11TERM_SETTINGS_PATH").is_some());
                    self.update(
                        Message::Interaction(InteractionField::CopyOnSelect, false),
                        context,
                    );
                    let Some(AppTab {
                        page: Page::Terminal(tab),
                        ..
                    }) = self.tabs.get(1)
                    else {
                        panic!()
                    };
                    let frame = tab.session.frames.load();
                    let (row, col) = frame
                        .rows
                        .iter()
                        .enumerate()
                        .find_map(|(row, line)| line.as_str().find("tab-one").map(|col| (row, col)))
                        .expect("text to select");
                    let metrics = self.metrics.get();
                    let pointer = |col| PointerEventInfo {
                        x: (PADDING + (col as f32 + 0.1) * metrics.cell_width) as f64,
                        y: (PADDING + (row as f32 + 0.1) * metrics.cell_height) as f64,
                        is_left_button_pressed: true,
                        ..Default::default()
                    };
                    self.pointer(1, pointer(col), MouseEventKind::Press, context);
                    self.pointer(1, pointer(col + 6), MouseEventKind::Move, context);
                    self.update(Message::CaptureLost(1), context);
                    self.pointer(
                        1,
                        PointerEventInfo {
                            is_left_button_pressed: false,
                            ..pointer(col + 6)
                        },
                        MouseEventKind::Release,
                        context,
                    );
                    let Some(AppTab {
                        page: Page::Terminal(tab),
                        ..
                    }) = self.tabs.get(1)
                    else {
                        panic!()
                    };
                    assert_eq!(
                        tab.selection.borrow().as_ref().unwrap().text(&frame, true),
                        "tab-one"
                    );
                    self.switch_tabs(1, context);
                    assert_eq!(self.tabs.active_id(), Some(1));
                    assert!(self.switcher.is_some());
                }
            }
            4 => {
                if std::env::var_os("W11TERM_INTERACTION_SMOKE").is_some() {
                    self.finish_switcher(false, context);
                    assert_eq!(self.tabs.active_id(), Some(1));
                    self.switch_tabs(1, context);
                    self.finish_switcher(true, context);
                    assert_eq!(self.tabs.active_id(), Some(2));
                    self.tabs.select(1);
                    self.activate(context);
                    self.update(Message::Shortcut(Shortcut::Zoom(2)), context);
                    assert_eq!(self.appearance.read().unwrap().font_size, 18.0);
                    self.update(Message::Shortcut(Shortcut::ResetZoom), context);
                }
                assert!(self.tabs.reorder(&[2, 1]));
                self.sync_selection(context);
                assert_eq!(self.tabs.active_id(), Some(1));
            }
            5 => {
                self.close_tab(2, context);
                assert_eq!(self.tabs.active_id(), Some(1));
            }
            6 => {
                self.utility(true, context);
                if std::env::var_os("W11TERM_FONT_SMOKE").is_some() {
                    assert!(
                        std::env::var_os("W11TERM_SETTINGS_PATH").is_some(),
                        "font smoke needs isolated settings"
                    );
                    let current = self.appearance.read().unwrap().font_family.clone();
                    let index = self
                        .fonts
                        .iter()
                        .position(|font| font == "Cascadia Code")
                        .or_else(|| self.fonts.iter().position(|font| font != &current))
                        .unwrap_or(0);
                    let selected = self
                        .fonts
                        .get(index)
                        .expect("font smoke needs an installed curated terminal font")
                        .clone();
                    self.update(Message::Font(Some(index)), context);
                    self.update(Message::Ligatures(false), context);
                    assert_eq!(self.appearance.read().unwrap().font_family, selected);
                    assert!(!self.appearance.read().unwrap().ligatures);
                    let saved = Appearance::load(&Appearance::path().unwrap()).unwrap();
                    assert_eq!(saved.font_family, selected);
                    assert!(!saved.ligatures);
                    assert!(
                        self.appearance_error.is_none(),
                        "{:?}",
                        self.appearance_error
                    );
                }
            }
            7 => self.utility(false, context),
            8 => {
                assert!(!self.keyboard.has_target());
                self.save_profile(4, context);
                let Page::NewProfile(draft) = &self.tabs.get(4).unwrap().page else {
                    panic!("missing form")
                };
                assert!(draft.error.is_some());
            }
            9 => self.close_tab(4, context),
            10 => self.close_tab(3, context),
            _ => {
                assert_eq!(self.tabs.active_id(), Some(1));
                assert!(text(self.tabs.get(1).unwrap()).contains("tab-one"));
                assert!(!text(self.tabs.get(1).unwrap()).contains("tab-two"));
                eprintln!(
                    "Tabbed UI smoke passed: independent buffers, switch, reorder, close, settings, profile form"
                );
                self.close_tab(1, context);
                return;
            }
        }
        if step == 7 && std::env::var_os("W11TERM_UI_SMOKE_PAUSE").is_some() {
            return;
        }
        context.spawn_background(move |_| {
            std::thread::sleep(std::time::Duration::from_millis(500));
            Message::Smoke(step + 1)
        });
    }
    fn profile_smoke_step(&mut self, step: u8, context: &ComponentContext<Self>) {
        eprintln!("Profile UI smoke step {step}");
        match step {
            0 => {
                assert!(std::env::var_os("W11TERM_DATA_DIR").is_some());
                assert_eq!(self.tabs.get(1).unwrap().ssh_profile_name(), Some("typo"));
                self.update(tab_menu_message("Mute: Off".into()), context);
                let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page else {
                    panic!()
                };
                assert!(tab.session.player.read().unwrap().mute);
                // Another connection may display the same label without belonging to that profile.
                self.open("typo".into(), Source::Demo, None, context);
                let ad_hoc = self.tabs.active_id().unwrap();
                self.update(tab_menu_message("Mute: Off".into()), context);
                self.update(tab_menu_message("Mute: On".into()), context);
                assert!(self.store.load().unwrap()["typo"].player.mute);
                let Page::Terminal(named) = &self.tabs.get(1).unwrap().page else {
                    panic!()
                };
                assert!(named.session.player.read().unwrap().mute);
                self.close_tab(ad_hoc, context);
                self.utility(true, context);
                self.edit_profile("typo".into(), context);
            }
            1 => {
                let id = self.tabs.active_id().unwrap();
                let Page::NewProfile(draft) = &self.tabs.get(id).unwrap().page else {
                    panic!()
                };
                assert_eq!(draft.name, "typo");
                assert_eq!(draft.host, "127.0.0.1");
                assert_eq!(
                    draft.terminal_type().unwrap().as_str(),
                    TerminalType::DEFAULT
                );
                assert_eq!(
                    draft.authentication, 1,
                    "native mounting changed the saved auth choice"
                );
                self.update(Message::Edit(id, Field::Name, "other".into()), context);
                self.save_profile_action(id, false, context);
                let Page::NewProfile(draft) = &self.tabs.get(id).unwrap().page else {
                    panic!()
                };
                assert!(draft.error.as_ref().unwrap().contains("already exists"));
                self.update(Message::Edit(id, Field::Name, "corrected".into()), context);
                self.update(Message::Auth(id, Some(2)), context);
                self.update(Message::TerminalPreset(id, Some(1)), context);
                self.update(
                    Message::Edit(
                        id,
                        Field::Port,
                        std::env::var("W11TERM_PROFILE_SMOKE_PORT").unwrap(),
                    ),
                    context,
                );
                self.save_profile_action(id, false, context);
                let Page::NewProfile(draft) = &self.tabs.get(id).unwrap().page else {
                    panic!()
                };
                assert!(draft.saved, "{:?}", draft.error);
                assert!(self.catalog.profiles.contains_key("corrected"));
                assert!(!self.catalog.profiles.contains_key("typo"));
                assert_eq!(self.tabs.get(1).unwrap().profile_name(), Some("corrected"));
                assert_eq!(self.tabs.get(1).unwrap().label, "corrected");
                let count = self.tabs.entries().len();
                self.edit_profile("corrected".into(), context);
                assert_eq!(self.tabs.entries().len(), count);
                self.utility(false, context);
                let blank = self.tabs.active_id().unwrap();
                assert_ne!(blank, id);
                self.close_tab(blank, context);
                self.tabs.select(id);
                self.activate(context);
            }
            2 => {
                let id = self.tabs.active_id().unwrap();
                let Page::NewProfile(draft) = &self.tabs.get(id).unwrap().page else {
                    panic!()
                };
                assert_eq!(draft.authentication, 2);
                assert!(draft.error.is_none());
                assert!(draft.saved, "native reconciliation cleared saved state");
                let document: serde_json::Value = serde_json::from_slice(
                    &std::fs::read(std::env::var_os("W11TERM_PROFILE_SMOKE_CONFIG").unwrap())
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(document["custom"], 42);
                assert_eq!(document["profiles"]["other"]["custom"], 99);
                assert_eq!(self.tabs.entries().len(), 3);
                self.save_profile(id, context);
            }
            30 => {
                let copy = self.tabs.active_id().unwrap();
                let Page::NewProfile(draft) = &self.tabs.get(copy).unwrap().page else {
                    panic!("expected copy form")
                };
                assert!(matches!(draft.origin, DraftOrigin::Copy(_)));
                assert_eq!(draft.name, "corrected copy");
                assert_eq!(draft.terminal_type().unwrap().as_str(), TerminalType::SIXEL);
                assert_eq!(draft.authentication, 2);
                self.update(Message::Edit(copy, Field::Name, "second".into()), context);
                self.update(Message::TerminalPreset(copy, Some(2)), context);
                self.update(
                    Message::Edit(copy, Field::CustomTerminal, "vt220".into()),
                    context,
                );
                self.update(Message::SaveProfileOnly(copy), context);
                assert!(
                    self.catalog.profiles.contains_key("corrected")
                        && self.catalog.profiles.contains_key("second")
                );
                assert_eq!(
                    self.store.load().unwrap()["second"].terminal_type.as_str(),
                    "vt220"
                );
                self.update(Message::DeleteProfile(copy), context);
            }
            31 => {
                let copy = self.tabs.active_id().unwrap();
                self.update(Message::ConfirmDeleteProfile(copy, false), context);
                assert!(self.store.load().unwrap().contains_key("second"));
                self.update(Message::DeleteProfile(copy), context);
                self.update(Message::ConfirmDeleteProfile(copy, true), context);
                assert!(!self.store.load().unwrap().contains_key("second"));
                self.edit_profile("corrected".into(), context);
                self.update(
                    Message::DeleteProfile(self.tabs.active_id().unwrap()),
                    context,
                );
            }
            32 => {
                let editor = self.tabs.active_id().unwrap();
                let existing = self.tabs.entries().iter().find(|entry| {
                    entry.value.ssh_profile_name() == Some("corrected")
                        && matches!(&entry.value.page, Page::Terminal(live) if live.session.frames.load().connection_generation > 0)
                }).expect("save-and-connect pane is connected to the loopback peer");
                let pane = existing.value.pane_id().unwrap();
                let entry = existing.id;
                let options = self.catalog.profiles["corrected"].clone();
                self.update(Message::ConfirmDeleteProfile(editor, true), context);
                assert!(!self.catalog.profiles.contains_key("corrected"));
                self.store.add("corrected", &options).unwrap();
                self.catalog = self.store.catalog().unwrap();
                let Page::Terminal(live) = &self.tabs.get(entry).unwrap().page else {
                    panic!("connection was closed")
                };
                assert_eq!(live.profile_name, None);
                assert_eq!(live.session.pane_id, pane);
                assert!(live.session.frames.load().connection_generation > 0);
                eprintln!(
                    "Profile UI smoke passed: rename, duplicate, cancel and confirm deletion, live pane identity and save-and-connect"
                );
                self.update(Message::CloseWindow, context);
                return;
            }
            _ => {
                let id = self.tabs.active_id().unwrap();
                let Page::Terminal(tab) = &self.tabs.get(id).unwrap().page else {
                    panic!("Save and connect did not open a terminal")
                };
                let pane_id = tab.session.pane_id;
                assert_ne!(pane_id, id, "utility tabs should not allocate pane IDs");
                assert_eq!(self.tab_for_pane(pane_id), Some(id));
                if tab.session.frames.load().connection_generation == 0 {
                    assert!(step < 24, "loopback connection did not become ready");
                } else {
                    assert_eq!(self.tabs.get(id).unwrap().label, "corrected");
                    assert_eq!(tab.profile_name.as_deref(), Some("corrected"));
                    self.update(tab_menu_message("Remote named sounds: Off".into()), context);
                    assert!(
                        self.store.load().unwrap()["corrected"]
                            .player
                            .remote_sounds_enabled()
                    );
                    assert!(matches!(self.tabs.get(1).unwrap().page, Page::Terminal(_)));
                    self.edit_profile("corrected".into(), context);
                    self.update(
                        Message::DuplicateProfile(self.tabs.active_id().unwrap()),
                        context,
                    );
                    context.spawn_background(move |_| {
                        std::thread::sleep(Duration::from_millis(500));
                        Message::Smoke(30)
                    });
                    return;
                }
            }
        }
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_millis(500));
            Message::Smoke(step + 1)
        });
    }
    fn player_smoke_step(&mut self, step: u8, context: &ComponentContext<Self>) {
        use w11term::prompts::{Answer, Challenge, Question};
        eprintln!("Player UI smoke step {step}");
        match step {
            0 | 2 => {
                let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page else {
                    panic!()
                };
                let prompts = tab.session.prompts.clone();
                context.spawn_background(move |_|{let question=if step==0{Question::HostKey{host:"loopback.test".into(),port:22,algorithm:"ssh-ed25519".into(),fingerprint:"SHA256:test-fingerprint".into()}}else{Question::Authentication{name:"MFA test".into(),instructions:"Isolated native prompt test".into(),challenges:vec![Challenge{label:"Account".into(),echo:true},Challenge{label:"Password".into(),echo:false}]}};
                    let answer=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(prompts.ask(question)).unwrap();
                    if step==0{assert!(matches!(answer,Answer::Trust(true)));}else{assert!(matches!(answer,Answer::Authentication(ref v)if v.len()==2&&v[0].as_str()=="pilot"&&v[1].as_str()=="secret"));}Message::Smoke(step+2)
                });
                context.spawn_background(move |_| {
                    std::thread::sleep(Duration::from_millis(500));
                    Message::Smoke(step + 1)
                });
            }
            1 | 3 => {
                let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page else {
                    panic!()
                };
                let prompt = tab.session.prompts.current().unwrap();
                assert_eq!(tab.prompt_id, Some(prompt.id));
                assert!(!self.keyboard.has_target());
                let pane = tab.session.pane_id;
                self.update(Message::MoveTabToWindow, context);
                assert!(
                    self.outgoing.is_none(),
                    "a pending SSH prompt opened another window"
                );
                assert_eq!(self.tab_for_pane(pane), Some(1));
                assert!(
                    self.window_error
                        .as_deref()
                        .is_some_and(|e| e.contains("SSH prompt"))
                );
                self.update(
                    tab_menu_message("Move pane to existing tab…".into()),
                    context,
                );
                assert!(
                    self.picker.is_none(),
                    "a pane move cannot take focus from an SSH prompt"
                );
                if step == 3 {
                    self.update(
                        Message::PromptEdit(1, prompt.id, 0, "pilot".into()),
                        context,
                    );
                    self.update(
                        Message::PromptEdit(1, prompt.id, 1, "secret".into()),
                        context,
                    );
                }
                self.update(
                    Message::PromptReply(1, prompt.id, ContentDialogResult::Primary),
                    context,
                );
            }
            4 => {
                self.windows.borrow_mut().remember("ui-smoke").unwrap();
                self.update(Message::Shortcut(Shortcut::Fullscreen), context);
                assert!(self.windows.borrow().fullscreen());
                context.spawn_background(|_| {
                    std::thread::sleep(Duration::from_millis(250));
                    Message::Smoke(5)
                });
            }
            5 => {
                let expected = {
                    let windows = self.windows.borrow();
                    let placement = &windows.profiles["ui-smoke"];
                    (placement.cols, placement.rows)
                };
                self.windows.borrow_mut().remember("ui-smoke").unwrap();
                {
                    let windows = self.windows.borrow();
                    let placement = &windows.profiles["ui-smoke"];
                    assert_eq!(
                        (placement.cols, placement.rows),
                        expected,
                        "fullscreen replaced the saved normal grid"
                    );
                }
                self.update(Message::Shortcut(Shortcut::Fullscreen), context);
                assert!(!self.windows.borrow().fullscreen());
                self.utility(false, context);
                context.spawn_background(|_| {
                    std::thread::sleep(Duration::from_millis(250));
                    Message::Smoke(6)
                });
            }
            6 => {
                assert!(!self.keyboard.has_target());
                let id = self.tabs.active_id().unwrap();
                self.update(Message::DraftPlayer(id, "rustmush".into(), true), context);
                let Page::NewProfile(draft) = &self.tabs.get(id).unwrap().page else {
                    panic!()
                };
                assert_eq!(draft.authentication, 3);
                assert!(draft.player.game_mode);
                assert!(!draft.player.alternate_scroll_enabled());
                self.update(
                    Message::DraftPlayer(id, "alternate_scroll".into(), false),
                    context,
                );
                let Page::NewProfile(draft) = &self.tabs.get(id).unwrap().page else {
                    panic!()
                };
                assert!(
                    draft.player.alternate_scroll.is_none(),
                    "native reconciliation overwrote automatic policy"
                );
                self.update(
                    Message::DraftPlayer(id, "alternate_scroll".into(), true),
                    context,
                );
                let Page::NewProfile(draft) = &self.tabs.get(id).unwrap().page else {
                    panic!()
                };
                assert_eq!(draft.player.alternate_scroll, Some(true));
                eprintln!(
                    "Player UI smoke passed: trust and private/native prompts, fullscreen roundtrip, wizard wheel policy."
                );
                let _ = context.window().request_close();
            }
            _ => {}
        }
    }
    fn search_smoke_step(&mut self, step: u8, context: &ComponentContext<Self>) {
        eprintln!("Search UI smoke step {step}");
        match step {
            0 => {
                let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page else {
                    panic!()
                };
                tab.session
                    .input
                    .send(Command::Text(
                        (0..80).map(|i| format!("needle-{i:03}\r")).collect(),
                    ))
                    .unwrap();
            }
            1 => {
                let mut prefs = self.appearance.read().unwrap().interaction.clone();
                prefs.scroll_to_zoom = false;
                self.keyboard.set_preferences(prefs);
                assert!(
                    self.keyboard.smoke_wheel(120),
                    "native wheel callback must consume the event"
                );
            }
            2 => {
                let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page else {
                    panic!()
                };
                assert!(
                    tab.session.frames.load().scroll_offset > 0,
                    "native wheel did not scroll history"
                );
                self.find(false, None, context);
            }
            3 => {
                let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page else {
                    panic!()
                };
                assert!(
                    tab.find.open && tab.find.focused,
                    "find textbox did not receive native focus"
                );
                self.update(Message::FindEdit(1, "needle-000".into()), context);
            }
            4 => {
                let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page else {
                    panic!()
                };
                let frame = tab.session.frames.load();
                let search = frame.search.as_ref().expect("search snapshot");
                assert_eq!(search.total, 1);
                assert!(!search.visible.is_empty());
                assert!(frame.scroll_offset > 0);
                self.update(Message::FindEdit(1, "needle-".into()), context);
            }
            5 => {
                let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page else {
                    panic!()
                };
                let frame = tab.session.frames.load();
                assert_eq!(frame.search.as_ref().unwrap().total, 80);
                assert_eq!(frame.search.as_ref().unwrap().selected, Some(0));
                self.find(false, Some(1), context);
            }
            6 => {
                let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page else {
                    panic!()
                };
                assert_eq!(
                    tab.session.frames.load().search.as_ref().unwrap().selected,
                    Some(1)
                );
                self.open("Second search lab".into(), Source::Demo, None, context);
            }
            7 => {
                let Page::Terminal(tab) = &self.tabs.get(2).unwrap().page else {
                    panic!()
                };
                assert!(!tab.find.open);
                assert!(tab.find.query.is_empty());
                self.tabs.select(1);
                self.activate(context);
            }
            8 => {
                let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page else {
                    panic!()
                };
                assert!(tab.find.open && tab.find.focused);
                assert_eq!(tab.find.query, "needle-");
                self.find(true, None, context);
            }
            _ => {
                let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page else {
                    panic!()
                };
                assert!(!tab.find.open);
                assert!(tab.session.frames.load().search.is_none());
                eprintln!(
                    "Search UI smoke passed: native wheel, textbox focus, scrollback matches/highlights, navigation, independent tabs, restore focus, Escape close"
                );
                let _ = context.window().request_close();
                return;
            }
        }
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_millis(400));
            Message::Smoke(step + 1)
        });
    }
}

#[cfg(test)]
mod find_tests {
    use super::Find;
    use w11term::search::{MAX_QUERY_BYTES, SearchSnapshot};

    #[test]
    fn oversized_regex_retains_one_complete_overflowing_character() {
        let prefix = "a".repeat(MAX_QUERY_BYTES - 1);
        let mut regex = format!("{prefix}🦀rest");
        let mut literal = regex.clone();
        Find::limit_query(&mut regex, true);
        Find::limit_query(&mut literal, false);
        assert_eq!(regex, format!("{prefix}🦀"));
        assert_eq!(literal, prefix);
    }

    #[test]
    fn status_ignores_an_earlier_query_or_option_during_debounce() {
        let find = Find {
            query: "abc".into(),
            regex: true,
            sensitive: true,
            ..Find::default()
        };
        let result = SearchSnapshot {
            query: find.query.clone(),
            regex: true,
            sensitive: true,
            total: 3,
            selected: Some(0),
            ..SearchSnapshot::default()
        };
        assert_eq!(find.status(Some(&result)), "1 / 3");
        for stale in [
            SearchSnapshot {
                query: "ab".into(),
                ..result.clone()
            },
            SearchSnapshot {
                regex: false,
                ..result.clone()
            },
            SearchSnapshot {
                sensitive: false,
                ..result
            },
        ] {
            assert_eq!(find.status(Some(&stale)), "Searching…");
        }
    }
}

#[cfg(test)]
mod profile_ui_tests {
    use super::*;

    #[test]
    fn duplicate_tab_actions_capture_the_group_at_invocation() {
        for action in [
            tab_menu_message("Duplicate tab layout    F8".into()),
            Message::Shortcut(KeyAction::DuplicateTabLayout.into()),
        ] {
            assert!(matches!(
                bind_tab_action(action, Some(42)),
                Message::DuplicateTabLayout(42)
            ));
        }
        assert!(matches!(
            bind_tab_action(tab_menu_message("Duplicate tab layout".into()), None),
            Message::ControlIdle
        ));
    }
    #[test]
    fn pane_menu_dispatch_preserves_profile_names_and_ignores_shortcut_labels() {
        for label in [
            "Focus right",
            "Focus right    Ctrl+Alt+Right",
            "Focus right    F8",
        ] {
            assert!(matches!(
                tab_menu_message(label.into()),
                Message::PaneNeighbor(Direction::Right)
            ));
        }
        assert!(
            matches!(tab_menu_message("Split right with tests    F8".into()),
            Message::SplitPane(Axis::Columns,Some(name)) if name=="tests    F8")
        );
        assert!(matches!(
            tab_menu_message("Move pane to existing tab…    F8".into()),
            Message::Shortcut(Shortcut::MovePaneToExistingTab)
        ));
    }
    #[test]
    fn state_labels_dispatch_the_same_action_before_and_after_toggling() {
        for (state, expected) in [(true, "Mute: On"), (false, "Mute: Off")] {
            let MenuItem::Item { label, enabled, .. } =
                state_menu_item("mute", "Mute", Some(state))
            else {
                panic!()
            };
            assert_eq!(label, expected);
            assert!(enabled);
            assert!(
                matches!(tab_menu_message(label),Message::TogglePlayer(field) if field=="mute")
            );
        }
        assert!(matches!(
            state_menu_item("mute", "Mute", None),
            MenuItem::Item { enabled: false, .. }
        ));
    }
    #[test]
    fn existing_profiles_prefill_all_authentication_modes_and_preferences() {
        for (expected, authentication) in [
            (0, Authentication::Agent),
            (
                1,
                Authentication::Identity {
                    identity: "key".into(),
                },
            ),
            (2, Authentication::None),
            (3, Authentication::Password),
            (4, Authentication::KeyboardInteractive),
        ] {
            let options = SshOptions {
                host: "test.invalid".into(),
                terminal_type: Default::default(),
                user: "pilot+character".into(),
                port: 6792,
                authentication,
                host_key: Some("SHA256:test".into()),
                proxy_jump: Vec::new(),
                durable: false,
                player: w11term::options::PlayerOptions {
                    mute: true,
                    game_mode: true,
                    ..Default::default()
                },
            };
            let draft = ProfileDraft::from_options("existing".into(), &options);
            assert_eq!(draft.authentication, expected);
            assert_eq!(draft.port, "6792");
            assert_eq!(draft.fingerprint, "SHA256:test");
            assert!(draft.player.mute && draft.player.game_mode);
            assert_eq!(draft.name, "existing");
        }
    }
}
