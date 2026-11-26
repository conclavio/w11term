use crate::{
    input::{KeyboardBridge, PickerStage, SearchField, Shortcut},
    renderer::{Metrics, PADDING, Renderer, STATUS_HEIGHT},
};
use anyhow::Context as _;
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    rc::Rc,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};
use w11term::{
    appearance::{Appearance, CursorGuide, DefaultCursorStyle, installed_fonts},
    clipboard::RemoteClipboardAccess,
    control::{Pane, Registry, Server},
    interaction::{
        Link, MultiLinePasteWarning, Point, Selection, TabSwitcherMode, link_at, safe_url,
        word_range,
    },
    options::{
        AgentPipe, Authentication, ColorScheme, LocalOptions, ProfileTextSize, Profiles,
        ScrollbackLines, Source, SshKeepalive, SshOptions, TabColor, TerminalType,
    },
    pane_layout::{Axis, Direction},
    profiles::{ProfileSnapshot, ProfileStore},
    session::{Command, PanePreferences, Session},
    shortcuts::Action as KeyAction,
    tabs::{BalancedPanes, PaneBalance, PaneRotation, PaneSwap, TabId, Tabs},
};
use wezterm_term::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind, TerminalSize};
use windows_canvas::{Canvas, Invalidator};
use windows_reactor::*;

const NEW_PROFILE: &str = "New Profile…";
const LAB: &str = "Terminal lab";
const RESTART_EXITED_PANE: &str = "Restart exited pane";
const STOP_SSH_ATTEMPT: &str = "Stop SSH attempt";
const RETRY_SSH_NOW: &str = "Retry SSH now";
const PIN_TAB: &str = "Pin tab";
const UNPIN_TAB: &str = "Unpin tab";

fn tab_pin_command(label: &str) -> Option<bool> {
    match label {
        PIN_TAB => Some(true),
        UNPIN_TAB => Some(false),
        _ => None,
    }
}

mod agent_key_picker;
mod attention;
mod bulk_tab_close;
mod clipboard_paste;
mod command_history;
mod config_import;
mod control_resize;
mod file_drawer;
mod file_picker;
mod folder_revoke;
mod folder_shares;
mod folder_sharing_state;
mod host_groups;
mod identity_picker;
mod keyboard_settings;
mod live_profile;
mod local_forwards;
mod local_profile;
mod pane_drop;
mod pane_text_size;
mod pattern_settings;
mod profile_forwards;
mod profile_jumps;
mod saved_commands;
mod shell_setup;
mod word_delimiters;
mod wsl_path;
use file_drawer::{FileAction, FileDrawer};
#[cfg(feature = "ui-smoke")]
mod accessibility_smoke;
mod command_palette;
mod directory_split;
mod disconnect_ssh;
mod durable_view;
mod explorer;
#[cfg(feature = "ui-smoke")]
mod ime_smoke;
mod inspector;
mod keyboard_copy;
mod keyboard_hints;
#[cfg(feature = "ui-smoke")]
mod local_smoke;
mod pane_broadcast;
mod pane_colors;
mod pane_picker;
mod pane_view;
mod previous_pane;
mod profile_query;
mod quick_connect;
mod recordings;
mod scrollback_bar;
mod session_browser;
mod sftp_transfers;
#[cfg(feature = "ui-smoke")]
mod split_smoke;
mod tab_colors;
mod tab_title;
mod tab_windows;
mod taskbar;
mod text_diff;
mod text_export;
mod text_size;
mod title_tools;
mod visual_bell;
pub use tab_windows::WindowLaunch;
mod workspace_view;
use pane_view::PaneSurface;

struct LiveTab {
    pane_name: Option<w11term::tabs::TabTitle>,
    forwards: Box<local_forwards::Routes>,
    folder_sharing: Option<w11term::folder_grants::PaneShares>,
    read_only: Rc<Cell<bool>>,
    startup: Option<Box<w11term::workspace::SshStartup>>,
    observed_frame: u64,
    files: Box<FileDrawer>,
    discovery: Option<session_browser::Browser>,
    copy_output_unavailable: Option<&'static str>,
    failed_command_jumps: [bool; 2],
    bookmark_actions: [bool; 4],
    can_clear_scrollback: bool,
    split_directory_report: Option<Arc<str>>,
    confirm_end: bool,
    durable_phase: Option<w11term::ssh::DurablePhase>,
    color_scheme: ColorScheme,
    source: Source,
    profile_name: Option<String>,
    pane_font_size: Cell<Option<f32>>,
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
    remember_ssh_key: bool,
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
    fn find_available(&self) -> bool {
        !self.device_busy
            && !self.browsing_sessions()
            && self.prompt_id.is_none()
            && self.session.frames.load().prompt.is_none()
            && self.session.prompts.current().is_none()
    }

    fn reconcile_selection(&mut self, frame: &w11term::terminal::Snapshot) -> bool {
        let mut selection = self.selection.borrow_mut();
        if selection.as_mut().is_some_and(|s| {
            s.alternate != frame.alternate
                || s.cols != frame.cols
                || !s.reconcile_scrollback_clear(frame)
        }) {
            selection.take();
            self.scroll_direction = 0;
            return true;
        }
        false
    }

    fn reset_transport_input(&mut self, keyboard: &KeyboardBridge) {
        self.remote_mouse = RemoteMouse::default();
        self.end_pointer_gesture();
        keyboard.reset_keys(&self.session.input);
    }

    fn end_pointer_gesture(&mut self) {
        self.surface.scrollbar.cancel();
        if let Some(release) = self.remote_mouse.release() {
            let _ = self.session.input.send(Command::Mouse(release));
        }
        self.finish_mouse_leave();
        if let Some(selection) = self.selection.borrow_mut().as_mut() {
            selection.dragging = false;
        }
        self.clicked_link = None;
        self.scroll_direction = 0;
        self.scroll_tick = false;
        self.gesture += 1;
    }

    fn mouse_left(&mut self, modifiers: KeyModifiers) {
        let connection = self.session.frames.load().connection_generation;
        self.remote_mouse.pending_leave = Some((connection, modifiers));
        self.finish_mouse_leave();
    }

    fn finish_mouse_leave(&mut self) {
        if self.remote_mouse.held.is_none()
            && let Some((connection, modifiers)) = self.remote_mouse.pending_leave.take()
        {
            let _ = self.session.input.send(Command::MouseLeave {
                connection,
                modifiers,
            });
        }
    }

    fn browsing_sessions(&self) -> bool {
        self.discovery.is_some()
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
    pending_leave: Option<(u64, KeyModifiers)>,
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
    fn same_origin(
        requested: Option<clipboard_paste::Origin>,
        current: Option<clipboard_paste::Origin>,
    ) -> bool {
        requested.is_some() && requested == current
    }

    fn accept_selection(&mut self, epoch: u64, seed: String) -> bool {
        if !self.open || self.epoch != epoch {
            return false;
        }
        self.query = seed;
        self.regex = false;
        true
    }

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
    groups: Vec<TabId>,
    index: usize,
}
#[derive(Clone, Copy)]
pub enum InteractionField {
    VisualBell,
    ApplicationTabTitles,
    FailedCommandMarks,
    StickyCommandContext,
    CopyOnSelect,
    LargePasteWarning,
    TrimBlock,
    TrimPaste,
    Snap,
    ScrollZoom,
    DetectUrls,
    Osc52,
    Osc52ForegroundOnly,
}

const MULTILINE_PASTE_CHOICES: [(MultiLinePasteWarning, &str); 3] = [
    (MultiLinePasteWarning::Automatic, "Automatic"),
    (MultiLinePasteWarning::Always, "Always"),
    (MultiLinePasteWarning::Never, "Never"),
];

enum SettingsEditor {
    Keyboard(keyboard_settings::KeyboardPage),
    Patterns(pattern_settings::PatternPage),
    SavedCommands(saved_commands::Editor),
    WordDelimiters(word_delimiters::Editor),
}
enum Page {
    LocalForwards(local_forwards::Panel),
    ForwardOverview(String),
    SharedFolders(folder_shares::Panel),
    Attention(attention::AttentionPage),
    SftpTransfers(Option<file_drawer::TransferId>),
    CommandHistory(command_history::HistoryPage),
    HostGroup(Box<host_groups::HostGroup>),
    Terminal(LiveTab),
    LocalProfile(local_profile::Draft),
    ImportConfig(config_import::ImportPage),
    Workspaces(workspace_view::WorkspacePage),
    Settings(Option<SettingsEditor>),
    NewProfile(ProfileDraft),
    QuickConnect(quick_connect::Draft),
}
struct AppTab {
    label: String,
    page: Page,
}

impl AppTab {
    fn pane_name(&self) -> Option<&w11term::tabs::TabTitle> {
        match &self.page {
            Page::Terminal(live) => live.pane_name.as_ref(),
            _ => None,
        }
    }

    fn exited_pane(&self) -> Option<ExitedPane> {
        let Page::Terminal(live) = &self.page else {
            return None;
        };
        Some(ExitedPane {
            pane: live.session.pane_id,
            token: live.session.events.restart_token()?,
        })
    }

    fn pending_pane_attempt(&self) -> Option<PendingPaneAttempt> {
        let Page::Terminal(live) = &self.page else {
            return None;
        };
        Some(PendingPaneAttempt {
            pane: live.session.pane_id,
            token: live.session.events.pending_ssh_attempt()?,
        })
    }

    fn pending_pane_retry(&self) -> Option<PendingPaneRetry> {
        let Page::Terminal(live) = &self.page else {
            return None;
        };
        Some(PendingPaneRetry {
            pane: live.session.pane_id,
            token: live.session.events.pending_ssh_retry()?,
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

    fn visited_terminal(&self) -> Option<u64> {
        match &self.page {
            Page::Terminal(tab) if !tab.browsing_sessions() => Some(tab.session.pane_id),
            _ => None,
        }
    }
}

#[derive(Clone, Default)]
enum DraftOrigin<T = ProfileSnapshot> {
    #[default]
    New,
    Edit(T),
    Copy(T),
}
impl<T> DraftOrigin<T> {
    fn editing(&self) -> Option<&T> {
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
    jump_editor: profile_jumps::Editor,
    local_forwards: Vec<w11term::options::LocalForwardPreset>,
    forward_editor: profile_forwards::Editor,
    durable: bool,
    saved: bool,
    name: String,
    host: String,
    user: String,
    port: String,
    connect_timeout: String,
    keepalive_interval: String,
    keepalive_max: String,
    reconnect_window: String,
    color_scheme: ColorScheme,
    font_size: String,
    tab_color: TabColor,
    scrollback_lines: String,
    terminal_preset: usize,
    custom_terminal: String,
    identity: String,
    certificate: String,
    agent_fingerprint: String,
    agent_pipe: String,
    fingerprint: String,
    host_key_alias: String,
    group_prompt: String,
    authentication: usize,
    error: Option<String>,
    player: w11term::options::PlayerOptions,
    device: Option<w11term::keys::DeviceKey>,
    generating: bool,
}

impl Default for ProfileDraft {
    fn default() -> Self {
        let keepalive = SshKeepalive::default();
        let player = w11term::options::PlayerOptions::default();
        Self {
            origin: DraftOrigin::New,
            folder_revoke: Default::default(),
            confirm_delete: false,
            proxy_jump: Vec::new(),
            jump_editor: Default::default(),
            local_forwards: Vec::new(),
            forward_editor: Default::default(),
            durable: false,
            saved: false,
            name: String::new(),
            host: String::new(),
            user: String::new(),
            port: "22".into(),
            connect_timeout: String::new(),
            keepalive_interval: keepalive.interval_seconds.to_string(),
            keepalive_max: keepalive.max_unanswered.to_string(),
            reconnect_window: player.reconnect_window_seconds.to_string(),
            color_scheme: ColorScheme::default(),
            font_size: String::new(),
            tab_color: TabColor::default(),
            scrollback_lines: ScrollbackLines::default().to_string(),
            terminal_preset: 0,
            custom_terminal: String::new(),
            identity: String::new(),
            certificate: String::new(),
            agent_fingerprint: String::new(),
            agent_pipe: String::new(),
            fingerprint: String::new(),
            host_key_alias: String::new(),
            group_prompt: String::new(),
            authentication: 0,
            error: None,
            player,
            device: None,
            generating: false,
        }
    }
}

impl ProfileDraft {
    fn ssh_options(&self) -> anyhow::Result<SshOptions> {
        anyhow::ensure!(
            !self.jump_editor.editing(),
            "Apply or cancel the pending edit under SSH jump hosts"
        );
        let (host, user, port) = parse_ssh_address(&self.host, &self.user, &self.port)?;
        let authentication = ssh_authentication(
            self.authentication,
            &self.identity,
            &self.certificate,
            &self.agent_fingerprint,
        )?;
        let mut player = self.player.clone();
        match self.reconnect_window.trim().parse() {
            Ok(seconds) => player.reconnect_window_seconds = seconds,
            Err(error) if player.reconnect => {
                return Err(error).context("Enter a positive SSH retry window in whole seconds");
            }
            Err(_) => {}
        }
        Ok(SshOptions {
            host,
            terminal_type: self.terminal_type()?,
            color_scheme: self.color_scheme,
            font_size: profile_font_size(&self.font_size)?,
            tab_color: self.tab_color,
            scrollback_lines: self.scrollback_lines.parse()?,
            user,
            port,
            connect_timeout_seconds: parse_ssh_connect_timeout(&self.connect_timeout)?,
            keepalive: parse_ssh_keepalive(&self.keepalive_interval, &self.keepalive_max)?,
            agent_pipe: ssh_agent_pipe(&authentication, &self.agent_pipe)?,
            authentication,
            host_key: (!self.fingerprint.trim().is_empty()).then(|| self.fingerprint.trim().into()),
            host_key_alias: self.host_key_alias(),
            player,
            proxy_jump: self.proxy_jump.clone(),
            local_forwards: self.local_forwards.clone(),
            group_prompt: (!self.group_prompt.trim().is_empty())
                .then(|| self.group_prompt.clone().try_into())
                .transpose()?,
            durable: self.durable,
        })
    }

    fn host_key_alias(&self) -> Option<String> {
        let alias = self.host_key_alias.trim();
        (!alias.is_empty()).then(|| alias.to_ascii_lowercase())
    }

    fn terminal_type(&self) -> anyhow::Result<TerminalType> {
        let name = match self.terminal_preset {
            0 => TerminalType::DEFAULT,
            1 => TerminalType::SIXEL,
            _ => &self.custom_terminal,
        };
        TerminalType::try_from(name.to_owned())
    }

    fn from_options(name: String, options: &SshOptions) -> Self {
        let (authentication, identity, certificate, agent_fingerprint) =
            ssh_auth_fields(&options.authentication);
        Self {
            proxy_jump: options.proxy_jump.clone(),
            local_forwards: options.local_forwards.clone(),
            name,
            durable: options.durable,
            host: options.host.clone(),
            user: options.user.clone(),
            port: options.port.to_string(),
            connect_timeout: options
                .connect_timeout_seconds
                .map(|seconds| seconds.to_string())
                .unwrap_or_default(),
            keepalive_interval: options.keepalive.interval_seconds.to_string(),
            keepalive_max: options.keepalive.max_unanswered.to_string(),
            reconnect_window: options.player.reconnect_window_seconds.to_string(),
            color_scheme: options.color_scheme,
            font_size: options
                .font_size
                .map(|size| size.to_string())
                .unwrap_or_default(),
            tab_color: options.tab_color,
            scrollback_lines: options.scrollback_lines.to_string(),
            terminal_preset: match options.terminal_type.as_str() {
                TerminalType::DEFAULT => 0,
                TerminalType::SIXEL => 1,
                _ => 2,
            },
            custom_terminal: options.terminal_type.as_str().into(),
            identity,
            certificate,
            agent_fingerprint,
            agent_pipe: options
                .agent_pipe
                .as_ref()
                .map(AgentPipe::as_str)
                .unwrap_or_default()
                .into(),
            fingerprint: options.host_key.clone().unwrap_or_default(),
            host_key_alias: options.host_key_alias.clone().unwrap_or_default(),
            group_prompt: options
                .group_prompt
                .as_ref()
                .map_or("", |pattern| pattern.as_str())
                .into(),
            authentication,
            player: options.player.clone(),
            ..Default::default()
        }
    }
}

const PRIVATE_KEY_AUTH: usize = 1;
const SPECIFIC_AGENT_KEY_AUTH: usize = 5;
const AGENT_CERTIFICATE_AUTH: usize = 6;
const SSH_AUTH_LABELS: [&str; 7] = [
    "Windows SSH agent",
    "Private key file",
    "None (keyless server)",
    "Password",
    "Keyboard-interactive (password / MFA)",
    "Specific SSH agent key",
    "SSH agent with user certificate",
];

fn uses_agent_auth(index: usize) -> bool {
    matches!(index, 0 | SPECIFIC_AGENT_KEY_AUTH | AGENT_CERTIFICATE_AUTH)
}

fn ssh_agent_pipe(
    authentication: &Authentication,
    value: &str,
) -> anyhow::Result<Option<AgentPipe>> {
    if authentication.uses_agent() {
        AgentPipe::optional(value)
    } else {
        Ok(None)
    }
}

fn parse_ssh_address(host: &str, user: &str, port: &str) -> anyhow::Result<(String, String, u16)> {
    let host = host.trim().trim_matches(['[', ']']);
    let user = user.trim();
    anyhow::ensure!(
        !host.is_empty() && !user.is_empty(),
        "Enter a host and user"
    );
    anyhow::ensure!(
        !host.chars().any(|c| c.is_whitespace() || c.is_control())
            && !user.chars().any(|c| c.is_whitespace() || c.is_control()),
        "Host and user cannot contain spaces or control characters"
    );
    w11term::options::validate_ssh_host(host)?;
    let port = port
        .trim()
        .parse::<std::num::NonZeroU16>()
        .map_err(|_| anyhow::anyhow!("Port must be between 1 and 65535"))?
        .get();
    Ok((host.into(), user.into(), port))
}

fn parse_ssh_connect_timeout(value: &str) -> anyhow::Result<Option<std::num::NonZeroU32>> {
    let value = value.trim();
    (!value.is_empty())
        .then(|| value.parse())
        .transpose()
        .context("Enter a positive SSH connection timeout in whole seconds or leave it blank for the default")
}

fn parse_ssh_keepalive(interval: &str, max: &str) -> anyhow::Result<SshKeepalive> {
    Ok(SshKeepalive {
        interval_seconds: interval
            .trim()
            .parse()
            .context("Enter the SSH keepalive interval in whole seconds. Use 0 to disable it")?,
        max_unanswered: max
            .trim()
            .parse()
            .context("Enter a positive whole number of unanswered SSH keepalives")?,
    })
}

fn ssh_auth_fields(authentication: &Authentication) -> (usize, String, String, String) {
    let empty = |index| (index, String::new(), String::new(), String::new());
    match authentication {
        Authentication::Agent => empty(0),
        Authentication::AgentKey { agent_fingerprint } => (
            SPECIFIC_AGENT_KEY_AUTH,
            String::new(),
            String::new(),
            agent_fingerprint.as_str().into(),
        ),
        Authentication::AgentCertificate {
            certificate,
            agent_fingerprint,
        } => (
            AGENT_CERTIFICATE_AUTH,
            String::new(),
            certificate.to_string_lossy().into_owned(),
            agent_fingerprint
                .as_ref()
                .map(|fingerprint| fingerprint.as_str().to_owned())
                .unwrap_or_default(),
        ),
        Authentication::Identity {
            identity,
            certificate,
        } => (
            PRIVATE_KEY_AUTH,
            identity.to_string_lossy().into_owned(),
            certificate
                .as_deref()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            String::new(),
        ),
        Authentication::None => empty(2),
        Authentication::Password => empty(3),
        Authentication::KeyboardInteractive => empty(4),
    }
}

fn ssh_authentication(
    index: usize,
    identity: &str,
    certificate: &str,
    agent_fingerprint: &str,
) -> anyhow::Result<Authentication> {
    Ok(match index {
        PRIVATE_KEY_AUTH => {
            let identity = identity.trim();
            let certificate = certificate.trim();
            anyhow::ensure!(!identity.is_empty(), "Enter the private key file path");
            Authentication::identity(
                identity,
                (!certificate.is_empty()).then(|| certificate.into()),
            )
        }
        2 => Authentication::None,
        3 => Authentication::Password,
        4 => Authentication::KeyboardInteractive,
        SPECIFIC_AGENT_KEY_AUTH => Authentication::AgentKey {
            agent_fingerprint: agent_fingerprint.parse()?,
        },
        AGENT_CERTIFICATE_AUTH => {
            let certificate = certificate.trim();
            let fingerprint = agent_fingerprint.trim();
            anyhow::ensure!(
                !certificate.is_empty(),
                "Enter the SSH user certificate file path"
            );
            Authentication::agent_certificate(
                certificate,
                (!fingerprint.is_empty())
                    .then(|| fingerprint.parse())
                    .transpose()?,
            )
        }
        _ => Authentication::Agent,
    })
}

fn color_scheme_editor(id: TabId, scheme: ColorScheme, context: &ViewContext<TerminalApp>) -> View {
    let schemes = ColorScheme::ALL;
    StackPanel::new().spacing(5.0).children((
        TextBlock::new().text("Terminal color scheme"),
        ComboBox::new()
            .automation_name("Terminal color scheme")
            .items_source(schemes.map(ColorScheme::label))
            .selected_index(schemes.iter().position(|s| *s == scheme))
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .on_selection_changed(context.callback(move |index: Option<usize>| {
                index
                    .and_then(|i| schemes.get(i))
                    .map_or(Message::ControlIdle, |&scheme| {
                        Message::ProfileColorScheme(id, scheme)
                    })
            })),
    ))
}

fn tab_color_editor(id: TabId, color: TabColor, context: &ViewContext<TerminalApp>) -> View {
    StackPanel::new().spacing(5.0).children((
        TextBlock::new().text("Tab color"),
        ComboBox::new()
            .automation_name("Tab color")
            .items_source(TabColor::ALL.map(TabColor::label))
            .selected_index(TabColor::ALL.iter().position(|value| *value == color))
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .on_selection_changed(context.callback(move |index: Option<usize>| {
                index
                    .and_then(|i| TabColor::ALL.get(i))
                    .map_or(Message::ControlIdle, |&color| {
                        Message::ProfileTabColor(id, color)
                    })
            })),
    ))
}

fn tab_header_content(label: String, pinned: bool, color: TabColor) -> (String, String) {
    let (header, name) = if let Some(marker) = color.marker() {
        (
            format!("{marker} {label}"),
            format!("{label} ({} tab)", color.label()),
        )
    } else {
        (label.clone(), label)
    };
    if pinned {
        (format!("📌 {header}"), format!("Pinned: {name}"))
    } else {
        (header, name)
    }
}

fn scrollback_editor(value: &str, on_change: Callback<String>) -> View {
    let hint = TextBlock::new().text_wrapping(TextWrapping::Wrap);
    let hint = match value.parse::<ScrollbackLines>() {
        Ok(_) => hint.text(format!(
            "Choose 0 to {} lines for new panes. Set 0 to disable scrollback.",
            ScrollbackLines::MAX
        )),
        Err(error) => hint
            .text(error.to_string())
            .foreground(Color::rgb(255, 160, 150)),
    };
    StackPanel::new().spacing(5.0).children((
        TextBlock::new().text("Scrollback lines"),
        TextBox::new()
            .automation_name("Scrollback lines")
            .text(value)
            .on_text_changed(on_change),
        hint,
    ))
}

fn profile_font_size(value: &str) -> anyhow::Result<Option<ProfileTextSize>> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let size = value.parse::<u8>().with_context(|| {
        format!(
            "Enter a whole text size from {} to {}",
            ProfileTextSize::MIN,
            ProfileTextSize::MAX
        )
    })?;
    size.try_into().map(Some)
}

fn profile_font_size_editor(value: &str, on_change: Callback<String>) -> View {
    let hint = TextBlock::new().text_wrapping(TextWrapping::Wrap);
    let hint = match profile_font_size(value) {
        Ok(_) => hint.text(format!(
            "Choose {} to {} for new panes. Leave blank to use the window's text size.",
            ProfileTextSize::MIN,
            ProfileTextSize::MAX
        )),
        Err(error) => hint
            .text(error.to_string())
            .foreground(Color::rgb(255, 160, 150)),
    };
    StackPanel::new().spacing(5.0).children((
        TextBlock::new().text("Terminal text size"),
        TextBox::new()
            .automation_name("Terminal text size")
            .text(value)
            .placeholder_text("Use window text size")
            .on_text_changed(on_change),
        hint,
    ))
}

fn command_menu_item(key: &str, label: impl Into<String>, enabled: bool) -> MenuItem {
    let label = label.into();
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
    if let Some(window) = tab_windows::menu_window(&label) {
        return Message::MoveTabToExistingWindow(None, window);
    }
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
        pane_text_size::INCREASE => Message::PaneTextSize(pane_text_size::Change::Increase),
        pane_text_size::DECREASE => Message::PaneTextSize(pane_text_size::Change::Decrease),
        pane_text_size::RESET => Message::PaneTextSize(pane_text_size::Change::Reset),
        "Attention inbox" => Message::Attention(attention::Action::Open),
        sftp_transfers::LABEL => Message::SftpTransfers(sftp_transfers::Action::Open),
        host_groups::LABEL => Message::HostGroup(host_groups::Action::Open),
        host_groups::NEW_TAB_LABEL => Message::HostGroup(host_groups::Action::NewTab),
        "Rename tab…" => Message::TabTitle(tab_title::Action::Show),
        "Duplicate tab layout" => Message::Shortcut(Shortcut::DuplicateTabLayout),
        "Reopen last closed tab layout" => Message::Shortcut(Shortcut::ReopenClosedTab),
        "Reopen last closed pane" => Message::Shortcut(Shortcut::ReopenClosedPane),
        "Move tab left" => Message::Shortcut(Shortcut::MoveTabLeft),
        "Move tab right" => Message::Shortcut(Shortcut::MoveTabRight),
        "SFTP files" => Message::Files(FileAction::Toggle),
        local_forwards::overview::LABEL => {
            Message::LocalForward(local_forwards::Action::OpenOverview)
        }
        "Workspace startup…" => {
            Message::Workspace(workspace_view::WorkspaceAction::ConfigureStartup)
        }
        "Open files here" => Message::Files(FileAction::OpenHere),
        "Copy last command output" => Message::CopyLastCommand,
        command if let Some(setup) = shell_setup::Setup::from_label(command) => {
            Message::CopyShellSetup(setup)
        }
        "Clear scrollback" => Message::Shortcut(Shortcut::ClearScrollback),
        "Command history" => Message::History(command_history::Action::Open),
        "Notify when command finishes" => Message::Attention(attention::Action::WatchCommand(true)),
        "Cancel command notification" => Message::Attention(attention::Action::WatchCommand(false)),
        "Split right" => Message::SplitPane(Axis::Columns, None),
        "Split below" => Message::SplitPane(Axis::Rows, None),
        command_palette::SPLIT_RIGHT_PROFILE_LABEL => Message::FindSplitProfile(Axis::Columns),
        command_palette::SPLIT_BELOW_PROFILE_LABEL => Message::FindSplitProfile(Axis::Rows),
        "Split right in current directory" => {
            Message::Shortcut(Shortcut::SplitRightInCurrentDirectory)
        }
        "Split below in current directory" => {
            Message::Shortcut(Shortcut::SplitBelowInCurrentDirectory)
        }
        "New tab in current directory" => Message::Shortcut(Shortcut::NewTabInCurrentDirectory),
        "Copy current directory" => Message::Shortcut(Shortcut::CopyCurrentDirectory),
        "Focus left" => Message::PaneNeighbor(Direction::Left),
        "Focus right" => Message::PaneNeighbor(Direction::Right),
        "Focus above" => Message::PaneNeighbor(Direction::Up),
        "Focus below" => Message::PaneNeighbor(Direction::Down),
        "Focus previous pane" => Message::Shortcut(Shortcut::FocusPreviousPane),
        "Resize left" => Message::Shortcut(Shortcut::ResizePane(Direction::Left)),
        "Resize right" => Message::Shortcut(Shortcut::ResizePane(Direction::Right)),
        "Resize up" => Message::Shortcut(Shortcut::ResizePane(Direction::Up)),
        "Resize down" => Message::Shortcut(Shortcut::ResizePane(Direction::Down)),
        "Swap pane left" => Message::Shortcut(Shortcut::SwapPane(Direction::Left)),
        "Swap pane right" => Message::Shortcut(Shortcut::SwapPane(Direction::Right)),
        "Swap pane up" => Message::Shortcut(Shortcut::SwapPane(Direction::Up)),
        "Swap pane down" => Message::Shortcut(Shortcut::SwapPane(Direction::Down)),
        "Zoom pane" | "Restore panes" => Message::ZoomPane,
        "Rotate split" => Message::Shortcut(Shortcut::RotateSplit),
        "Balance panes" => Message::Shortcut(Shortcut::BalancePanes),
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
        title_tools::FIND_SCROLLBACK => Message::Shortcut(Shortcut::Find),
        "Find pane…" => Message::Shortcut(Shortcut::PanePicker),
        command_palette::FIND_TAB_LABEL => Message::Shortcut(Shortcut::FindTab),
        command_palette::FIND_COMMAND_HISTORY_LABEL => {
            Message::Shortcut(Shortcut::FindCommandHistory)
        }
        command_palette::FIND_RUNNING_COMMANDS_LABEL => Message::OpenRunningCommands,
        pane_picker::FIND_TEXT_LABEL => Message::Shortcut(Shortcut::FindAllPanes),
        "Keyboard copy mode" => Message::Shortcut(Shortcut::CopyMode),
        "Select all terminal text" => Message::Shortcut(Shortcut::SelectAll),
        "Copy selection" => Message::Shortcut(Shortcut::Copy),
        "Keyboard hints" => Message::Shortcut(Shortcut::KeyboardHints),
        _ => Message::ControlIdle,
    }
}

fn bind_tab_action(message: Message, group: Option<TabId>) -> Message {
    match message {
        Message::MoveTabToExistingWindow(None, window) => group
            .map_or(Message::ControlIdle, |id| {
                Message::MoveTabToExistingWindow(Some(id), window)
            }),
        Message::Shortcut(Shortcut::DuplicateTabLayout) => {
            group.map_or(Message::ControlIdle, Message::DuplicateTabLayout)
        }
        Message::Shortcut(Shortcut::MoveTabLeft) => group.map_or(Message::ControlIdle, |id| {
            Message::MoveTab(id, Direction::Left)
        }),
        Message::Shortcut(Shortcut::MoveTabRight) => group.map_or(Message::ControlIdle, |id| {
            Message::MoveTab(id, Direction::Right)
        }),
        message => message,
    }
}

#[derive(Clone, Copy)]
pub enum Field {
    Name,
    Host,
    User,
    Port,
    KeepaliveInterval,
    ConnectTimeout,
    KeepaliveMax,
    ReconnectWindow,
    ScrollbackLines,
    FontSize,
    CustomTerminal,
    Identity,
    Certificate,
    AgentFingerprint,
    AgentPipe,
    Fingerprint,
    HostKeyAlias,
    GroupPrompt,
}

enum UiWork {
    FolderAuthority,
    Control(w11term::control::UiRequest),
    AttentionChanged,
    TextSizeChanged,
}

struct CaretMenuBinding {
    broadcast: Option<pane_broadcast::Action>,
    close_other_tabs: Option<bulk_tab_close::Offer>,
    close_tabs_right: Option<bulk_tab_close::Offer>,
    recordings: recordings::Choices,
    previous: Option<previous_pane::Target>,
    forward: Option<local_forwards::Origin>,
    directory_split: Option<directory_split::Origin>,
    pane: Option<u64>,
    paste: Option<clipboard_paste::Origin>,
    group: Option<TabId>,
    arrangement: PaneArrangement,
    closed_group: Option<TabId>,
    closed_pane: Option<TabId>,
    exited: Option<ExitedPane>,
    pending_ssh: Option<PendingPaneAttempt>,
    pending_retry: Option<PendingPaneRetry>,
    disconnect_ssh: Option<disconnect_ssh::Target>,
    on_click: Callback<String>,
}

/// Pane layout actions offered while this menu is visible.
#[derive(Clone, Default, PartialEq)]
struct PaneArrangement {
    balance: Option<PaneBalance>,
    rotation: Option<PaneRotation>,
    left: Option<PaneSwap>,
    right: Option<PaneSwap>,
    up: Option<PaneSwap>,
    down: Option<PaneSwap>,
}

impl PaneArrangement {
    fn for_tab<T>(tabs: &Tabs<T>) -> Self {
        if !matches!(
            tabs.active_layout(),
            Some(w11term::pane_layout::Layout::Split { .. })
        ) {
            return Self::default();
        }
        Self {
            balance: None,
            left: tabs.pane_swap(Direction::Left),
            rotation: tabs.pane_rotation(),
            right: tabs.pane_swap(Direction::Right),
            up: tabs.pane_swap(Direction::Up),
            down: tabs.pane_swap(Direction::Down),
        }
    }

    fn get(&self, direction: Direction) -> Option<PaneSwap> {
        match direction {
            Direction::Left => self.left,
            Direction::Right => self.right,
            Direction::Up => self.up,
            Direction::Down => self.down,
        }
    }

    fn rotate(&self) -> Message {
        self.rotation
            .clone()
            .map_or(Message::ControlIdle, Message::RotateSplit)
    }

    fn balance(&self) -> Message {
        self.balance
            .clone()
            .map_or(Message::ControlIdle, Message::BalancePanes)
    }

    fn message(&self, direction: Direction) -> Message {
        self.get(direction)
            .map_or(Message::ControlIdle, Message::SwapPanes)
    }
}

pub struct TerminalApp {
    caret_interval: Option<u32>,
    visual_bell_timer: Option<crate::ui_timer::RetryTimer>,
    folder_wake: Arc<dyn Fn() + Send + Sync>,
    folder_panel_timer: Option<(TabId, crate::ui_timer::RetryTimer)>,
    forward_watch: Option<crate::ui_timer::RetryTimer>,
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
    broadcast_typing: Option<pane_broadcast::Plan>,
    _control: Arc<Server>,
    window_launch: w11term::options::Options,
    opening: Option<Rc<crate::workspace_window::Opening>>,
    incoming: Option<Rc<tab_windows::Transfer>>,
    outgoing: Option<Rc<tab_windows::Transfer>>,
    window_timer: Option<crate::ui_timer::RetryTimer>,
    host_group_timer: Option<crate::ui_timer::RetryTimer>,
    native_bound: bool,
    window_error: Option<String>,
    pending_directory_copy: Option<directory_split::PendingCopy>,
    export: text_export::Export,
    history_reference: Option<command_history::OutputCapture>,
    recordings: recordings::Recordings,
    registry: Registry,
    control_rx: Arc<std::sync::Mutex<std::sync::mpsc::Receiver<UiWork>>>,
    control_task: Option<ComponentTask>,
    tabs: Tabs<AppTab>,
    closed_tabs: VecDeque<workspace_view::ClosedTab>,
    closed_pane: Option<workspace_view::ClosedPane>,
    metrics: Rc<Cell<Metrics>>,
    invalidator: Invalidator,
    header: RefCell<Option<View>>,
    caret_menu_binding: Rc<RefCell<Option<CaretMenuBinding>>>,
    frame_tasks: BTreeMap<TabId, ComponentTask>,
    watcher_retry: Option<crate::ui_timer::RetryTimer>,
    watch_epoch: u64,
    default_source: Source,
    default_label: String,
    default_profile: Option<String>,
    catalog: Profiles,
    store: ProfileStore,
    profile_error: Option<String>,
    profile_action_error: Option<String>,
    profile_filter: String,
    width: f64,
    selection_pending: bool,
    selection_epoch: u64,
    client_size: (f64, f64),
    window_height: f64,
    initial_grid: Rc<crate::window_state::RequestedGrid>,
    appearance: Arc<RwLock<Appearance>>,
    text_size: Option<text_size::Monitor>,
    fonts: Vec<String>,
    appearance_error: Option<String>,
    saved_commands: w11term::saved_commands::Commands,
    dividers: Rc<RefCell<pane_view::DividerState>>,
    measured_layout: Option<Rc<pane_view::PaneGeometry>>,
    rotation_hover: Option<pane_view::RotationHover>,
    pointer_over: Option<TabId>,
    overlay_visible: Cell<bool>,
    mru_groups: Vec<TabId>,
    switcher: Option<TabSwitcher>,
    picker: Option<pane_picker::Picker>,
    running_interrupt_pending: Option<pane_picker::PendingRunningInterrupt>,
    title_editor: Option<tab_title::Editor>,
    disconnect_review: Option<Arc<disconnect_ssh::Review>>,
    name_edit_serial: u64,
    identity_picker: Option<identity_picker::Pending>,
    clipboard: clipboard_paste::State,
    pane_drop: pane_drop::State,
    focused_osc52: Option<Arc<RemoteClipboardAccess>>,
    hints: Rc<RefCell<Option<keyboard_hints::Hints>>>,
    copy_mode: Rc<RefCell<Option<keyboard_copy::ActiveCopy>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExitedPane {
    pane: u64,
    token: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingPaneAttempt {
    pane: u64,
    token: w11term::events::PendingSshAttempt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingPaneRetry {
    pane: u64,
    token: w11term::events::PendingSshRetry,
}

pub enum Message {
    LocalForward(local_forwards::Action),
    ProfileForward(TabId, profile_forwards::Edit),
    ProfileJump(TabId, profile_jumps::Edit),
    FolderAuthority,
    FolderShare(folder_shares::Action),
    FolderRevoke(TabId, u64, folder_revoke::Action),
    RestartExited(ExitedPane),
    StopSshAttempt(PendingPaneAttempt),
    RetrySshNow(PendingPaneRetry),
    DisconnectSsh(disconnect_ssh::Action),
    Export(text_export::Action),
    Recording(recordings::Action),
    ClearScrollback(u64),
    ScrollFailedCommand(Option<clipboard_paste::Origin>, bool),
    ToggleScrollbackBookmark(Option<clipboard_paste::Origin>),
    ScrollToBookmark(Option<clipboard_paste::Origin>, bool),
    PaneColorScheme(u64, ColorScheme),
    TabTitle(tab_title::Action),
    Attention(attention::Action),
    SftpTransfers(sftp_transfers::Action),
    AttentionChanged,
    VisualBellTick,
    ToggleInspector,
    InspectorTick,
    PaneTextSize(pane_text_size::Change),
    InspectSshRoute(u64, bool),
    PaletteChoose(command_palette::CommandId),
    PickerQuery(String),
    OpenRunningCommands,
    RunningCommandsTick(Arc<()>),
    RunningInterrupt(pane_picker::RunningInterruptAction),
    WatchRunningCommand(Arc<()>, u64, w11term::events::RunningCommandTarget, bool),
    WindowHistory(pane_picker::WindowHistoryAction),
    PaneText(pane_picker::TextAction),
    PickerHover(u64),
    PickerFrame(u64, u64),
    BookmarksRead(Arc<()>, Option<Vec<w11term::terminal::BookmarkEntry>>),
    PickerChoose(u64),
    PlacePane(TabId, TabId, Direction),
    PickerFocus(u8),
    PickerFromMenu(Shortcut),
    SetTitleFiles(u64, bool),
    PickerFocusAfterMenu(Arc<()>),
    LocalProfile(local_profile::Action),
    Files(FileAction),
    HintRevealed(Arc<()>, Result<(), String>),
    AccessibleSelection(crate::accessibility::SelectionRequest),
    SplitPane(Axis, Option<String>),
    FindSplitProfile(Axis),
    OpenInDirectory(directory_split::Destination, directory_split::Origin),
    OpenExplorer(directory_split::Origin),
    CopyCurrentDirectory(directory_split::Origin),
    DirectoryCopyReady(Arc<()>, Result<String, String>),
    ExplorerOpened(Result<(), String>),
    PaneNeighbor(Direction),
    FocusPreviousPane(previous_pane::Target),
    SwapPanes(PaneSwap),
    RotateSplit(PaneRotation),
    BalancePanes(PaneBalance),
    ZoomPane,
    ClosePane,
    MovePaneToTab,
    MoveTabToWindow,
    MoveTabToExistingWindow(Option<TabId>, u64),
    DuplicateTabLayout(TabId),
    ReopenClosedTab(Option<TabId>),
    ReopenClosedPane(Option<TabId>),
    MoveTab(TabId, Direction),
    SetTabPinned(TabId, bool),
    SetTabColor(TabId, Option<TabColor>),
    CloseTabs(bulk_tab_close::Offer),
    WindowTick,
    WindowTargetsChanged,
    DismissWindowError,
    TransferRequest(u64, u64),
    TransferReady(u64),
    TransferClaim(u64),
    TransferDone(u64),
    TransferCanceled(u64),
    PaneFocus(TabId),
    SetPaneReadOnly(bool),
    BroadcastTyping(pane_broadcast::Action),
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
    ControlResizeDone(control_resize::Completion),
    PromptEdit(TabId, u64, usize, String),
    PromptRememberSshKey(TabId, u64, bool),
    PromptReply(TabId, u64, ContentDialogResult),
    GenerateKey(TabId),
    KeyGenerated(TabId, Result<w11term::keys::DeviceKey, String>),
    CopyPublicKey(TabId),
    CopyLastCommand,
    CopyShellSetup(shell_setup::Setup),
    History(command_history::Action),
    HostGroup(host_groups::Action),
    DraftPlayer(TabId, String, bool),
    Durability(durable_view::Action),
    Discovery(session_browser::Action),
    FrameReady(TabId, u64),
    JumpToCommandContext(TabId, w11term::terminal::ScrollbackTarget),
    CloseWindow,
    Select(Option<usize>, u64, bool),
    CloseTab(String),
    Reorder(Vec<String>, Option<TabId>, u64),
    SyncSelection(u64),
    Shortcut(Shortcut),
    ProfileMenu(String),
    QuickConnect(quick_connect::Action),
    SshIdentity(identity_picker::Action),
    MoveProfile(String, isize, Arc<[String]>),
    SetProfilePin(String, bool),
    FilterProfiles(String),
    StartupProfile(String),
    WindowActivation(bool),
    SettingsMenu,
    Keyboard(keyboard_settings::Edit),
    Patterns(pattern_settings::Edit),
    SavedCommands(saved_commands::Edit),
    WordDelimiters(word_delimiters::Edit),
    Workspace(workspace_view::WorkspaceAction),
    ImportConfig(config_import::Action),
    Font(Option<usize>),
    DefaultCursorStyle(Option<usize>),
    CursorGuide(Option<usize>),
    Ligatures(bool),
    FollowWindowsTextSize(bool),
    WindowsTextSizeChanged,
    Interaction(InteractionField, bool),
    MultilinePasteWarning(Option<usize>),
    TabMode(Option<usize>),
    PickTab(TabId),
    CaptureLost(TabId),
    FinalizeCapture(TabId, u64),
    PointerExit(TabId),
    DragScroll(TabId),
    PasteRaw,
    Paste,
    Clipboard(clipboard_paste::Action),
    PaneDrop(pane_drop::Action),
    CopyResult(TabId, u64, Result<(), String>),
    SelectAll(Option<clipboard_paste::Origin>),
    PaneFind(Option<clipboard_paste::Origin>, Shortcut),
    CopySelection(Option<clipboard_paste::Origin>),
    SelectAllResult(
        TabId,
        u64,
        Result<w11term::copy_mode::CopyMode, &'static str>,
    ),
    CopyWordResult(TabId, u64, Option<Point>),
    Pointer(TabId, PointerEventInfo, MouseEventKind),
    RenderError(TabId, String),
    RetryRender(TabId),
    Edit(TabId, Field, String),
    Auth(TabId, Option<usize>),
    TerminalPreset(TabId, Option<usize>),
    ProfileColorScheme(TabId, ColorScheme),
    ProfileTabColor(TabId, TabColor),
    SaveProfile(TabId),
    SaveProfileOnly(TabId),
    ConnectOnce(TabId),
    EditProfile(String),
    EditActiveProfile,
    DuplicateProfile(TabId),
    DeleteProfile(TabId),
    ConfirmDeleteProfile(TabId, bool),
    WindowSize(WindowSize),
    Geometry {
        client: (f64, f64),
        delta: (f64, f64),
        epoch: u64,
        first: bool,
    },
    PaneMeasured(TabId),
    GridFallback(u64),
    FindEdit(TabId, String),
    FindSeed(
        TabId,
        u64,
        u64,
        Arc<w11term::interaction::FindSelection>,
        String,
    ),
    FindOption(TabId, FindOption, bool),
    FindApply(TabId, u64),
    FindFocus(TabId, u64, u8),
    FindFocusResult(TabId, u64, bool),
    #[cfg(feature = "ui-smoke")]
    Smoke(u8),
}

fn live_tab(label: String, source: Source, preferences: impl Into<PanePreferences>) -> AppTab {
    live_tab_sized(
        label,
        source,
        TerminalSize::default(),
        preferences,
        None,
        None,
    )
}
fn live_tab_sized(
    label: String,
    source: Source,
    size: TerminalSize,
    preferences: impl Into<PanePreferences>,
    connection: Option<w11term::ssh::ConnectionShare>,
    saved: Option<w11term::durable::DurableReference>,
) -> AppTab {
    let session = if matches!(&source, Source::Ssh(ssh) if ssh.durable) {
        Session::start_durable(source.clone(), size, preferences, connection, saved)
    } else if let Source::Replay(path) = &source {
        Session::start_replay_pane(path.clone(), size, preferences)
    } else if let Source::ViewRecording(path) = &source {
        Session::start_view_recording_pane_with_preferences(path.clone(), size, preferences)
    } else {
        Session::start_configured_with_connection(source.clone(), size, preferences, connection)
    };
    live_tab_for_session(label, source, session)
}

fn live_tab_for_session(label: String, source: Source, session: Session) -> AppTab {
    AppTab {
        label,
        page: Page::Terminal(LiveTab {
            pane_name: None,
            forwards: Box::new(local_forwards::Routes::new(&session)),
            folder_sharing: None,
            startup: None,
            observed_frame: 0,
            files: Box::default(),
            discovery: None,
            copy_output_unavailable: Some("waiting for shell markers"),
            failed_command_jumps: [false; 2],
            bookmark_actions: [false; 4],
            can_clear_scrollback: false,
            split_directory_report: None,
            color_scheme: source.color_scheme(),
            source,
            confirm_end: false,
            durable_phase: None,
            profile_name: None,
            pane_font_size: Cell::new(None),
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
            remember_ssh_key: false,
            connection_generation: 0,
        }),
    }
}

fn source_label(source: &Source) -> String {
    match source {
        Source::Local(shell) if let Some(distro) = shell.wsl_distribution() => {
            format!("WSL: {distro}")
        }
        Source::Local(shell) => shell
            .executable
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        Source::Demo => LAB.into(),
        Source::Replay(path) | Source::ViewRecording(path) => {
            format!("Replay · {}", path.display())
        }
        Source::TimedReplay(path) | Source::PausedReplay(path) => {
            format!("Timed replay · {}", path.display())
        }
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
                        Ok(UiWork::TextSizeChanged) => return Message::WindowsTextSizeChanged,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(_) => return Message::ControlIdle,
                    }
                }
            },
            Message::WatchRejected,
        ));
    }
    fn connection_ui(&mut self, id: TabId) {
        let active = self.tabs.active_id() == Some(id);
        let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.get_mut(id)
        else {
            return;
        };
        let frame = tab.session.frames.load();
        tab.observe_file_connection(frame.connection_generation, active);
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
                Some(w11term::prompts::Question::IdentityPassphrase { .. }) => {
                    vec![zeroize::Zeroizing::new(String::new())]
                }
                _ => Vec::new(),
            };
            tab.prompt_id = prompt_id;
            tab.remember_ssh_key = false;
        }
        if tab.connection_generation != frame.connection_generation {
            self.header.borrow_mut().take();
            tab.connection_generation = frame.connection_generation;
            self.clipboard.input_origin.update(|origin| {
                origin.map(|mut origin| {
                    if origin.pane == tab.session.pane_id {
                        origin.connection = frame.connection_generation;
                    }
                    origin
                })
            });
            tab.reset_transport_input(&self.keyboard);
        }
        if prompt_id.is_some() {
            self.title_editor = None;
            self.disconnect_review = None;
            tab.files.cancel_entry_dialog();
            tab.files.end_filter_edit();
        }
        let editing_files = tab.files.entry_dialog_open() || tab.files.filter_editing();
        self.keyboard
            .watch_file_drawer(tab.files.open.then_some(tab.session.pane_id));
        self.clipboard.connection_changed(
            tab.session.pane_id,
            frame.connection_generation,
            prompt_id.is_some() || tab.read_only.get() || editing_files,
        );
        self.keyboard.set_modal(
            prompt_id.is_some()
                || self.title_editor.is_some()
                || self.disconnect_review.is_some()
                || editing_files
                || self.clipboard.is_open(),
        );
        let discovery = tab
            .discovery
            .as_ref()
            .and_then(|_| tab.session.durable_state());
        self.keyboard.watch_search_field(
            tab.discovery
                .as_ref()
                .zip(discovery.as_ref())
                .filter(|(_, state)| state.phase.is_browser() && prompt_id.is_none())
                .map(|(browser, state)| {
                    SearchField::Sessions(id, state.listing_revision, browser.query_revision)
                })
                .or_else(|| {
                    tab.files
                        .keyboard_search(tab.session.pane_id)
                        .filter(|_| prompt_id.is_none())
                }),
        );
        // A browser can pass through Connecting or Failed after Choose. Keep
        // keyboard input local until the verified remote PTY is attached.
        let can_send_keys = prompt_id.is_none()
            && !tab.device_busy
            && (tab.discovery.is_none()
                || discovery.is_some_and(|state| {
                    matches!(
                        state.phase,
                        w11term::ssh::DurablePhase::Attached | w11term::ssh::DurablePhase::Direct
                    )
                }));
        if changed || (tab.discovery.is_some() && self.keyboard.has_target() != can_send_keys) {
            self.keyboard.set_target(if can_send_keys {
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
        } else if tab.discovery.is_none() && !can_send_keys {
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
            Question::HostKey{host,port,host_key_alias,algorithm,fingerprint}=>("Trust this SSH host?".into(),"Accept and save",vec![
                TextBlock::new().text(match host_key_alias {
                    Some(alias) => format!("Connecting to {host}:{port}\nSave the host key as {alias}"),
                    None => format!("First connection to {host}:{port}"),
                }).text_wrapping(TextWrapping::Wrap).into(),
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
            Question::IdentityPassphrase { identity, retry } => {
                let instructions = if *retry {
                    "Could not unlock this key. Try the passphrase again."
                } else {
                    "Enter the passphrase to unlock this private key on this device."
                };
                let value = tab.answers.first().map_or("", |value| value.as_str());
                let field = PasswordBox::new()
                    .password(value)
                    .automation_name("Key passphrase")
                    .password_reveal_mode(PasswordRevealMode::Hidden)
                    .on_password_changed(context.callback(move |value| {
                        Message::PromptEdit(id, prompt_id, 0, value)
                    }));
                (
                    "Unlock SSH private key".into(),
                    "Unlock",
                    vec![
                        TextBlock::new().text(identity).text_wrapping(TextWrapping::Wrap).into(),
                        TextBlock::new().text(instructions).text_wrapping(TextWrapping::Wrap).into(),
                        field.into(),
                        CheckBox::new()
                            .automation_name("Keep this key unlocked for reconnects")
                            .is_checked(tab.remember_ssh_key)
                            .on_is_checked_changed(context.callback(move |remember| {
                                Message::PromptRememberSshKey(id, prompt_id, remember)
                            }))
                            .content("Keep this key unlocked for reconnects"),
                    ],
                )
            }
        };
        ContentDialog::new()
            .is_open(true)
            .title(title)
            .primary_button_text(primary)
            .close_button_text("Cancel")
            .on_closed(context.callback(move |result| Message::PromptReply(id, prompt_id, result)))
            .content(
                ScrollViewer::new().max_height(360.0).content(
                    StackPanel::new()
                        .max_width(500.0)
                        .spacing(12.0)
                        .keyed_children(
                            children
                                .into_iter()
                                .enumerate()
                                .map(|(i, v)| KeyedView::new(i.to_string(), v)),
                        ),
                ),
            )
    }
    fn find(&mut self, close: bool, direction: Option<isize>, context: &ComponentContext<Self>) {
        let opening = !close
            && direction.is_none()
            && matches!(self.tabs.active(), Some(AppTab { page: Page::Terminal(tab), .. }) if !tab.find.open);
        if opening && self.request_copy_mode_find(context) {
            return;
        }
        let selection = if opening {
            self.tabs.active().and_then(|entry| {
                let Page::Terminal(tab) = &entry.page else {
                    return None;
                };
                tab.selection
                    .borrow()
                    .as_ref()?
                    .find_target(&tab.session.frames.load())
            })
        } else {
            None
        };
        self.show_find(close, direction, selection, context);
    }

    fn show_find(
        &mut self,
        close: bool,
        direction: Option<isize>,
        selection: Option<w11term::interaction::FindSelection>,
        context: &ComponentContext<Self>,
    ) {
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
        if !close && !tab.find_available() {
            self.keyboard.set_search(false);
            return;
        }
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
            if let Some(selection) = selection {
                let target = Arc::new(selection);
                let (reply, rx) = tokio::sync::oneshot::channel();
                if tab
                    .session
                    .input
                    .send(Command::FindSelection(target.clone(), reply))
                    .is_ok()
                {
                    let epoch = tab.find.epoch;
                    let view = self.watch_epoch;
                    context.spawn_background(move |_| {
                        rx.blocking_recv()
                            .ok()
                            .flatten()
                            .map_or(Message::ControlIdle, |seed| {
                                Message::FindSeed(id, epoch, view, target, seed)
                            })
                    });
                }
            }
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
        let origin = self.paste_origin();
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
                            .on_click(context.callback(move |_| {
                                Message::PaneFind(origin, Shortcut::FindPrevious)
                            }))
                            .content(TextBlock::new().text("↑")),
                        Button::new()
                            .grid_column(2)
                            .style(ButtonStyle::Subtle)
                            .automation_name("Next match (Enter)")
                            .on_click(
                                context.callback(move |_| {
                                    Message::PaneFind(origin, Shortcut::FindNext)
                                }),
                            )
                            .content(TextBlock::new().text("↓")),
                        Button::new()
                            .grid_column(3)
                            .style(ButtonStyle::Subtle)
                            .automation_name("Close find (Escape)")
                            .on_click(
                                context.callback(move |_| {
                                    Message::PaneFind(origin, Shortcut::FindClose)
                                }),
                            )
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
        let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.active()
        else {
            return false;
        };
        let frame = tab.session.frames.load();
        self.copy_mode.borrow().as_ref().is_some_and(|mode| {
            Some(mode.pane) == self.tabs.active_id()
                && mode.state.valid(&frame)
                && mode.state.selection().is_some()
        }) || tab
            .selection
            .borrow()
            .as_ref()
            .is_some_and(|selection| selection.can_copy(&frame))
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
            let mut groups: Vec<_> = self
                .tabs
                .headers()
                .iter()
                .map(|tab| tab.group_id())
                .collect();
            if mode == TabSwitcherMode::Mru {
                // A tab's focused pane can move to another tab. The group keeps its history.
                groups.sort_by_cached_key(|group| {
                    self.mru_groups
                        .iter()
                        .position(|g| g == group)
                        .unwrap_or(usize::MAX)
                });
            }
            let index = groups
                .iter()
                .position(|id| Some(*id) == self.tabs.active_group_id())
                .unwrap_or(0);
            self.switcher = Some(TabSwitcher { groups, index });
        }
        if let Some(switcher) = &mut self.switcher
            && !switcher.groups.is_empty()
        {
            switcher.index = (switcher.index as isize + delta)
                .rem_euclid(switcher.groups.len() as isize) as usize;
        }
        self.keyboard.set_switcher(true);
    }
    fn finish_switcher(&mut self, commit: bool, context: &ComponentContext<Self>) {
        if let Some(switcher) = self.switcher.take()
            && commit
            && let Some(group) = switcher.groups.get(switcher.index)
        {
            self.select_switched_group(*group, context);
        }
        self.keyboard.set_switcher(false);
    }
    fn select_switched_group(&mut self, group: TabId, context: &ComponentContext<Self>) {
        if let Some(focused) = tab_title::switcher_leaf(&self.tabs.headers(), group) {
            self.tabs.select(focused);
            self.activate(context);
        }
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
        let keyboard_navigation = self
            .copy_mode
            .borrow()
            .as_ref()
            .is_some_and(|m| m.pane == id)
            || self.hints.borrow().as_ref().is_some_and(|h| h.pane == id);
        // Scrolling a background pane preserves the active pane's keyboard route and editors.
        if !keyboard_navigation
            && let Some(AppTab {
                page: Page::Terminal(tab),
                ..
            }) = self.tabs.get_mut(id)
            && tab.remote_mouse.held.is_none()
            && tab.clicked_link.is_none()
            && !tab.selection.borrow().as_ref().is_some_and(|s| s.dragging)
            && tab.surface.scrollbar.pointer(
                pointer,
                kind,
                |mark| {
                    let frame = tab.session.frames.load();
                    let retained = match mark {
                        Some(scrollback_bar::ScrollMark::FailedCommand(row)) => {
                            frame.failed_command_rows.contains(&row)
                        }
                        Some(scrollback_bar::ScrollMark::Bookmark(row)) => {
                            frame.scrollback_bookmarks.binary_search(&row).is_ok()
                        }
                        None => true,
                    };
                    if !retained {
                        return None;
                    }
                    frame.scrollback_view()
                },
                |view, row| {
                    let _ = tab.session.input.send(view.scroll_to(row));
                },
            )
        {
            if kind == MouseEventKind::Press {
                tab.gesture += 1;
            }
            if tab.hover.borrow_mut().take().is_some() {
                tab.surface.invalidator.invalidate();
            }
            if let Some(inspector) = &mut self.inspection {
                inspector.clear_pointer();
            }
            return;
        }
        if kind != MouseEventKind::Move {
            self.header.borrow_mut().take();
        }
        if kind == MouseEventKind::Press {
            self.keyboard.return_from_drawer();
        }
        if kind == MouseEventKind::Press
            && let Some(active) = self.tabs.active_id()
            && let Some(AppTab {
                page: Page::Terminal(tab),
                ..
            }) = self.tabs.get_mut(active)
            && tab.files.filter_editing()
        {
            tab.files.end_filter_edit();
            self.connection_ui(active);
        }
        if keyboard_navigation {
            return;
        }
        if kind == MouseEventKind::Press && self.tabs.active_id() != Some(id) {
            self.focus_pane(id, context);
        }
        self.keyboard.hover_pane(id);
        let active = self.tabs.active_id() == Some(id);
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
        if tab.reconcile_selection(&frame) && active {
            self.keyboard.set_selection(false);
        }
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
                if kind == MouseEventKind::Release {
                    tab.finish_mouse_leave();
                }
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
        let cursor_changed =
            self.appearance.read().unwrap().default_cursor_style != appearance.default_cursor_style;
        self.keyboard
            .set_preferences(appearance.interaction.clone());
        for entry in self.tabs.entries() {
            if let Page::Terminal(tab) = &entry.value.page {
                tab.session
                    .remote_clipboard
                    .set_policy(appearance.interaction.remote_clipboard_policy());
                if cursor_changed {
                    let _ = tab.session.input.send(Command::SetDefaultCursorShape(
                        appearance.default_cursor_style.shape(),
                    ));
                }
            }
        }
        *self.appearance.write().unwrap() = appearance.clone();
        self.refresh_remote_clipboard_focus();
        self.appearance_error = Appearance::path()
            .and_then(|path| appearance.save(&path))
            .err()
            .map(|e| format!("Settings could not be saved: {e:#}"));
    }
    fn remeasure_terminal_font(&mut self, appearance: &Appearance) -> bool {
        let panes = self
            .tabs
            .entries()
            .iter()
            .filter_map(|entry| match &entry.value.page {
                Page::Terminal(tab) => Some(tab),
                _ => None,
            });
        let pane_size = |tab: &LiveTab| appearance.terminal_font_size_for(tab.font_size_override());
        let sizes =
            std::iter::once(appearance.terminal_font_size()).chain(panes.clone().map(pane_size));
        let mut renderers = BTreeMap::new();
        for size in sizes {
            if let std::collections::btree_map::Entry::Vacant(entry) =
                renderers.entry(size.to_bits())
            {
                match Renderer::with_size(&appearance.font_family, appearance.ligatures, size) {
                    Ok(renderer) => {
                        entry.insert(renderer);
                    }
                    Err(error) => {
                        self.appearance_error = Some(format!("Unable to use font: {error}"));
                        return false;
                    }
                }
            }
        }
        for tab in panes {
            tab.remeasure_font(renderers.get_mut(&pane_size(tab).to_bits()).unwrap());
        }
        self.keyboard.set_selection(false);
        self.invalidator.invalidate();
        true
    }

    fn apply_appearance(&mut self, appearance: Appearance, context: &ComponentContext<Self>) {
        if self.remeasure_terminal_font(&appearance) {
            self.save_preferences(appearance);
            self.refresh_picker_font(context);
        }
    }

    fn update_windows_text_size(&mut self, factor: f64, context: &ComponentContext<Self>) {
        let mut appearance = self.appearance.read().unwrap().clone();
        let previous_size = appearance.terminal_font_size();
        appearance.set_windows_text_scale(factor);
        let remeasure = previous_size != appearance.terminal_font_size();
        if !remeasure || self.remeasure_terminal_font(&appearance) {
            *self.appearance.write().unwrap() = appearance;
            if remeasure {
                self.refresh_picker_font(context);
            }
        }
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
            TextBlock::new().text("Follow Windows text size").into(),
            ToggleSwitch::new().automation_name("Follow Windows text size").is_on(appearance.follow_windows_text_size)
                .on_toggled(context.callback(Message::FollowWindowsTextSize)).into(),
            TextBlock::new().text("Default cursor style").into(),
            ComboBox::new().width(260.0).automation_name("Default cursor style")
                .items_source(DefaultCursorStyle::CHOICES.iter().map(|(_, label)| *label))
                .selected_index(DefaultCursorStyle::CHOICES.iter().position(|(style, _)| *style == appearance.default_cursor_style))
                .on_selection_changed(context.callback(Message::DefaultCursorStyle)).into(),
            TextBlock::new().text("Cursor guide").into(),
            ComboBox::new().width(260.0).automation_name("Cursor guide")
                .items_source(CursorGuide::CHOICES.iter().map(|(_, label)| *label))
                .selected_index(CursorGuide::CHOICES.iter().position(|(mode, _)| *mode == appearance.cursor_guide))
                .on_selection_changed(context.callback(Message::CursorGuide)).into(),
            TextBlock::new().text("Applies to all tabs. Changes are saved automatically.").into(),
        ];
        let mut profiles: Vec<View> =
            vec![TextBlock::new().text("Profiles").font_size(20.0).into()];
        let order: Arc<[String]> = self.catalog.profile_order.as_slice().into();
        let startup = self.catalog.startup_profile.as_deref();
        let startup_choices: Vec<_> = std::iter::once((None, LAB.to_owned()))
            .chain(order.iter().map(|name| {
                let backend = if self.catalog.profiles.contains_key(name) {
                    "SSH"
                } else {
                    "Local"
                };
                (Some(name.as_str()), format!("{name} · {backend}"))
            }))
            .collect();
        let startup_label = startup_choices
            .iter()
            .find(|(name, _)| *name == startup)
            .map_or(LAB, |(_, label)| label.as_str());
        let startup_items = startup_choices
            .iter()
            .enumerate()
            .map(|(index, (_, label))| {
                MenuItem::item(format!("startup-profile-{index}"), label.clone())
            })
            .collect::<Vec<_>>();
        profiles.push(TextBlock::new().text("Startup profile").into());
        profiles.push(
            Button::new()
                .horizontal_alignment(HorizontalAlignment::Left)
                .automation_name(format!("Startup profile, {startup_label}"))
                .is_enabled(self.profile_error.is_none())
                .content(
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            TextBlock::new().text(startup_label),
                            FontIcon::new().glyph("\u{e70d}"),
                        )),
                )
                // A stable callback keeps the flyout mounted across Settings redraws.
                .menu(Menu::new(
                    startup_items,
                    context.callback(Message::StartupProfile),
                )),
        );
        profiles.push(
            TextBox::new()
                .automation_name("Filter profiles")
                .placeholder_text("Filter by profile name or SSH address")
                .text(&self.profile_filter)
                .on_text_changed(context.callback(Message::FilterProfiles))
                .into(),
        );
        let query = profile_query::Query::new(&self.profile_filter);
        let mut rows = Vec::with_capacity(order.len());
        for (index, name) in order.iter().enumerate() {
            let ssh = self.catalog.profiles.get(name);
            let endpoint = ssh.map(profile_query::address);
            let details = endpoint.as_deref().unwrap_or("Local");
            if query.rank(name, details).is_none() {
                continue;
            }
            let edit_name = name.clone();
            let edit = if ssh.is_some() {
                Button::new()
                    .automation_name(format!("Edit profile {name}"))
                    .on_click(context.callback(move |_| Message::EditProfile(edit_name.clone())))
            } else {
                Button::new()
                    .automation_name(format!("Edit local profile {name}"))
                    .on_click(context.callback(move |_| {
                        Message::LocalProfile(local_profile::Action::Open(Some(edit_name.clone())))
                    }))
            };
            let movement = |direction, glyph, delta, enabled| {
                let order = order.clone();
                let name = name.clone();
                title_tools::button(&format!("Move {name} {direction}"))
                    .is_enabled(enabled)
                    .on_click(context.callback(move |_| {
                        Message::MoveProfile(name.clone(), delta, order.clone())
                    }))
                    .content(TextBlock::new().font_size(16.0).text(glyph).tooltip_with(
                        title_tools::below(format!("Move {direction} in profile order")),
                    ))
            };
            let pinned = self.catalog.pinned_profiles.contains(name);
            let (pin_action, pin_glyph) = if pinned {
                (format!("Unpin {name} from New Tab"), "\u{e77a}")
            } else {
                (format!("Pin {name} to New Tab"), "\u{e718}")
            };
            let pin_name = name.clone();
            let pin = title_tools::button(&pin_action)
                .is_enabled(self.profile_error.is_none())
                .on_click(
                    context.callback(move |_| Message::SetProfilePin(pin_name.clone(), !pinned)),
                )
                .content(FontIcon::new().glyph(pin_glyph))
                .tooltip_with(title_tools::below(pin_action));
            let label = format!("{name} · {details}");
            rows.push(KeyedView::new(
                name.clone(),
                Grid::new()
                    .columns([GridLength::STAR, GridLength::Auto])
                    .column_spacing(8.0)
                    .children((
                        TextBlock::new()
                            .text(&label)
                            .vertical_alignment(VerticalAlignment::Center)
                            .text_trimming(TextTrimming::CharacterEllipsis)
                            .tooltip_with(title_tools::below(label)),
                        StackPanel::new()
                            .grid_column(1)
                            .orientation(Orientation::Horizontal)
                            .spacing(2.0)
                            .children((
                                edit.content("Edit…"),
                                pin,
                                movement("up", "↑", -1, index > 0),
                                movement("down", "↓", 1, index + 1 < order.len()),
                            )),
                    )),
            ));
        }
        if rows.is_empty() {
            profiles.push(
                TextBlock::new()
                    .text(if query.is_empty() {
                        "No saved profiles yet."
                    } else {
                        "No profiles match this filter."
                    })
                    .into(),
            );
        } else {
            if !query.is_empty() {
                profiles.push(
                    TextBlock::new()
                        .text(format!(
                            "Showing {} of {} profiles",
                            rows.len(),
                            order.len()
                        ))
                        .into(),
                );
            }
            profiles.push(StackPanel::new().spacing(8.0).keyed_children(rows));
        }
        profiles.push(
            Button::new()
                .on_click(
                    context.callback(|_| Message::LocalProfile(local_profile::Action::Open(None))),
                )
                .content("New local shell profile…"),
        );
        for error in self.profile_error.iter().chain(&self.profile_action_error) {
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
                .on_click(context.callback(|_| Message::SavedCommands(saved_commands::Edit::Show)))
                .content("Saved commands"),
        );
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
        children.push(
            Button::new()
                .on_click(
                    context.callback(|_| Message::WordDelimiters(word_delimiters::Edit::Show)),
                )
                .content("Word selection delimiters…"),
        );
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
                InteractionField::LargePasteWarning,
                "Warn about large pastes",
                appearance.interaction.large_paste_warning,
            ),
            (
                InteractionField::Snap,
                "Snap window resizing to the character grid",
                appearance.interaction.snap_to_grid_on_resize,
            ),
            (
                InteractionField::ApplicationTabTitles,
                "Use application titles for tabs",
                appearance.interaction.application_tab_titles,
            ),
            (
                InteractionField::FailedCommandMarks,
                "Mark failed commands on the scrollbar",
                appearance.interaction.failed_command_marks,
            ),
            (
                InteractionField::StickyCommandContext,
                "Show command context in scrollback",
                appearance.interaction.sticky_command_context,
            ),
            (
                InteractionField::VisualBell,
                "Show a visual bell",
                appearance.interaction.visual_bell,
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
            (
                InteractionField::Osc52ForegroundOnly,
                "Limit OSC 52 to the foreground pane",
                appearance.interaction.osc52_foreground_only,
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
        children.push(TextBlock::new().text("Multiline paste warning").into());
        children.push(
            ComboBox::new()
                .automation_name("Multiline paste warning")
                .items_source(MULTILINE_PASTE_CHOICES.map(|(_, label)| label))
                .selected_index(
                    MULTILINE_PASTE_CHOICES.iter().position(|(mode, _)| {
                        *mode == appearance.interaction.multi_line_paste_warning
                    }),
                )
                .on_selection_changed(context.callback(Message::MultilinePasteWarning))
                .into(),
        );
        children.push(TextBlock::new().text("Automatic warns when the application will receive multiple lines without bracketed paste. Raw paste always sends text without brackets.").text_wrapping(TextWrapping::Wrap).into());
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
        let titles = tab_title::followed_headers(
            &self.tabs,
            self.appearance
                .read()
                .unwrap()
                .interaction
                .application_tab_titles,
            |tab| tab.pane_name().is_some(),
        );
        self.registry
            .replace(self.tabs.entries().iter().filter_map(|entry| {
                let Page::Terminal(tab) = &entry.value.page else {
                    return None;
                };
                tab.session.frames.watch_window(Some((
                    &self.inbox,
                    tab.session.pane_id,
                    titles.contains(&entry.id),
                )));
                tab.session.events.connect_inbox(
                    tab.session.pane_id,
                    &entry.value.label,
                    &self.inbox,
                );
                Some(Pane {
                    id: tab.session.pane_id,
                    tab_id: self.tabs.group_id(entry.id)?,
                    pane_name: tab.pane_name.as_ref().map(|name| name.as_str().to_owned()),
                    tab_title: self
                        .tabs
                        .title_for(entry.id)
                        .map(|title| title.as_str().to_owned()),
                    visible: self.tabs.visible(entry.id),
                    read_only: tab.read_only.get(),
                    label: entry.value.label.clone(),
                    active: self.tabs.active_id() == Some(entry.id),
                    input: tab.session.input.clone(),
                    capture: tab
                        .font_size_override()
                        .map(|size| pane_text_size::capture(self.appearance.clone(), Some(size))),
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

    fn restart_exited_pane(
        &mut self,
        exit: ExitedPane,
        login_permits: Option<Arc<tokio::sync::Semaphore>>,
    ) -> anyhow::Result<bool> {
        let Some(id) = self.tab_for_pane(exit.pane) else {
            return Ok(false);
        };
        let Some(Page::Terminal(live)) = self.tabs.get_mut(id).map(|tab| &mut tab.page) else {
            return Ok(false);
        };
        if live.session.events.restart_token() != Some(exit.token) {
            return Ok(false);
        }
        if let Some(permits) = login_permits {
            live.session.restart_group_ssh(exit.token, permits)?;
        } else {
            live.session.restart_exited(exit.token)?;
        }
        self.clipboard.connection_changed(exit.pane, 0, true);
        live.reset_transport_input(&self.keyboard);
        Ok(true)
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

    fn change_pointer_pane(&mut self, next: Option<TabId>) {
        if self.pointer_over == next {
            return;
        }
        if let Some(old) = std::mem::replace(&mut self.pointer_over, next)
            && let Some(AppTab {
                page: Page::Terminal(tab),
                ..
            }) = self.tabs.get_mut(old)
        {
            tab.mouse_left(crate::input::modifiers());
        }
        if let Some(next) = next
            && let Some(AppTab {
                page: Page::Terminal(tab),
                ..
            }) = self.tabs.get_mut(next)
        {
            tab.remote_mouse.pending_leave = None;
        }
    }

    fn active_terminal_group(&self) -> Option<TabId> {
        match &self.tabs.active()?.page {
            Page::Terminal(_) => self.tabs.active_group_id(),
            _ => None,
        }
    }

    fn active_tab_is_pinned(&self) -> bool {
        self.tabs
            .active_group_id()
            .is_some_and(|group| self.tabs.group_is_pinned(group))
    }

    fn can_close_active_pane(&self) -> bool {
        self.active_terminal_group().is_some()
            && (!self.active_tab_is_pinned()
                || matches!(
                    self.tabs.active_layout(),
                    Some(w11term::pane_layout::Layout::Split { .. })
                ))
    }

    fn cancel_frame_tasks(&mut self) {
        self.watch_epoch += 1;
        self.rotation_hover = None;
        self.dividers.borrow_mut().invalidate();
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
                .map(|inspector| inspector.current.clone());
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

    fn refresh_notification_visibility(&self) {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetForegroundWindow, IsIconic, IsWindowVisible,
        };
        let (foreground, shown) = self.keyboard.hwnd().map_or((false, false), |hwnd| unsafe {
            (
                GetForegroundWindow() == hwnd,
                IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool(),
            )
        });
        for entry in self.tabs.entries() {
            if let Page::Terminal(tab) = &entry.value.page {
                let visible = shown && self.tabs.visible(entry.id);
                let focused = visible && foreground && self.tabs.active_id() == Some(entry.id);
                tab.session
                    .events
                    .set_notification_visibility(focused, visible);
            }
        }
    }

    fn refresh_remote_clipboard_focus(&mut self) {
        use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

        let active = self.keyboard.hwnd().and_then(|hwnd| {
            let Page::Terminal(tab) = &self.tabs.active()?.page else {
                return None;
            };
            (unsafe { GetForegroundWindow() } == hwnd)
                .then(|| (tab.session.remote_clipboard.clone(), hwnd.0 as usize))
        });
        if let Some(previous) = self.focused_osc52.take()
            && active
                .as_ref()
                .is_none_or(|(access, _)| !Arc::ptr_eq(&previous, access))
        {
            previous.set_focused_window(None);
        }
        if let Some((access, window)) = active {
            access.set_focused_window(Some(window));
            self.focused_osc52 = Some(access);
        }
    }

    fn sync_active_window_profile(&mut self, group_changed: bool) {
        let profile = self
            .tabs
            .active()
            .and_then(AppTab::profile_name)
            .filter(|name| {
                matches!(
                    self.tabs.active_layout(),
                    Some(w11term::pane_layout::Layout::Leaf(_))
                ) && (self.catalog.profiles.contains_key(*name)
                    || self.catalog.local.contains_key(*name))
            })
            .map(str::to_owned);
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
    }

    fn refresh_wheel_panes(&self) {
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
    }

    fn activate(&mut self, context: &ComponentContext<Self>) {
        self.update_host_group_timer(context);
        if self.pointer_over.is_some_and(|id| !self.tabs.visible(id)) {
            self.change_pointer_pane(None);
        }
        self.pane_drop.cancel();
        self.cancel_directory_copy();
        self.refresh_remote_clipboard_focus();
        self.refresh_notification_visibility();
        self.tabs.record_pane_focus(AppTab::visited_terminal);
        self.clipboard.input_origin.set(self.paste_origin());
        self.discard_inactive_paste();
        self.bind_folder_shares();
        self.activate_folder_panel(context);
        self.watch_forward_panel(context);
        self.title_editor = None;
        self.disconnect_review = None;
        self.cancel_inactive_entry_dialog();
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
        let group = self.tabs.active_id().and_then(|id| self.tabs.group_id(id));
        let previous_group = self.active_group.replace(group);
        let group_changed = previous_group != group;
        if group_changed
            && let Some(previous) = previous_group
            && let Some(tab) = self.tabs.get_mut(previous)
        {
            match &mut tab.page {
                Page::NewProfile(draft) => draft.folder_revoke.cancel_confirmation(),
                Page::HostGroup(group) => group.deactivated(),
                _ => {}
            }
        }
        self.sync_active_window_profile(group_changed);
        if let Some(group) = group {
            self.mru_groups.retain(|g| *g != group);
            self.mru_groups.insert(0, group);
        }
        self.cancel_frame_tasks();
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
        } else if self.initial_grid.get().is_some()
            && self
                .initial_grid
                .pane()
                .is_some_and(|pane| Some(pane) != self.tabs.active_id())
        {
            self.initial_grid.set(None);
        }
        for entry in self.tabs.entries() {
            if let Page::Terminal(tab) = &entry.value.page {
                if self.tabs.active_id() == Some(entry.id) {
                    self.metrics = tab.surface.metrics.clone();
                    self.invalidator = tab.surface.invalidator.clone();
                } else {
                    tab.surface.pause_cursor();
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
            self.keyboard.watch_file_drawer(None);
            self.keyboard.watch_search_field(None);
        }
        self.keyboard.set_target(target);
        self.sync_ime_surface();
        self.refresh_wheel_panes();
        if let Some(id) = self.tabs.active_id() {
            self.connection_ui(id);
            self.discovery_action(session_browser::Action::Focus(id, 0), context);
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
        // A startup placement can arrive after a control resize has already settled.
        if self.incoming.is_some()
            || !self.native_bound
            || self.initial_grid.get().is_some()
            || self.initial_grid.pane().is_some()
        {
            return;
        }
        if let Some(name) = &self.window_profile {
            let windows = self.windows.borrow();
            if windows.fullscreen() {
                return;
            }
            if windows.restore(name).is_ok()
                && matches!(self.tabs.active().map(|tab| &tab.page), Some(Page::Terminal(tab))
                    if tab.pane_font_size.get().is_none())
                && let Some(placement) = windows.profiles.get(name)
                && let Some((cols, rows)) = placement.cols.zip(placement.rows)
                && (1..=1000).contains(&cols)
                && (1..=500).contains(&rows)
            {
                self.initial_grid.set(Some((cols, rows)));
            }
        }
    }

    fn close_pane_entry(&mut self, id: TabId, context: &ComponentContext<Self>) {
        if !matches!(
            self.tabs.layout_for(id),
            Some(w11term::pane_layout::Layout::Split { .. })
        ) {
            self.close_user_tab(id, context);
            return;
        }
        let selected = self.tabs.active_id() == Some(id);
        let in_active_tab = self.tabs.group_id(id) == self.tabs.active_group_id();
        self.remember_closed_pane(id);
        if selected {
            self.keyboard.set_target(None);
        }
        if self.pointer_over == Some(id) {
            self.change_pointer_pane(None);
        }
        self.tabs.remove(id);
        self.sync_selection(context);
        if selected {
            self.activate(context);
        } else if in_active_tab {
            // Refresh layout-dependent state without retargeting the keyboard
            // or restarting selection and find in the surviving active pane.
            self.pane_drop.cancel();
            self.cancel_frame_tasks();
            self.sync_active_window_profile(false);
            self.refresh_wheel_panes();
            self.refresh_notification_visibility();
            let survivors = self
                .tabs
                .active_layout()
                .expect("split has a surviving pane")
                .leaves();
            self.panes_rearranged(survivors);
            self.watch_frames(context);
            self.refresh_open_commands();
        } else {
            self.publish_panes();
            self.refresh_open_commands();
        }
    }

    fn control_close_pane(
        &mut self,
        pane: u64,
        context: &ComponentContext<Self>,
    ) -> anyhow::Result<serde_json::Value> {
        let id = self
            .tab_for_pane(pane)
            .with_context(|| format!("unknown terminal pane {pane}"))?;
        let tab_closed = matches!(
            self.tabs.layout_for(id),
            Some(w11term::pane_layout::Layout::Leaf(_))
        );
        if tab_closed {
            let group = self.tabs.group_id(id).context("the pane has no tab")?;
            anyhow::ensure!(
                !self.tabs.group_is_pinned(group),
                "the pane's tab is pinned"
            );
            anyhow::ensure!(
                self.tabs.entries().len() > 1,
                "Use the window controls to close its last tab"
            );
        }
        self.close_pane_entry(id, context);
        Ok(serde_json::json!({"pane":pane,"tab_closed":tab_closed}))
    }

    fn close_tab(&mut self, id: TabId, context: &ComponentContext<Self>) {
        self.close_tabs(std::iter::once(id), context);
    }

    fn close_tabs(
        &mut self,
        entries: impl IntoIterator<Item = TabId>,
        context: &ComponentContext<Self>,
    ) {
        let mut closed = false;
        let mut active_closed = false;
        for id in entries {
            let Some(group) = self.tabs.group_id(id) else {
                continue;
            };
            if self.active_group.get() == Some(group) {
                active_closed = true;
                self.keyboard.set_target(None);
                self.cancel_frame_tasks();
            }
            self.tabs.remove_group(id);
            self.mru_groups.retain(|g| *g != group);
            closed = true;
        }
        if !closed {
            return;
        }
        self.publish_panes();
        if self.tabs.entries().is_empty() {
            let _ = context.window().request_close();
        } else {
            self.sync_selection(context);
            if active_closed {
                self.activate(context);
            } else {
                self.refresh_open_commands();
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
        let tab = self.make_live_tab(label, source, profile);
        self.insert_tab(tab, context);
    }

    fn make_live_tab(&self, label: String, source: Source, profile: Option<String>) -> AppTab {
        live_tab(
            label,
            source,
            self.appearance.read().unwrap().pane_preferences(),
        )
        .with_profile(profile)
    }

    fn insert_tab(&mut self, tab: AppTab, context: &ComponentContext<Self>) {
        self.tabs.insert(tab);
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
            self.appearance.read().unwrap().pane_preferences(),
            connection,
            None,
        )
        .with_profile(profile_name);
        self.insert_split(id, leaf, axis, context);
    }

    fn insert_split(
        &mut self,
        id: TabId,
        leaf: AppTab,
        axis: Axis,
        context: &ComponentContext<Self>,
    ) {
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
        let name = self.catalog.copy_name(&base.name);
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
    fn connect_once(&mut self, id: TabId, context: &ComponentContext<Self>) {
        let Some(tab) = self.tabs.get_mut(id) else {
            return;
        };
        let Page::NewProfile(draft) = &mut tab.page else {
            return;
        };
        if !matches!(draft.origin, DraftOrigin::New) || draft.generating {
            return;
        }
        let prepared = draft.ssh_options().and_then(|mut options| {
            options.prepare_profile_connection(self.store.path())?;
            Ok(options)
        });
        match prepared {
            Ok(options) => {
                let source = Source::Ssh(options);
                *tab = live_tab(
                    source_label(&source),
                    source,
                    self.appearance.read().unwrap().pane_preferences(),
                );
                self.sync_selection(context);
                self.activate(context);
            }
            Err(error) => draft.error = Some(format!("{error:#}")),
        }
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
            let options = draft.ssh_options()?;
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
                            self.appearance.read().unwrap().pane_preferences(),
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
        arrangement: PaneArrangement,
        directory_split: Option<directory_split::Origin>,
        previous: Option<previous_pane::Target>,
        recordings: recordings::Choices,
    ) -> Callback<String> {
        let pane = self.tabs.active().and_then(AppTab::pane_id);
        let pending_ssh = self.tabs.active().and_then(AppTab::pending_pane_attempt);
        let pending_retry = self.tabs.active().and_then(AppTab::pending_pane_retry);
        let disconnect_ssh = self.disconnect_target();
        let paste = self.paste_origin();
        let group = self.tabs.active_group_id();
        let mut cached = self.caret_menu_binding.borrow_mut();
        let closed_group = self.closed_tabs.back().map(|closed| closed.group);
        let closed_pane = self.closed_pane.as_ref().map(|closed| closed.id);
        let forward = self.forward_origin();
        let broadcast = self.broadcast_offer();
        let close_other_tabs = self.bulk_close_offer(bulk_tab_close::Scope::Other);
        let close_tabs_right = self.bulk_close_offer(bulk_tab_close::Scope::Right);
        if let Some(binding) = cached.as_ref()
            && binding.broadcast == broadcast
            && binding.close_other_tabs == close_other_tabs
            && binding.close_tabs_right == close_tabs_right
            && binding.previous == previous
            && binding.forward == forward
            && binding.pane == pane
            && binding.paste == paste
            && binding.recordings == recordings
            && binding.group == group
            && binding.closed_group == closed_group
            && binding.closed_pane == closed_pane
            && binding.arrangement == arrangement
            && binding.directory_split == directory_split
            && binding.exited == exited
            && binding.pending_ssh == pending_ssh
            && binding.pending_retry == pending_retry
            && binding.disconnect_ssh == disconnect_ssh
        {
            return binding.on_click.clone();
        }
        // Reactor replaces a flyout when its callback changes. Keep the same
        // callback while repainting the title bar for the same action target.
        let split_target = directory_split.clone();
        let offered = arrangement.clone();
        let offered_broadcast = broadcast.clone();
        let offered_close_other = close_other_tabs.clone();
        let offered_close_right = close_tabs_right.clone();
        let on_click = context.callback(move |label: String| match label.as_str() {
            label if pane_broadcast::is_label(label) => offered_broadcast
                .clone()
                .filter(|action| action.label() == label)
                .map_or(Message::ControlIdle, Message::BroadcastTyping),
            label if let Some(scheme) = pane_colors::from_menu(label) => pane
                .map_or(Message::ControlIdle, |id| {
                    Message::PaneColorScheme(id, scheme)
                }),
            local_forwards::LABEL => local_forwards::open(forward),
            title_tools::SHOW_FILES | title_tools::HIDE_FILES => pane
                .map_or(Message::ControlIdle, |id| {
                    Message::SetTitleFiles(id, label == title_tools::SHOW_FILES)
                }),
            tab_title::RENAME_PANE => tab_title::show_pane(pane.zip(group)),
            live_profile::SAVE => live_profile::open(paste),
            directory_split::OPEN_EXPLORER => {
                directory_split::explorer_message(split_target.as_ref())
            }
            label if let Some(color) = tab_colors::from_menu(label) => group
                .map_or(Message::ControlIdle, |group| {
                    Message::SetTabColor(group, color)
                }),
            label if let Some(scope) = bulk_tab_close::Scope::from_label(label) => match scope {
                bulk_tab_close::Scope::Other => &offered_close_other,
                bulk_tab_close::Scope::Right => &offered_close_right,
            }
            .clone()
            .map_or(Message::ControlIdle, Message::CloseTabs),
            label if let Some(pinned) = tab_pin_command(label) => {
                group.map_or(Message::ControlIdle, |id| Message::SetTabPinned(id, pinned))
            }
            folder_shares::LABEL => pane.map_or(Message::ControlIdle, |id| {
                Message::FolderShare(folder_shares::Action::Open(id))
            }),
            text_export::SCROLLBACK_LABEL => {
                Message::Export(text_export::Action::StartScrollback(pane))
            }
            text_export::COPY_VIEWPORT_LABEL => {
                Message::Export(text_export::Action::CopyPaneText(pane))
            }
            label if let Some(destination) = text_export::ImageDestination::from_label(label) => {
                Message::Export(text_export::Action::StartPaneImage(pane, destination))
            }
            value if recordings::is_command(value) => recordings::invoke(recordings.target(value)),
            RESTART_EXITED_PANE => exited.map_or(Message::ControlIdle, Message::RestartExited),
            STOP_SSH_ATTEMPT => pending_ssh.map_or(Message::ControlIdle, Message::StopSshAttempt),
            RETRY_SSH_NOW => pending_retry.map_or(Message::ControlIdle, Message::RetrySshNow),
            disconnect_ssh::LABEL => disconnect_ssh.map_or(Message::ControlIdle, |target| {
                Message::DisconnectSsh(disconnect_ssh::Action::Review(target))
            }),
            "Paste" => Message::Clipboard(clipboard_paste::Action::Start(paste, false)),
            "Paste raw (no bracketed paste)" => {
                Message::Clipboard(clipboard_paste::Action::Start(paste, true))
            }
            label if let Some(enabled) = attention::long_command_toggle(label) => {
                attention::set_long_commands(paste, enabled)
            }
            _ => match tab_menu_message(label) {
                Message::Shortcut(
                    shortcut @ (Shortcut::PanePicker
                    | Shortcut::FindTab
                    | Shortcut::FindCommandHistory
                    | Shortcut::FindAllPanes
                    | Shortcut::CommandPalette
                    | Shortcut::MovePaneToExistingTab),
                ) => Message::PickerFromMenu(shortcut),
                Message::Shortcut(Shortcut::ReopenClosedTab) => {
                    Message::ReopenClosedTab(closed_group)
                }
                Message::Shortcut(Shortcut::ReopenClosedPane) => {
                    Message::ReopenClosedPane(closed_pane)
                }
                Message::Shortcut(Shortcut::FocusPreviousPane) => previous_pane::message(previous),
                Message::Shortcut(Shortcut::SwapPane(direction)) => offered.message(direction),
                Message::Shortcut(Shortcut::RotateSplit) => offered.rotate(),
                Message::Shortcut(Shortcut::BalancePanes) => offered.balance(),
                Message::Shortcut(shortcut)
                    if let Some(destination) = directory_split::destination(shortcut) =>
                {
                    directory_split::message(destination, split_target.as_ref())
                }
                Message::Shortcut(Shortcut::CopyCurrentDirectory) => {
                    directory_split::copy_message(split_target.as_ref())
                }
                Message::Shortcut(Shortcut::ClearScrollback) => {
                    pane.map_or(Message::ControlIdle, Message::ClearScrollback)
                }
                Message::Shortcut(Shortcut::SelectAll) => Message::SelectAll(paste),
                Message::Shortcut(Shortcut::Copy) => Message::CopySelection(paste),
                message => bind_tab_action(message, group),
            },
        });
        *cached = Some(CaretMenuBinding {
            broadcast,
            close_other_tabs,
            close_tabs_right,
            recordings,
            previous,
            closed_group,
            closed_pane,
            forward,
            directory_split,
            pane,
            paste,
            group,
            arrangement,
            exited,
            pending_ssh,
            pending_retry,
            disconnect_ssh,
            on_click: on_click.clone(),
        });
        on_click
    }

    fn header_view(&self, context: &ViewContext<Self>, application_titles: bool) -> View {
        let mut progress = BTreeMap::new();
        let mut programs: BTreeMap<u64, (u64, Arc<wezterm_term::ProgramStatus>)> = BTreeMap::new();
        for entry in self.tabs.entries() {
            if let Page::Terminal(pane) = &entry.value.page {
                let group_id = entry.group_id();
                let (next, status) = pane.session.events.terminal_activity();
                if next != wezterm_term::Progress::None {
                    let group = progress.entry(group_id).or_default();
                    *group = taskbar::most_important(*group, next);
                }
                if let Some(status) = status
                    && programs
                        .get(&group_id)
                        .is_none_or(|(_, old)| status.state > old.state)
                {
                    programs.insert(group_id, (entry.id, status));
                }
            }
        }
        if let Some(hwnd) = self.keyboard.hwnd() {
            let state = progress
                .values()
                .copied()
                .chain(programs.values().map(|(_, s)| taskbar::program_progress(s)))
                .fold(wezterm_term::Progress::None, taskbar::most_important);
            self.taskbar.borrow_mut().show(hwnd, state);
        }
        let layout = title_tools::Layout::new(self.width, self.tabs.headers().len());
        let tab_strip = TabView::new()
            .height(44.0)
            .width(layout.tab_width)
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
                let epoch = self.selection_epoch;
                let pending = self.selection_pending;
                context.callback(move |order| {
                    if pending {
                        Message::ControlIdle
                    } else {
                        Message::Reorder(order, selected, epoch)
                    }
                })
            })
            .collection_slot(
                TabViewSlot::TabItems,
                self.tabs.headers().into_iter().map(|tab| {
                    let group = tab.group_id();
                    let pinned = self.tabs.group_is_pinned(group);
                    let title = self.tab_label(tab.id, application_titles);
                    let legacy = progress.get(&group).copied().unwrap_or_default();
                    let program = programs
                        .get(&group)
                        .filter(|(_, s)| taskbar::prefer_program(s, legacy));
                    let label = match program {
                        Some((_, status)) => taskbar::program_tab_header(title.as_ref(), status),
                        None => taskbar::tab_header(title.as_ref(), legacy),
                    };
                    let tooltip = match program {
                        Some((pane, status)) => taskbar::program_tooltip(&label, *pane, status),
                        None => label.clone(),
                    };
                    let color =
                        self.tabs
                            .group_color(group)
                            .unwrap_or_else(|| match &tab.value.page {
                                Page::Terminal(live) => live.source.tab_color(),
                                _ => TabColor::None,
                            });
                    let (header, name) = tab_header_content(label, pinned, color);
                    KeyedView::new(
                        group.to_string(),
                        TabViewItem::new()
                            .header(header)
                            .automation_name(name)
                            .tag(tab.id.to_string())
                            .is_closable(!pinned)
                            .tooltip_with(Tooltip::rich(
                                TextBlock::new()
                                    .max_width(420.0)
                                    .text_wrapping(TextWrapping::Wrap)
                                    .text(tooltip),
                            )),
                    )
                }),
            );
        let plus = title_tools::new_tab_button(self.profile_items(), context);
        let exited = self.tabs.active().and_then(AppTab::exited_pane);
        let arrangement = self.pane_arrangement();
        let directory_split = self.directory_split_origin();
        let previous = previous_pane::target(&self.tabs);
        let recordings = self.recording_choices();
        let menu_callback = self.caret_menu_callback(
            context,
            exited,
            arrangement.clone(),
            directory_split.clone(),
            previous,
            recordings,
        );
        let more = title_tools::button("Tab menu")
            .automation_id("w11term-title-more")
            .content(
                FontIcon::new()
                    .glyph("\u{e712}")
                    .tooltip_with(title_tools::below("More actions")),
            )
            .menu(Menu::new(
                self.caret_items(
                    exited.is_some(),
                    &arrangement,
                    directory_split.as_ref(),
                    previous,
                    recordings,
                ),
                menu_callback.clone(),
            ));
        let tools = self.title_tools(&layout, &arrangement, menu_callback, context);
        let right_header = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .vertical_alignment(VerticalAlignment::Center)
            .children((plus, more, tools));
        TitleBar::new()
            .preferred_height(WindowTitleBarHeight::Tall)
            .slots([
                SlotView::new(
                    TitleBarSlot::Content,
                    Grid::new()
                        // Keep the tabs left aligned within the space beside the native RightHeader.
                        .width(layout.content_width)
                        .columns([GridLength::Auto, GridLength::STAR])
                        .children((
                            tab_strip,
                            Border::new().grid_column(1).content(
                                self.recording_indicator().map_or_else(
                                    || Border::new().into(),
                                    |message| -> View {
                                        TextBlock::new()
                                            .vertical_alignment(VerticalAlignment::Center)
                                            .automation_name(&message)
                                            .text(&message)
                                            .text_trimming(TextTrimming::CharacterEllipsis)
                                            .tooltip_with(title_tools::below(message))
                                    },
                                ),
                            ),
                        )),
                ),
                SlotView::new(TitleBarSlot::RightHeader, right_header),
            ])
    }

    fn profile_items(&self) -> Vec<MenuItem> {
        let mut menu = vec![
            MenuItem::item("find-profile", command_palette::FIND_PROFILE_LABEL),
            MenuItem::separator("profile-search-end"),
        ];
        let profile_item = |name: &String| {
            if let Some(ssh) = self.catalog.profiles.get(name) {
                MenuItem::item(format!("profile:{name}"), profile_label(name, ssh))
            } else {
                MenuItem::item(format!("local:{name}"), format!("{name} · Local"))
            }
        };
        let (pinned, others): (Vec<_>, Vec<_>) = self
            .catalog
            .profile_order
            .iter()
            .partition(|name| self.catalog.pinned_profiles.contains(*name));
        if pinned.is_empty() {
            menu.extend(others.into_iter().map(profile_item));
        } else {
            menu.extend(pinned.into_iter().map(profile_item));
            if !others.is_empty() {
                menu.push(MenuItem::submenu(
                    "other-profiles",
                    "Other profiles",
                    others.into_iter().map(profile_item),
                ));
            }
        }
        if self.profile_error.is_some() {
            menu.push(MenuItem::disabled("error", "Could not load profiles"));
        }
        menu.extend([
            MenuItem::separator("profiles-end"),
            MenuItem::item("quick-connect", quick_connect::LABEL),
            MenuItem::item("new-profile", NEW_PROFILE),
            MenuItem::item("lab", LAB),
        ]);
        menu
    }

    fn caret_items(
        &self,
        can_restart: bool,
        arrangement: &PaneArrangement,
        directory: Option<&directory_split::Origin>,
        previous: Option<previous_pane::Target>,
        recordings: recordings::Choices,
    ) -> Vec<MenuItem> {
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
            self.binding_menu("workspaces", "Workspaces…", KeyAction::Workspaces),
            MenuItem::item("ssh-host-group", host_groups::LABEL),
            MenuItem::item("sftp-transfers", sftp_transfers::LABEL),
            MenuItem::item("active-port-forwards", local_forwards::overview::LABEL),
            MenuItem::item("inbox", "Attention inbox"),
            MenuItem::item("settings", "Settings"),
            self.clipboard_menu(),
            MenuItem::separator("window-actions"),
            self.binding_menu(
                "command-palette",
                "Command palette…",
                KeyAction::CommandPalette,
            ),
            self.pane_menu(can_restart, arrangement, directory, previous.is_some()),
            command_menu_item("rename-tab", "Rename tab…", self.move_source().is_some()),
            tab_colors::menu(
                self.active_terminal_group().is_some(),
                self.tabs
                    .active_group_id()
                    .and_then(|group| self.tabs.group_color(group)),
            ),
            command_menu_item(
                "pin-tab",
                if self.active_tab_is_pinned() {
                    UNPIN_TAB
                } else {
                    PIN_TAB
                },
                self.active_terminal_group().is_some(),
            ),
            self.duplicate_tab_item(),
            command_menu_item(
                "close-other-tabs",
                bulk_tab_close::Scope::Other.label(),
                self.bulk_close_offer(bulk_tab_close::Scope::Other)
                    .is_some(),
            ),
            command_menu_item(
                "close-tabs-right",
                bulk_tab_close::Scope::Right.label(),
                self.bulk_close_offer(bulk_tab_close::Scope::Right)
                    .is_some(),
            ),
            self.binding_menu_enabled(
                "reopen_closed_tab",
                KeyAction::ReopenClosedTab.binding().label,
                KeyAction::ReopenClosedTab,
                !self.closed_tabs.is_empty(),
            ),
            self.binding_menu_enabled(
                KeyAction::ReopenClosedPane.binding().id,
                KeyAction::ReopenClosedPane.binding().label,
                KeyAction::ReopenClosedPane,
                self.closed_pane.is_some(),
            ),
            self.binding_menu_enabled(
                "move-tab-left",
                "Move tab left",
                KeyAction::MoveTabLeft,
                self.tabs.can_move_active_tab(Direction::Left),
            ),
            self.binding_menu_enabled(
                "move-tab-right",
                "Move tab right",
                KeyAction::MoveTabRight,
                self.tabs.can_move_active_tab(Direction::Right),
            ),
            self.scrollback_find_item(),
            self.binding_menu("pane-picker", "Find pane…", KeyAction::PanePicker),
            self.binding_menu(
                "find-tab",
                command_palette::FIND_TAB_LABEL,
                KeyAction::FindTab,
            ),
            self.binding_menu(
                "find-all-panes",
                pane_picker::FIND_TEXT_LABEL,
                KeyAction::FindAllPanes,
            ),
            self.binding_menu(
                "find-command-history",
                command_palette::FIND_COMMAND_HISTORY_LABEL,
                KeyAction::FindCommandHistory,
            ),
            MenuItem::item(
                "find-running-commands",
                command_palette::FIND_RUNNING_COMMANDS_LABEL,
            ),
            MenuItem::item("new-ssh-group-tab", host_groups::NEW_TAB_LABEL),
            recordings.menu(),
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
                "local-forward",
                local_forwards::LABEL,
                self.forward_origin().is_some(),
            ),
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
                "save-live-ssh-profile",
                live_profile::SAVE,
                self.saveable_ssh_origin().is_some(),
            ),
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
        self.binding_menu_enabled(key, label, action, true)
    }
    fn binding_menu_enabled(
        &self,
        key: &str,
        label: &str,
        action: KeyAction,
        enabled: bool,
    ) -> MenuItem {
        let description = self
            .appearance
            .read()
            .unwrap()
            .shortcuts
            .get(action.binding())
            .map_or_else(|| label.to_owned(), |chord| format!("{label}    {chord}"));
        command_menu_item(key, description, enabled)
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
    fn panes_rearranged(&mut self, panes: impl IntoIterator<Item = TabId>) {
        for id in panes {
            if let Some(AppTab {
                page: Page::Terminal(live),
                ..
            }) = self.tabs.get_mut(id)
            {
                live.end_pointer_gesture();
                live.last_click = None;
                live.hover.borrow_mut().take();
                live.surface.invalidator.invalidate();
            }
        }
        self.dividers.borrow_mut().invalidate();
        self.sync_ime_surface();
        self.publish_panes();
    }

    fn pane_arrangement(&self) -> PaneArrangement {
        if self.incoming.is_some() || self.outgoing.is_some() {
            PaneArrangement::default()
        } else {
            let mut arrangement = PaneArrangement::for_tab(&self.tabs);
            arrangement.balance = self.available_balance();
            if arrangement.rotation.is_some() && !self.rotation_fits() {
                arrangement.rotation = None;
            }
            arrangement
        }
    }

    fn split_items(&self) -> [MenuItem; 4] {
        let can_choose = self.move_source().is_some();
        [
            self.binding_menu("split-right", "Split right", KeyAction::SplitRight),
            self.binding_menu("split-below", "Split below", KeyAction::SplitBelow),
            command_menu_item(
                "find-profile-split-right",
                command_palette::SPLIT_RIGHT_PROFILE_LABEL,
                can_choose,
            ),
            command_menu_item(
                "find-profile-split-below",
                command_palette::SPLIT_BELOW_PROFILE_LABEL,
                can_choose,
            ),
        ]
    }

    fn split_arrangement_items(&self, arrangement: &PaneArrangement) -> [MenuItem; 2] {
        [
            (KeyAction::RotateSplit, arrangement.rotation.is_some()),
            (KeyAction::BalancePanes, arrangement.balance.is_some()),
        ]
        .map(|(action, enabled)| {
            let binding = action.binding();
            self.binding_menu_enabled(binding.id, binding.label, action, enabled)
        })
    }

    fn active_layout_is_split(&self) -> bool {
        matches!(
            self.tabs.active_layout(),
            Some(w11term::pane_layout::Layout::Split { .. })
        )
    }

    fn pane_zoom_item(&self) -> MenuItem {
        self.binding_menu(
            "zoom-pane",
            if self.tabs.zoomed() {
                "Restore panes"
            } else {
                "Zoom pane"
            },
            KeyAction::ZoomPane,
        )
    }

    fn rename_pane_item(&self) -> MenuItem {
        command_menu_item(
            "rename-pane",
            tab_title::RENAME_PANE,
            self.move_source().is_some(),
        )
    }

    fn pane_menu(
        &self,
        can_restart: bool,
        arrangement: &PaneArrangement,
        directory: Option<&directory_split::Origin>,
        can_focus_previous: bool,
    ) -> MenuItem {
        let binding = KeyAction::FocusPreviousPane.binding();
        let previous_item = self.binding_menu_enabled(
            binding.id,
            binding.label,
            binding.action,
            can_focus_previous,
        );
        if self.tabs.active().and_then(AppTab::pane_id).is_none() {
            return if can_focus_previous {
                MenuItem::submenu("panes", "Panes", [previous_item])
            } else {
                MenuItem::disabled("panes", "Panes")
            };
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
        let can_split_directory = directory.is_some();
        let mut items = Vec::from(self.split_items());
        items.push(self.pane_text_size_menu());
        items.extend([
            self.binding_menu_enabled(
                "split-right-here",
                "Split right in current directory",
                KeyAction::SplitRightInCurrentDirectory,
                can_split_directory,
            ),
            self.binding_menu_enabled(
                "split-below-here",
                "Split below in current directory",
                KeyAction::SplitBelowInCurrentDirectory,
                can_split_directory,
            ),
            self.binding_menu_enabled(
                "new-tab-here",
                "New tab in current directory",
                KeyAction::NewTabInCurrentDirectory,
                can_split_directory,
            ),
            self.binding_menu_enabled(
                "copy-current-directory",
                "Copy current directory",
                KeyAction::CopyCurrentDirectory,
                can_split_directory,
            ),
            command_menu_item("restart-exited", RESTART_EXITED_PANE, can_restart),
            move_item,
            if self.outgoing.is_none() && self.incoming.is_none() {
                MenuItem::item("move-tab-window", "Move tab to new window")
            } else {
                MenuItem::disabled("move-tab-window", "Move tab to new window")
            },
            self.existing_window_menu(),
            self.binding_menu("copy-mode", "Keyboard copy mode", KeyAction::CopyMode),
            self.binding_menu("keyboard-hints", "Keyboard hints", KeyAction::KeyboardHints),
            self.broadcast_menu(),
        ]);
        if self
            .tabs
            .active()
            .and_then(AppTab::pending_pane_retry)
            .is_some()
        {
            items.push(MenuItem::item("retry-ssh-now", RETRY_SSH_NOW));
        }
        if self
            .tabs
            .active()
            .and_then(AppTab::pending_pane_attempt)
            .is_some()
        {
            items.push(MenuItem::item("stop-ssh-attempt", STOP_SSH_ATTEMPT));
        }
        if self.disconnect_target().is_some() {
            items.push(MenuItem::item(
                "disconnect-ssh-shell",
                disconnect_ssh::LABEL,
            ));
        }
        if matches!(self.tabs.active(), Some(AppTab { page: Page::Terminal(tab), .. }) if matches!(tab.source, Source::Ssh(_)))
        {
            items.push(MenuItem::item("files", "SFTP files"));
            items.push(MenuItem::item("files-here", "Open files here"));
        } else if matches!(self.tabs.active(), Some(AppTab { page: Page::Terminal(tab), .. }) if matches!(tab.source, Source::Local(_)))
        {
            items.push(command_menu_item(
                "open-explorer-folder",
                directory_split::OPEN_EXPLORER,
                self.can_open_explorer(directory),
            ));
        }
        if let Some(AppTab {
            page: Page::Terminal(live),
            ..
        }) = self.tabs.active()
        {
            if live.session.is_direct_ssh_shell()
                || matches!(&live.source, Source::Local(shell) if shell.wsl_distribution().is_some())
            {
                items.extend(shell_setup::Setup::menu_items());
            }
            items.push(self.rename_pane_item());
            if pane_colors::available(live) {
                items.push(pane_colors::menu(live.color_scheme));
            }
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
                text_export::SCROLLBACK_LABEL,
                self.export.pending.is_none(),
            ));
            let can_capture =
                self.export.pending.is_none() && !live.browsing_sessions() && !live.device_busy;
            items.push(command_menu_item(
                "copy-visible-text",
                text_export::COPY_VIEWPORT_LABEL,
                can_capture,
            ));
            items.push(command_menu_item(
                "copy-pane-image",
                text_export::COPY_PANE_IMAGE_LABEL,
                can_capture,
            ));
            items.push(command_menu_item(
                "save-pane-image",
                text_export::SAVE_PANE_IMAGE_LABEL,
                can_capture,
            ));
            let clear = KeyAction::ClearScrollback.binding();
            items.push(self.binding_menu_enabled(
                clear.id,
                clear.label,
                clear.action,
                live.can_clear_scrollback,
            ));
            items.push(match live.session.events.command_watch() {
                Some(true) => MenuItem::item("command-watch", "Cancel command notification"),
                Some(false) => MenuItem::item("command-watch", "Notify when command finishes"),
                None => MenuItem::disabled("command-watch", "Notify when command finishes"),
            });
            items.push(match live.session.events.long_command_notifications() {
                Some(enabled) => state_menu_item(
                    "long-command-notifications",
                    attention::LONG_COMMANDS,
                    Some(enabled),
                ),
                None => MenuItem::disabled("long-command-notifications", attention::LONG_COMMANDS),
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
        for (action, target) in [
            (KeyAction::SwapPaneLeft, arrangement.left),
            (KeyAction::SwapPaneRight, arrangement.right),
            (KeyAction::SwapPaneUp, arrangement.up),
            (KeyAction::SwapPaneDown, arrangement.down),
        ] {
            let binding = action.binding();
            items.push(if target.is_some() {
                self.binding_menu(binding.id, binding.label, action)
            } else {
                MenuItem::disabled(binding.id, binding.label)
            });
        }
        items.extend(self.split_arrangement_items(arrangement));
        items.push(previous_item);
        if self.active_layout_is_split() {
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
            items.push(self.pane_zoom_item());
            items.push(self.binding_menu_enabled(
                "close-pane",
                "Close pane",
                KeyAction::ClosePane,
                self.can_close_active_pane(),
            ));
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
            (
                Field::ConnectTimeout,
                "SSH connection timeout (seconds)",
                &draft.connect_timeout,
                "Default when blank",
            ),
            (
                Field::HostKeyAlias,
                "Host key alias (optional)",
                &draft.host_key_alias,
                "Name used to verify this server in known_hosts",
            ),
            (
                Field::GroupPrompt,
                "SSH group prompt regex (optional)",
                &draft.group_prompt,
                "Full appliance prompt, for example racadm>>",
            ),
            (
                Field::KeepaliveInterval,
                "SSH keepalive interval (seconds)",
                &draft.keepalive_interval,
                "0 disables keepalives",
            ),
            (
                Field::KeepaliveMax,
                "Unanswered keepalives allowed",
                &draft.keepalive_max,
                "1 or more",
            ),
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
        fields.push(scrollback_editor(
            &draft.scrollback_lines,
            context.callback(move |v| Message::Edit(id, Field::ScrollbackLines, v)),
        ));
        fields.push(color_scheme_editor(id, draft.color_scheme, context));
        fields.push(profile_font_size_editor(
            &draft.font_size,
            context.callback(move |v| Message::Edit(id, Field::FontSize, v)),
        ));
        fields.push(tab_color_editor(id, draft.tab_color, context));
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
                    .automation_name("Authentication")
                    .items_source(SSH_AUTH_LABELS)
                    .selected_index(draft.authentication)
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .on_selection_changed(context.callback(move |v| Message::Auth(id, v))),
            )),
        );
        use identity_picker::{Kind, Target};
        let can_browse = self.identity_picker.is_none() && !draft.generating;
        if uses_agent_auth(draft.authentication) {
            fields.push(agent_key_picker::pipe_field(
                "SSH agent pipe (optional)",
                &draft.agent_pipe,
                context.callback(move |value| Message::Edit(id, Field::AgentPipe, value)),
            ));
        }
        if draft.authentication == PRIVATE_KEY_AUTH {
            fields.push(identity_picker::field(
                Target::Endpoint(id, Kind::PrivateKey),
                &draft.identity,
                can_browse,
                context.callback(move |value| Message::Edit(id, Field::Identity, value)),
                context,
            ));
        }
        if let Some(kind) = Kind::certificate(draft.authentication) {
            fields.push(identity_picker::field(
                Target::Endpoint(id, kind),
                &draft.certificate,
                can_browse,
                context.callback(move |value| Message::Edit(id, Field::Certificate, value)),
                context,
            ));
        }
        if draft.authentication == SPECIFIC_AGENT_KEY_AUTH {
            fields.push(agent_key_picker::field(
                "SSH agent key fingerprint",
                &draft.agent_fingerprint,
                &draft.agent_pipe,
                context.callback(move |value| Message::Edit(id, Field::AgentFingerprint, value)),
            ));
        } else if draft.authentication == AGENT_CERTIFICATE_AUTH {
            fields.push(agent_key_picker::optional_pin(
                "Agent key fingerprint (optional)",
                &draft.agent_fingerprint,
                context.callback(move |value| Message::Edit(id, Field::AgentFingerprint, value)),
            ));
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
        fields.push(draft.jump_editor.view(
            id,
            &draft.proxy_jump,
            self.identity_picker.is_none(),
            context,
        ));
        fields.push(
            draft
                .forward_editor
                .view(id, &draft.local_forwards, context),
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
        fields.push(
            StackPanel::new().spacing(6.0).children((
                CheckBox::new()
                    .is_checked(draft.player.reconnect)
                    .on_is_checked_changed(
                        context.callback(move |value| {
                            Message::DraftPlayer(id, "reconnect".into(), value)
                        }),
                    )
                    .content("Reconnect after a network interruption"),
                TextBlock::new().text("SSH retry window (seconds)"),
                TextBox::new()
                    .automation_name("SSH retry window (seconds)")
                    .is_enabled(draft.player.reconnect)
                    .text(&draft.reconnect_window)
                    .on_text_changed(
                        context.callback(move |value| {
                            Message::Edit(id, Field::ReconnectWindow, value)
                        }),
                    ),
            )),
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
                    .is_enabled(!draft.generating && !draft.jump_editor.editing())
                    .on_click(context.callback(move |_| Message::SaveProfileOnly(id)))
                    .content(if matches!(draft.origin, DraftOrigin::Copy(_)) {
                        "Save copy"
                    } else {
                        "Save changes"
                    }),
            );
        }
        let is_new = matches!(draft.origin, DraftOrigin::New);
        let mut actions = vec![KeyedView::new(
            "save",
            Button::new()
                .style(is_new.then_some(ButtonStyle::Accent))
                .is_enabled(!draft.generating && !draft.jump_editor.editing())
                .on_click(context.callback(move |_| Message::SaveProfile(id)))
                .content("Save and connect"),
        )];
        if is_new {
            actions.push(KeyedView::new(
                "once",
                Button::new()
                    .is_enabled(!draft.generating && !draft.jump_editor.editing())
                    .on_click(context.callback(move |_| Message::ConnectOnce(id)))
                    .content("Connect without saving"),
            ));
        }
        actions.push(KeyedView::new(
            "cancel",
            Button::new()
                .on_click(context.callback(move |_| Message::CloseTab(id.to_string())))
                .content(if draft.saved { "Close" } else { "Cancel" }),
        ));
        fields.push(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(10.0)
                .keyed_children(actions),
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
        let group_window = input.group.as_ref();
        let child_window = moving || group_window.is_some();
        let sender = context.sender();
        let clipboard = clipboard_paste::State::default();
        let paste_origin = clipboard.input_origin.clone();
        let active_group = Rc::new(Cell::new(None));
        let shortcut_group = active_group.clone();
        let caret_menu_binding: Rc<RefCell<Option<CaretMenuBinding>>> = Rc::default();
        let shortcut_menu = caret_menu_binding.clone();
        let keyboard = KeyboardBridge::install(move |shortcut| {
            sender.send(match shortcut {
                Shortcut::Find
                | Shortcut::FindNext
                | Shortcut::FindPrevious
                | Shortcut::FindClose => Message::PaneFind(paste_origin.get(), shortcut),
                Shortcut::SelectAll => Message::SelectAll(paste_origin.get()),
                Shortcut::Paste | Shortcut::PasteRaw => {
                    Message::Clipboard(clipboard_paste::Action::Start(
                        paste_origin.get(),
                        shortcut == Shortcut::PasteRaw,
                    ))
                }
                Shortcut::SwapPane(direction) => shortcut_menu
                    .borrow()
                    .as_ref()
                    .map_or(Message::ControlIdle, |menu| {
                        menu.arrangement.message(direction)
                    }),
                Shortcut::RotateSplit => shortcut_menu
                    .borrow()
                    .as_ref()
                    .map_or(Message::ControlIdle, |menu| menu.arrangement.rotate()),
                Shortcut::BalancePanes => shortcut_menu
                    .borrow()
                    .as_ref()
                    .map_or(Message::ControlIdle, |menu| menu.arrangement.balance()),
                shortcut if let Some(destination) = directory_split::destination(shortcut) => {
                    let menu = shortcut_menu.borrow();
                    directory_split::message(
                        destination,
                        menu.as_ref().and_then(|m| m.directory_split.as_ref()),
                    )
                }
                Shortcut::CopyCurrentDirectory => {
                    let menu = shortcut_menu.borrow();
                    directory_split::copy_message(
                        menu.as_ref().and_then(|m| m.directory_split.as_ref()),
                    )
                }
                _ => bind_tab_action(Message::Shortcut(shortcut), shortcut_group.get()),
            });
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
        let (control_sender, control_rx) = std::sync::mpsc::sync_channel::<UiWork>(8);
        let text_size = text_size::Monitor::open(control_sender.clone())
            .inspect_err(|error| log::warn!("Opening Windows text size settings: {error}"))
            .ok();
        if let Some(factor) = text_size.as_ref().and_then(text_size::Monitor::current) {
            appearance.set_windows_text_scale(factor);
        }
        let renderer = Renderer::with_size(
            &appearance.font_family,
            appearance.ligatures,
            appearance.terminal_font_size_for(
                options
                    .source
                    .profile_font_size()
                    .map(ProfileTextSize::as_f32),
            ),
        )
        .expect("creating DirectWrite formats");
        keyboard.set_shortcuts(
            appearance
                .shortcuts
                .keymap()
                .expect("validated keyboard settings"),
        );
        let appearance = Arc::new(RwLock::new(appearance));
        let grid = if !child_window && (options.cols.is_some() || options.rows.is_some()) {
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
        let osc52 = appearance.read().unwrap().pane_preferences();
        if let Some(workspace) = opening.as_ref().map(|window| &window.layout) {
            workspace_view::append_resolved_workspace(
                &mut tabs,
                &workspace.layout,
                workspace.sources.clone(),
                osc52,
            );
        } else if !child_window {
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
        let new_window = input
            .transfer
            .as_ref()
            .and_then(|transfer| transfer.new_window.as_ref())
            .or_else(|| group_window.map(|group| group.claim()));
        let registry = new_window.map_or_else(
            || Registry::window(1, opening.as_ref().map(|window| window.key.clone())),
            |window| window.registry.clone(),
        );
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
        let capture = pane_text_size::capture(appearance.clone(), None);
        registry.set_capture(capture.clone());
        let control = new_window.map_or_else(
            || {
                Arc::new(
                    Server::start(registry.clone(), move |frame, size, path| {
                        capture(frame, size, path)
                    })
                    .expect("starting local control pipe"),
                )
            },
            |window| window.server.clone(),
        );
        let active = registry.clone();
        let sender = context.sender();
        keyboard.on_activation(move |focused| {
            if focused {
                active.activate();
            }
            sender.send(Message::WindowActivation(focused));
        });
        let mut app = Self {
            caret_interval: crate::cursor_blink::system_interval(),
            visual_bell_timer: None,
            folder_wake,
            inbox,
            taskbar: RefCell::default(),
            accessible: Default::default(),
            active_group,
            windows: Rc::new(RefCell::new(crate::window_state::Windows::load())),
            window_profile: None,
            window_restored: child_window || options.cols.is_some() || options.rows.is_some(),
            keyboard,
            broadcast_typing: None,
            _control: control,
            window_launch: options.clone(),
            opening: opening.clone(),
            incoming: input.transfer.clone(),
            outgoing: None,
            window_timer: None,
            host_group_timer: None,
            native_bound: false,
            window_error: None,
            pending_directory_copy: None,
            export: Default::default(),
            history_reference: None,
            recordings: Default::default(),
            registry,
            control_rx: Arc::new(std::sync::Mutex::new(control_rx)),
            control_task: None,
            tabs,
            closed_tabs: VecDeque::new(),
            closed_pane: None,
            metrics: Rc::new(Cell::new(Metrics::default())),
            invalidator: Invalidator::new(),
            header: RefCell::new(None),
            caret_menu_binding,
            folder_panel_timer: None,
            forward_watch: None,
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
            profile_action_error: None,
            profile_filter: String::new(),
            width: client_size.0,
            window_height: client_size.1,
            client_size,
            initial_grid: Rc::new(crate::window_state::RequestedGrid::new(grid)),
            appearance,
            text_size,
            fonts,
            appearance_error,
            saved_commands: Default::default(),
            dividers: Rc::default(),
            measured_layout: None,
            rotation_hover: None,
            pointer_over: None,
            overlay_visible: Cell::new(false),
            mru_groups: Vec::new(),
            switcher: None,
            picker: None,
            running_interrupt_pending: None,
            title_editor: None,
            disconnect_review: None,
            name_edit_serial: 0,
            identity_picker: None,
            clipboard,
            pane_drop: Default::default(),
            focused_osc52: None,
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
        if let Some(group_window) = group_window {
            app.start_group_window(&group_window.launch, context);
        } else {
            app.activate(context);
        }
        app.watch_control(context);
        if group_window.is_none() {
            app.sync_selection(context);
        }
        #[cfg(feature = "ui-smoke")]
        if !child_window && std::env::var_os("W11TERM_UI_SMOKE").is_some() {
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
                         || std::env::var_os("W11TERM_FILE_SMOKE").is_some()
                         || std::env::var_os("W11TERM_SCROLLBAR_SMOKE").is_some()
                         || std::env::var_os("W11TERM_WORD_SELECTION_SMOKE").is_some()))
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
        self.reconcile_broadcast();
        if !matches!(
            &message,
            Message::FrameReady(..)
                | Message::FolderShare(folder_shares::Action::Panel(..))
                | Message::LocalForward(local_forwards::Action::Panel(..))
                | Message::WindowActivation(_)
                | Message::TabTitle(_)
                | Message::Files(..)
                | Message::Keyboard(keyboard_settings::Edit::Text(..))
                | Message::FilterProfiles(_)
                | Message::Patterns(
                    pattern_settings::Edit::Name(_) | pattern_settings::Edit::Expression(_)
                )
                | Message::SavedCommands(
                    saved_commands::Edit::Name(_) | saved_commands::Edit::Text(_)
                )
                | Message::PickerQuery(..)
                | Message::RunningCommandsTick(..)
                | Message::RunningInterrupt(..)
                | Message::WatchRunningCommand(..)
                | Message::WindowHistory(..)
                | Message::Discovery(
                    session_browser::Action::Query(..) | session_browser::Action::Focus(..)
                )
                | Message::PaneText(..)
                | Message::HostGroup(host_groups::Action::Tick | host_groups::Action::Edit(..))
                | Message::PickerHover(..)
                | Message::PickerFrame(..)
                | Message::BookmarksRead(..)
                | Message::PickerFocus(..)
                | Message::PickerFocusAfterMenu(..)
                | Message::PaneRatio(..)
                | Message::PaneMeasured(_)
                | Message::DividerPress(..)
                | Message::DividerEnd(..)
                | Message::HoverPane(..)
                | Message::PointerExit(..)
                | Message::Pointer(..)
                | Message::CaptureLost(..)
                | Message::FinalizeCapture(..)
                | Message::VisualBellTick
                | Message::InspectorTick
        ) {
            self.header.borrow_mut().take();
        }
        if matches!(
            &message,
            Message::Control(_)
                | Message::AttentionChanged
                | Message::FolderAuthority
                | Message::WindowsTextSizeChanged
        ) {
            if let Some(scale) = self
                .text_size
                .as_ref()
                .and_then(text_size::Monitor::take_change)
            {
                self.update_windows_text_size(scale, context);
            }
            let wake = self.inbox.acknowledge_wake();
            if wake.flash_taskbar
                && let Some(hwnd) = self.keyboard.hwnd()
            {
                attention::flash_background_window(hwnd);
            }
            self.ring_visual_bells(wake.bells, context);
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
            Message::LocalForward(action) => self.forward_action(action, context),
            Message::FolderShare(action) => self.folder_action(action, context),
            Message::FolderRevoke(id, ticket, action) => {
                self.folder_revoke(id, ticket, action, context)
            }
            Message::RestartExited(exit) => {
                // A native menu may still be delivering a callback for a previous exit or pane.
                if let Err(error) = self.restart_exited_pane(exit, None) {
                    self.window_error =
                        Some(format!("Could not restart pane {}: {error:#}", exit.pane));
                }
                self.refresh_open_commands();
            }
            Message::StopSshAttempt(attempt) => {
                // An open flyout may still hold a token from a previous attempt.
                if let Some(tab) = self.tabs.active()
                    && tab.pending_pane_attempt() == Some(attempt)
                    && let Page::Terminal(live) = &tab.page
                {
                    live.session.events.cancel_ssh_attempt(attempt.token);
                }
                self.refresh_open_commands();
            }
            Message::RetrySshNow(retry) => {
                if let Some(tab) = self.tabs.active()
                    && tab.pending_pane_retry() == Some(retry)
                    && let Page::Terminal(live) = &tab.page
                {
                    live.session.events.retry_ssh_now(retry.token);
                }
                self.refresh_open_commands();
            }
            Message::DisconnectSsh(action) => self.handle_ssh_disconnect(action, context),
            Message::Export(action) => {
                self.handle_text_export(action, context);
                self.refresh_open_commands();
            }
            Message::Recording(action) => {
                self.recording_action(action, context);
                self.refresh_open_commands();
            }
            Message::ScrollFailedCommand(origin, older) => {
                if origin.is_some()
                    && origin == self.paste_origin()
                    && let Some(AppTab {
                        page: Page::Terminal(tab),
                        ..
                    }) = self.tabs.active()
                    && let Some((view, row)) =
                        tab.session.frames.load().adjacent_failed_command(older)
                {
                    let _ = tab.session.input.send(view.scroll_to(Some(row)));
                }
            }
            Message::ToggleScrollbackBookmark(origin) => {
                if origin.is_some()
                    && origin == self.paste_origin()
                    && let Some(AppTab {
                        page: Page::Terminal(tab),
                        ..
                    }) = self.tabs.active()
                    && let Some(target) = tab.session.frames.load().bookmark_target()
                {
                    let _ = tab
                        .session
                        .input
                        .send(Command::ToggleScrollbackBookmark(target));
                }
            }
            Message::ScrollToBookmark(origin, older) => {
                if origin.is_some()
                    && origin == self.paste_origin()
                    && let Some(AppTab {
                        page: Page::Terminal(tab),
                        ..
                    }) = self.tabs.active()
                    && let Some(target) = tab.session.frames.load().adjacent_bookmark(older)
                {
                    let _ = tab.session.input.send(Command::ScrollViewport(target));
                }
            }
            Message::ClearScrollback(pane) => {
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tab_for_pane(pane).and_then(|id| self.tabs.get(id))
                {
                    let _ = tab.session.input.send(Command::ClearScrollback);
                }
            }
            Message::PaneColorScheme(pane, scheme) => {
                let active = self.tabs.active_id().and_then(|id| self.tabs.get_mut(id));
                if pane_colors::apply(active, pane, scheme) {
                    self.refresh_open_commands();
                }
            }
            Message::DismissWindowError => self.window_error = None,
            Message::MoveTabToWindow => self.move_tab_window(None, context),
            Message::MoveTabToExistingWindow(group, window) => {
                if group.is_some() && self.tabs.active_group_id() == group {
                    self.move_tab_window(Some(window), context);
                }
            }
            Message::DuplicateTabLayout(group) => {
                self.window_error = self
                    .duplicate_tab_layout(group, context)
                    .err()
                    .map(|error| format!("Couldn’t duplicate tab layout. {error:#}"));
            }
            Message::ReopenClosedTab(Some(group)) => {
                self.window_error = self
                    .reopen_closed_tab(group, context)
                    .err()
                    .map(|error| format!("Couldn’t reopen tab layout. {error:#}"));
            }
            Message::ReopenClosedTab(None) => {}
            Message::ReopenClosedPane(Some(id)) => {
                self.window_error = self
                    .reopen_closed_pane(id, context)
                    .err()
                    .map(|error| format!("Couldn’t reopen pane. {error:#}"));
            }
            Message::ReopenClosedPane(None) => {}
            Message::MoveTab(group, direction) => {
                if self.tabs.active_group_id() == Some(group)
                    && self.tabs.move_active_tab(direction)
                {
                    self.sync_selection(context);
                    self.refresh_open_commands();
                }
            }
            Message::SetTabPinned(group, pinned) => {
                if self.active_terminal_group() == Some(group)
                    && self.tabs.set_group_pinned(group, pinned)
                {
                    self.sync_selection(context);
                    self.refresh_open_commands();
                }
            }
            Message::SetTabColor(group, color) => {
                if self.active_terminal_group() == Some(group)
                    && self.tabs.set_group_color(group, color)
                {
                    self.refresh_open_commands();
                }
            }
            Message::CloseTabs(offer) => self.close_tabs_from_offer(offer, context),
            Message::WindowTick => self.window_tick(context),
            Message::WindowTargetsChanged => self.refresh_open_commands(),
            Message::TransferRequest(source, id) => self.accept_transfer(source, id, context),
            Message::TransferReady(id) => self.offer_group(id, context),
            Message::TransferClaim(id) => self.claim_group(id, context),
            Message::TransferDone(id) => self.finish_transfer(id, context),
            Message::TransferCanceled(id) => self.cancel_transfer(id, context),
            Message::TabTitle(action) => self.edit_tab_title(action, context),
            Message::Attention(action) => self.handle_attention(action, context),
            Message::SftpTransfers(action) => self.handle_sftp_transfers(action, context),
            Message::AttentionChanged | Message::FolderAuthority => {}
            Message::VisualBellTick => self.expire_visual_bells(),
            Message::InspectorTick => {}
            Message::ToggleInspector => {
                self.show_inspector = !self.show_inspector;
                self.sync_inspector();
                self.cancel_frame_tasks();
                self.watch_frames(context);
            }
            Message::PaneTextSize(change) => self.change_pane_text_size(change),
            Message::InspectSshRoute(pane, open) => {
                if let Some(inspector) = self
                    .inspection
                    .as_mut()
                    .filter(|current| current.pane == pane)
                {
                    inspector.set_ssh_expanded(open);
                }
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
            Message::CopyShellSetup(setup) => {
                if let Err(error) = setup.copy() {
                    self.window_error = Some(format!("Couldn’t copy the shell setup. {error:#}"));
                }
            }
            Message::History(action) => self.handle_history(action, context),
            Message::HostGroup(action) => self.handle_host_group(action, context),
            Message::PaletteChoose(id) => self.choose_command(id, context),
            Message::PickerQuery(query) => self.filter_picker(query, context),
            Message::OpenRunningCommands => self.open_running_commands(context),
            Message::RunningCommandsTick(launch) => self.refresh_running_commands(&launch),
            Message::RunningInterrupt(action) => {
                self.running_interrupt(action, context);
                self.focus_picker_input(0, context);
            }
            Message::WatchRunningCommand(launch, pane, target, armed) => {
                self.watch_selected_running_command(&launch, pane, target, armed);
            }
            Message::WindowHistory(action) => self.window_history_action(action, context),
            Message::PaneText(action) => self.pane_text_action(action, context),
            Message::PickerHover(pane) => self.hover_picker(pane, context),
            Message::PickerFrame(pane, generation) => self.picker_frame(pane, generation, context),
            Message::BookmarksRead(launch, entries) => self.bookmarks_received(&launch, entries),
            Message::PickerChoose(pane) => self.choose_picker_pane(pane, context),
            Message::PlacePane(source, target, side) => {
                self.place_pane(source, target, side, context)
            }
            Message::PickerFocus(attempt) => self.focus_picker_input(attempt, context),
            Message::PickerFromMenu(shortcut) => self.open_picker_from_menu(shortcut, context),
            Message::SetTitleFiles(pane, open) => {
                if matches!(self.tabs.active().map(|tab| &tab.page), Some(Page::Terminal(live))
                    if live.session.pane_id == pane && live.files.open != open)
                {
                    self.file_action(FileAction::Toggle, context);
                }
            }
            Message::PickerFocusAfterMenu(launch) => self.focus_picker_after_menu(&launch),
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
                            .as_deref()
                            .cloned()
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
                                let cached = self
                                    .catalog
                                    .profiles
                                    .get_mut(label)
                                    .context("profile was removed")?;
                                let auth = if remove {
                                    Authentication::Password
                                } else {
                                    Authentication::identity(
                                        cached
                                            .player
                                            .device_key
                                            .as_deref()
                                            .context("device key path missing")?,
                                        None,
                                    )
                                };
                                let mut updated = cached.clone();
                                updated.set_authentication(auth.clone());
                                self.store.edit(label, label, &updated, cached)?;
                                *cached = updated;
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
                        if let Some(cached) = self.catalog.profiles.get_mut(&name) {
                            let mut updated = cached.clone();
                            updated.player = preferences.clone();
                            match self.store.edit(&name, &name, &updated, cached) {
                                Ok(()) => *cached = updated,
                                Err(error) => log::warn!("Saving player preferences: {error:#}"),
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
            Message::HintRevealed(request, result) => self.hint_revealed(request, result),
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
            Message::Control(w11term::control::UiRequest::ListSshGroups { reply }) => {
                if !reply.is_closed() {
                    let _ = reply.send(Ok(self.control_ssh_groups()));
                }
            }
            Message::Control(w11term::control::UiRequest::ListSftpTransfers { pane, reply }) => {
                if !reply.is_closed() {
                    let _ = reply.send(Ok(self.control_sftp_transfers(pane)));
                }
            }
            Message::Control(w11term::control::UiRequest::CancelSftpTransfer {
                transfer_id,
                reply,
            }) => {
                if !reply.is_closed() {
                    let result = self.control_cancel_sftp_transfer(transfer_id);
                    let _ = reply.send(result.map_err(|error| format!("{error:#}")));
                }
            }
            Message::Control(w11term::control::UiRequest::OpenSshGroup { name, reply }) => {
                if !reply.is_closed() {
                    let result = self.control_open_saved_host_group(&name, context);
                    let _ = reply.send(result.map_err(|error| format!("{error:#}")));
                }
            }
            Message::Control(w11term::control::UiRequest::ListSavedSshGroups { reply }) => {
                if !reply.is_closed() {
                    let result = self.control_list_saved_host_groups();
                    let _ = reply.send(result.map_err(|error| format!("{error:#}")));
                }
            }
            Message::Control(w11term::control::UiRequest::GetSshGroupRoster {
                tab_id,
                readiness,
                reply,
            }) => {
                if !reply.is_closed() {
                    let waiting = readiness.is_some();
                    self.control_ssh_group_roster(tab_id, readiness, reply);
                    if waiting {
                        self.update_host_group_timer(context);
                    }
                }
            }
            Message::Control(w11term::control::UiRequest::GetSshGroupRun {
                tab_id,
                run_id,
                wait_for,
                reply,
            }) => {
                if !reply.is_closed() {
                    self.control_ssh_group_run(tab_id, run_id, wait_for, reply);
                    if wait_for.is_some() {
                        self.update_host_group_timer(context);
                    }
                }
            }
            Message::Control(w11term::control::UiRequest::GetSshGroupRetryPlan {
                tab_id,
                run_id,
                reply,
            }) => {
                if !reply.is_closed() {
                    let _ = reply.send(self.control_ssh_group_retry_plan(tab_id, run_id));
                }
            }
            Message::Control(w11term::control::UiRequest::RetrySshGroupRun {
                tab_id,
                run_id,
                pane_ids,
                reply,
            }) => {
                if !reply.is_closed() {
                    let _ = reply.send(self.control_retry_ssh_group_run(tab_id, run_id, &pane_ids));
                    self.update_host_group_timer(context);
                }
            }
            Message::Control(w11term::control::UiRequest::SshGroupQueue {
                tab_id,
                run_id,
                action,
                reply,
            }) => {
                if !reply.is_closed() {
                    let _ = reply.send(self.control_ssh_group_queue(tab_id, run_id, action));
                    self.update_host_group_timer(context);
                }
            }
            Message::Control(w11term::control::UiRequest::InterruptSshGroupCommand {
                tab_id,
                run_id,
                pane_id,
                reply,
            }) => {
                if !reply.is_closed() {
                    let _ = reply
                        .send(self.control_interrupt_ssh_group_command(tab_id, run_id, pane_id));
                    self.update_host_group_timer(context);
                }
            }
            Message::Control(w11term::control::UiRequest::CaptureSshGroupReport {
                tab_id,
                run_id,
                format,
                reply,
            }) => {
                if !reply.is_closed() {
                    let _ =
                        reply.send(self.control_capture_ssh_group_report(tab_id, run_id, format));
                }
            }
            Message::Control(w11term::control::UiRequest::SubmitSshGroupCommand {
                tab_id,
                pane_ids,
                command,
                options,
                reply,
            }) => {
                if !reply.is_closed() {
                    self.control_submit_ssh_group_command(
                        tab_id, &pane_ids, command, *options, reply, context,
                    );
                }
            }
            Message::Control(w11term::control::UiRequest::ReadSshGroupOutput {
                tab_id,
                run_id,
                pane,
                follow,
                reply,
            }) => {
                if !reply.is_closed() {
                    let _ = reply.send(self.control_ssh_group_output(tab_id, run_id, pane, follow));
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
                        self.appearance.read().unwrap().pane_preferences(),
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
            Message::Control(w11term::control::UiRequest::ClosePane { pane, reply }) => {
                if !reply.is_closed() {
                    let result = self.control_close_pane(pane, context);
                    let _ = reply.send(result.map_err(|error| format!("{error:#}")));
                }
            }
            Message::Control(w11term::control::UiRequest::Resize {
                pane,
                cols,
                rows,
                reply,
            }) => self.control_resize(pane, cols, rows, reply, context),
            Message::ControlResizeDone(completion) => completion.observe(self, context),
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
            Message::PromptRememberSshKey(id, prompt, remember) => {
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get_mut(id)
                    && tab.prompt_id == Some(prompt)
                {
                    tab.remember_ssh_key = remember;
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
                            Some(w11term::prompts::Question::IdentityPassphrase { .. }) => {
                                tab.answers.pop().map_or(
                                    w11term::prompts::Answer::Cancel,
                                    |passphrase| w11term::prompts::Answer::IdentityPassphrase {
                                        passphrase,
                                        remember: tab.remember_ssh_key,
                                    },
                                )
                            }
                            Some(w11term::prompts::Question::Authentication { .. }) => {
                                w11term::prompts::Answer::Authentication(std::mem::take(
                                    &mut tab.answers,
                                ))
                            }
                            None => w11term::prompts::Answer::Cancel,
                        }
                    } else {
                        w11term::prompts::Answer::Cancel
                    };
                    let _ = tab.session.prompts.answer(prompt, answer);
                    tab.answers.clear();
                    tab.remember_ssh_key = false;
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
                            draft.certificate.clear();
                            draft.player.device_key = Some(Box::new(key.path.clone()));
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
                    let mut pane_actions_changed = false;
                    let mut discovery_changed = false;
                    let mut focus_session_search = false;
                    if let Some(AppTab {
                        page: Page::Terminal(tab),
                        ..
                    }) = self.tabs.get_mut(id)
                    {
                        if let Some(inspector) = &mut self.inspection
                            && inspector.pane == tab.session.pane_id
                        {
                            inspector.refresh(tab.session.frames.inspection());
                            inspector.sync_running_timer(context);
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
                        tab.observe_file_connection(frame.connection_generation, active);
                        if active {
                            tab.follow_shell_directory(&frame, context);
                        }
                        let unavailable = frame.copy_output_unavailable();
                        let jumps = [true, false]
                            .map(|older| frame.adjacent_failed_command(older).is_some());
                        if tab.failed_command_jumps != jumps {
                            tab.failed_command_jumps = jumps;
                            pane_actions_changed |= active;
                        }
                        let bookmarks = [
                            frame.bookmark_target().is_some(),
                            frame.adjacent_bookmark(true).is_some(),
                            frame.adjacent_bookmark(false).is_some(),
                            !frame.scrollback_bookmarks.is_empty(),
                        ];
                        if tab.bookmark_actions != bookmarks {
                            tab.bookmark_actions = bookmarks;
                            pane_actions_changed |= active;
                        }
                        if active
                            && self
                                .picker
                                .as_ref()
                                .is_some_and(|picker| picker.bookmark_results_expired(&frame))
                        {
                            pane_actions_changed = true;
                        }
                        let can_clear = !frame.alternate
                            && (frame.scroll_offset > 0
                                || frame.first_stable_row > frame.oldest_stable_row);
                        let report = frame.reported_directory.as_ref().filter(|_| {
                            tab.can_report_split_directory(frame.connection_generation)
                        });
                        if tab.forwards.connection_changed()
                            || (active && tab.connection_generation != frame.connection_generation)
                            || tab.can_clear_scrollback != can_clear
                            || tab.split_directory_report.as_ref() != report
                        {
                            tab.can_clear_scrollback = can_clear;
                            tab.split_directory_report = report.cloned();
                            if active {
                                self.header.borrow_mut().take();
                                pane_actions_changed = true;
                            }
                        }
                        if tab.copy_output_unavailable != unavailable {
                            tab.copy_output_unavailable = unavailable;
                            self.header.borrow_mut().take();
                        }
                        let phase = tab.session.durable_state().map(|s| s.phase);
                        focus_session_search = tab.discovery.is_some()
                            && phase.is_some_and(|p| p.is_browser())
                            && (tab.prompt_id.is_some() && frame.prompt.is_none()
                                || tab.durable_phase.is_some_and(|p| !p.is_browser()));
                        if tab.durable_phase != phase {
                            discovery_changed = tab.discovery.is_some();
                            tab.durable_phase = phase;
                            tab.confirm_end = false;
                            self.header.borrow_mut().take();
                            pane_actions_changed |= active;
                        }
                    }
                    if active {
                        // A durable session browser can become the live shell in place.
                        if discovery_changed {
                            self.tabs.record_pane_focus(AppTab::visited_terminal);
                        }
                        self.refresh_copy_mode(id, context);
                        self.connection_ui(id);
                        if focus_session_search {
                            self.discovery_action(session_browser::Action::Focus(id, 0), context);
                        }
                    }
                    if pane_actions_changed || discovery_changed {
                        self.refresh_open_commands();
                    }
                    if let Some(AppTab {
                        page: Page::Terminal(tab),
                        ..
                    }) = self.tabs.get_mut(id)
                    {
                        let frame = tab.session.frames.load();
                        if tab.reconcile_selection(&frame) && active {
                            self.keyboard.set_selection(false);
                            self.header.borrow_mut().take();
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
            Message::JumpToCommandContext(id, target) => {
                if self.tabs.visible(id)
                    && let Some(AppTab {
                        page: Page::Terminal(tab),
                        ..
                    }) = self.tabs.get(id)
                {
                    let _ = tab.session.input.send(Command::ScrollViewport(target));
                    self.focus_pane(id, context);
                }
            }
            Message::FocusPreviousPane(target) => {
                if previous_pane::target(&self.tabs) == Some(target) {
                    self.focus_previous_pane(target, context);
                }
            }
            Message::BroadcastTyping(action) => self.apply_broadcast(action),
            Message::SetPaneReadOnly(enabled) => {
                if enabled && self.broadcast_typing.is_some() {
                    self.stop_broadcast();
                }
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
                if self.dividers.borrow().dragging(epoch, &path)
                    && self.tabs.set_ratio(&path, ratio)
                    && self
                        .caret_menu_binding
                        .borrow()
                        .as_ref()
                        .is_some_and(|menu| menu.arrangement != self.pane_arrangement())
                {
                    // A drag can change which leaf has the longest shared edge.
                    self.header.borrow_mut().take();
                }
            }
            Message::PaneNeighbor(direction) => {
                if self.tabs.focus_neighbor(direction) {
                    self.activate(context);
                }
            }
            Message::SwapPanes(target) => {
                if self.incoming.is_none()
                    && self.outgoing.is_none()
                    && self.tabs.swap_panes(target)
                {
                    let panes = target.panes();
                    self.rotation_hover = None;
                    self.keyboard.panes_swapped(panes);
                    self.panes_rearranged(panes);
                }
            }
            Message::RotateSplit(target) => {
                if self.incoming.is_none()
                    && self.outgoing.is_none()
                    && self.rotation_fits()
                    && let Some(panes) = self.tabs.rotate_split(target)
                {
                    self.rotation_hover = if self.overlay_visible.get() {
                        None
                    } else {
                        self.capture_rotation_hover(&panes)
                    };
                    for &id in &panes {
                        self.keyboard.leave_pane(id);
                    }
                    self.panes_rearranged(panes);
                }
            }
            Message::BalancePanes(target) => {
                if self.incoming.is_none()
                    && self.outgoing.is_none()
                    && let Some(plan) = self.measure_balance(&target)
                    && self.tabs.balance_panes(plan)
                {
                    let panes = target.layout().leaves();
                    self.rotation_hover = self.reset_balance_measurements(&panes);
                    for &id in &panes {
                        self.keyboard.leave_pane(id);
                    }
                    self.panes_rearranged(panes);
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
                    self.close_pane_entry(id, context);
                }
            }
            Message::SplitPane(axis, profile) => self.split_pane(axis, profile, context),
            Message::FindSplitProfile(axis) => self.open_profile_search(Some(axis), context),
            Message::OpenInDirectory(destination, origin) => {
                self.open_in_directory(destination, origin, context)
            }
            Message::OpenExplorer(origin) => self.open_explorer(origin, context),
            Message::CopyCurrentDirectory(origin) => self.copy_current_directory(origin, context),
            Message::DirectoryCopyReady(request, result) => {
                self.finish_directory_copy(request, result)
            }
            Message::ExplorerOpened(Err(error)) => {
                self.window_error = Some(format!("Couldn’t open File Explorer. {error}"));
            }
            Message::ExplorerOpened(Ok(())) => {}
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
                    self.close_user_tab(id, context);
                }
            }
            Message::Reorder(order, selected, epoch) => {
                // A delayed drag event belongs to the header order it was rendered with.
                if self.selection_pending || epoch != self.selection_epoch {
                    return;
                }
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
                    } else {
                        self.refresh_open_commands();
                    }
                }
            }
            Message::Shortcut(shortcut) => match shortcut {
                Shortcut::SubmitSessionSearch(id, listing, query) => {
                    self.submit_session_search(id, listing, query)
                }
                Shortcut::SubmitFileSearch(pane, generation, serial) => self.file_action(
                    FileAction::Search(pane, generation, file_drawer::find::Action::Run(serial)),
                    context,
                ),
                Shortcut::CloseFileSearch(pane, generation, serial) => self.file_action(
                    FileAction::Search(pane, generation, file_drawer::find::Action::Back(serial)),
                    context,
                ),
                Shortcut::CommandPalette => self.open_command_palette(context),
                Shortcut::PanePicker => self.open_picker(context),
                Shortcut::FindTab => self.open_tab_search(context),
                Shortcut::FindCommandHistory => self.open_window_command_history(context),
                Shortcut::FindAllPanes => self.open_text_picker(context),
                Shortcut::MovePaneToExistingTab => self.open_move_picker(context),
                Shortcut::PickerPlace(side) => {
                    if let Some((source, target)) =
                        self.picker.as_ref().and_then(|picker| picker.placement())
                    {
                        self.place_pane(source, target, side, context);
                    }
                }
                Shortcut::CopyMode => {
                    self.open_copy_mode();
                }
                Shortcut::SelectAll => self.select_all(self.paste_origin(), context),
                // Keyboard input uses the worker receiving the keydown. Menus carry its pane ID.
                Shortcut::ClearScrollback => {}
                Shortcut::CopyKey(key) => self.copy_key(key, context),
                Shortcut::KeyboardHints => self.open_hints(),
                Shortcut::HintKey(key) => self.hint_key(key, context),
                Shortcut::PickerAccept => self.accept_picker(context),
                Shortcut::PickerCancel => {
                    if self.cancel_running_interrupt_review() {
                        self.focus_picker_input(0, context);
                    } else {
                        self.close_picker(context);
                    }
                }
                Shortcut::PickerHistoryOption(option) => self.cycle_window_history(option, context),
                Shortcut::PickerSaveHistory => self.save_window_history(context),
                Shortcut::PickerWatchRunningCommand => self.toggle_selected_running_command_watch(),
                Shortcut::PickerRunningOutput => self.toggle_running_command_output(),
                Shortcut::PickerReviewRunningInterrupt => {
                    self.review_selected_running_interrupt(context)
                }
                Shortcut::PickerConfirmRunningInterrupt => {
                    self.confirm_selected_running_interrupt(context)
                }
                Shortcut::PickerMove(delta) => {
                    if let Some(picker) = &mut self.picker {
                        picker.move_by(delta);
                        self.refresh_running_command_output(false);
                        self.sync_picker_preview(context);
                        self.announce_text_selection();
                    }
                }
                Shortcut::SplitRight => self.split_pane(Axis::Columns, None, context),
                Shortcut::ResizePane(direction) => self.keyboard_resize(direction),
                // The native shortcut and palette bind both panes before dispatch.
                Shortcut::SwapPane(_)
                | Shortcut::RotateSplit
                | Shortcut::BalancePanes
                | Shortcut::SplitRightInCurrentDirectory
                | Shortcut::SplitBelowInCurrentDirectory
                | Shortcut::NewTabInCurrentDirectory
                | Shortcut::CopyCurrentDirectory => {}
                Shortcut::SplitBelow => self.split_pane(Axis::Rows, None, context),
                Shortcut::FocusPane(direction) => {
                    self.update(Message::PaneNeighbor(direction), context)
                }
                Shortcut::FocusPreviousPane => {
                    if let Some(target) = previous_pane::target(&self.tabs) {
                        self.focus_previous_pane(target, context);
                    }
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
                Shortcut::CaretPresentationChanged => {
                    self.refresh_notification_visibility();
                    self.caret_interval = crate::cursor_blink::system_interval();
                }
                Shortcut::DrawerFocusChanged => {}
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
                    self.apply_appearance(appearance, context);
                }
                Shortcut::ResetZoom => {
                    let mut appearance = self.appearance.read().unwrap().clone();
                    appearance.font_size = 16.0;
                    self.apply_appearance(appearance, context);
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
                Shortcut::DuplicateTabLayout | Shortcut::MoveTabLeft | Shortcut::MoveTabRight => {
                    self.update(
                        bind_tab_action(Message::Shortcut(shortcut), self.tabs.active_group_id()),
                        context,
                    );
                }
                Shortcut::CloseTab => {
                    if let Some(id) = self.tabs.active_id() {
                        self.close_user_tab(id, context);
                    }
                }
                Shortcut::ReopenClosedTab => {
                    let group = self.closed_tabs.back().map(|closed| closed.group);
                    self.update(Message::ReopenClosedTab(group), context);
                }
                Shortcut::ReopenClosedPane => {
                    let id = self.closed_pane.as_ref().map(|closed| closed.id);
                    self.update(Message::ReopenClosedPane(id), context);
                }
                Shortcut::Select(index) => {
                    if let Some(id) = self.tabs.headers().get(index).map(|t| t.id) {
                        self.tabs.select(id);
                        self.activate(context);
                    }
                }
            },
            Message::ProfileMenu(label) => {
                if label == command_palette::FIND_PROFILE_LABEL {
                    self.open_profile_search(None, context);
                } else if label == NEW_PROFILE {
                    self.utility(false, context);
                } else if label == quick_connect::LABEL {
                    self.open_quick_connect(context);
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
            Message::QuickConnect(action) => self.edit_quick_connect(action, context),
            Message::SshIdentity(action) => self.ssh_identity_action(action, context),
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
            Message::SavedCommands(edit) => self.edit_saved_commands(edit),
            Message::WordDelimiters(edit) => self.edit_word_delimiters(edit),
            Message::WindowActivation(false) => {
                self.refresh_notification_visibility();
                if let Some(access) = self.focused_osc52.take() {
                    access.set_focused_window(None);
                }
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.active()
                {
                    tab.surface.pause_cursor();
                }
            }
            Message::WindowActivation(true) => {
                self.refresh_notification_visibility();
                self.refresh_remote_clipboard_focus();
                self.header.borrow_mut().take();
                self.caret_interval = crate::cursor_blink::system_interval();
                self.refresh_profiles();
                self.refresh_open_commands();
                for entry in self.tabs.entries() {
                    if let Page::Terminal(live) = &entry.value.page
                        && let Some(shares) = &live.folder_sharing
                    {
                        shares.refresh_profile();
                    }
                }
            }
            Message::FilterProfiles(query) => {
                if matches!(
                    self.tabs.active().map(|tab| &tab.page),
                    Some(Page::Settings(None))
                ) {
                    self.profile_filter = query;
                }
            }
            Message::MoveProfile(name, delta, order) => {
                match self.store.move_profile(&name, delta, &order) {
                    Ok(catalog) => {
                        self.catalog = catalog;
                        self.profile_error = None;
                        self.profile_action_error = None;
                    }
                    Err(error) => {
                        self.refresh_profiles();
                        self.profile_action_error = Some(format!(
                            "Could not move {name}: {error:#}. Review the current order and try again."
                        ));
                    }
                }
            }
            Message::SetProfilePin(name, pinned) => {
                match self.store.set_profile_pin(&name, pinned) {
                    Ok(catalog) => {
                        self.catalog = catalog;
                        self.profile_error = None;
                        self.profile_action_error = None;
                    }
                    Err(error) => {
                        self.refresh_profiles();
                        self.profile_action_error = Some(format!(
                            "Could not update the New Tab pin for {name}: {error:#}"
                        ));
                    }
                }
            }
            Message::StartupProfile(label) => {
                let selected = match label.rsplit_once(" · ") {
                    Some((name, "SSH" | "Local")) => Some(name),
                    None if label == LAB => None,
                    _ => return,
                };
                let expected = self.catalog.startup_profile.as_deref();
                if selected != expected {
                    match self.store.set_startup_profile(selected, expected) {
                        Ok(catalog) => {
                            self.catalog = catalog;
                            self.profile_error = None;
                            self.profile_action_error = None;
                        }
                        Err(error) => {
                            self.refresh_profiles();
                            self.profile_action_error =
                                Some(format!("Could not set startup profile. {error:#}"));
                        }
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
                self.finish_keyboard_copy(pane, request, result, context)
            }
            Message::PaneFind(origin, shortcut) => {
                if Find::same_origin(origin, self.paste_origin()) {
                    self.update(Message::Shortcut(shortcut), context);
                } else {
                    self.keyboard.set_search(matches!(self.tabs.active(), Some(AppTab { page: Page::Terminal(tab), .. }) if tab.find.open && tab.find_available()));
                }
            }
            Message::SelectAll(origin) => self.select_all(origin, context),
            Message::CopySelection(origin) => {
                if origin.is_some() && self.paste_origin() == origin && self.active_selection() {
                    if self.copy_mode.borrow().is_some() {
                        self.copy_key(w11term::copy_mode::CopyKey::Copy, context);
                    } else {
                        self.copy();
                    }
                }
            }
            Message::SelectAllResult(pane, request, result) => {
                self.finish_select_all(pane, request, result, context)
            }
            Message::CopyWordResult(pane, request, point) => {
                self.finish_copy_word(pane, request, point, context)
            }
            Message::Clipboard(action) => self.clipboard_action(action, context),
            Message::PaneDrop(action) => self.pane_drop_action(action, context),
            Message::MultilinePasteWarning(index) => {
                if let Some(&(mode, _)) = index.and_then(|index| MULTILINE_PASTE_CHOICES.get(index))
                {
                    let mut appearance = self.appearance.read().unwrap().clone();
                    if appearance.interaction.multi_line_paste_warning != mode {
                        appearance.interaction.multi_line_paste_warning = mode;
                        self.save_preferences(appearance);
                    }
                }
            }
            Message::Interaction(field, value) => {
                let mut appearance = self.appearance.read().unwrap().clone();
                let setting = match field {
                    InteractionField::ApplicationTabTitles => {
                        &mut appearance.interaction.application_tab_titles
                    }
                    InteractionField::CopyOnSelect => &mut appearance.interaction.copy_on_select,
                    InteractionField::LargePasteWarning => {
                        &mut appearance.interaction.large_paste_warning
                    }
                    InteractionField::TrimBlock => &mut appearance.interaction.trim_block_selection,
                    InteractionField::TrimPaste => &mut appearance.interaction.trim_paste,
                    InteractionField::Snap => &mut appearance.interaction.snap_to_grid_on_resize,
                    InteractionField::ScrollZoom => &mut appearance.interaction.scroll_to_zoom,
                    InteractionField::FailedCommandMarks => {
                        &mut appearance.interaction.failed_command_marks
                    }
                    InteractionField::StickyCommandContext => {
                        &mut appearance.interaction.sticky_command_context
                    }
                    InteractionField::VisualBell => &mut appearance.interaction.visual_bell,
                    InteractionField::DetectUrls => &mut appearance.interaction.detect_urls,
                    InteractionField::Osc52 => &mut appearance.interaction.osc52,
                    InteractionField::Osc52ForegroundOnly => {
                        &mut appearance.interaction.osc52_foreground_only
                    }
                };
                if *setting != value {
                    *setting = value;
                    self.save_preferences(appearance);
                    if matches!(field, InteractionField::ApplicationTabTitles) {
                        self.publish_panes();
                    }
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
            Message::PickTab(group) => {
                self.finish_switcher(false, context);
                self.select_switched_group(group, context);
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
                self.begin_divider(epoch, &path, coordinate);
            }
            Message::DividerEnd(epoch, path) => {
                self.dividers.borrow_mut().end(epoch, &path);
            }
            Message::HoverPane(id) => {
                if self.tabs.visible(id) {
                    self.change_pointer_pane(Some(id));
                }
                self.keyboard.hover_pane(id);
                if self.tabs.active_id() != Some(id)
                    && let Some(inspector) = &mut self.inspection
                {
                    inspector.clear_pointer();
                }
            }
            Message::PointerExit(id) => {
                if self.pointer_over == Some(id) {
                    self.change_pointer_pane(None);
                }
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
                        self.apply_appearance(appearance, context);
                    }
                }
            }
            Message::Font(None) => {}
            Message::DefaultCursorStyle(index) => {
                if let Some(&(style, _)) =
                    index.and_then(|index| DefaultCursorStyle::CHOICES.get(index))
                {
                    let mut appearance = self.appearance.read().unwrap().clone();
                    if appearance.default_cursor_style != style {
                        appearance.default_cursor_style = style;
                        self.save_preferences(appearance);
                    }
                }
            }
            Message::CursorGuide(index) => {
                if let Some(&(mode, _)) = index.and_then(|index| CursorGuide::CHOICES.get(index)) {
                    let mut appearance = self.appearance.read().unwrap().clone();
                    if appearance.cursor_guide != mode {
                        appearance.cursor_guide = mode;
                        self.save_preferences(appearance);
                    }
                }
            }
            Message::Ligatures(enabled) => {
                let mut appearance = self.appearance.read().unwrap().clone();
                if appearance.ligatures != enabled {
                    appearance.ligatures = enabled;
                    self.apply_appearance(appearance, context);
                }
            }
            Message::FollowWindowsTextSize(enabled) => {
                let mut appearance = self.appearance.read().unwrap().clone();
                if appearance.follow_windows_text_size != enabled {
                    let previous_size = appearance.terminal_font_size();
                    appearance.follow_windows_text_size = enabled;
                    if previous_size == appearance.terminal_font_size() {
                        self.save_preferences(appearance);
                    } else {
                        self.apply_appearance(appearance, context);
                    }
                }
            }
            Message::WindowsTextSizeChanged => {}
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
            Message::PaneMeasured(id) => {
                if self.tabs.visible(id) && !self.tabs.zoomed() {
                    crate::input::ime::position_after_layout();
                    if self.overlay_visible.get() {
                        self.rotation_hover = None;
                    } else {
                        self.restore_rotation_hover();
                    }
                    let available = self.incoming.is_none() && self.outgoing.is_none();
                    let rotation = available && self.rotation_fits();
                    let balance = available && self.available_balance().is_some();
                    let changed = self
                        .caret_menu_binding
                        .borrow()
                        .as_ref()
                        .is_none_or(|cached| {
                            cached.arrangement.rotation.is_some() != rotation
                                || cached.arrangement.balance.is_some() != balance
                        });
                    if changed {
                        self.header.borrow_mut().take();
                    }
                    self.refresh_open_commands();
                }
            }
            Message::Geometry {
                client,
                delta,
                epoch,
                first,
            } => {
                if self.initial_grid.epoch() == epoch && self.initial_grid.get().is_some() {
                    // WinUI can report a client extent different from the size
                    // requested through ResizeClient with a custom title bar.
                    // After the first resize, apply the remaining error to the
                    // last request, or the same caption offset would recur.
                    let estimate = (client.0 + delta.0, client.1 + delta.1);
                    self.client_size = if first && estimate != self.client_size {
                        estimate
                    } else {
                        (self.client_size.0 + delta.0, self.client_size.1 + delta.1)
                    };
                    if first {
                        context.spawn_background_with_rejection(
                            move |_| {
                                std::thread::sleep(Duration::from_millis(750));
                                Message::GridFallback(epoch)
                            },
                            Message::GridFallback(epoch),
                        );
                    }
                }
            }
            Message::GridFallback(epoch) => {
                if self.initial_grid.expire(epoch) {
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
                        Field::KeepaliveInterval => &mut draft.keepalive_interval,
                        Field::ConnectTimeout => &mut draft.connect_timeout,
                        Field::KeepaliveMax => &mut draft.keepalive_max,
                        Field::ReconnectWindow => &mut draft.reconnect_window,
                        Field::ScrollbackLines => &mut draft.scrollback_lines,
                        Field::FontSize => &mut draft.font_size,
                        Field::CustomTerminal => &mut draft.custom_terminal,
                        Field::Identity => &mut draft.identity,
                        Field::Certificate => &mut draft.certificate,
                        Field::AgentFingerprint => &mut draft.agent_fingerprint,
                        Field::AgentPipe => &mut draft.agent_pipe,
                        Field::Fingerprint => &mut draft.fingerprint,
                        Field::HostKeyAlias => &mut draft.host_key_alias,
                        Field::GroupPrompt => &mut draft.group_prompt,
                    };
                    let changed = *target != text;
                    *target = text;
                    if changed {
                        draft.saved = false;
                        draft.error = None;
                    }
                }
            }
            Message::ProfileForward(id, action) => {
                if let Some(AppTab {
                    page: Page::NewProfile(draft),
                    ..
                }) = self.tabs.get_mut(id)
                    && draft
                        .forward_editor
                        .update(&mut draft.local_forwards, action)
                {
                    draft.saved = false;
                    draft.error = None;
                }
            }
            Message::ProfileJump(id, action) => {
                if let Some(AppTab {
                    page: Page::NewProfile(draft),
                    ..
                }) = self.tabs.get_mut(id)
                    && draft.jump_editor.update(&mut draft.proxy_jump, action)
                {
                    draft.saved = false;
                    draft.error = None;
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
            Message::ProfileColorScheme(id, scheme) => {
                match self.tabs.get_mut(id).map(|tab| &mut tab.page) {
                    Some(Page::NewProfile(draft)) if draft.color_scheme != scheme => {
                        draft.color_scheme = scheme;
                        draft.saved = false;
                        draft.error = None;
                    }
                    Some(Page::LocalProfile(draft)) => draft.set_color_scheme(scheme),
                    _ => {}
                }
            }
            Message::ProfileTabColor(id, color) => {
                match self.tabs.get_mut(id).map(|tab| &mut tab.page) {
                    Some(Page::NewProfile(draft)) if draft.tab_color != color => {
                        draft.tab_color = color;
                        draft.saved = false;
                        draft.error = None;
                    }
                    Some(Page::LocalProfile(draft)) => draft.set_tab_color(color),
                    _ => {}
                }
            }
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
            Message::ConnectOnce(id) => self.connect_once(id, context),
            Message::Pointer(id, pointer, kind) => self.pointer(id, pointer, kind, context),
            Message::FindSeed(id, epoch, view, target, seed) => {
                if self.watch_epoch == view
                    && self.tabs.active_id() == Some(id)
                    && let Some(AppTab {
                        page: Page::Terminal(tab),
                        ..
                    }) = self.tabs.get_mut(id)
                    && tab.find_available()
                    && target.current(&tab.session.frames.load())
                    && tab.find.accept_selection(epoch, seed)
                {
                    tab.find.apply(&tab.session);
                }
            }
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
                    && tab.find_available()
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
        self.discard_inactive_paste();
        self.reconcile_broadcast();
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
            && self.disconnect_review.is_none()
            && !self.clipboard.is_open()
            && let (Some(parent), Some(bounds)) = (crate::input::window(), self.focused_pane_rect())
        {
            let frame = tab.session.frames.load();
            let marked = self
                .copy_mode
                .borrow()
                .as_ref()
                .filter(|mode| Some(mode.pane) == self.tabs.active_id() && mode.state.valid(&frame))
                .and_then(|m| m.state.selection());
            let metrics = tab.surface.metrics.get();
            let sender = context.sender();
            self.accessible.update(
                parent,
                crate::accessibility::Pane {
                    id: tab.session.pane_id,
                    label: tab
                        .pane_name
                        .as_ref()
                        .map_or(label.as_str(), |n| n.as_str()),
                    appearance: &self.appearance,
                    frames: &tab.session.frames,
                    input: &tab.session.input,
                    geometry: crate::accessibility::Geometry {
                        bounds,
                        cell_width: f64::from(metrics.cell_width * metrics.scale),
                        cell_height: f64::from(metrics.cell_height * metrics.scale),
                        padding: f64::from(PADDING * metrics.scale),
                    },
                    selection: marked
                        .as_ref()
                        .or(tab.selection.borrow().as_ref())
                        .filter(|selection| selection.survives_scrollback_clear(&frame)),
                },
                move |request| {
                    sender.send(Message::AccessibleSelection(request));
                },
            );
        } else {
            self.accessible.clear();
        }
        let application_titles = self
            .appearance
            .read()
            .unwrap()
            .interaction
            .application_tab_titles;
        let title = self.tabs.active_id().map_or_else(
            || std::borrow::Cow::Borrowed("Terminal"),
            |id| self.tab_label(id, application_titles),
        );
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
            let header = self.header_view(context, application_titles);
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
                Page::Settings(Some(SettingsEditor::WordDelimiters(editor))) => {
                    word_delimiters::view(editor, context)
                }
                Page::Attention(page) => self.attention_view(page, context),
                Page::SftpTransfers(expanded) => self.sftp_transfers_view(*expanded, context),
                Page::CommandHistory(page) => self.history_view(page, context),
                Page::HostGroup(page) => self.host_group_view(id, page, context),
                Page::SharedFolders(page) => self.folder_panel_view(page, context),
                Page::LocalForwards(page) => self.forward_view(id, page, context),
                Page::ForwardOverview(feedback) => self.forward_overview(id, feedback, context),
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
                Page::Settings(Some(SettingsEditor::SavedCommands(page))) => {
                    saved_commands::view(page, &self.saved_commands, context)
                }
                Page::LocalProfile(draft) => self.local_profile_view(id, draft, context),
                Page::Workspaces(page) => self.workspace_view(id, page, context),
                Page::ImportConfig(page) => self.import_view(id, page, context),
                Page::NewProfile(draft) => self.profile_form(id, draft, context),
                Page::QuickConnect(draft) => {
                    quick_connect::view(id, draft, self.identity_picker.is_none(), context)
                }
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
        if let Some(plan) = &self.broadcast_typing {
            views.push(KeyedView::new("broadcast-typing", plan.indicator(context)));
        }
        let base_layers = views.len();
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
                "text-export-status",
                Button::new()
                    .grid_row(1)
                    .automation_name(message)
                    .horizontal_alignment(HorizontalAlignment::Right)
                    .vertical_alignment(VerticalAlignment::Bottom)
                    .on_click(context.callback(|_| Message::Export(text_export::Action::Dismiss)))
                    .content(format!("{message}  ·  Dismiss")),
            ));
        }
        if let Some(message) = &self.recordings.status {
            views.push(KeyedView::new(
                "recording-status",
                Button::new()
                    .grid_row(1)
                    .automation_name(message)
                    .horizontal_alignment(HorizontalAlignment::Left)
                    .vertical_alignment(VerticalAlignment::Bottom)
                    .on_click(context.callback(|_| Message::Recording(recordings::Action::Dismiss)))
                    .content(format!("{message}  ·  Dismiss")),
            ));
        }
        if let Some(inspection) = &self.inspection
            && self.hints.borrow().is_none()
            && self.copy_mode.borrow().is_none()
        {
            views.push(KeyedView::new(
                "terminal-inspector",
                inspection.view(self.window_height, context),
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
            let headers = self.tabs.headers();
            let choices: Vec<_> = switcher
                .groups
                .iter()
                .enumerate()
                .filter_map(|(index, group)| {
                    tab_title::switcher_leaf(&headers, *group)
                        .map(|pane| (index, *group, self.tab_label(pane, application_titles)))
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
            views.push(KeyedView::new(editor.key(), editor.view(context)));
        }
        if let Some(dialog) = self.disconnect_dialog(context) {
            views.push(KeyedView::new("disconnect-ssh-shell", dialog));
        }
        if let Some(dialog) = self.paste_dialog(context) {
            views.push(dialog);
        }
        if let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.active()
            && self.title_editor.is_none()
            && self.disconnect_review.is_none()
            && tab.session.frames.load().prompt.is_none()
            && let Source::Ssh(ssh) = &tab.source
            && let Some(dialog) =
                tab.files
                    .entry_dialog(tab.session.pane_id, ssh, &self.export, context)
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
        self.overlay_visible.set(views.len() > base_layers);
        Grid::new()
            .rows([GridLength::Auto, GridLength::STAR, GridLength::Auto])
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
        if let Some(path) = std::env::var_os("W11TERM_WORD_SELECTION_SMOKE") {
            self.word_selection_smoke(std::path::Path::new(&path), context);
            return;
        }
        if let Some(path) = std::env::var_os("W11TERM_SCROLLBAR_SMOKE") {
            self.scrollbar_pointer_smoke(std::path::Path::new(&path), context);
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
                assert_eq!(draft.color_scheme, ColorScheme::Default);
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
                    Message::ProfileColorScheme(id, ColorScheme::OneHalfDark),
                    context,
                );
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
                assert_eq!(draft.color_scheme, ColorScheme::OneHalfDark);
                assert_eq!(draft.authentication, 2);
                self.update(Message::Edit(copy, Field::Name, "second".into()), context);
                self.update(Message::TerminalPreset(copy, Some(2)), context);
                self.update(
                    Message::ProfileColorScheme(copy, ColorScheme::OneHalfLight),
                    context,
                );
                self.update(
                    Message::Edit(copy, Field::CustomTerminal, "vt220".into()),
                    context,
                );
                self.update(Message::SaveProfileOnly(copy), context);
                assert!(
                    self.catalog.profiles.contains_key("corrected")
                        && self.catalog.profiles.contains_key("second")
                );
                let profiles = self.store.load().unwrap();
                assert_eq!(profiles["second"].terminal_type.as_str(), "vt220");
                assert_eq!(profiles["second"].color_scheme, ColorScheme::OneHalfLight);
                assert_eq!(profiles["corrected"].color_scheme, ColorScheme::OneHalfDark);
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
                let before = std::fs::read(self.store.path()).unwrap();
                self.utility(false, context);
                let once = self.tabs.active_id().unwrap();
                self.update(Message::ConnectOnce(once), context);
                let Page::NewProfile(draft) = &mut self.tabs.get_mut(once).unwrap().page else {
                    panic!("Invalid one-off connection should leave the form open");
                };
                assert_eq!(draft.error.as_deref(), Some("Enter a host and user"));
                *draft = ProfileDraft::from_options(String::new(), &options);
                self.update(Message::ConnectOnce(once), context);
                assert!(matches!(
                    &self.tabs.get(once).unwrap().page,
                    Page::Terminal(_)
                ));
                assert_eq!(std::fs::read(self.store.path()).unwrap(), before);
            }
            33..=52 => {
                let Page::Terminal(live) = &self.tabs.active().unwrap().page else {
                    panic!("Expected a one-off SSH pane")
                };
                assert!(live.profile_name.is_none());
                if live.session.frames.load().connection_generation == 0 {
                    assert!(step < 52, "one-off SSH connection did not become ready");
                } else {
                    eprintln!(
                        "Profile UI smoke passed: profile edits, live pane identity, saved and one-off connections"
                    );
                    self.update(Message::CloseWindow, context);
                    return;
                }
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
                context.spawn_background(move |_|{let question=if step==0{Question::HostKey{host:"loopback.test".into(),port:22,host_key_alias:None,algorithm:"ssh-ed25519".into(),fingerprint:"SHA256:test-fingerprint".into()}}else{Question::Authentication{name:"MFA test".into(),instructions:"Isolated native prompt test".into(),challenges:vec![Challenge{label:"Account".into(),echo:true},Challenge{label:"Password".into(),echo:false}]}};
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
    fn find_selected_text_never_opens_a_queued_gesture_in_a_retargeted_pane_or_login() {
        use super::clipboard_paste::Origin;
        let origin = Origin {
            pane: 3,
            group: 4,
            connection: 5,
        };
        assert!(Find::same_origin(Some(origin), Some(origin)));
        for current in [
            None,
            Some(Origin { pane: 6, ..origin }),
            Some(Origin { group: 6, ..origin }),
            Some(Origin {
                connection: 6,
                ..origin
            }),
        ] {
            assert!(!Find::same_origin(Some(origin), current));
        }
        assert!(!Find::same_origin(None, None));
    }

    #[test]
    fn find_selected_text_only_seeds_the_original_open_search_and_keeps_match_case() {
        let mut find = Find {
            query: "old.*".into(),
            regex: true,
            sensitive: true,
            epoch: 5,
            ..Default::default()
        };
        assert!(!find.accept_selection(5, "after close".into()));
        find.open = true;
        assert!(!find.accept_selection(4, "prior opening".into()));
        assert!(find.regex && find.sensitive);
        assert_eq!(find.query, "old.*");
        assert!(find.accept_selection(5, "雪[.]".into()));
        assert!(!find.regex && find.sensitive);
        assert_eq!(find.query, "雪[.]");
    }

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
    fn tab_color_labels_keep_the_pin_and_report_the_color_to_accessibility() {
        assert_eq!(
            tab_header_content("Production".into(), true, TabColor::Red),
            (
                "📌 🔴 Production".into(),
                "Pinned: Production (Red tab)".into()
            )
        );
        assert_eq!(
            tab_header_content("Terminal".into(), false, TabColor::None),
            ("Terminal".into(), "Terminal".into())
        );
    }

    #[test]
    fn balance_actions_keep_the_offered_layout_after_geometry_or_tab_changes() {
        let mut tabs = Tabs::default();
        let left = tabs.insert(());
        let right = tabs.split(left, (), Axis::Columns).unwrap();
        tabs.set_ratio(&[], 0.3);
        let offered = PaneArrangement {
            balance: tabs.pane_balance(),
            ..Default::default()
        };
        assert!(matches!(
            tab_menu_message("Balance panes    F8".into()),
            Message::Shortcut(Shortcut::BalancePanes)
        ));
        assert!(matches!(
            PaneArrangement::default().balance(),
            Message::ControlIdle
        ));
        let Message::BalancePanes(original) = offered.balance() else {
            unreachable!()
        };
        let measure =
            |target: &PaneBalance| target.measure(6.0, |_, _| Some((160.0, 16.0))).unwrap();
        let other = tabs.insert(());
        tabs.split(other, (), Axis::Columns);
        assert!(!tabs.balance_panes(measure(&original)));
        tabs.select(right);
        tabs.set_ratio(&[], 0.4);
        assert!(!tabs.balance_panes(measure(&original)));
        let fresh = tabs.pane_balance().unwrap();
        assert_ne!(
            command_palette::CommandId::BalancePanes(original),
            command_palette::CommandId::BalancePanes(fresh.clone())
        );
        assert!(tabs.balance_panes(measure(&fresh)));
        assert_eq!(tabs.active_id(), Some(right));
    }

    #[test]
    fn swap_actions_use_the_neighbors_offered_when_invoked() {
        let mut tabs = Tabs::default();
        let first = tabs.insert(());
        let second = tabs.split(first, (), Axis::Columns).unwrap();
        let third = tabs.split(second, (), Axis::Rows).unwrap();
        tabs.select(second);
        let arrangement = PaneArrangement::for_tab(&tabs);
        for (action, direction, available) in [
            (KeyAction::SwapPaneLeft, Direction::Left, true),
            (KeyAction::SwapPaneRight, Direction::Right, false),
            (KeyAction::SwapPaneUp, Direction::Up, false),
            (KeyAction::SwapPaneDown, Direction::Down, true),
        ] {
            assert_eq!(Shortcut::from(action), Shortcut::SwapPane(direction));
            let label = format!("{}    F8", action.binding().label);
            let Message::Shortcut(Shortcut::SwapPane(parsed)) = tab_menu_message(label) else {
                panic!("missing pane swap for {action:?}");
            };
            assert_eq!(parsed, direction);
            assert_eq!(
                matches!(arrangement.message(parsed), Message::SwapPanes(_)),
                available
            );
        }
        let Message::SwapPanes(pending) = arrangement.message(Direction::Left) else {
            unreachable!()
        };
        tabs.select(third);
        assert!(!tabs.swap_panes(pending));
        let fresh = tabs.pane_swap(Direction::Left).unwrap();
        assert_ne!(
            command_palette::CommandId::SwapPanes(pending),
            command_palette::CommandId::SwapPanes(fresh)
        );
    }

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
    fn move_tab_commands_keep_the_group_of_the_invoking_menu_or_key() {
        for (action, direction) in [
            (KeyAction::MoveTabLeft, Direction::Left),
            (KeyAction::MoveTabRight, Direction::Right),
        ] {
            let label = format!("{}    F8", action.binding().label);
            for event in [tab_menu_message(label), Message::Shortcut(action.into())] {
                assert!(matches!(bind_tab_action(event, Some(42)),
                    Message::MoveTab(42, dir) if dir == direction));
            }
            assert!(matches!(
                bind_tab_action(Message::Shortcut(action.into()), None),
                Message::ControlIdle
            ));
        }
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
        let mut fresh = ProfileDraft::default();
        assert!(fresh.host_key_alias().is_none());
        fresh.host_key_alias = "  TeSt.Example  ".into();
        assert_eq!(fresh.host_key_alias().as_deref(), Some("test.example"));
        for (expected, authentication) in [
            (0, Authentication::Agent),
            (
                1,
                Authentication::identity("key", Some("key-cert.pub".into())),
            ),
            (2, Authentication::None),
            (3, Authentication::Password),
            (4, Authentication::KeyboardInteractive),
            (
                SPECIFIC_AGENT_KEY_AUTH,
                Authentication::AgentKey {
                    agent_fingerprint: "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
                        .parse()
                        .unwrap(),
                },
            ),
            (
                AGENT_CERTIFICATE_AUTH,
                Authentication::agent_certificate(
                    "user-cert.pub",
                    Some(
                        "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
                            .parse()
                            .unwrap(),
                    ),
                ),
            ),
        ] {
            let options = SshOptions {
                host: "test.invalid".into(),
                terminal_type: Default::default(),
                color_scheme: ColorScheme::OneHalfLight,
                keepalive: SshKeepalive {
                    interval_seconds: 17,
                    max_unanswered: 4.try_into().unwrap(),
                },
                connect_timeout_seconds: Some(20.try_into().unwrap()),
                font_size: Some(22.try_into().unwrap()),
                tab_color: TabColor::Purple,
                scrollback_lines: "0".parse().unwrap(),
                user: "pilot+character".into(),
                port: 6792,
                agent_pipe: authentication
                    .uses_agent()
                    .then(|| r"\\.\pipe\work-agent".parse().unwrap()),
                authentication,
                host_key: Some("SHA256:test".into()),
                host_key_alias: Some("cockpit.example".into()),
                group_prompt: Some("rack-[12]>>".to_owned().try_into().unwrap()),
                proxy_jump: Vec::new(),
                local_forwards: Vec::new(),
                durable: false,
                player: w11term::options::PlayerOptions {
                    mute: true,
                    game_mode: true,
                    ..Default::default()
                },
            };
            let mut draft = ProfileDraft::from_options("existing".into(), &options);
            assert_eq!(draft.authentication, expected);
            assert_eq!(draft.ssh_options().unwrap().agent_pipe, options.agent_pipe);
            assert_eq!(
                draft.ssh_options().unwrap().authentication,
                options.authentication
            );
            assert_eq!(draft.port, "6792");
            assert_eq!(draft.connect_timeout, "20");
            assert_eq!(draft.keepalive_interval, "17");
            assert_eq!(draft.keepalive_max, "4");
            assert_eq!(draft.scrollback_lines, "0");
            assert_eq!(draft.color_scheme, ColorScheme::OneHalfLight);
            assert_eq!(draft.font_size, "22");
            assert_eq!(draft.fingerprint, "SHA256:test");
            assert_eq!(draft.host, "test.invalid");
            assert_eq!(draft.host_key_alias, "cockpit.example");
            assert_eq!(draft.group_prompt, "rack-[12]>>");
            assert_eq!(
                draft.ssh_options().unwrap().group_prompt,
                options.group_prompt
            );
            assert_eq!(
                draft.ssh_options().unwrap().connect_timeout_seconds,
                options.connect_timeout_seconds
            );
            draft.connect_timeout = "0".into();
            assert!(draft.ssh_options().is_err());
            draft.connect_timeout.clear();
            assert!(
                draft
                    .ssh_options()
                    .unwrap()
                    .connect_timeout_seconds
                    .is_none()
            );
            draft.group_prompt = "[".into();
            assert!(draft.ssh_options().is_err());
            draft.group_prompt.clear();
            assert!(draft.ssh_options().unwrap().group_prompt.is_none());
            assert!(draft.player.mute && draft.player.game_mode);
            assert_eq!(draft.name, "existing");
        }
    }
}
