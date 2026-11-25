//! The terminal engine is independent of Reactor, rendering, and SSH.
use crate::options::{ColorScheme, ResolvedColorScheme, ScrollbackLines};
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use wezterm_term::color::{ColorPalette, SelectionColors, SrgbaTuple};
use wezterm_term::config::NewlineCanon;
use wezterm_term::{
    CellAttributes, CursorPosition, Line, PointerShape, Terminal, TerminalConfiguration,
    TerminalSize,
};

mod color_scheme;
mod export;
mod kitty;
mod retained_rows;
pub use retained_rows::RetainedRows;
mod scrollback_view;
pub use scrollback_view::{
    BookmarkEntry, BookmarkScreen, BookmarkTarget, CommandContext, ScrollbackTarget, ScrollbackView,
};
#[cfg(test)]
mod program_status_tests;
#[cfg(test)]
mod scrollback_tests;
#[cfg(test)]
mod vscode_shell_tests;
pub(crate) use export::{export_text, retained_text_rows};
pub use kitty::KeypadKey;

pub type Rgba = [f32; 4];

pub fn scroll_wheel(
    terminal: &mut Terminal,
    offset: &mut usize,
    last: wezterm_term::MouseEvent,
    lines: isize,
    modifiers: wezterm_term::KeyModifiers,
    alternate_scroll: bool,
) -> anyhow::Result<()> {
    use wezterm_term::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    if (terminal.is_mouse_grabbed() || (terminal.is_alt_screen_active() && alternate_scroll))
        && !modifiers.contains(KeyModifiers::SHIFT)
    {
        let button = if lines > 0 {
            MouseButton::WheelUp(1)
        } else {
            MouseButton::WheelDown(1)
        };
        // The engine emits one report per event, regardless of the count field.
        for _ in 0..lines.unsigned_abs().min(1000) {
            terminal.mouse_event(MouseEvent {
                kind: MouseEventKind::Press,
                button,
                modifiers,
                ..last
            })?;
        }
    } else if !terminal.is_alt_screen_active() {
        *offset = offset
            .saturating_add_signed(lines)
            .min(crate::search::max_offset(terminal));
    }
    Ok(())
}

#[derive(Debug)]
struct Config {
    scrollback_lines: ScrollbackLines,
    palette: ColorPalette,
    scheme: ResolvedColorScheme,
}

impl Config {
    fn new(scrollback_lines: ScrollbackLines, scheme: ResolvedColorScheme) -> Self {
        Self {
            scrollback_lines,
            scheme,
            palette: color_scheme::palette(scheme),
        }
    }
}

impl TerminalConfiguration for Config {
    fn bidi_mode(&self) -> wezterm_term::config::BidiMode {
        wezterm_term::config::BidiMode {
            enabled: true,
            hint: wezterm_bidi::ParagraphDirectionHint::LeftToRight,
        }
    }

    fn color_palette(&self) -> ColorPalette {
        self.palette.clone()
    }

    fn prefers_light_colors(&self) -> bool {
        self.scheme == ResolvedColorScheme::OneHalfLight
    }

    fn scrollback_size(&self) -> usize {
        self.scrollback_lines.get()
    }
    fn enable_kitty_graphics(&self) -> bool {
        true
    }
    fn enable_kitty_keyboard(&self) -> bool {
        true
    }
    fn canonicalize_pasted_newlines(&self) -> NewlineCanon {
        NewlineCanon::CarriageReturn
    }
    fn alternate_buffer_wheel_scroll_speed(&self) -> u8 {
        1
    }
}

#[derive(Debug)]
pub enum TransportInput {
    Authenticate(crate::options::Authentication),
    Sftp(crate::sftp::Job),
    DisconnectShell {
        connection: u64,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    Data(Vec<u8>),
    /// Parser-generated reply on a viewer transport that distinguishes it from user input.
    Automatic(Vec<u8>),
    Raw(Vec<u8>, tokio::sync::oneshot::Sender<Result<(), String>>),
    /// The receiving transport must still be the one that approved this write.
    Scoped {
        connection: u64,
        bytes: Vec<u8>,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
        kind: ScopedInput,
    },
    Resize(TerminalSize),
}

impl TransportInput {
    pub(crate) fn reject_input(self, reason: &str) {
        match self {
            Self::Raw(_, reply) | Self::DisconnectShell { reply, .. } => {
                let _ = reply.send(Err(reason.into()));
            }
            Self::Scoped { reply, kind, .. } => kind.reject(reply, reason),
            _ => unreachable!("only acknowledged input has a reply"),
        }
    }
}

#[derive(Clone, Debug)]
pub enum ScopedInput {
    Clipboard,
    Notification,
    GroupCommand(crate::events::GroupWriteReceipt),
    GroupInterrupt(crate::events::GroupInterruptTarget),
}

impl ScopedInput {
    pub(crate) fn reject(
        &self,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
        reason: &str,
    ) {
        if let Self::GroupCommand(receipt) = self {
            receipt.reject(reason);
        }
        let _ = reply.send(Err(reason.into()));
    }

    pub(crate) fn stale_error(&self) -> &'static str {
        match self {
            Self::Clipboard => {
                "The pane connection changed. Paste again to use the new connection."
            }
            Self::Notification => "The terminal connection changed before the notification reply.",
            Self::GroupCommand(_) => "The host reconnected before the command was sent.",
            Self::GroupInterrupt(_) => "The host reconnected before Ctrl+C was sent.",
        }
    }
}

pub(crate) const GROUP_COMMAND_ENDED: &str = "The selected group command is no longer running.";

struct ReplyWriter {
    sender: mpsc::UnboundedSender<TransportInput>,
    message: fn(Vec<u8>) -> TransportInput,
}

impl Write for ReplyWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.sender
            .send((self.message)(bytes.to_vec()))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "session closed"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn create(size: TerminalSize, writer: mpsc::UnboundedSender<TransportInput>) -> Terminal {
    create_with_profile(
        size,
        writer,
        ScrollbackLines::default(),
        ColorScheme::Default,
    )
}

pub fn create_with_profile(
    size: TerminalSize,
    writer: mpsc::UnboundedSender<TransportInput>,
    scrollback_lines: ScrollbackLines,
    scheme: ColorScheme,
) -> Terminal {
    let resolved = scheme.resolve(|| *crate::windows_appearance::subscribe().borrow());
    create_with_palette(size, writer, scrollback_lines, resolved)
}

pub(crate) fn create_with_palette(
    size: TerminalSize,
    writer: mpsc::UnboundedSender<TransportInput>,
    scrollback_lines: ScrollbackLines,
    scheme: ResolvedColorScheme,
) -> Terminal {
    Terminal::with_nonblocking_writer(
        size,
        Arc::new(Config::new(scrollback_lines, scheme)),
        "w11term",
        env!("CARGO_PKG_VERSION"),
        Box::new(ReplyWriter {
            sender: writer,
            message: TransportInput::Data,
        }),
    )
}

pub(crate) fn set_color_scheme(
    terminal: &mut Terminal,
    scrollback_lines: ScrollbackLines,
    scheme: ResolvedColorScheme,
) {
    terminal.set_config(Arc::new(Config::new(scrollback_lines, scheme)));
}

pub(crate) fn replace_transport(
    terminal: &mut Terminal,
    writer: mpsc::UnboundedSender<TransportInput>,
) {
    terminal.replace_nonblocking_writer(Box::new(ReplyWriter {
        sender: writer,
        message: TransportInput::Data,
    }));
}

/// View archived or reconstructed output without applying historical desktop effects.
pub(crate) fn configure_output_viewer(terminal: &mut Terminal, events: &crate::events::Events) {
    struct ViewerAlerts(crate::events::Events);
    impl wezterm_term::AlertHandler for ViewerAlerts {
        fn alert(&mut self, alert: wezterm_term::Alert) {
            if let wezterm_term::Alert::WindowTitleChanged(title) = alert {
                self.0.push("title", serde_json::json!({"title": title}));
            }
        }
    }
    events.command_stream(None);
    events.notification_stream(None);
    terminal.clear_clipboard();
    terminal.set_notification_handler(Box::new(ViewerAlerts(events.clone())));
}

/// Herdr can send a screen reconstruction on attach or while a viewer falls behind.
/// Screen paint and subsequent live bytes share the same stream.
pub(crate) fn enter_herdr_viewer(
    terminal: &mut Terminal,
    writer: &mpsc::UnboundedSender<TransportInput>,
    events: &crate::events::Events,
) {
    configure_output_viewer(terminal, events);
    terminal.set_nonblocking_viewer_writers(
        Box::new(ReplyWriter {
            sender: writer.clone(),
            message: TransportInput::Data,
        }),
        Box::new(ReplyWriter {
            sender: writer.clone(),
            message: TransportInput::Automatic,
        }),
    );
}

/// WezTerm's kitty encoder lives in its input-types crate, outside Terminal::key_down.
pub fn send_key(
    terminal: &mut Terminal,
    writer: &mpsc::UnboundedSender<TransportInput>,
    key: wezterm_term::KeyCode,
    modifiers: wezterm_term::KeyModifiers,
    down: bool,
) -> anyhow::Result<()> {
    send_key_event(terminal, writer, key, modifiers, down, false)
}
pub fn send_key_event(
    terminal: &mut Terminal,
    writer: &mpsc::UnboundedSender<TransportInput>,
    key: wezterm_term::KeyCode,
    modifiers: wezterm_term::KeyModifiers,
    down: bool,
    repeat: bool,
) -> anyhow::Result<()> {
    send_key_event_detailed(
        terminal,
        writer,
        key,
        modifiers,
        down,
        repeat,
        &KeyDetails::default(),
    )
}
#[derive(Clone, Debug, Default)]
pub struct KeyDetails {
    pub base: Option<char>,
    /// Unshifted US PC-101 position when distinct from the active layout.
    pub physical: Option<char>,
    /// Active layout's Shift level for the same position, independent of Caps Lock.
    pub shifted: Option<char>,
    /// Text for this physical repeat under the current Windows modifier state.
    pub repeat_text: Option<String>,
    /// Right Alt and its synthesized Control were consumed by this layout.
    pub altgr: bool,
    pub text: Option<String>,
    pub keypad: Option<KeypadKey>,
    pub leds: wezterm_input_types::KeyboardLedStatus,
}
pub fn send_key_event_detailed(
    terminal: &mut Terminal,
    writer: &mpsc::UnboundedSender<TransportInput>,
    key: wezterm_term::KeyCode,
    modifiers: wezterm_term::KeyModifiers,
    down: bool,
    repeat: bool,
    details: &KeyDetails,
) -> anyhow::Result<()> {
    if let Some(encoded) = key_override(terminal, key, modifiers, down, repeat, details)? {
        if !encoded.is_empty() {
            writer.send(TransportInput::Data(encoded.into_bytes()))?;
        }
        return Ok(());
    }
    if down {
        terminal.key_down(key, modifiers)
    } else {
        terminal.key_up(key, modifiers)
    }
}

fn key_override(
    terminal: &Terminal,
    key: wezterm_term::KeyCode,
    modifiers: wezterm_term::KeyModifiers,
    down: bool,
    repeat: bool,
    details: &KeyDetails,
) -> anyhow::Result<Option<String>> {
    match terminal.get_keyboard_encoding() {
        termwiz::input::KeyboardEncoding::Kitty(flags) => Ok(encode_kitty_key_event(
            flags, key, modifiers, down, repeat, details,
        )),
        _ if down && details.text.is_some() => Ok(details.text.clone()),
        termwiz::input::KeyboardEncoding::Xterm if down && terminal.xterm_other_keys().0 != 0 => {
            let key = if repeat {
                details
                    .repeat_text
                    .as_deref()
                    .and_then(|text| {
                        let mut chars = text.chars();
                        let c = chars.next()?;
                        chars
                            .next()
                            .is_none()
                            .then_some(wezterm_term::KeyCode::Char(c))
                    })
                    .unwrap_or(key)
            } else {
                key
            };
            let encoded = if details.keypad.is_some() {
                terminal.encode_keypad_key(key, modifiers, down)?
            } else {
                terminal.encode_key(key, modifiers, down)?
            };
            Ok(Some(encoded))
        }
        _ => Ok(None),
    }
}

pub(crate) fn encode_key_input(
    terminal: &Terminal,
    key: wezterm_term::KeyCode,
    modifiers: wezterm_term::KeyModifiers,
    down: bool,
    repeat: bool,
    details: &KeyDetails,
) -> anyhow::Result<String> {
    match key_override(terminal, key, modifiers, down, repeat, details)? {
        Some(encoded) => Ok(encoded),
        None => terminal.encode_key(key, modifiers, down),
    }
}

fn encode_kitty_key_event(
    flags: wezterm_input_types::KittyKeyboardFlags,
    key: wezterm_term::KeyCode,
    modifiers: wezterm_term::KeyModifiers,
    down: bool,
    repeat: bool,
    details: &KeyDetails,
) -> Option<String> {
    use wezterm_input_types::{KeyCode as K, KeyEvent, KeyboardLedStatus, Modifiers};
    use wezterm_term::KeyCode as L;
    let all =
        flags.contains(wezterm_input_types::KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES);
    let reports = flags.contains(wezterm_input_types::KittyKeyboardFlags::REPORT_EVENT_TYPES);
    if let Some(native) = kitty::NativeKey::from_event(flags, key, details) {
        return Some(native.encode(flags, key, modifiers, details, down, repeat));
    }
    let safety_key = matches!(
        key,
        L::Enter | L::Tab | L::Backspace | L::Char('\r' | '\t' | '\x08')
    );
    if safety_key && !all && !down {
        return Some(String::new());
    }
    if let Some(code) = match key {
        L::Shift | L::LeftShift => Some(57441),
        L::RightShift => Some(57447),
        L::Control | L::LeftControl => Some(57442),
        L::RightControl => Some(57448),
        L::Alt | L::LeftAlt | L::LeftMenu => Some(57443),
        L::RightAlt | L::RightMenu => Some(57449),
        L::Super | L::LeftWindows => Some(57444),
        L::RightWindows => Some(57450),
        L::CapsLock => Some(57358),
        L::NumLock => Some(57360),
        _ => None,
    } {
        if all && (down || reports) {
            let modifier = 1
                + u8::from(modifiers.contains(wezterm_term::KeyModifiers::SHIFT))
                + 2 * u8::from(modifiers.contains(wezterm_term::KeyModifiers::ALT))
                + 4 * u8::from(modifiers.contains(wezterm_term::KeyModifiers::CTRL))
                + 8 * u8::from(modifiers.contains(wezterm_term::KeyModifiers::SUPER));
            let event = if !reports {
                ""
            } else if !down {
                ":3"
            } else if repeat {
                ":2"
            } else {
                ""
            };
            return Some(format!("\x1b[{code};{modifier}{event}u"));
        }
        return Some(String::new());
    }
    if let Some(text) = &details.text
        && !all
    {
        return Some(if down { text.clone() } else { String::new() });
    }
    let rich = match key {
        L::Char(c) => K::Char(c),
        L::Enter => K::Char('\r'),
        L::Escape => K::Char('\x1b'),
        L::Tab => K::Char('\t'),
        L::Backspace => K::Char('\x08'),
        L::Delete => K::Char('\x7f'),
        L::LeftArrow => K::LeftArrow,
        L::RightArrow => K::RightArrow,
        L::UpArrow => K::UpArrow,
        L::DownArrow => K::DownArrow,
        L::Home => K::Home,
        L::End => K::End,
        L::PageUp => K::PageUp,
        L::PageDown => K::PageDown,
        L::Insert => K::Insert,
        L::Function(n) => K::Function(n),
        _ => return None,
    };
    let mut mods = Modifiers::NONE;
    if modifiers.contains(wezterm_term::KeyModifiers::SHIFT) {
        mods |= Modifiers::SHIFT;
    }
    if modifiers.contains(wezterm_term::KeyModifiers::CTRL) {
        mods |= Modifiers::CTRL;
    }
    if modifiers.contains(wezterm_term::KeyModifiers::ALT) {
        mods |= Modifiers::ALT;
    }
    if modifiers.contains(wezterm_term::KeyModifiers::SUPER) {
        mods |= Modifiers::SUPER;
    }
    // The pinned encoder treats an unmodified DEL character as Backspace
    // before reaching its functional-key branch. Preserve physical Delete
    // for every negotiated flag set and every event type.
    if matches!(key, L::Delete | L::Char('\x7f')) {
        let reports = flags.contains(wezterm_input_types::KittyKeyboardFlags::REPORT_EVENT_TYPES);
        if !down && !reports {
            return Some(String::new());
        }
        let modifier = 1
            + u8::from(mods.contains(Modifiers::SHIFT))
            + 2 * u8::from(mods.contains(Modifiers::ALT))
            + 4 * u8::from(mods.contains(Modifiers::CTRL))
            + 8 * u8::from(mods.contains(Modifiers::SUPER));
        let event = if !reports {
            ""
        } else if !down {
            ":3"
        } else if repeat {
            ":2"
        } else {
            ""
        };
        return Some(format!("\x1b[3;{modifier}{event}~"));
    }
    let report_repeat = repeat
        && down
        && (!safety_key || all)
        && flags.contains(wezterm_input_types::KittyKeyboardFlags::REPORT_EVENT_TYPES);
    let encode_flags = if report_repeat {
        flags | wezterm_input_types::KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
    } else {
        flags
    };
    let mut encoded = KeyEvent {
        key: rich.clone(),
        modifiers: mods,
        leds: KeyboardLedStatus::empty(),
        repeat_count: 1,
        key_is_down: down,
        raw: None,
        #[cfg(windows)]
        win32_uni_char: None,
    }
    .encode_kitty(encode_flags);
    // Upstream's legacy-character branch also bypasses REPORT_ALL_KEYS.
    // Repair that branch before adding the repeat event type.
    if encode_flags
        .contains(wezterm_input_types::KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES)
        && down
        && !encoded.starts_with("\x1b[")
        && let K::Char(character) = rich
    {
        let base = if character == '\x08' {
            '\x7f'
        } else {
            character.to_ascii_lowercase()
        };
        let modifier = 1
            + u8::from(mods.contains(Modifiers::SHIFT))
            + 2 * u8::from(mods.contains(Modifiers::ALT))
            + 4 * u8::from(mods.contains(Modifiers::CTRL))
            + 8 * u8::from(mods.contains(Modifiers::SUPER));
        let alternate = if encode_flags
            .contains(wezterm_input_types::KittyKeyboardFlags::REPORT_ALTERNATE_KEYS)
            && base != character
        {
            format!(":{}", character as u32)
        } else {
            String::new()
        };
        let text = if encode_flags
            .contains(wezterm_input_types::KittyKeyboardFlags::REPORT_ASSOCIATED_TEXT)
            && !character.is_control()
            && !mods.intersects(Modifiers::CTRL | Modifiers::ALT | Modifiers::SUPER)
        {
            format!(";{}", character as u32)
        } else {
            String::new()
        };
        encoded = format!("\x1b[{}{alternate};{modifier}{text}u", base as u32);
    }
    // The pinned upstream encoder only distinguishes press/release. Supply
    // the negotiated repeat event type without changing its key encoding.
    if report_repeat
        && encoded.starts_with("\x1b[")
        && let Some(separator) = encoded.find(';')
    {
        let start = separator + 1;
        let digits = encoded[start..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        encoded.insert_str(start + digits, ":2");
    }
    if encoded.starts_with("\x1b[") && encoded.ends_with('u') {
        if let Some(base) = details.base
            && let Some(separator) = encoded.find(';')
        {
            let alternate = if flags
                .contains(wezterm_input_types::KittyKeyboardFlags::REPORT_ALTERNATE_KEYS)
                && modifiers.contains(wezterm_term::KeyModifiers::SHIFT)
                && let L::Char(character) = key
                && character != base
            {
                format!(":{}", character as u32)
            } else {
                String::new()
            };
            encoded.replace_range(2..separator, &format!("{}{alternate}", base as u32));
        }
        if let Some(text) = &details.text
            && down
            && flags.contains(wezterm_input_types::KittyKeyboardFlags::REPORT_ASSOCIATED_TEXT)
        {
            let codepoints = text
                .chars()
                .map(|c| (c as u32).to_string())
                .collect::<Vec<_>>()
                .join(":");
            let separator = encoded.find(';').unwrap();
            let text_start = encoded[separator + 1..]
                .find(';')
                .map(|i| separator + 1 + i)
                .unwrap_or(encoded.len() - 1);
            encoded.replace_range(text_start..encoded.len() - 1, &format!(";{codepoints}"));
        }
    }
    Some(encoded)
}

/// Encode the text and Enter together so they cannot reach different connections.
pub(crate) fn group_command_bytes(terminal: &Terminal, text: &str) -> anyhow::Result<Vec<u8>> {
    use wezterm_term::{KeyCode, KeyModifiers};
    let mut encoded = terminal.encode_paste(text);
    encoded.push_str(&group_key_press(
        terminal,
        KeyCode::Enter,
        KeyModifiers::NONE,
    )?);
    Ok(encoded.into_bytes())
}

pub(crate) fn group_interrupt_bytes(terminal: &Terminal) -> anyhow::Result<Vec<u8>> {
    use wezterm_term::{KeyCode, KeyModifiers};
    Ok(group_key_press(terminal, KeyCode::Char('c'), KeyModifiers::CTRL)?.into_bytes())
}

fn group_key_press(
    terminal: &Terminal,
    key: wezterm_term::KeyCode,
    modifiers: wezterm_term::KeyModifiers,
) -> anyhow::Result<String> {
    let mut encoded = String::new();
    for down in [true, false] {
        let edge = encode_key_input(
            terminal,
            key,
            modifiers,
            down,
            false,
            &KeyDetails::default(),
        )?;
        encoded.push_str(&edge);
    }
    Ok(encoded)
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub generation: u64,
    pub rows: Vec<Arc<Line>>,
    /// Whether the last visible cell continues onto the following physical row.
    row_wraps: Vec<bool>,
    pub palette: ColorPalette,
    pub selection_colors: SelectionColors,
    pub visual_bell_color: Option<SrgbaTuple>,
    pub cursor: CursorPosition,
    pub cursor_visible: bool,
    pub application_cursor_guide: bool,
    pub pointer_shape: Option<PointerShape>,
    pub cols: usize,
    pub scroll_offset: usize,
    pub title: String,
    application_title: Option<Arc<ApplicationTitle>>,
    pub command_history: Arc<std::collections::VecDeque<Arc<wezterm_term::CompletedCommand>>>,
    pub failed_command_rows: Arc<[isize]>,
    pub scrollback_bookmarks: Arc<[isize]>,
    pub command_context: Option<CommandContext>,
    pub reported_directory: Option<Arc<str>>,
    pub status: String,
    pub bytes_received: u64,
    pub first_stable_row: isize,
    pub oldest_stable_row: isize,
    pub(crate) row_epoch: u64,
    pub(crate) seqno: usize,
    pub(crate) history_clear_count: u64,
    /// Most recent successful local history clear. The floor is the first row
    /// retained at the time of that clear, even if new output has since arrived.
    pub scrollback_clear_generation: u64,
    pub scrollback_clear_floor: Option<isize>,
    pub alternate: bool,
    pub bracketed_paste: bool,
    pub mouse_grabbed: bool,
    pub search: Option<crate::search::SearchSnapshot>,
    pub prompt: Option<crate::prompts::Prompt>,
    pub connection_generation: u64,
    pub size: TerminalSize,
    images: HashMap<usize, FrozenImage>,
    /// Indices of rows whose cell data references the frozen image table.
    image_rows: Vec<usize>,
    cell_maps: std::sync::OnceLock<Arc<[crate::text_layout::CellMap]>>,
}

#[derive(Clone, Debug)]
struct FrozenImage {
    kitty_id: Option<u32>,
    hash: [u8; 32],
    data: Arc<wezterm_term::image::ImageDataType>,
}

#[derive(Debug)]
struct ApplicationTitle {
    raw: Arc<str>,
    caption: String,
}

impl Snapshot {
    pub fn has_command(&self, link: crate::events::CommandLink) -> bool {
        self.command_for_link(link).is_some()
    }

    pub fn command_for_link(
        &self,
        link: crate::events::CommandLink,
    ) -> Option<&Arc<wezterm_term::CompletedCommand>> {
        if self.connection_generation != link.generation {
            return None;
        }
        self.command_history
            .iter()
            .find(|command| command.id == link.id)
    }

    /// Summarize the newest completed command and identify its retained history entry.
    pub fn latest_command_output_preview(&self) -> Option<(crate::events::CommandLink, String)> {
        let command = self.command_history.back()?;
        let status = command.exit_code.map_or_else(
            || "Exit status unavailable".to_owned(),
            |code| format!("Exit {code}"),
        );
        let line = match &command.output {
            Ok(text) => command_output_tail(text),
            Err(_) => "Output unavailable".to_owned(),
        };
        Some((
            crate::events::CommandLink {
                generation: self.connection_generation,
                id: command.id,
            },
            format!("{status} · {line}"),
        ))
    }

    /// Summarize the lowest nonblank row of this snapshot, masking concealed cells.
    pub fn last_visible_text_line(&self) -> Option<String> {
        self.rows.iter().rev().find_map(|row| {
            let mut text = String::new();
            let mut remaining = 200;
            for cell in row
                .visible_cells()
                .take_while(|cell| cell.cell_index() < self.cols)
            {
                let value = if cell.attrs().invisible() {
                    " "
                } else {
                    cell.str()
                };
                let value = if text.is_empty() {
                    value.trim_start()
                } else {
                    value
                };
                for ch in value.chars().take(remaining) {
                    text.push(ch);
                    remaining -= 1;
                }
                if remaining == 0 {
                    break;
                }
            }
            let caption = crate::events::display_field(&text);
            (!caption.is_empty()).then_some(caption)
        })
    }

    /// A bounded single-line caption from the application's window title.
    /// A shell that has only set an icon title still uses its profile label.
    pub fn application_title(&self) -> Option<&str> {
        self.application_title
            .as_ref()
            .map(|title| title.caption.as_str())
            .filter(|s| !s.is_empty())
    }

    pub fn copy_output_unavailable(&self) -> Option<&'static str> {
        match self.command_history.back() {
            Some(command) => command.output.as_ref().err().copied(),
            None => Some("waiting for shell markers"),
        }
    }

    pub fn capture(
        terminal: &wezterm_term::TerminalState,
        previous: Option<&Self>,
        generation: u64,
        scroll_offset: usize,
        status: String,
        bytes_received: u64,
    ) -> Self {
        // DECSCNM exchanges the display defaults. Leave the underlying OSC colors
        // and indexed palette intact so queries and later resets see the originals.
        let mut palette = terminal.palette();
        if terminal.get_reverse_video() {
            std::mem::swap(&mut palette.foreground, &mut palette.background);
        }
        terminal.recolor_indexed_kitty_images(&palette);
        let screen = terminal.screen();
        let offset = if terminal.is_alt_screen_active() {
            0
        } else {
            scroll_offset.min(screen.phys_row(0))
        };
        let start = screen.phys_row(0).saturating_sub(offset);
        let first_stable_row = screen.phys_to_stable_row_index(start);
        let alternate = terminal.is_alt_screen_active();
        let mut lines = screen.lines_in_phys_range(start..start + screen.physical_rows);
        terminal.project_kitty_placements(&mut lines, start);
        let mut rows = Vec::with_capacity(lines.len());
        let mut row_wraps = Vec::with_capacity(lines.len());
        let mut images = HashMap::new();
        let mut image_rows = Vec::new();
        for (index, line) in lines.into_iter().enumerate() {
            let stable = first_stable_row + index as isize;
            let candidate = previous
                .filter(|old| old.row_epoch == screen.row_epoch())
                .and_then(|old| {
                    let i = usize::try_from(stable - old.first_stable_row).ok()?;
                    let row = old.rows.get(i)?;
                    let imaged = old.image_rows.binary_search(&i).is_ok();
                    // Projected images can move without changing the underlying row revision.
                    (row.current_seqno() == line.current_seqno() && (!imaged || **row == line))
                        .then_some((row, old.row_wraps[i], imaged))
                });
            // Two placements may have equal pixels but different image allocations.
            // Freeze tiles from the row we'll keep so its pointer keys resolve.
            let scan = match candidate {
                Some((row, _, true)) => row.as_ref(),
                _ => &line,
            };
            let mut wrapped = false;
            let mut has_images = false;
            for cell in scan.visible_cells() {
                wrapped = cell.attrs().wrapped();
                for tile in cell.attrs().images().into_iter().flatten() {
                    has_images = true;
                    let source = tile.image_data();
                    let identity = Arc::as_ptr(source) as usize;
                    if images.contains_key(&identity) {
                        continue;
                    }
                    let guard = source.data();
                    let index = terminal.kitty_animation_frame(tile.image_id());
                    use wezterm_term::image::ImageDataType;
                    let hash = match &*guard {
                        ImageDataType::Rgba8 { hash, .. } => *hash,
                        ImageDataType::AnimRgba8 { hashes, .. } if index < hashes.len() => {
                            hashes[index]
                        }
                        _ => source.hash(),
                    };
                    let frozen = previous
                        .and_then(|old| old.images.get(&identity))
                        .filter(|old| old.hash == hash)
                        .cloned()
                        .unwrap_or_else(|| {
                            let data = match &*guard {
                                ImageDataType::AnimRgba8 {
                                    width,
                                    height,
                                    frames,
                                    hashes,
                                    ..
                                } if index < frames.len() && index < hashes.len() => {
                                    ImageDataType::Rgba8 {
                                        width: *width,
                                        height: *height,
                                        data: frames[index].clone(),
                                        hash: hashes[index],
                                    }
                                }
                                data => data.clone(),
                            };
                            FrozenImage {
                                hash,
                                kitty_id: tile.image_id(),
                                data: Arc::new(data),
                            }
                        });
                    images.insert(identity, frozen);
                }
            }
            row_wraps.push(wrapped);
            if has_images {
                image_rows.push(index);
            }
            rows.push(match candidate {
                Some((row, old_wrap, imaged)) if old_wrap == wrapped && imaged == has_images => {
                    Arc::clone(row)
                }
                _ => Arc::new(line),
            });
        }
        let cursor = terminal.cursor_pos();
        Self {
            generation,
            rows,
            row_wraps,
            palette,
            selection_colors: terminal.selection_color_overrides(),
            visual_bell_color: terminal.visual_bell_color(),
            cursor,
            cursor_visible: offset == 0
                && cursor.visibility == termwiz::surface::CursorVisibility::Visible,
            application_cursor_guide: terminal.application_cursor_guide(),
            pointer_shape: terminal.application_pointer_shape(),
            cols: screen.physical_cols,
            scroll_offset: offset,
            title: terminal.get_title().to_owned(),
            application_title: terminal.application_window_title().map(|raw| {
                previous
                    .and_then(|s| s.application_title.as_ref())
                    .filter(|old| Arc::ptr_eq(&old.raw, raw))
                    .cloned()
                    .unwrap_or_else(|| {
                        Arc::new(ApplicationTitle {
                            raw: raw.clone(),
                            caption: crate::events::display_field(raw),
                        })
                    })
            }),
            command_history: terminal.command_history().clone(),
            failed_command_rows: scrollback_view::failed_command_rows(terminal, previous),
            scrollback_bookmarks: scrollback_view::bookmark_rows(screen, previous),
            command_context: scrollback_view::command_context(
                terminal,
                first_stable_row,
                offset,
                previous,
            ),
            reported_directory: terminal.reported_current_dir().cloned(),
            status,
            bytes_received,
            first_stable_row,
            oldest_stable_row: screen.phys_to_stable_row_index(0),
            row_epoch: screen.row_epoch(),
            seqno: terminal.current_seqno(),
            history_clear_count: screen.history_clear_count(),
            scrollback_clear_generation: 0,
            scrollback_clear_floor: None,
            alternate,
            bracketed_paste: terminal.bracketed_paste_enabled(),
            mouse_grabbed: terminal.is_mouse_grabbed(),
            search: None,
            prompt: None,
            connection_generation: 0,
            size: terminal.get_size(),
            images,
            image_rows,
            cell_maps: Default::default(),
        }
    }

    /// Shared by drawing, local pointer input, and assistive clients.
    pub fn cell_map(&self, row: usize) -> Option<&crate::text_layout::CellMap> {
        self.cell_maps().get(row)
    }

    pub fn row_is_wrapped(&self, row: usize) -> bool {
        self.row_wraps[row]
    }

    pub fn visual_cursor_column(&self) -> usize {
        usize::try_from(self.cursor.y)
            .ok()
            .and_then(|row| self.cell_map(row))
            .map_or(self.cursor.x, |map| map.visual_grid(self.cursor.x))
    }

    pub fn cell_maps(&self) -> &Arc<[crate::text_layout::CellMap]> {
        self.cell_maps.get_or_init(|| {
            self.rows
                .iter()
                .map(|line| crate::text_layout::CellMap::new(line, self.cols))
                .collect()
        })
    }

    /// Pixels referenced by a presentation never change underneath the renderer.
    pub fn image_data(
        &self,
        source: &Arc<wezterm_term::image::ImageData>,
    ) -> &wezterm_term::image::ImageDataType {
        &self.images[&(Arc::as_ptr(source) as usize)].data
    }

    pub fn animation_deadline(
        &self,
        terminal: &wezterm_term::TerminalState,
    ) -> Option<std::time::Instant> {
        self.images
            .values()
            .filter_map(|img| terminal.kitty_animation_deadline_for(img.kitty_id?))
            .min()
    }

    pub fn same_presentation(&self, old: &Self) -> bool {
        self.size == old.size
            && self.row_epoch == old.row_epoch
            && self.history_clear_count == old.history_clear_count
            && self.first_stable_row == old.first_stable_row
            && self.oldest_stable_row == old.oldest_stable_row
            && self.scrollback_clear_generation == old.scrollback_clear_generation
            && self.scroll_offset == old.scroll_offset
            && self.alternate == old.alternate
            && self.bracketed_paste == old.bracketed_paste
            && self.cursor_visible == old.cursor_visible
            && self.application_cursor_guide == old.application_cursor_guide
            && self.pointer_shape == old.pointer_shape
            && self.mouse_grabbed == old.mouse_grabbed
            && (
                self.cursor.x,
                self.cursor.y,
                self.cursor.shape,
                self.cursor.visibility,
            ) == (
                old.cursor.x,
                old.cursor.y,
                old.cursor.shape,
                old.cursor.visibility,
            )
            && self.palette == old.palette
            && self.selection_colors == old.selection_colors
            && self.visual_bell_color == old.visual_bell_color
            && self.title == old.title
            && self.application_title() == old.application_title()
            && Arc::ptr_eq(&self.command_history, &old.command_history)
            && self.failed_command_rows == old.failed_command_rows
            && self.scrollback_bookmarks == old.scrollback_bookmarks
            && self.command_context == old.command_context
            && self.reported_directory == old.reported_directory
            && self.status == old.status
            && self.bytes_received / 1024 == old.bytes_received / 1024
            && self.connection_generation == old.connection_generation
            && self.prompt.as_ref().map(|p| p.id) == old.prompt.as_ref().map(|p| p.id)
            && self.search == old.search
            && self.rows.len() == old.rows.len()
            && self
                .rows
                .iter()
                .zip(&old.rows)
                .all(|(a, b)| Arc::ptr_eq(a, b) || a == b)
            && self.images.len() == old.images.len()
            && self
                .images
                .iter()
                .all(|(id, image)| old.images.get(id).is_some_and(|old| old.hash == image.hash))
    }
}

fn command_output_tail(text: &str) -> String {
    const SCAN_BYTES: usize = 4096;
    let start = text.ceil_char_boundary(text.len().saturating_sub(SCAN_BYTES));
    text[start..]
        .rsplit(['\r', '\n', '\u{2028}', '\u{2029}'])
        .find_map(|line| {
            let caption = crate::events::display_field(line.trim());
            (!caption.is_empty()).then_some(caption)
        })
        .unwrap_or_else(|| {
            if start == 0 {
                "No output".to_owned()
            } else {
                "Open history for earlier output".to_owned()
            }
        })
}

/// Only an immutable frame crosses onto the UI thread. Output is never queued as frames.
mod modes;
pub use modes::TerminalModes;

#[derive(Clone, Debug)]
pub struct FrameStore(Arc<(Mutex<ViewState>, Condvar, tokio::sync::Notify)>);

#[derive(Debug)]
struct ViewState {
    frame: Arc<Snapshot>,
    inspection: Option<TerminalModes>,
    inbox: Option<WindowInbox>,
}

#[derive(Debug)]
struct WindowInbox {
    inbox: crate::events::Inbox,
    pane: u64,
    follows_title: bool,
}

impl FrameStore {
    pub fn new(frame: Snapshot) -> Self {
        Self(Arc::new((
            Mutex::new(ViewState {
                frame: Arc::new(frame),
                inspection: None,
                inbox: None,
            }),
            Condvar::new(),
            tokio::sync::Notify::new(),
        )))
    }
    pub fn load(&self) -> Arc<Snapshot> {
        self.0.0.lock().unwrap().frame.clone()
    }
    pub fn publish(&self, frame: Snapshot) {
        let inbox = {
            let mut state = self.0.0.lock().unwrap();
            let inbox = state
                .inbox
                .as_ref()
                .filter(|owner| {
                    let old = &state.frame;
                    (owner.follows_title && frame.application_title() != old.application_title())
                        || ((frame.connection_generation != old.connection_generation
                            || !Arc::ptr_eq(&frame.command_history, &old.command_history))
                            && owner.inbox.command_links_changed(owner.pane, |link| {
                                frame.has_command(link) != old.has_command(link)
                            }))
                })
                .map(|owner| owner.inbox.clone());
            state.frame = Arc::new(frame);
            inbox
        };
        self.0.1.notify_all();
        self.0.2.notify_waiters();
        if let Some(inbox) = inbox {
            inbox.refresh();
        }
    }

    /// Wake the owning window when a committed caption or inbox link changes.
    pub fn watch_window(&self, owner: Option<(&crate::events::Inbox, u64, bool)>) {
        self.0.0.lock().unwrap().inbox = owner.map(|(inbox, pane, follows_title)| WindowInbox {
            inbox: inbox.clone(),
            pane,
            follows_title,
        });
    }
    pub async fn changed(&self, generation: u64, deadline: tokio::time::Instant) -> bool {
        let changed = self.0.2.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if self.load().generation != generation {
            return true;
        }
        tokio::time::timeout_at(deadline, changed).await.is_ok()
    }
    pub fn inspection(&self) -> Option<TerminalModes> {
        self.0.0.lock().unwrap().inspection
    }
    pub(crate) fn set_inspection(&self, modes: Option<TerminalModes>) {
        let mut state = self.0.0.lock().unwrap();
        if state.inspection != modes {
            state.inspection = modes;
            // Wake the native watcher only. Control consumers await committed frames.
            self.0.1.notify_all();
        }
    }
    pub fn wait_for_change(&self, generation: u64, cancelled: impl Fn() -> bool) {
        self.wait_for_view(generation, None, cancelled);
    }
    /// Wait on the frame and optionally also the last observed inspector value.
    /// An open inspector may still be waiting for its initial mode report.
    pub fn wait_for_view(
        &self,
        generation: u64,
        inspected: Option<Option<TerminalModes>>,
        cancelled: impl Fn() -> bool,
    ) {
        let mut state = self.0.0.lock().unwrap();
        while state.frame.generation == generation
            && inspected.is_none_or(|previous| previous == state.inspection)
            && !cancelled()
        {
            state = self
                .0
                .1
                .wait_timeout(state, Duration::from_millis(250))
                .unwrap()
                .0;
        }
    }
}

pub fn rgba(color: SrgbaTuple) -> Rgba {
    [color.0, color.1, color.2, color.3]
}

pub fn colors(attrs: &CellAttributes, palette: &ColorPalette) -> (Rgba, Rgba) {
    let mut fg = rgba(palette.resolve_fg(attrs.foreground()));
    let mut bg = rgba(palette.resolve_bg(attrs.background()));
    if attrs.reverse() {
        std::mem::swap(&mut fg, &mut bg);
    }
    if attrs.invisible() {
        fg = bg;
    }
    if attrs.intensity() == wezterm_term::Intensity::Half {
        fg[..3].iter_mut().for_each(|v| *v *= 0.6);
    }
    (fg, bg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wezterm_term::{KeyCode, KeyModifiers};

    fn watched_terminal() -> (Terminal, crate::events::Events) {
        let (mut term, _) = engine();
        let events = crate::events::Events::default();
        term.set_notification_handler(Box::new(crate::sound::Alerts {
            events: events.clone(),
            notices: mpsc::channel(1).0,
            player: Default::default(),
        }));
        (term, events)
    }

    fn completions(events: &crate::events::Events) -> Vec<serde_json::Value> {
        events
            .since(0)
            .events
            .into_iter()
            .filter(|e| e.kind == "command-finished")
            .map(|e| e.data)
            .collect()
    }

    #[test]
    fn history_input_requires_the_current_shell_to_finish_its_command() {
        let (mut term, events) = watched_terminal();
        assert!(!events.idle_shell(1));
        events.command_stream(Some(1));
        assert!(events.idle_shell(1));
        term.advance_bytes(b"\x1b]133;C;aid=outer\x07");
        assert!(!events.idle_shell(1));
        term.advance_bytes(b"\x1b]133;D;0;aid=inner\x07");
        assert!(!events.idle_shell(1));
        term.advance_bytes(b"\x1b]133;D;0;aid=outer\x07");
        assert!(events.idle_shell(1));
        events.command_stream(Some(2));
        assert!(!events.idle_shell(1));
        assert!(events.idle_shell(2));
    }

    #[test]
    fn group_command_prompt_is_one_use_and_bound_to_the_stream() {
        use crate::events::ShellPrompt;
        let (mut term, events) = watched_terminal();
        let ready = || match events.shell_prompt() {
            Some(ShellPrompt::Ready(token)) => token,
            status => panic!("expected a fresh prompt, got {status:?}"),
        };
        events.command_stream(Some(1));
        assert_eq!(events.shell_prompt(), Some(ShellPrompt::Unverified));
        term.advance_bytes(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        assert!(term.at_shell_input_start());
        let first = ready();
        assert!(events.claim_shell_prompt(first));
        assert!(!events.claim_shell_prompt(first));
        term.advance_bytes(b"x");
        assert!(!term.at_shell_input_start());
        term.advance_bytes(b"\r\n\x1b]633;A\x07$ \x1b]633;B\x07");
        let edited = ready();
        events.invalidate_shell_prompt();
        assert!(!events.claim_shell_prompt(edited));
        term.advance_bytes(b"\r\n\x1b]133;B\x07");
        assert_eq!(events.shell_prompt(), Some(ShellPrompt::InUse));
        term.advance_bytes(b"\x1b]133;A\x07");
        assert_eq!(events.shell_prompt(), Some(ShellPrompt::Opening));
        term.advance_bytes(b"$ \x1b]133;B\x07");
        let old_stream = ready();
        events.command_stream(None);
        events.command_stream(Some(2));
        term.advance_bytes(b"\r\n\x1b]133;A\x07$ \x1b]133;B\x07");
        let replacement = ready();
        assert!(!events.claim_shell_prompt(old_stream));
        assert!(events.claim_shell_prompt(replacement));
    }

    #[test]
    fn group_command_encodes_one_paste_and_the_negotiated_enter() {
        let (mut term, mut outgoing) = engine();
        assert!(
            encode_key_input(
                &term,
                KeyCode::LeftShift,
                KeyModifiers::SHIFT,
                true,
                false,
                &KeyDetails::default()
            )
            .unwrap()
            .is_empty()
        );
        assert_eq!(
            group_command_bytes(&term, "echo snow").unwrap(),
            b"echo snow\r"
        );
        term.advance_bytes(b"\x1b[?2004h\x1b[>11u");
        let mut expected = b"\x1b[200~echo snow\x1b[201~".to_vec();
        for down in [true, false] {
            let (writer, mut key) = mpsc::unbounded_channel();
            send_key(&mut term, &writer, KeyCode::Enter, KeyModifiers::NONE, down).unwrap();
            match key.try_recv().or_else(|_| outgoing.try_recv()) {
                Ok(TransportInput::Data(bytes)) => expected.extend(bytes),
                item => panic!("expected Kitty Enter: {item:?}"),
            }
        }
        assert_eq!(group_command_bytes(&term, "echo snow").unwrap(), expected);
    }

    #[test]
    fn group_result_follows_exact_shell_markers_and_keeps_its_exit_status() {
        use crate::events::{GroupExecution, GroupOutput};
        let (mut term, events) = watched_terminal();
        events.command_stream(Some(1));
        term.advance_bytes(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        let (notice, outcome) = tokio::sync::watch::channel(GroupExecution::AwaitingMarkers);
        events.observe_group_command(
            1,
            "echo east".into(),
            notice,
            crate::events::GroupTranscript::default().host(1),
        );
        term.advance_bytes(b"echo east\r\n\x1b]133;C\x07");
        let GroupExecution::Running { target, .. } = *outcome.borrow() else {
            panic!("expected running command")
        };
        assert!(events.group_command_running(target));
        assert!(
            !events.group_command_running(crate::events::GroupInterruptTarget {
                serial: target.serial + 1,
                ..target
            })
        );
        assert!(
            !events.group_command_running(crate::events::GroupInterruptTarget {
                generation: 2,
                ..target
            })
        );
        term.advance_bytes(b"east result\r\n\x1b]133;D;17\x07");
        assert!(!events.group_command_running(target));
        let GroupExecution::Finished(result) = outcome.borrow().clone() else {
            panic!("the matching completion must be captured");
        };
        assert_eq!(result.link.generation, 1);
        assert_eq!(result.exit_code, Some(17));
        assert_eq!(term.last_command().unwrap().id, result.link.id);
        assert!(
            matches!(&result.output, GroupOutput::Captured { text, truncated: false }
            if text == "east result\r\n")
        );

        term.advance_bytes(b"\x1b]633;A\x07$ \x1b]633;B\x07");
        let (notice, late) = tokio::sync::watch::channel(GroupExecution::AwaitingMarkers);
        events.observe_group_command(
            1,
            "echo west".into(),
            notice,
            crate::events::GroupTranscript::default().host(1),
        );
        term.advance_bytes(b"\r\n\x1b]633;C\x07west result\r\n");
        assert!(matches!(*late.borrow(), GroupExecution::AwaitingMarkers));
        term.advance_bytes(b"\x1b]633;E;echo west\x07");
        assert!(matches!(*late.borrow(), GroupExecution::Running { .. }));
        term.advance_bytes(b"\x1b]633;D;0\x07");
        assert!(matches!(&*late.borrow(), GroupExecution::Finished(result)
            if result.exit_code == Some(0) && result.link.id != term.command_history()[0].id));

        term.advance_bytes(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        let (notice, prompt_only) = tokio::sync::watch::channel(GroupExecution::AwaitingMarkers);
        events.observe_group_command(
            1,
            "next".into(),
            notice,
            crate::events::GroupTranscript::default().host(1),
        );
        term.advance_bytes(b"next\r\n\x1b]133;C\x07output\r\n\x1b]133;A\x07");
        assert!(
            matches!(&*prompt_only.borrow(), GroupExecution::Finished(result)
            if result.exit_code.is_none())
        );
    }

    #[test]
    fn live_group_output_waits_for_the_verified_line_and_publishes_each_rendered_line_once() {
        use crate::events::{GroupExecution, GroupTranscript};

        let (mut term, events) = watched_terminal();
        let transcript = GroupTranscript::default();
        let lines = || {
            transcript
                .snapshot()
                .lines
                .into_iter()
                .map(|line| line.text.to_string())
                .collect::<Vec<_>>()
        };
        events.command_stream(Some(1));
        term.advance_bytes(b"\x1b]633;A\x07$ \x1b]633;B\x07");
        let (notice, result) = tokio::sync::watch::channel(GroupExecution::AwaitingMarkers);
        let mut host = transcript.host(42);
        let receipt = host.require_delivery();
        events.observe_group_command(1, "show".into(), notice, host);
        term.advance_bytes(b"\r\n\x1b]633;C\x07\x1b[31mfirst\x1b[0m\r\nunfinished");
        term.notify_running_shell_output();
        assert!(lines().is_empty());
        term.advance_bytes(b"\x1b]633;E;show\x07");
        term.notify_running_shell_output();
        assert!(lines().is_empty());
        receipt.acknowledge();
        term.notify_running_shell_output();
        term.notify_running_shell_output();
        assert_eq!(lines(), ["first"]);
        term.advance_bytes(b"\x1b[?1049halternate\r\n");
        term.notify_running_shell_output();
        assert_eq!(lines(), ["first"]);
        term.advance_bytes(
            b"\x1b[?1049l\x1b]633;D;0\x07\r\n\x1b]633;A\x07new prompt\x1b]633;B\x07",
        );
        term.notify_running_shell_output();
        assert!(matches!(*result.borrow(), GroupExecution::Finished(_)));
        assert_eq!(lines(), ["first", "unfinished"]);
        assert!(
            transcript
                .snapshot()
                .lines
                .iter()
                .all(|line| line.pane == 42)
        );
    }

    #[test]
    fn live_group_output_resumes_after_reflow_and_bounds_wrapped_unicode_lines() {
        use crate::events::{GroupExecution, GroupTranscript};

        let (mut term, events) = watched_terminal();
        events.command_stream(Some(1));
        term.advance_bytes(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        let transcript = GroupTranscript::default();
        let (notice, _result) = tokio::sync::watch::channel(GroupExecution::AwaitingMarkers);
        events.observe_group_command(1, "run".into(), notice, transcript.host(1));
        term.advance_bytes(b"run\r\n\x1b]133;C\x07before resize\r\n");
        term.notify_running_shell_output();
        term.resize(TerminalSize {
            cols: 20,
            rows: 12,
            ..TerminalSize::default()
        });
        term.notify_running_shell_output();

        let text = format!("{}\r\nafter resize\r\n", "雪".repeat(400));
        term.advance_bytes(text.as_bytes());
        term.notify_running_shell_output();
        let snapshot = transcript.snapshot();
        assert_eq!(snapshot.lines.len(), 4);
        assert_eq!(&*snapshot.lines[0].text, "before resize");
        assert!(snapshot.lines[1].text.contains("was skipped"));
        assert!(snapshot.lines[2].shortened);
        assert_eq!(&*snapshot.lines[2].text, "雪".repeat(341));
        assert_eq!(&*snapshot.lines[3].text, "after resize");
    }

    #[test]
    fn group_interrupt_target_cannot_be_reused_after_terminal_reset() {
        use crate::events::GroupExecution;
        let (mut term, events) = watched_terminal();
        events.command_stream(Some(1));
        let mut previous = None;
        for _ in 0..2 {
            term.advance_bytes(b"\x1b]133;A\x07$ \x1b]133;B\x07");
            let (notice, outcome) = tokio::sync::watch::channel(GroupExecution::AwaitingMarkers);
            events.observe_group_command(
                1,
                "run".into(),
                notice,
                crate::events::GroupTranscript::default().host(1),
            );
            term.advance_bytes(b"run\r\n\x1b]133;C\x07");
            let GroupExecution::Running { target, .. } = *outcome.borrow() else {
                panic!("expected running command")
            };
            assert_eq!(target.generation, 1);
            assert!(events.group_command_running(target));
            if let Some(old) = previous {
                assert_ne!(old, target);
                assert!(!events.group_command_running(old));
            }
            previous = Some(target);
            term.advance_bytes(b"\x1bc");
            assert!(!events.group_command_running(target));
            assert!(matches!(
                outcome.borrow().clone(),
                GroupExecution::Unavailable(_)
            ));
        }
    }

    #[test]
    fn group_result_rejects_another_command_overlapping_markers_and_reconnect() {
        use crate::events::GroupExecution;
        let (mut term, events) = watched_terminal();
        events.command_stream(Some(1));
        for tail in [
            b"echo other\r\n\x1b]133;C\x07unrelated\r\n\x1b]133;D;0\x07".as_slice(),
            b"run\r\n\x1b]133;C\x07\x1b]133;C\x07nested\r\n\x1b]133;D;0\x07",
            b"run\r\n\x1b]133;C;aid=parent\x07partial\r\n\x1b]133;D;0;aid=other\x07",
        ] {
            term.advance_bytes(b"\x1b]133;A\x07$ \x1b]133;B\x07");
            let (notice, outcome) = tokio::sync::watch::channel(GroupExecution::AwaitingMarkers);
            events.observe_group_command(
                1,
                "run".into(),
                notice,
                crate::events::GroupTranscript::default().host(1),
            );
            term.advance_bytes(tail);
            assert!(matches!(*outcome.borrow(), GroupExecution::Unavailable(_)));
        }
        term.advance_bytes(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        let (notice, outcome) = tokio::sync::watch::channel(GroupExecution::AwaitingMarkers);
        events.observe_group_command(
            1,
            "run".into(),
            notice,
            crate::events::GroupTranscript::default().host(1),
        );
        term.advance_bytes(b"run\r\n\x1b]133;C\x07");
        let GroupExecution::Running { target, .. } = *outcome.borrow() else {
            panic!("expected running command")
        };
        events.command_stream(None);
        events.command_stream(Some(2));
        assert!(!events.group_command_running(target));
        term.advance_bytes(
            b"\x1b]133;A\x07$ \x1b]133;B\x07run\r\n\x1b]133;C\x07new\r\n\x1b]133;D;0\x07",
        );
        assert!(
            matches!(*outcome.borrow(), GroupExecution::Unavailable(reason) if reason.starts_with("The connection closed"))
        );
    }

    #[test]
    fn group_interrupt_uses_the_panes_ctrl_c_encoding() {
        let (mut term, mut outgoing) = engine();
        assert_eq!(group_interrupt_bytes(&term).unwrap(), b"\x03");
        term.advance_bytes(b"\x1b[>11u");
        let mut expected = Vec::new();
        for down in [true, false] {
            let (writer, mut key) = mpsc::unbounded_channel();
            send_key(
                &mut term,
                &writer,
                KeyCode::Char('c'),
                KeyModifiers::CTRL,
                down,
            )
            .unwrap();
            match key.try_recv().or_else(|_| outgoing.try_recv()) {
                Ok(TransportInput::Data(bytes)) => expected.extend(bytes),
                item => panic!("expected Kitty Ctrl+C: {item:?}"),
            }
        }
        assert_eq!(group_interrupt_bytes(&term).unwrap(), expected);
    }

    #[test]
    fn command_watch_fires_once_for_the_first_paired_primary_command_in_a_batch() {
        let (mut term, events) = watched_terminal();
        events.command_stream(Some(1));
        assert!(events.set_command_watch(true));
        term.advance_bytes(b"\x1b]133;B\x07cancelled\x1b]133;A\x07\x1b]133;D;1\x07\x1b[?1049h\x1b]133;C\x07\x1b]133;D;2\x07\x1b[?1049l");
        assert_eq!(events.command_watch(), Some(true));
        term.advance_bytes(b"\x1b[?2026h\x1b]133;C\x07secret\x1b]133;D;42\x07\x1b]133;D;3\x07\x1b]133;A\x07\x1b]133;C\x07\x1b]133;D;0\x07\x1b[?2026l");
        assert_eq!(completions(&events), [serde_json::json!({"exit_code":42})]);
        assert_eq!(events.command_watch(), Some(false));
        // A late arm after the batch must not fire on the last stored result.
        assert!(events.set_command_watch(true));
        term.advance_bytes(b"\x1b]133;D;0\x07");
        assert_eq!(completions(&events).len(), 1);
    }

    #[test]
    fn command_watch_tracks_current_command_across_capture_loss_and_nested_markers() {
        let (mut term, events) = watched_terminal();
        events.command_stream(Some(1));
        term.advance_bytes(b"\x1b]133;C;aid=shell\x07private output\x1b[3J");
        assert!(events.set_command_watch(true));
        for b in b"\x1b]133;C;aid=shell\x07\x1b]133;C;aid=nested\x07\x1b]133;D;5;aid=nested\x1b\\\x1b]133;A;aid=nested\x07\x1b[?1049h\x1b]133;D;9;aid=shell\x07\x1b[?1049l\x1b]133;N;aid=shell\x1b\\\x1b]133;D;8;aid=shell\x07" {
            term.advance_bytes([*b]);
        }
        assert_eq!(
            completions(&events),
            [serde_json::json!({"exit_code":null})]
        );
        assert!(
            events
                .since(0)
                .events
                .iter()
                .all(|entry| entry.command.is_none())
        );
        for end in [b"\x1b]133;A\x07".as_slice(), b"\x1b]133;D\x07"] {
            assert!(events.set_command_watch(true));
            term.advance_bytes(b"\x1b]133;C\x07");
            term.advance_bytes(end);
        }
        assert_eq!(
            completions(&events),
            vec![serde_json::json!({"exit_code":null}); 3]
        );
        let links: Vec<_> = events
            .since(0)
            .events
            .into_iter()
            .filter_map(|entry| entry.command)
            .collect();
        let history = term.command_history();
        assert_eq!(
            links,
            [
                crate::events::CommandLink {
                    generation: 1,
                    id: history[history.len() - 2].id
                },
                crate::events::CommandLink {
                    generation: 1,
                    id: history.back().unwrap().id
                },
            ]
        );
    }

    #[test]
    fn command_watch_cancels_on_reset_disconnect_and_viewer_transition() {
        let (mut term, events) = watched_terminal();
        assert_eq!(events.shell_generation(), None);
        assert!(!events.set_command_watch(true));
        term.advance_bytes(b"\x1b]133;C\x07historical output");
        events.command_stream(Some(1));
        assert!(events.set_command_watch(true));
        term.advance_bytes(b"\x1b]133;D;11\x07");
        assert_eq!(events.command_watch(), Some(true));
        for reset in [b"\x1bc".as_slice(), b"\x1b[!p", b"\x1b]133;B\x07"] {
            events.set_command_watch(true);
            term.advance_bytes(b"\x1b]133;C\x07");
            term.advance_bytes(reset);
            assert_eq!(events.command_watch(), Some(false));
            assert_eq!(events.shell_generation(), Some(1));
            term.advance_bytes(b"\x1b]133;D;42\x07");
        }
        events.set_command_watch(true);
        term.advance_bytes(b"\x1b]133;C\x07");
        events.command_stream(None);
        assert_eq!(events.shell_generation(), None);
        assert_eq!(events.command_watch(), None);
        term.reset_connection();
        events.command_stream(Some(2));
        assert_eq!(events.shell_generation(), Some(2));
        term.advance_bytes(b"\x1b]133;D;2\x07\x1b]133;C\x07");
        assert!(events.set_command_watch(true));
        assert!(events.set_command_watch(false));
        term.advance_bytes(b"\x1b]133;D;4\x07");
        assert!(events.set_command_watch(true));
        enter_herdr_viewer(&mut term, &mpsc::unbounded_channel().0, &events);
        assert_eq!(events.shell_generation(), None);
        assert!(!events.set_command_watch(true));
        term.advance_bytes(b"\x1b]133;C\x07old bytes\x1b]133;D;99\x07");
        assert!(completions(&events).is_empty());
    }

    #[test]
    fn osc9_progress_retains_values_across_warnings_and_clears_on_reset() {
        use wezterm_term::Progress::*;
        let (mut term, _) = engine();
        for (sequence, expected) in [
            ("9;4;2", Error(10)),
            ("9;4;1;0", Percentage(0)),
            ("9;4;1;27", Percentage(27)),
            ("9;4;3", Indeterminate),
            ("9;4;4", Paused(27)),
            ("9;4;2;0", Error(27)),
            ("9;4;4;63", Paused(63)),
            ("9;4;1;bogus", Paused(63)),
            ("9;4;1;900000000000000000000", Percentage(100)),
            ("9;4;0", None),
            ("9;4;4", Paused(10)),
        ] {
            for byte in format!("\x1b]{sequence}\x1b\\").bytes() {
                term.advance_bytes([byte]);
            }
            assert_eq!(term.get_progress(), expected, "{sequence}");
        }
        term.reset_connection();
        assert_eq!(term.get_progress(), None);
        term.advance_bytes(b"\x1b]9;4;2\x07");
        assert_eq!(term.get_progress(), Error(10));
        term.advance_bytes(b"\x1bc");
        assert_eq!(term.get_progress(), None);
    }

    #[test]
    fn completed_shell_output_uses_markers_scrollback_and_real_line_breaks() {
        let (mut term, _) = engine();
        term.resize(TerminalSize {
            cols: 6,
            rows: 2,
            ..TerminalSize::default()
        });
        for byte in b"\x1b]133;A\x07> \x1b]133;B\x07ls\r\n\x1b]133;C\x07\x1b[31m123456789\x1b[0m\r\n\r\nend\x1b]133;D;42\x1b\\" {
            term.advance_bytes([*byte]);
        }
        let first = term.last_command().unwrap().clone();
        assert_eq!(first.command.as_deref(), Some("ls"));
        assert_eq!(first.exit_code, Some(42));
        assert_eq!(first.output.as_deref(), Ok("123456789\r\n\r\nend"));
        term.advance_bytes(b"\x1b]133;A\x07> \x1b]133;C\x07\x1b]133;D;0\x07");
        assert_eq!(term.last_command().unwrap().output.as_deref(), Ok(""));
        assert_eq!(term.last_command().unwrap().exit_code, Some(0));
        assert!(term.last_command().unwrap().command.is_none());
        assert!(Arc::ptr_eq(term.command_history().front().unwrap(), &first));
        assert_eq!(first.output.as_deref(), Ok("123456789\r\n\r\nend"));
    }

    #[test]
    fn reported_command_text_preserves_the_original_utf8_and_line_breaks() {
        let marker = termwiz::escape::osc::OperatingSystemCommand::parse(&[
            b"133",
            b"C",
            b"aid=shell",
            b"cmdline_url=%20printf%20%E9%9B%AA+%3B%0Atrue%09%25%20",
        ]);
        assert_eq!(
            marker.to_string(),
            "\x1b]133;C;aid=shell;cmdline_url=%20printf%20%E9%9B%AA%2B%3B%0Atrue%09%25%20\x1b\\"
        );
        let (mut term, _) = engine();
        let stream = b"\x1b]133;A\x07$ \x1b]133;B\x07displayed continuation prompt\r\n\
            \x1b]133;C;cmdline_url=%20printf%20%E9%9B%AA+%3B%0Atrue%09%25%20;aid=shell\x1b\\\
            result\x1b]133;D;23;aid=shell\x07";
        for byte in stream {
            term.advance_bytes([*byte]);
        }
        let entry = term.last_command().unwrap();
        assert_eq!(entry.command.as_deref(), Some(" printf 雪+;\ntrue\t% "));
        assert_eq!(entry.output.as_deref(), Ok("result"));
        assert_eq!(entry.exit_code, Some(23));

        let maximum = "%21".repeat(4096);
        term.advance_bytes(format!(
            "\x1b]133;A\x07$ \x1b]133;C;cmdline_url={maximum}\x07\x1b]133;D;0\x07"
        ));
        assert_eq!(
            term.last_command().unwrap().command.as_deref(),
            Some("!".repeat(4096).as_str())
        );
    }

    #[test]
    fn invalid_reported_commands_fall_back_to_visible_input_and_keep_the_status() {
        let (mut term, events) = watched_terminal();
        events.command_stream(Some(1));
        let too_long = "%41".repeat(4097);
        for (index, report) in [
            "",
            "%",
            "%4Z",
            "%FF",
            "%1B",
            "%C2%85",
            "%0D",
            "%20%0A",
            "split\x01text",
            &too_long,
        ]
        .into_iter()
        .enumerate()
        {
            assert!(events.set_command_watch(true));
            term.advance_bytes(format!(
                "\x1b]133;A\x07$ \x1b]133;B\x07visible input\r\n\
                 \x1b]133;C;cmdline_url={report};aid=shell\x1b\\\
                 result\x1b]133;D;17;aid=shell\x07"
            ));
            let entry = term.last_command().unwrap();
            assert_eq!(entry.command.as_deref(), Some("visible input"));
            assert_eq!(entry.output.as_deref(), Ok("result"));
            assert_eq!(entry.exit_code, Some(17));
            let recorded = completions(&events);
            assert_eq!(recorded.len(), index + 1);
            assert_eq!(recorded[index], serde_json::json!({"exit_code": 17}));
        }
    }

    #[test]
    fn latest_command_preview_uses_the_last_completed_output_and_its_link() {
        let (mut term, _) = engine();
        term.advance_bytes(
            b"\x1b]133;C\x07older\r\n\x1b]133;D;0\x07\x1b]133;C\x07first\r\nlast\r\n\x1b]133;D;17\x07",
        );
        let mut frame = Snapshot::capture(&term, None, 0, 0, String::new(), 0);
        frame.connection_generation = 3;
        let (link, preview) = frame.latest_command_output_preview().unwrap();
        assert_eq!(link.generation, 3);
        assert_eq!(link.id, term.last_command().unwrap().id);
        assert_eq!(frame.command_for_link(link).unwrap().exit_code, Some(17));
        assert_eq!(preview, "Exit 17 · last");
    }

    #[test]
    fn visible_text_preview_skips_blank_rows_and_masks_concealed_cells() {
        let (mut term, _) = engine();
        let read = |term: &Terminal| {
            Snapshot::capture(term, None, 0, 0, String::new(), 0).last_visible_text_line()
        };
        assert_eq!(read(&term), None);
        term.advance_bytes("older\r\n  latest \x1b[8msecret\x1b[28m雪\r\n\x1b[8mhidden\x1b[28m");
        assert_eq!(read(&term).as_deref(), Some("latest 雪"));
    }

    #[test]
    fn command_output_tail_is_plain_text_and_bounds_its_unicode_scan() {
        assert_eq!(
            command_output_tail("first\r\n\tlast  \u{202e}雪\r\n\r\n"),
            "last 雪"
        );
        assert_eq!(command_output_tail(" \r\n\t"), "No output");
        assert_eq!(
            command_output_tail(&format!("earlier\n{}", " ".repeat(4096))),
            "Open history for earlier output"
        );
        let long = "雪".repeat(2048);
        assert_eq!(command_output_tail(&long), "雪".repeat(200));
    }

    #[test]
    fn command_history_is_bounded_and_frozen_across_eviction_and_reset() {
        let (mut term, _) = engine();
        term.advance_bytes(b"\x1b]133;B\x07cancelled\x1b]133;A\x07");
        assert!(term.command_history().is_empty());
        term.advance_bytes(b"\x1b]133;B\x07echo first\r\n\x1b]133;C\x07first\x1b]133;D;0\x07");
        let frozen = term.command_history().clone();
        assert!(term.command_output_row(frozen[0].id).is_some());
        for _ in 0..64 {
            term.advance_bytes(b"\x1b]133;C\x07ok\x1b]133;D;1\x07");
        }
        assert_eq!(term.command_history().len(), 64);
        assert_eq!(term.command_history().front().unwrap().id, 2);
        assert_eq!(term.command_output_row(frozen[0].id), None);
        assert_eq!(frozen[0].command.as_deref(), Some("echo first"));
        assert_eq!(frozen[0].output.as_deref(), Ok("first"));
        term.reset_connection();
        assert!(term.command_history().is_empty());
        term.advance_bytes(b"\x1b]133;C\x07new\x1b]133;D\x07");
        assert_eq!(term.command_history()[0].id, 66);
    }

    #[test]
    fn command_output_anchor_follows_reflow_and_expires_when_scrollback_is_cleared() {
        let (mut term, _) = engine();
        let size = |cols| TerminalSize {
            cols,
            rows: 4,
            ..TerminalSize::default()
        };
        term.resize(size(24));
        term.advance_bytes(
            b"prelude\r\nprefix-12345\x1b]133;C\x07target output\r\n\x1b]133;D;0\x07",
        );
        let id = term.last_command().unwrap().id;
        for _ in 0..16 {
            term.advance_bytes(b"unrelated output\r\n");
        }
        for cols in [24, 9, 31] {
            term.resize(size(cols));
            let row = term.command_output_row(id).unwrap();
            let screen = term.screen();
            assert!(row < screen.visible_row_to_stable_row(0));
            let phys = screen.stable_row_to_phys(row).unwrap();
            assert!(
                screen.lines_in_phys_range(phys..phys + 1)[0]
                    .as_str()
                    .contains("target")
            );
        }
        term.advance_bytes(b"\x1b[?1049h");
        assert_eq!(term.command_output_row(id), None);
        term.advance_bytes(b"\x1b[?1049l");
        assert!(term.command_output_row(id).is_some());
        term.advance_bytes(b"\x1b[3J");
        assert_eq!(term.command_output_row(id), None);
        assert_eq!(
            term.last_command().unwrap().output.as_deref(),
            Ok("target output\r\n")
        );
    }

    #[test]
    fn command_output_anchor_is_removed_when_its_visible_row_is_erased() {
        let (mut term, _) = engine();
        term.resize(TerminalSize {
            cols: 6,
            rows: 4,
            ..TerminalSize::default()
        });
        term.advance_bytes(b"pref: \x1b]133;C\x07result\x1b]133;D;0\x07");
        let id = term.last_command().unwrap().id;
        assert_eq!(
            term.command_output_row(id),
            Some(term.screen().visible_row_to_stable_row(1))
        );
        term.advance_bytes(b"\x1b[2K");
        assert_eq!(term.command_output_row(id), None);
        assert_eq!(term.last_command().unwrap().output.as_deref(), Ok("result"));
    }

    #[test]
    fn command_history_evicts_old_outputs_at_the_byte_limit() {
        let (mut term, _) = engine();
        term.resize(TerminalSize {
            cols: 1024,
            rows: 513,
            ..TerminalSize::default()
        });
        let row = Line::from_text(&"x".repeat(1024), &CellAttributes::default(), 1, None);
        for _ in 0..8 {
            term.advance_bytes(b"\x1b[1;1H\x1b]133;C\x07");
            let screen = term.screen_mut();
            for index in screen.phys_row(0)..screen.phys_row(512) {
                *screen.line_mut(index) = row.clone();
            }
            term.advance_bytes(b"\x1b[513;1H\x1b]133;D;0\x07");
        }
        assert_eq!(term.command_history().len(), 7);
        assert_eq!(term.command_history().front().unwrap().id, 2);
        assert!(term.last_command().unwrap().output.as_ref().unwrap().len() > 512 * 1024);
    }

    #[test]
    fn conemu_directory_reports_round_trip_windows_paths_and_stay_local() {
        let (mut local, _) = engine();
        let (mut ssh, _) = engine();
        local.enable_conemu_directory_reports();
        ssh.advance_bytes(b"\x1b]7;file://server/home/pilot\x07");
        for (path, terminator) in [
            (r#""C:\Work\100% #;雪""#, "\x07"),
            (r"D:\literal%2F\bracket[1]", "\x1b\\"),
            (r"X:\", "\x07"),
        ] {
            let sequence = format!("\x1b]9;9;{path}{terminator}");
            for chunk in sequence.as_bytes().chunks(3) {
                local.advance_bytes(chunk);
                ssh.advance_bytes(chunk);
            }
            assert_eq!(
                local.get_current_dir().unwrap().to_file_path().unwrap(),
                std::path::Path::new(path.trim_matches('"'))
            );
            assert_eq!(
                ssh.reported_current_dir().unwrap().as_ref(),
                "file://server/home/pilot"
            );
        }
        let many_semicolons = format!(r"C:\{}suffix", ";".repeat(100));
        local.advance_bytes(format!("\x1b]9;9;{many_semicolons}\x07"));
        assert_eq!(
            local.get_current_dir().unwrap().to_file_path().unwrap(),
            std::path::Path::new(&many_semicolons)
        );
        local.advance_bytes(format!("\x1b]9;9;C:\\{}\x07", ";".repeat(140 * 1024)));
        assert!(local.reported_current_dir().is_none());
        for bad in [
            "Env:",
            r"\\server\share",
            r"\\?\C:\a",
            r"C:relative",
            r"C:\one\..\two",
            "",
        ] {
            local.advance_bytes(b"\x1b]7;file:///C:/previous\x07");
            local.advance_bytes(format!("\x1b]9;9;{bad}\x07"));
            assert!(local.reported_current_dir().is_none(), "{bad:?}");
        }
        local.advance_bytes(b"\x1b]9;9;C:\\first\x07\x1b]7;file:///C:/second\x07");
        assert_eq!(
            local.reported_current_dir().unwrap().as_ref(),
            "file:///C:/second"
        );
        local.reset_connection();
        assert!(local.reported_current_dir().is_none());
    }

    #[test]
    fn wsl_conemu_reports_accept_only_wsl_shares_beside_dos_paths() {
        let (mut wsl, _) = engine();
        let (mut local, _) = engine();
        wsl.enable_wsl_directory_reports();
        local.enable_conemu_directory_reports();
        for path in [
            r#""\\wsl.localhost\QA Linux\home\100% # 雪""#,
            r"\\WSL$\QA Linux\home\100% # 雪",
        ] {
            let report = format!("\x1b]9;9;{path}\x1b\\");
            wsl.advance_bytes(report.as_bytes());
            local.advance_bytes(report.as_bytes());
            assert_eq!(
                wsl.reported_current_dir().unwrap().as_ref(),
                "file://wsl.localhost/QA%20Linux/home/100%25%20%23%20%E9%9B%AA"
            );
            assert!(local.reported_current_dir().is_none());
        }
        for path in [
            r"\\server\QA Linux\a",
            r"\\wsl$\QA Linux\..\other",
            r"\\wsl.localhost\",
        ] {
            wsl.advance_bytes(format!("\x1b]9;9;{path}\x07"));
            assert!(wsl.reported_current_dir().is_none(), "{path}");
        }
        wsl.advance_bytes(b"\x1b]9;9;C:\\work\x07");
        assert_eq!(
            wsl.reported_current_dir().unwrap().as_ref(),
            "file:///C:/work"
        );
    }

    #[test]
    fn ssh_iterm_directory_snapshot_is_bound_to_the_current_login() {
        let (mut ssh, _) = engine();
        let (mut local, _) = engine();
        ssh.enable_iterm_directory_reports("alice", "host.example");
        local.enable_conemu_directory_reports();
        local.advance_bytes(b"\x1b]7;file:///C:/work\x07");
        let report =
            b"\x1b]1337;RemoteHost=alice@host.example\x07\x1b]1337;CurrentDir=/srv/demo\x1b\\";
        for bytes in report.chunks(3) {
            ssh.advance_bytes(bytes);
            local.advance_bytes(bytes);
        }
        let frame = Snapshot::capture(&ssh, None, 1, 0, String::new(), 0);
        assert_eq!(
            frame.reported_directory.as_deref(),
            Some("file://host.example/srv/demo")
        );
        assert_eq!(
            local.reported_current_dir().map(AsRef::as_ref),
            Some("file:///C:/work")
        );

        ssh.reset_connection();
        ssh.advance_bytes(b"\x1b]1337;CurrentDir=/from-old-login\x07");
        assert!(ssh.reported_current_dir().is_none());
        ssh.advance_bytes(report);
        assert_eq!(
            ssh.reported_current_dir().map(AsRef::as_ref),
            frame.reported_directory.as_deref()
        );
        ssh.advance_bytes(b"\x1b]1337;RemoteHost=other@host.example\x07");
        assert!(ssh.reported_current_dir().is_none());
        // Captures used by a visible picker or hint overlay keep their original directory.
        assert_eq!(
            frame.reported_directory.as_deref(),
            Some("file://host.example/srv/demo")
        );
    }

    #[test]
    fn incomplete_and_oversized_iterm_reports_do_not_publish_partial_directories() {
        let (mut ssh, _) = engine();
        ssh.enable_iterm_directory_reports("alice", "host.example");
        ssh.advance_bytes(b"\x1b]1337;RemoteHost=alice@host.example\x07");
        let prompt = b"\x1b]1337;CurrentDir=/shell\x07";
        for bad in [
            b"\x1b]1337;CurrentDir=/interrupted\x1b[0m".to_vec(),
            format!("\x1b]1337;CurrentDir=/{}\x07", "a".repeat(140 * 1024)).into_bytes(),
        ] {
            ssh.advance_bytes(prompt);
            ssh.advance_bytes(&bad);
            assert!(ssh.reported_current_dir().is_none());
            ssh.advance_bytes(prompt);
            assert_eq!(
                ssh.reported_current_dir().map(AsRef::as_ref),
                Some("file://host.example/shell")
            );
        }
    }

    #[test]
    fn shell_metadata_follows_primary_pane_and_connection() {
        let (mut term, _) = engine();
        term.advance_bytes(b"\x1b]7;file://localhost/old%20dir\x07\x1b]133;C\x07left");
        term.advance_bytes(
            b"\x1b[?1049h\x1b]133;C\x07private alternate\x1b]133;D;99\x07\x1b[?1049l",
        );
        term.advance_bytes(b" right\x1b]133;D\x07");
        let command = term.last_command().unwrap();
        assert_eq!(command.output.as_deref(), Ok("left right"));
        assert_eq!(command.exit_code, None);
        assert_eq!(
            term.reported_current_dir().map(AsRef::as_ref),
            Some("file://localhost/old%20dir")
        );
        let (other, _) = engine();
        assert!(other.last_command().is_none() && other.reported_current_dir().is_none());
        term.reset_connection();
        assert!(term.last_command().is_none() && term.reported_current_dir().is_none());
        term.advance_bytes(b"\x1b]133;C\x07fresh\x1b]133;A\x07");
        assert_eq!(term.last_command().unwrap().output.as_deref(), Ok("fresh"));
        term.advance_bytes(b"\x1b]133;C\x07bad code\x1b]133;D;unknown\x07");
        assert_eq!(term.last_command().unwrap().exit_code, None);
    }

    #[test]
    fn oversized_command_retains_status_and_never_copies_a_truncated_prefix() {
        let (mut term, _) = engine();
        term.resize(TerminalSize {
            cols: 1024,
            rows: 513,
            ..TerminalSize::default()
        });
        term.advance_bytes(b"\x1b]133;C\x07");
        let row = Line::from_text(&"é".repeat(1024), &CellAttributes::default(), 1, None);
        let screen = term.screen_mut();
        for index in screen.phys_row(0)..screen.phys_row(512) {
            *screen.line_mut(index) = row.clone();
        }
        term.advance_bytes(b"\x1b[513;1H\x1b]133;D;0\x07");
        let command = term.last_command().unwrap();
        assert_eq!(command.exit_code, Some(0));
        assert_eq!(
            command.output,
            Err("Command output exceeds the 1 MiB clipboard limit")
        );
    }

    #[test]
    fn partial_command_output_is_unavailable_after_history_is_cleared() {
        let (mut term, events) = watched_terminal();
        events.command_stream(Some(1));
        term.resize(TerminalSize {
            cols: 16,
            rows: 2,
            ..TerminalSize::default()
        });
        term.advance_bytes(b"\x1b]133;C\x07begin\r\nmiddle\r\nend\x1b[3J");
        assert!(events.set_command_watch(true));
        term.advance_bytes(b"\x1b]133;D;7\x07");
        let command = term.last_command().unwrap();
        assert_eq!(command.exit_code, Some(7));
        assert_eq!(command.output, Err("Command output left scrollback"));
        assert_eq!(completions(&events), [serde_json::json!({"exit_code":7})]);
    }

    #[test]
    fn resizing_a_running_command_maps_the_start_after_a_wide_cell() {
        let (mut term, _) = engine();
        term.resize(TerminalSize {
            cols: 7,
            rows: 5,
            ..TerminalSize::default()
        });
        term.advance_bytes("hello界\x1b]133;C\x07abc界defg".as_bytes());
        term.resize(TerminalSize {
            cols: 5,
            rows: 5,
            ..TerminalSize::default()
        });
        term.advance_bytes(b"\x1b]133;D;0\x07");
        assert_eq!(
            term.last_command().unwrap().output.as_deref(),
            Ok("abc界defg")
        );
    }

    #[test]
    fn xterm_report_sgr_returns_common_text_attributes_and_colors() {
        let (mut term, mut out) = engine();
        term.advance_bytes(b"\x1b[1;3;4:3;31;48;5;220mA\x1b[32mB\r\n\x1b[24;31mCD");
        term.advance_bytes(b"\x1b[3;1H\x1b[0;38;2;11;22;33;58;5;32mEFG\x1b[0m");
        let before = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
        for (query, expected) in [
            ("\x1b[1;1;2;2#|", "\x1b[0;1;3;48:5:220m"),
            ("\x1b[1;1;1;1#|", "\x1b[0;1;3;4:3;31;48:5:220m"),
            ("\x1b[3;2;3;3#|", "\x1b[0;38:2::11:22:33;58:5:32m"),
            ("\x1b[1;1;3;3#|", "\x1b[0m"),
        ] {
            for byte in query.bytes() {
                term.advance_bytes([byte]);
            }
            assert_eq!(receive(&mut out), expected.as_bytes(), "{query:?}");
        }
        term.advance_bytes(b"\x1bP$qm\x1b\\\x1b[6n");
        assert_eq!(receive(&mut out), b"\x1bP1$r0m\x1b\\");
        assert_eq!(receive(&mut out), b"\x1b[3;4R");
        let after = Snapshot::capture(&term, Some(&before), 2, 0, String::new(), 0);
        assert!(
            before
                .rows
                .iter()
                .zip(after.rows.iter())
                .all(|(a, b)| Arc::ptr_eq(a, b))
        );
    }

    #[test]
    fn xterm_report_sgr_uses_origin_margins_for_defaults_and_clamping() {
        let (tx, mut out) = mpsc::unbounded_channel();
        let size = TerminalSize {
            cols: 7,
            rows: 4,
            ..Default::default()
        };
        let mut term = create(size, tx);
        term.advance_bytes(b"\x1b[1;31m\x1b[2;3Habc\x1b[3;3Hdef\x1b[0m");
        term.advance_bytes(b"\x1b[2;3r\x1b[?69h\x1b[3;5s\x1b[?6h");
        for (query, expected) in [
            ("\x1b[#|", "\x1b[0;1;31m"),
            ("\x1b[0;0;0;0#|", "\x1b[0;1;31m"),
            ("\x1b[1;1;4294967295;999#|", "\x1b[0;1;31m"),
            ("\x1b[999;999;999;999#|", "\x1b[0;1;31m"),
            ("\x1b[2;3;1;1#|", "\x1b[0m"),
        ] {
            term.advance_bytes(query);
            assert_eq!(receive(&mut out), expected.as_bytes(), "{query:?}");
        }
        term.advance_bytes(b"\x1b[1:2;1;1;1#|\x1b[?6l\x1b[#|");
        assert_eq!(receive(&mut out), b"\x1b[0m");
        assert!(out.try_recv().is_err());
    }

    #[test]
    fn xterm_report_sgr_covers_wide_cells_and_the_active_screen() {
        let (tx, mut out) = mpsc::unbounded_channel();
        let size = TerminalSize {
            cols: 8,
            rows: 3,
            ..Default::default()
        };
        let mut term = create(size, tx);
        term.advance_bytes("\x1b#6\x1b[34m界X \x1b[0m");
        for (query, expected) in [
            ("\x1b[1;2;1;2#|", "\x1b[0;34m"),
            ("\x1b[1;1;1;4#|", "\x1b[0;34m"),
            ("\x1b[1;1;1;5#|", "\x1b[0m"),
            ("\x1b[2;1;2;1#|", "\x1b[0m"),
        ] {
            term.advance_bytes(query);
            assert_eq!(receive(&mut out), expected.as_bytes(), "{query:?}");
        }
        term.advance_bytes(b"\x1b[?1049h\x1b[1;2;1;2#|\x1b[?1049l\x1b[1;2;1;2#|");
        assert_eq!(receive(&mut out), b"\x1b[0m");
        assert_eq!(receive(&mut out), b"\x1b[0;34m");
    }

    #[test]
    fn xterm_sgr_stack_restores_text_style_while_leaving_the_current_link() {
        use wezterm_term::{Intensity, Underline};

        let (mut term, _) = engine();
        term.advance_bytes(b"\x1b]8;;https://example.test/old\x1b\\");
        term.advance_bytes(b"\x1b[1;3;4:3;5;7;8;9;53;73m\x1b[38;2;11;22;33;48;5;200;58;5;220mA");
        for byte in b"\x1b[#{\x1b[0m\x1b]8;;https://example.test/new\x1b\\\x1b[#}B" {
            term.advance_bytes([*byte]);
        }
        let frame = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
        let mut cells = frame.rows[0].visible_cells();
        let a = cells.next().unwrap();
        let b = cells.next().unwrap();
        assert_eq!((a.str(), b.str()), ("A", "B"));
        let (original, restored) = (a.attrs(), b.attrs());
        assert_eq!(
            restored.hyperlink().unwrap().uri(),
            "https://example.test/new"
        );
        assert_eq!(restored.intensity(), Intensity::Bold);
        assert_eq!(restored.underline(), Underline::Curly);
        assert!(restored.italic() && restored.reverse() && restored.invisible());
        assert!(restored.strikethrough() && restored.overline());
        assert_eq!(restored.blink(), original.blink());
        assert_eq!(restored.vertical_align(), original.vertical_align());
        assert_eq!(restored.foreground(), original.foreground());
        assert_eq!(restored.background(), original.background());
        assert_eq!(restored.underline_color(), original.underline_color());
    }

    #[test]
    fn xterm_sgr_stack_restores_only_the_selected_attributes() {
        use wezterm_term::{Intensity, Underline, color::ColorAttribute};

        let (mut term, _) = engine();
        term.advance_bytes(b"\x1b[1;31m\x1b[1;10#p\x1b[22;32;44;21m\x1b[#qA");
        term.advance_bytes(b"\x1b[2;4#{\x1b[2;4m\x1b[#}B");
        let frame = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
        let mut cells = frame.rows[0].visible_cells();
        let first = cells.next().unwrap();
        let second = cells.next().unwrap();
        assert_eq!(first.attrs().foreground(), ColorAttribute::PaletteIndex(1));
        assert_eq!(first.attrs().background(), ColorAttribute::PaletteIndex(4));
        assert_eq!(first.attrs().intensity(), Intensity::Bold);
        assert_eq!(first.attrs().underline(), Underline::Double);
        assert_eq!(second.attrs().foreground(), ColorAttribute::PaletteIndex(1));
        assert_eq!(second.attrs().background(), ColorAttribute::PaletteIndex(4));
        assert_eq!(second.attrs().intensity(), Intensity::Normal);
        assert_eq!(second.attrs().underline(), Underline::None);
    }

    #[test]
    fn kitty_fractional_text_keeps_ratios_local_and_preserves_grid_advance() {
        use wezterm_term::FractionalTextScale;
        let (mut terminal, mut wire) = engine();
        terminal.advance_bytes(concat!(
            "\x1b]66;n=1:d=2:h=1:v=2:w=1;ab\x07",
            "\x1b]66;v=1:d=3:n=2;界x\x1b\\",
            "\x1b]66;w=5:n=2:d=2;invalid\x07",
            "\x1b]66;w=5:n=1:d=16;invalid\x07",
            "\x1b]66;w=5:n=1:d=2:h=3;invalid\x07",
            "\x1b]66;s=8:n=1:d=2;invalid\x07",
            "Z\x1b[6n"
        ));
        assert_eq!(receive(&mut wire), b"\x1b[1;6R");
        let check_frame = |frame: &Snapshot, tail_row: usize, tail_col: usize| {
            let mut cells = frame.rows[0].visible_cells();
            let cell = cells.next().unwrap();
            assert_eq!((cell.str(), cell.width()), ("ab", 1));
            assert_eq!(
                cell.attrs().fractional_text_scale(),
                FractionalTextScale::new(1, 2, 1, 2)
            );
            for (expected, width) in [("界", 2), ("x", 1)] {
                let cell = cells.next().unwrap();
                assert_eq!((cell.str(), cell.width()), (expected, width));
                assert_eq!(
                    cell.attrs().fractional_text_scale(),
                    FractionalTextScale::new(2, 3, 0, 1)
                );
                assert!(!cell.attrs().explicit_width());
            }
            let tail = frame.rows[tail_row].get_cell(tail_col).unwrap();
            assert_eq!(tail.str(), "Z");
            assert_eq!(tail.attrs().fractional_text_scale(), None);
        };
        check_frame(
            &Snapshot::capture(&terminal, None, 1, 0, String::new(), 0),
            0,
            4,
        );
        let rows = terminal.screen().physical_rows;
        terminal.resize(TerminalSize {
            cols: 4,
            rows,
            ..TerminalSize::default()
        });
        check_frame(
            &Snapshot::capture(&terminal, None, 2, 0, String::new(), 0),
            1,
            0,
        );

        terminal.resize(TerminalSize {
            cols: 6,
            rows,
            ..TerminalSize::default()
        });
        terminal.advance_bytes("\x1b[2J\x1b[H12345\x1b]66;n=1:d=2;界\x07Q\x1b[6n");
        assert_eq!(receive(&mut wire), b"\x1b[2;4R");
        let frame = Snapshot::capture(&terminal, None, 3, 0, String::new(), 0);
        let mut cells = frame.rows[1].visible_cells();
        let wide = cells.next().unwrap();
        assert_eq!((wide.cell_index(), wide.width(), wide.str()), (0, 2, "界"));
        assert!(wide.attrs().fractional_text_scale().is_some());
        assert_eq!(cells.next().unwrap().cell_index(), 2);
    }

    #[test]
    fn kitty_text_width_places_literal_text_and_answers_the_width_probe() {
        let (mut terminal, mut wire) = engine();
        for bytes in [
            b"\r\x1b[6n".as_slice(),
            b"\x1b]66;w=2; \x07\x1b[6n",
            b"\x1b]66;s=8; \x07\x1b[6n",
        ] {
            terminal.advance_bytes(bytes);
        }
        assert_eq!(receive(&mut wire), b"\x1b[1;1R");
        assert_eq!(receive(&mut wire), b"\x1b[1;3R");
        assert_eq!(receive(&mut wire), b"\x1b[1;3R");
        for byte in "\x1b]66;w=7;字;a\x1b\\X".as_bytes() {
            terminal.advance_bytes([*byte]);
        }
        let frame = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        let cells: Vec<_> = frame.rows[0].visible_cells().take(3).collect();
        assert_eq!((cells[0].cell_index(), cells[0].width()), (0, 2));
        assert_eq!((cells[1].cell_index(), cells[1].width()), (2, 7));
        assert_eq!(cells[1].str(), "字;a");
        assert_eq!((cells[2].cell_index(), cells[2].str()), (9, "X"));
        terminal.advance_bytes(b"\x1b[1;7H!");
        let frame = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        assert!(frame.rows[0].as_str().starts_with("     !  X"));
    }

    #[test]
    fn kitty_text_width_survives_scrollback_and_reflow() {
        let (mut terminal, _) = engine();
        let size = TerminalSize {
            cols: 8,
            rows: 3,
            ..TerminalSize::default()
        };
        terminal.resize(size);
        terminal.advance_bytes(b"1234567\x1b]66;w=5;hello\x07X\r\nmore\r\nscroll\r\n");
        let expect_width = |terminal: &Terminal| {
            let screen = terminal.screen();
            let line = screen
                .physical_lines()
                .find(|line| line.as_str().contains("hello"))
                .expect("retained sized text");
            let mut visible = line.visible_cells();
            let block = visible.find(|cell| cell.str() == "hello").unwrap();
            assert_eq!(block.width(), 5);
            let next = visible.next().unwrap();
            assert_eq!(next.cell_index(), block.cell_index() + 5);
            assert_eq!(next.str(), "X");
        };
        assert!(terminal.screen().phys_row(0) >= 2);
        expect_width(&terminal);
        terminal.resize(TerminalSize { cols: 14, ..size });
        expect_width(&terminal);
        terminal.resize(TerminalSize { cols: 4, ..size });
        assert!(terminal.screen().physical_lines().all(|line| {
            !line.as_str().contains("hello")
                && line
                    .visible_cells()
                    .all(|cell| cell.cell_index() + cell.width() <= 4)
        }));
    }

    #[test]
    fn kitty_text_width_preserves_combining_marks_and_edits_whole_cells() {
        let (mut terminal, mut wire) = engine();
        terminal.advance_bytes("\x1b]66;w=1;♥\x07\u{fe0f}\u{0301}\x1b[6n".as_bytes());
        assert_eq!(receive(&mut wire), b"\x1b[1;2R");
        let frame = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        let first = frame.rows[0].visible_cells().next().unwrap();
        assert_eq!(first.str(), "♥\u{fe0f}\u{0301}");
        assert_eq!(first.width(), 1);
        for edit in ["@", "P", "X", "K"] {
            terminal.advance_bytes(b"\x1b[2J\x1b[H[A\x1b]66;w=6;sized\x07Z");
            terminal.advance_bytes(format!("\x1b[1;6H\x1b[{edit}").as_bytes());
            let frame = Snapshot::capture(&terminal, None, 2, 0, String::new(), 0);
            assert!(!frame.rows[0].as_str().contains("sized"), "{edit}");
        }
        terminal.advance_bytes(b"\x1b[2J\x1b[H\x1b[?7l\x1b[80G\x1b]66;w=5;end\x07\x1b[6n");
        assert_eq!(receive(&mut wire), b"\x1b[1;80R");
        let frame = Snapshot::capture(&terminal, None, 3, 0, String::new(), 0);
        let last = frame.rows[0].visible_cells().last().unwrap();
        assert_eq!(
            (last.cell_index(), last.width(), last.str()),
            (75, 5, "end")
        );
    }

    #[test]
    fn kitty_multicell_text_keeps_rows_linked_during_print_and_edit() {
        let (mut terminal, mut wire) = engine();
        terminal.advance_bytes("\x1b]66;s=3:w=2:n=1:d=2;W\x07\u{0301}!\x1b[6n");
        assert_eq!(receive(&mut wire), b"\x1b[1;8R");
        terminal.advance_bytes(b"\x1b[2;3H?\x1b[6n");
        assert_eq!(receive(&mut wire), b"\x1b[2;8R");
        let frame = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        for (offset, line) in frame.rows[..3].iter().enumerate() {
            let cell = line.get_cell(0).unwrap();
            let slice = cell.attrs().multicell_row().unwrap();
            assert_eq!((cell.width(), slice.offset()), (6, offset as u8));
            assert_eq!(slice.run().text(), "W\u{0301}");
            assert_eq!(cell.str(), if offset == 0 { "W\u{0301}" } else { " " });
        }
        assert_eq!(frame.rows[1].get_cell(6).unwrap().str(), "?");
        terminal.advance_bytes(b"\x1b[3;3H\x1b[X");
        let frame = Snapshot::capture(&terminal, None, 2, 0, String::new(), 0);
        assert!(
            frame.rows[..3]
                .iter()
                .all(|line| !line.contains_multicell_text())
        );
        assert_eq!(frame.rows[0].get_cell(6).unwrap().str(), "!");
        assert_eq!(frame.rows[1].get_cell(6).unwrap().str(), "?");
    }

    #[test]
    fn kitty_multicell_text_wraps_past_its_lower_slice_with_autowrap_off() {
        let (mut terminal, mut wire) = engine();
        terminal.resize(TerminalSize {
            cols: 8,
            rows: 4,
            ..TerminalSize::default()
        });
        terminal.advance_bytes(b"\x1b[?7l\x1b[1;3H\x1b]66;s=2:w=3;W\x07\x1b[2;8Hq\x1b[6n");
        assert_eq!(receive(&mut wire), b"\x1b[3;2R");
        terminal.advance_bytes(b"\x1b[4;2H\x1b]66;s=7;Q\x07\x1b[6n");
        assert_eq!(receive(&mut wire), b"\x1b[4;2R");
        let frame = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        assert!(
            frame.rows[0]
                .get_cell(2)
                .unwrap()
                .attrs()
                .multicell_row()
                .is_some()
        );
        assert_eq!(frame.rows[2].get_cell(0).unwrap().str(), "q");
    }

    #[test]
    fn kitty_multicell_text_survives_scrollback_and_clears_if_it_no_longer_fits() {
        let (mut terminal, _) = engine();
        let size = TerminalSize {
            cols: 10,
            rows: 4,
            ..TerminalSize::default()
        };
        terminal.resize(size);
        terminal.advance_bytes(b"\x1b[3;5H\x1b]66;s=3:w=2;W\x07\x1b[4;1H\r\n");
        let slices = |terminal: &Terminal| {
            terminal
                .screen()
                .physical_lines()
                .filter_map(|line| {
                    let cell = line.get_cell(4)?;
                    cell.attrs().multicell_row().map(|slice| slice.offset())
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(slices(&terminal), [0, 1, 2]);
        terminal.resize(TerminalSize { cols: 14, ..size });
        assert_eq!(slices(&terminal), [0, 1, 2]);
        terminal.resize(TerminalSize { cols: 5, ..size });
        assert!(
            terminal
                .screen()
                .physical_lines()
                .all(|line| !line.contains_multicell_text())
        );
        terminal.resize(TerminalSize { cols: 8, ..size });

        for edit in [
            "\x1b[2;1H\x1b[L",
            "\x1b[2;1H\x1b[M",
            "\x1b[2;6H\x1b[P",
            "\x1b[1;1H\x1b[2M",
        ] {
            terminal.advance_bytes(b"\x1b[2J\x1b[1;5H\x1b]66;s=2:w=2;A\x07");
            assert_eq!(slices(&terminal), [0, 1]);
            terminal.advance_bytes(edit);
            assert!(slices(&terminal).is_empty(), "{edit:?}");
        }
    }

    #[test]
    fn kitty_multicell_reflow_keeps_a_tail_and_its_cursor_beside_the_lower_slice() {
        use wezterm_term::color::ColorAttribute;
        let (mut terminal, _) = engine();
        let size = TerminalSize {
            cols: 6,
            rows: 5,
            ..TerminalSize::default()
        };
        terminal.resize(size);
        terminal.advance_bytes(b"\x1b]66;s=3;A\x07bc\x1b[31mX\x1b[0m\x1b[1;6H");
        let before = Snapshot::capture(&terminal, None, 0, 0, String::new(), 0);
        assert!(before.bookmark_target().unwrap().toggle(&mut terminal, 0));
        terminal.resize(TerminalSize { cols: 5, ..size });
        let after = Snapshot::capture(&terminal, None, 0, 0, String::new(), 0);
        for (offset, line) in after.rows[..3].iter().enumerate() {
            let cell = line.get_cell(0).unwrap();
            assert_eq!(cell.attrs().multicell_row().unwrap().offset(), offset as u8);
            assert_eq!(cell.width(), 3);
        }
        assert_eq!(after.rows[0].as_str().trim_end(), "Abc");
        assert!(after.rows[0].last_cell_was_wrapped());
        let tail = after.rows[1].get_cell(3).unwrap();
        assert_eq!(tail.str(), "X");
        assert_eq!(tail.attrs().foreground(), ColorAttribute::PaletteIndex(1));
        assert_eq!((after.cursor.x, after.cursor.y), (3, 1));
        assert_eq!(&*after.scrollback_bookmarks, &[after.first_stable_row + 1]);
    }

    #[test]
    fn kitty_multicell_reflow_moves_fitting_rectangles_past_the_edge_and_back() {
        let (mut terminal, _) = engine();
        let size = TerminalSize {
            cols: 8,
            rows: 5,
            ..TerminalSize::default()
        };
        terminal.resize(size);
        terminal.advance_bytes(b"12345\x1b]66;s=2;W\x07q\x1b[1;8H");
        terminal.resize(TerminalSize { cols: 6, ..size });
        let frame = Snapshot::capture(&terminal, None, 0, 0, String::new(), 0);
        assert_eq!(frame.rows[0].as_str().trim_end(), "12345");
        assert_eq!(frame.rows[1].as_str().trim_end(), "Wq");
        assert_eq!(
            frame.rows[2]
                .get_cell(0)
                .unwrap()
                .attrs()
                .multicell_row()
                .unwrap()
                .offset(),
            1
        );
        assert_eq!((frame.cursor.x, frame.cursor.y), (2, 1));
        terminal.resize(size);
        let frame = Snapshot::capture(&terminal, None, 0, 0, String::new(), 0);
        assert_eq!(frame.rows[0].as_str().trim_end(), "12345Wq");
        assert_eq!(
            frame.rows[1]
                .get_cell(5)
                .unwrap()
                .attrs()
                .multicell_row()
                .unwrap()
                .offset(),
            1
        );
        assert_eq!((frame.cursor.x, frame.cursor.y), (7, 0));
    }

    #[test]
    fn kitty_multicell_reflow_keeps_text_visible_when_an_oversized_rectangle_is_removed() {
        let (mut terminal, _) = engine();
        let size = TerminalSize {
            cols: 7,
            rows: 6,
            ..TerminalSize::default()
        };
        terminal.resize(size);
        terminal.advance_bytes(b"\x1b]66;s=4;W\x07abc\x1b[1;7H");
        terminal.resize(TerminalSize {
            cols: 3,
            rows: 3,
            ..size
        });
        let frame = Snapshot::capture(&terminal, None, 0, 0, String::new(), 0);
        assert_eq!(frame.rows[0].as_str().trim_end(), "abc");
        assert_eq!((frame.cursor.x, frame.cursor.y), (2, 0));
        assert!(
            terminal
                .screen()
                .physical_lines()
                .all(|line| !line.contains_multicell_text())
        );

        terminal.resize(size);
        terminal.advance_bytes(b"\x1b[?1049h\x1b[1;5H\x1b]66;s=2;R\x07X");
        terminal.resize(TerminalSize { cols: 5, ..size });
        let frame = Snapshot::capture(&terminal, None, 0, 0, String::new(), 0);
        assert!(frame.alternate);
        assert!(
            frame
                .rows
                .iter()
                .all(|line| !line.contains_multicell_text())
        );
        assert!(frame.rows.iter().all(|line| !line.as_str().contains('X')));
    }

    #[test]
    fn xterm_other_keys_settings_roundtrip_and_reset() {
        let (mut terminal, mut wire) = engine();
        terminal.advance_bytes(b"\x1b[?4m\x1b[?4g");
        assert_eq!(receive(&mut wire), b"\x1b[>4;0m");
        assert_eq!(receive(&mut wire), b"\x1b[>4;0f");
        for byte in b"\x1b[>4;2m\x1b[>4;1f\x1b[?1;4m\x1b[?4g\x1bP$q>4m\x1b\\\x1bP$q>4f\x1b\\" {
            terminal.advance_bytes([*byte]);
        }
        let report = b"\x1b[>4;2m\x1b[>4;1f";
        assert_eq!(receive(&mut wire), b"\x1b[>4;2m");
        assert_eq!(receive(&mut wire), b"\x1b[>4;1f");
        assert_eq!(receive(&mut wire), b"\x1bP1$r>4;2m\x1b\\");
        assert_eq!(receive(&mut wire), b"\x1bP1$r>4;1f\x1b\\");
        terminal.advance_bytes(b"\x1b[>4;9m\x1b[>4;2f\x1b[>4;2;9m\x1b[>4;1:f\x1b[?4;bad");
        assert_eq!(terminal.xterm_other_keys(), (2, true));
        terminal.advance_bytes(b"\x1b[>4n");
        assert_eq!(terminal.xterm_other_keys(), (0, true));
        terminal.advance_bytes(report);
        assert_eq!(terminal.xterm_other_keys(), (2, true));
        let inspect = TerminalModes::read(&terminal).as_json();
        assert_eq!(inspect["keyboard"]["modify_other_keys"], 2);
        assert_eq!(inspect["keyboard"]["format_other_keys"], "csi-u");
        terminal.advance_bytes(b"\x1b[>4m\x1b[>4f");
        assert_eq!(terminal.xterm_other_keys(), (0, false));
        for reset in [b"\x1b[>m\x1b[>f".as_slice(), b"\x1b[!p", b"\x1bc"] {
            terminal.advance_bytes(report);
            terminal.advance_bytes(reset);
            assert_eq!(terminal.xterm_other_keys(), (0, false));
        }
        terminal.advance_bytes(report);
        terminal.reset_connection();
        assert_eq!(terminal.xterm_other_keys(), (0, false));
    }

    #[test]
    fn xterm_other_keys_distinguishes_control_aliases_and_preserves_key_classes() {
        use wezterm_term::{KeyCode as K, KeyModifiers as M};
        let (mut terminal, mut wire) = engine();
        for (level, cases) in [
            (
                1,
                vec![
                    (K::Char('i'), M::CTRL, "\t"),
                    (K::Char('A'), M::CTRL | M::SHIFT, "\x01"),
                    (K::Char('3'), M::CTRL, "\x1b"),
                    (K::Char(','), M::CTRL, "\x1b[27;5;44~"),
                    (K::Tab, M::SHIFT, "\x1b[Z"),
                    (K::Tab, M::CTRL, "\x1b[27;5;9~"),
                ],
            ),
            (
                2,
                vec![
                    (K::Char('i'), M::CTRL, "\x1b[27;5;105~"),
                    (K::Char('I'), M::CTRL | M::SHIFT, "\x1b[27;6;73~"),
                    (K::Char('É'), M::SHIFT, "\x1b[27;2;201~"),
                    (K::Char('a'), M::ALT, "\x1b[27;3;97~"),
                    (K::Char('a'), M::NONE, "a"),
                    (K::Tab, M::NONE, "\t"),
                    (K::Tab, M::SHIFT, "\x1b[27;2;9~"),
                ],
            ),
            (
                3,
                vec![
                    (K::Char(' '), M::NONE, "\x1b[27;1;32~"),
                    (K::Tab, M::NONE, "\x1b[27;1;9~"),
                    (K::Backspace, M::NONE, "\x1b[27;1;127~"),
                    (K::LeftArrow, M::CTRL, "\x1b[1;5D"),
                    (K::Function(12), M::NONE, "\x1b[24~"),
                    (K::Delete, M::NONE, "\x1b[3~"),
                ],
            ),
        ] {
            terminal.advance_bytes(format!("\x1b[>4;{level}m"));
            for (key, mods, expected) in cases {
                terminal.key_down(key, mods).unwrap();
                terminal.key_up(key, mods).unwrap();
                assert_eq!(
                    receive(&mut wire),
                    expected.as_bytes(),
                    "{level} {key:?} {mods:?}"
                );
            }
        }
        terminal.advance_bytes(b"\x1b[>4;1f");
        let keypad = KeyDetails {
            keypad: Some(KeypadKey::Enter),
            ..Default::default()
        };
        let composed = KeyDetails {
            text: Some("´q".into()),
            ..Default::default()
        };
        let held = KeyDetails {
            repeat_text: Some("Q".into()),
            ..Default::default()
        };
        for (key, mods, repeat, details, expected) in [
            (K::Enter, M::NONE, false, &keypad, "\r"),
            (K::Char('q'), M::NONE, false, &composed, "´q"),
            (K::Char('q'), M::SHIFT, true, &held, "\x1b[81;2u"),
        ] {
            assert_eq!(
                encode_key_input(&terminal, key, mods, true, repeat, details).unwrap(),
                expected
            );
        }
        terminal.advance_bytes(b"\x1b[>3u");
        let no_details = KeyDetails::default();
        assert_eq!(
            encode_key_input(&terminal, K::Char('i'), M::CTRL, false, false, &no_details).unwrap(),
            "\x1b[105;5:3u"
        );
        terminal.advance_bytes(b"\x1b[<u");
        assert_eq!(
            encode_key_input(&terminal, K::Char('i'), M::CTRL, true, false, &no_details).unwrap(),
            "\x1b[105;5u"
        );
    }

    #[test]
    fn kitty_pointer_queries_and_stacks_follow_the_active_screen() {
        let (mut term, mut out) = engine();
        for byte in b"\x1b]22;hand2\x07\x1b]22;>help,crosshair\x1b\\" {
            term.advance_bytes([*byte]);
        }
        term.advance_bytes(b"\x1b]22;?__current__,__default__,__grabbed__,text,copy,unknown\x07");
        assert_eq!(
            receive(&mut out),
            b"\x1b]22;crosshair,default,default,1,0,0\x1b\\"
        );
        term.advance_bytes(b"\x1b]22;=ew-resize\x07\x1b]22;<ignored\x07");
        assert_eq!(term.application_pointer_shape(), Some(PointerShape::Help));
        term.advance_bytes(b"\x1b[?1049h");
        assert_eq!(term.application_pointer_shape(), None);
        term.advance_bytes(b"\x1b]22;wait\x07\x1b[?1049l");
        assert_eq!(term.application_pointer_shape(), Some(PointerShape::Help));
        term.advance_bytes(b"\x1b]22;\x07\x1b]22;?__current__\x07");
        assert_eq!(receive(&mut out), b"\x1b]22;0\x1b\\");
        term.advance_bytes(b"\x1b]22;<\x07\x1b[?1049h");
        assert_eq!(term.application_pointer_shape(), Some(PointerShape::Wait));
        term.advance_bytes(b"\x1b[!p\x1b[?1049l");
        assert_eq!(term.application_pointer_shape(), None);
        term.advance_bytes(b"\x1b]22;text\x07\x1bc");
        assert_eq!(term.application_pointer_shape(), None);
        term.advance_bytes(b"\x1b]22;text\x07");
        term.reset_connection();
        assert_eq!(term.application_pointer_shape(), None);
    }

    #[test]
    fn kitty_pointer_eviction_and_invalid_requests_preserve_the_visible_shape() {
        let (mut term, mut out) = engine();
        term.advance_bytes(b"\x1b]22;pointer\x07");
        term.advance_bytes(format!("\x1b]22;>{}wait\x07", "wait,".repeat(15)));
        term.advance_bytes(format!("\x1b]22;>text,{}\x07", "x".repeat(4200)));
        term.advance_bytes(b"\x1b]22;unknown\x07\x1b]22;?__current__\x07");
        assert_eq!(receive(&mut out), b"\x1b]22;wait\x1b\\");
        for _ in 0..16 {
            term.advance_bytes(b"\x1b]22;<\x07");
        }
        assert_eq!(term.application_pointer_shape(), None);
    }

    #[test]
    fn xterm_sgr_stack_caps_saved_levels_and_pairs_extra_pushes_with_pops() {
        use wezterm_term::color::ColorAttribute;

        let (mut term, _) = engine();
        for index in 20..=30 {
            term.advance_bytes(format!("\x1b[38;5;{index}m\x1b[#{{"));
        }
        term.advance_bytes(b"\x1b[38;5;42m\x1b[#}A");
        for _ in 0..10 {
            term.advance_bytes(b"\x1b[#}X");
        }
        term.advance_bytes(b"\x1b[#}B\x1b[0#{\x1b[35m\x1b[#}C");
        let frame = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
        let expected = std::iter::once(42).chain((20..30).rev()).chain([20, 5]);
        let colors = frame.rows[0]
            .visible_cells()
            .map(|cell| cell.attrs().foreground());
        assert!(colors.eq(expected.map(ColorAttribute::PaletteIndex)));
    }

    #[test]
    fn xterm_sgr_stack_survives_terminal_resets_and_clears_on_reconnect() {
        use wezterm_term::color::ColorAttribute;

        let (mut term, _) = engine();
        for reset in ["\x1bc", "\x1b[!p"] {
            term.advance_bytes(format!("\x1b[31m\x1b[#p{reset}\x1b[#q\x1b[H"));
            term.advance_bytes(b"A");
            let frame = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
            let cell = frame.rows[0].visible_cells().next().unwrap();
            assert_eq!(cell.attrs().foreground(), ColorAttribute::PaletteIndex(1));
        }
        term.advance_bytes(b"\x1b[31m\x1b[#p");
        term.reset_connection();
        term.advance_bytes(b"\x1b[32m\x1b[#qB");
        let frame = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
        let cell = frame.rows[0].visible_cells().next().unwrap();
        assert_eq!(cell.attrs().foreground(), ColorAttribute::PaletteIndex(2));
    }

    #[test]
    fn xterm_private_modes_restore_named_values_without_consuming_them() {
        let (mut term, mut out) = engine();
        term.advance_bytes(b"\x1b[?1;2004h\x1b[?25l");
        // Save both set and reset values. A numerically valid unknown mode can
        // appear anywhere in the list.
        term.advance_bytes(b"\x1b[?1;65530;25;2004s\x1b[?1;2004l\x1b[?25h");
        term.advance_bytes(b"\x1b[?65530;2004r");
        assert!(term.bracketed_paste_enabled());
        term.send_paste("saved paste").unwrap();
        assert_eq!(receive(&mut out), b"\x1b[200~saved paste\x1b[201~");
        term.key_down(KeyCode::UpArrow, KeyModifiers::NONE).unwrap();
        assert_eq!(receive(&mut out), b"\x1b[A");

        for _ in 0..2 {
            term.advance_bytes(b"\x1b[?1l\x1b[?25h\x1b[?1;25r\x1b[?25$p");
            assert_eq!(receive(&mut out), b"\x1b[?25;2$y");
            term.key_down(KeyCode::UpArrow, KeyModifiers::NONE).unwrap();
            assert_eq!(receive(&mut out), b"\x1bOA");
        }

        term.advance_bytes(b"\x1b[?2004l\x1b[?2004s\x1b[?2004h\x1b[?2004r");
        term.send_paste("new saved value").unwrap();
        assert_eq!(receive(&mut out), b"new saved value");
    }

    #[test]
    fn xterm_private_modes_save_mouse_groups_and_preserve_both_screen_buffers() {
        use wezterm_term::{MouseButton, MouseEvent, MouseEventKind};

        let (mut term, mut out) = engine();
        term.advance_bytes(b"primary\x1b[?47s\x1b[?1049halternate");
        term.advance_bytes(b"\x1b[?1047r\x1b[?1049$p");
        assert_eq!(receive(&mut out), b"\x1b[?1049;2$y");
        assert!(text(&term).starts_with("primary"));
        term.advance_bytes(b"\x1b[?47h\x1b[?1049s\x1b[?47l\x1b[?47r");
        assert!(term.is_alt_screen_active());
        assert!(text(&term).starts_with("alternate"));

        // Each group saves its current state, including when the named alias
        // differs from the active reporting level or encoding.
        term.advance_bytes(b"\x1b[?1002;1006h\x1b[?1000;1005s");
        term.advance_bytes(b"\x1b[?1002l\x1b[?1003;1016h\x1b[?1003;1016r");
        assert!(term.button_event_mouse_enabled());
        assert!(!term.any_event_mouse_enabled());
        assert_eq!(term.get_mouse_encoding(), wezterm_term::MouseEncoding::SGR);
        term.mouse_event(MouseEvent {
            kind: MouseEventKind::Press,
            button: MouseButton::Left,
            x: 2,
            y: 3,
            x_pixel_offset: 0,
            y_pixel_offset: 0,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();
        assert_eq!(receive(&mut out), b"\x1b[<0;3;4M");
    }

    #[test]
    fn xterm_private_modes_restore_cursor_and_resize_side_effects() {
        let (mut term, mut out) = engine();
        term.advance_bytes(b"\x1b[5;10H\x1b[?1048s\x1b[H\x1b[?1048r\x1b[6n");
        assert_eq!(receive(&mut out), b"\x1b[5;10R");
        term.advance_bytes(b"\x1b[2;8r\x1b[?6h\x1b[?6s\x1b[?6l\x1b[5;10H\x1b[?6r\x1b[6n");
        assert_eq!(receive(&mut out), b"\x1b[1;1R");
        assert_eq!(term.cursor_pos().y, 1);

        term.advance_bytes(b"\x1b[?2048h");
        assert_eq!(receive(&mut out), b"\x1b[48;24;80;480;800t");
        term.advance_bytes(b"\x1b[?2048s\x1b[?2048l\x1b[?2048r");
        assert_eq!(receive(&mut out), b"\x1b[48;24;80;480;800t");
    }

    #[test]
    fn xterm_private_modes_keep_saves_through_soft_reset_and_clear_on_full_reset() {
        for reset_connection in [false, true] {
            let (mut term, mut out) = engine();
            term.advance_bytes(b"\x1b[?1;2004h\x1b[?1;2004s\x1b[!p\x1b[?2004l\x1b[?1;2004r");
            assert!(term.bracketed_paste_enabled());
            term.key_down(KeyCode::UpArrow, KeyModifiers::NONE).unwrap();
            assert_eq!(receive(&mut out), b"\x1bOA");

            term.advance_bytes(b"\x1b[5;10H\x1b[?1048s");
            if reset_connection {
                term.reset_connection();
            } else {
                term.advance_bytes(b"\x1bc");
            }
            term.advance_bytes(b"\x1b[?1;2004;1048r\x1b[6n");
            assert_eq!(receive(&mut out), b"\x1b[1;1R");
            assert!(!term.bracketed_paste_enabled());
            term.key_down(KeyCode::UpArrow, KeyModifiers::NONE).unwrap();
            assert_eq!(receive(&mut out), b"\x1b[A");

            term.advance_bytes(b"keep\x1b[?3s\x1b[?3r\x1b[?7r");
            assert!(text(&term).starts_with("keep"));
            term.advance_bytes(b"\x1b[?7$p");
            assert_eq!(receive(&mut out), b"\x1b[?7;1$y");
        }
    }

    #[test]
    fn synchronized_output_queries_and_resets_follow_parser_state() {
        let (mut t, mut rx) = engine();
        for byte in b"\x1b[?2026h" {
            t.advance_bytes([*byte]);
        }
        assert!(t.synchronized_output());
        t.advance_bytes(b"\x1b[?2026$p");
        assert_eq!(receive(&mut rx), b"\x1b[?2026;1$y");
        t.advance_bytes(b"\x1b[?2026l\x1b[?2026$p");
        assert_eq!(receive(&mut rx), b"\x1b[?2026;2$y");
        t.advance_bytes(b"\x1b[?2026h\x1bc");
        assert!(!t.synchronized_output());
        t.advance_bytes(b"\x1b[?2026h");
        t.reset_connection();
        assert!(!t.synchronized_output());
    }
    #[test]
    fn delete_and_backspace_stay_distinct_for_every_kitty_event_and_modifier() {
        for flags in [1, 3, 8, 11] {
            let (tx, mut rx) = mpsc::unbounded_channel();
            let mut t = create(TerminalSize::default(), tx.clone());
            t.advance_bytes(format!("\x1b[>{flags}u"));
            for (modifiers, code) in [
                (KeyModifiers::NONE, 1),
                (KeyModifiers::SHIFT, 2),
                (KeyModifiers::CTRL, 5),
                (KeyModifiers::ALT, 3),
                (KeyModifiers::SUPER, 9),
                (
                    KeyModifiers::SHIFT
                        | KeyModifiers::CTRL
                        | KeyModifiers::ALT
                        | KeyModifiers::SUPER,
                    16,
                ),
            ] {
                for key in [KeyCode::Delete, KeyCode::Char('\x7f'), KeyCode::Backspace] {
                    for (down, repeat) in [(true, false), (true, true), (false, false)] {
                        send_key_event(&mut t, &tx, key, modifiers, down, repeat).unwrap();
                        let report = flags & 2 != 0;
                        if !down && (!report || (key == KeyCode::Backspace && flags & 8 == 0)) {
                            assert!(rx.try_recv().is_err());
                            continue;
                        }
                        let event = if !report || (key == KeyCode::Backspace && flags & 8 == 0) {
                            ""
                        } else if !down {
                            ":3"
                        } else if repeat {
                            ":2"
                        } else {
                            ""
                        };
                        let expected = if key == KeyCode::Backspace {
                            if modifiers == KeyModifiers::NONE && flags & 8 == 0 && down {
                                "\x7f".to_owned()
                            } else {
                                format!("\x1b[127;{code}{event}u")
                            }
                        } else {
                            format!("\x1b[3;{code}{event}~")
                        };
                        assert_eq!(
                            receive(&mut rx),
                            expected.as_bytes(),
                            "flags={flags}, {key:?}, {modifiers:?}, down={down}, repeat={repeat}"
                        );
                    }
                }
            }
            t.reset_connection();
            send_key_event(
                &mut t,
                &tx,
                KeyCode::Delete,
                KeyModifiers::NONE,
                true,
                false,
            )
            .unwrap();
            assert_eq!(receive(&mut rx), b"\x1b[3~");
            send_key_event(
                &mut t,
                &tx,
                KeyCode::Backspace,
                KeyModifiers::NONE,
                true,
                false,
            )
            .unwrap();
            assert_eq!(receive(&mut rx), b"\x7f");
        }
    }
    #[test]
    fn wheel_policy_preserves_mouse_reporting_and_never_synthesizes_game_keys() {
        use wezterm_term::{MouseButton, MouseEvent, MouseEventKind};
        let last = MouseEvent {
            kind: MouseEventKind::Move,
            button: MouseButton::None,
            modifiers: KeyModifiers::NONE,
            x: 0,
            y: 0,
            x_pixel_offset: 0,
            y_pixel_offset: 0,
        };
        for flags in [0, 3, 11] {
            for alternate in [false, true] {
                for tracking in [false, true] {
                    for shift in [false, true] {
                        for policy in [false, true] {
                            let (mut t, mut rx) = engine();
                            for _ in 0..30 {
                                t.advance_bytes(b"line\r\n");
                            }
                            if alternate {
                                t.advance_bytes(b"\x1b[?1049h");
                            }
                            if tracking {
                                t.advance_bytes(b"\x1b[?1000h\x1b[?1006h");
                            }
                            if flags != 0 {
                                t.advance_bytes(format!("\x1b[>{flags}u"));
                            }
                            let modifiers = if shift {
                                KeyModifiers::SHIFT
                            } else {
                                KeyModifiers::NONE
                            };
                            let mut offset = 0;
                            scroll_wheel(&mut t, &mut offset, last, 1, modifiers, policy).unwrap();
                            if !shift && tracking {
                                assert_eq!(receive(&mut rx), b"\x1b[<64;1;1M");
                            } else if !shift && alternate && policy {
                                assert_eq!(receive(&mut rx), b"\x1b[A");
                            } else {
                                assert!(rx.try_recv().is_err());
                                assert_eq!(offset, usize::from(!alternate));
                            }
                            // Drain an ordered engine reply as a barrier: the
                            // engine writes on another thread, so try_recv alone
                            // cannot prove that no cursor key was queued.
                            t.advance_bytes(b"\x1b[?2026$p");
                            assert_eq!(receive(&mut rx), b"\x1b[?2026;2$y");
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn wheel_scrolls_history_reports_mouse_and_leaves_alternate_history_to_the_app() {
        let (mut terminal, mut replies) = engine();
        terminal.resize(TerminalSize {
            rows: 3,
            cols: 20,
            ..Default::default()
        });
        for i in 0..12 {
            terminal.advance_bytes(format!("line {i}\r\n"));
        }
        let last = wezterm_term::MouseEvent {
            kind: wezterm_term::MouseEventKind::Move,
            button: wezterm_term::MouseButton::None,
            modifiers: KeyModifiers::NONE,
            x: 0,
            y: 0,
            x_pixel_offset: 0,
            y_pixel_offset: 0,
        };
        let mut offset = 0;
        scroll_wheel(
            &mut terminal,
            &mut offset,
            last,
            3,
            KeyModifiers::NONE,
            true,
        )
        .unwrap();
        assert_eq!(offset, 3);
        assert!(replies.try_recv().is_err());
        scroll_wheel(
            &mut terminal,
            &mut offset,
            last,
            isize::MAX,
            KeyModifiers::NONE,
            true,
        )
        .unwrap();
        assert_eq!(offset, terminal.screen().phys_row(0));
        let snapshot = Snapshot::capture(&terminal, None, 1, usize::MAX, String::new(), 0);
        assert_eq!(
            snapshot.first_stable_row,
            terminal.screen().phys_to_stable_row_index(0)
        );
        terminal.advance_bytes(b"\x1b[?1000h\x1b[?1006h");
        scroll_wheel(
            &mut terminal,
            &mut offset,
            last,
            2,
            KeyModifiers::NONE,
            true,
        )
        .unwrap();
        assert_eq!(receive(&mut replies), b"\x1b[<64;1;1M");
        assert_eq!(receive(&mut replies), b"\x1b[<64;1;1M");
        scroll_wheel(
            &mut terminal,
            &mut offset,
            last,
            -2,
            KeyModifiers::SHIFT,
            true,
        )
        .unwrap();
        assert!(replies.try_recv().is_err());
        let primary = offset;
        terminal.advance_bytes(b"\x1b[?1000l\x1b[?1049h");
        scroll_wheel(
            &mut terminal,
            &mut offset,
            last,
            2,
            KeyModifiers::NONE,
            true,
        )
        .unwrap();
        assert_eq!(receive(&mut replies), b"\x1b[A");
        assert_eq!(receive(&mut replies), b"\x1b[A");
        assert_eq!(offset, primary);
        assert_eq!(
            Snapshot::capture(&terminal, None, 2, offset, String::new(), 0).scroll_offset,
            0
        );
        terminal.advance_bytes(b"\x1b[?1049l");
        assert_eq!(
            Snapshot::capture(&terminal, None, 3, offset, String::new(), 0).scroll_offset,
            primary
        );
    }
    #[test]
    fn game_wheel_policy_and_explicit_override_survive_reconnect() {
        let (mut t, mut rx) = engine();
        let mut profile = crate::options::PlayerOptions {
            game_mode: true,
            ..Default::default()
        };
        let last = wezterm_term::MouseEvent {
            kind: wezterm_term::MouseEventKind::Move,
            button: wezterm_term::MouseButton::None,
            modifiers: KeyModifiers::NONE,
            x: 0,
            y: 0,
            x_pixel_offset: 0,
            y_pixel_offset: 0,
        };
        let mut offset = 0;
        for _ in 0..2 {
            t.reset_connection();
            t.advance_bytes(b"\x1b[?1049h\x1b[>3u");
            profile.alternate_scroll = None;
            scroll_wheel(
                &mut t,
                &mut offset,
                last,
                1,
                KeyModifiers::NONE,
                profile.alternate_scroll_enabled(),
            )
            .unwrap();
            t.advance_bytes(b"\x1b[?2026$p");
            assert_eq!(receive(&mut rx), b"\x1b[?2026;2$y");
            profile.alternate_scroll = Some(true);
            scroll_wheel(
                &mut t,
                &mut offset,
                last,
                1,
                KeyModifiers::NONE,
                profile.alternate_scroll_enabled(),
            )
            .unwrap();
            assert_eq!(receive(&mut rx), b"\x1b[A");
        }
    }

    #[test]
    fn osc52_writes_utf8_clipboard_and_never_answers_read_queries() {
        #[derive(Default)]
        struct Clipboard(Mutex<Vec<Option<String>>>);
        impl wezterm_term::Clipboard for Clipboard {
            fn set_contents(
                &self,
                _selection: wezterm_term::ClipboardSelection,
                data: Option<String>,
            ) -> anyhow::Result<()> {
                self.0.lock().unwrap().push(data);
                Ok(())
            }
        }
        let (mut terminal, mut replies) = engine();
        let sink = Arc::new(Clipboard::default());
        let clipboard: Arc<dyn wezterm_term::Clipboard> = sink.clone();
        terminal.set_clipboard(&clipboard);
        terminal.advance_bytes(b"\x1b]52;c;aGVsbG8g");
        terminal.advance_bytes(b"8J+MjQ==\x07");
        assert_eq!(
            sink.0.lock().unwrap().as_slice(),
            &[Some("hello 🌍".into())]
        );
        terminal.advance_bytes(b"\x1b]52;c;?\x1b\\");
        assert!(
            replies.try_recv().is_err(),
            "remote host must not receive local clipboard contents"
        );
        assert_eq!(sink.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn printable_layout_keys_match_stock_kitty_wire_fixture() {
        #[derive(serde::Deserialize, Debug)]
        struct Case {
            logical: char,
            shifted: char,
            physical: char,
            mods: Vec<String>,
            flags: u8,
            action: String,
            text: Option<String>,
            wire: String,
        }
        let cases: Vec<Case> = serde_json::from_str(include_str!(
            "../tests/fixtures/keyboard/physical-layout.json"
        ))
        .unwrap();
        for case in cases {
            let mut modifiers = KeyModifiers::NONE;
            let mut details = KeyDetails {
                base: Some(case.logical),
                physical: Some(case.physical),
                shifted: Some(case.shifted),
                ..Default::default()
            };
            for modifier in &case.mods {
                match modifier.as_str() {
                    "CTRL" => modifiers |= KeyModifiers::CTRL,
                    "SHIFT" => modifiers |= KeyModifiers::SHIFT,
                    "CAPS_LOCK" => {
                        details.leds |= wezterm_input_types::KeyboardLedStatus::CAPS_LOCK
                    }
                    "NUM_LOCK" => details.leds |= wezterm_input_types::KeyboardLedStatus::NUM_LOCK,
                    _ => panic!("unknown modifier {case:?}"),
                }
            }
            let character = case
                .text
                .as_deref()
                .and_then(|s| s.chars().next())
                .unwrap_or_else(|| {
                    if modifiers.contains(KeyModifiers::SHIFT) {
                        case.shifted
                    } else {
                        case.logical
                    }
                });
            let (down, repeat) = match case.action.as_str() {
                "press" => (true, false),
                "repeat" => (true, true),
                "release" => (false, false),
                _ => panic!("unknown action {case:?}"),
            };
            let (writer, mut output) = mpsc::unbounded_channel();
            let mut terminal = create(TerminalSize::default(), writer.clone());
            if case.flags != 0 {
                terminal.advance_bytes(format!("\x1b[>{}u", case.flags));
            }
            send_key_event_detailed(
                &mut terminal,
                &writer,
                KeyCode::Char(character),
                modifiers,
                down,
                repeat,
                &details,
            )
            .unwrap();
            let actual = if case.wire.is_empty() {
                vec![]
            } else {
                receive(&mut output)
            };
            assert_eq!(actual, case.wire.as_bytes(), "{case:?}");
            assert!(output.try_recv().is_err(), "{case:?}");
        }
    }

    #[test]
    fn equal_physical_keys_are_omitted_and_composed_layout_text_survives() {
        for (flags, key, base, physical, text, down, repeat, expected) in [
            (15, 'z', Some('z'), 'z', None, true, false, "\x1b[122;1u"),
            (31, '@', None, 'q', None, true, false, "\x1b[64::113;;64u"),
            (
                31,
                'é',
                Some('é'),
                'e',
                None,
                false,
                false,
                "\x1b[233::101;1:3u",
            ),
            (
                31,
                '´',
                None,
                'q',
                Some("´q"),
                true,
                false,
                "\x1b[180::113;;180:113u",
            ),
            (7, 'z', Some('z'), 'y', None, true, true, "z"),
        ] {
            let (writer, mut output) = mpsc::unbounded_channel();
            let mut terminal = create(TerminalSize::default(), writer.clone());
            terminal.advance_bytes(format!("\x1b[>{flags}u"));
            send_key_event_detailed(
                &mut terminal,
                &writer,
                KeyCode::Char(key),
                KeyModifiers::NONE,
                down,
                repeat,
                &KeyDetails {
                    base,
                    physical: Some(physical),
                    text: text.map(str::to_owned),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(receive(&mut output), expected.as_bytes());
        }
    }

    #[test]
    fn alternate_shift_level_is_selected_by_the_release_modifiers() {
        let (writer, mut output) = mpsc::unbounded_channel();
        let mut terminal = create(TerminalSize::default(), writer.clone());
        terminal.advance_bytes(b"\x1b[>15u");
        let details = KeyDetails {
            base: Some('z'),
            shifted: Some('Z'),
            physical: Some('y'),
            ..Default::default()
        };
        for (pressed_character, release_mods, expected) in [
            ('z', KeyModifiers::SHIFT, "\x1b[122:90:121;2:3u"),
            ('Z', KeyModifiers::NONE, "\x1b[122::121;1:3u"),
        ] {
            send_key_event_detailed(
                &mut terminal,
                &writer,
                KeyCode::Char(pressed_character),
                release_mods,
                false,
                false,
                &details,
            )
            .unwrap();
            assert_eq!(receive(&mut output), expected.as_bytes());
        }
        terminal.advance_bytes(b"\x1b[>31u");
        let details = KeyDetails {
            repeat_text: Some("Z".into()),
            ..details
        };
        send_key_event_detailed(
            &mut terminal,
            &writer,
            KeyCode::Char('z'),
            KeyModifiers::SHIFT,
            true,
            true,
            &details,
        )
        .unwrap();
        assert_eq!(receive(&mut output), b"\x1b[122:90:121;2:2;90u");
    }

    #[test]
    fn keypad_matches_stock_kitty_wire_fixture() {
        #[derive(serde::Deserialize, Debug)]
        struct Case {
            key: String,
            flags: u8,
            action: String,
            text: Option<String>,
            #[serde(default)]
            num_lock: bool,
            wire: String,
        }
        let cases: Vec<Case> =
            serde_json::from_str(include_str!("../tests/fixtures/keyboard/keypad.json")).unwrap();
        for case in cases {
            let (key, keypad) = match case.key.as_str() {
                "ENTER" => (KeyCode::Char('\r'), None),
                "LEFT" => (KeyCode::LeftArrow, None),
                "KP_ENTER" => (KeyCode::Char('\r'), Some(KeypadKey::Enter)),
                "KP_LEFT" => (KeyCode::LeftArrow, Some(KeypadKey::Left)),
                "KP_DELETE" => (KeyCode::Delete, Some(KeypadKey::Delete)),
                "KP_BEGIN" => (KeyCode::Clear, Some(KeypadKey::Begin)),
                "KP_4" => (KeyCode::Char('4'), Some(KeypadKey::Digit4)),
                "KP_DECIMAL" => (KeyCode::Char(','), Some(KeypadKey::Decimal)),
                "KP_ADD" => (KeyCode::Char('+'), Some(KeypadKey::Add)),
                _ => panic!("unhandled fixture {case:?}"),
            };
            let (writer, mut wire) = mpsc::unbounded_channel();
            let mut terminal = create(TerminalSize::default(), writer.clone());
            if case.flags != 0 {
                terminal.advance_bytes(format!("\x1b[>{}u", case.flags));
            }
            let mut details = KeyDetails {
                keypad,
                ..Default::default()
            };
            details.leds.set(
                wezterm_input_types::KeyboardLedStatus::NUM_LOCK,
                case.num_lock,
            );
            if case.text.as_deref().is_some_and(|t| t.chars().count() > 1) {
                details.text = case.text.clone();
            }
            let (down, repeat) = match case.action.as_str() {
                "press" => (true, false),
                "repeat" => (true, true),
                "release" => (false, false),
                _ => panic!("unknown action {case:?}"),
            };
            send_key_event_detailed(
                &mut terminal,
                &writer,
                key,
                KeyModifiers::NONE,
                down,
                repeat,
                &details,
            )
            .unwrap();
            if case.wire.is_empty() {
                assert!(wire.try_recv().is_err(), "{case:?}");
            } else {
                assert_eq!(receive(&mut wire), case.wire.as_bytes(), "{case:?}");
                assert!(wire.try_recv().is_err(), "{case:?}");
            }
        }
    }

    #[test]
    fn keypad_text_uses_the_active_layout_and_only_requested_press_events() {
        let details = KeyDetails {
            keypad: Some(KeypadKey::Decimal),
            text: Some("٫ ".into()),
            leds: wezterm_input_types::KeyboardLedStatus::NUM_LOCK
                | wezterm_input_types::KeyboardLedStatus::CAPS_LOCK,
            ..Default::default()
        };
        for (flags, modifiers, down, repeat, expected) in [
            (1, KeyModifiers::NONE, true, false, "٫ "),
            (3, KeyModifiers::NONE, true, true, "٫ "),
            (8, KeyModifiers::NONE, true, false, "\x1b[57409;193u"),
            (
                27,
                KeyModifiers::SHIFT,
                true,
                false,
                "\x1b[57409;194;1643:32u",
            ),
            (27, KeyModifiers::CTRL, true, true, "\x1b[57409;197:2u"),
            (27, KeyModifiers::NONE, false, false, "\x1b[57409;193:3u"),
            (3, KeyModifiers::NONE, false, false, "\x1b[57409;193:3u"),
        ] {
            let (writer, mut wire) = mpsc::unbounded_channel();
            let mut terminal = create(TerminalSize::default(), writer.clone());
            terminal.advance_bytes(format!("\x1b[>{flags}u"));
            send_key_event_detailed(
                &mut terminal,
                &writer,
                KeyCode::Char('٫'),
                modifiers,
                down,
                repeat,
                &details,
            )
            .unwrap();
            assert_eq!(receive(&mut wire), expected.as_bytes());
        }
    }

    #[test]
    fn herdr_repaints_preserve_text_title_and_distinguish_replies_from_real_input() {
        #[derive(Default)]
        struct Clipboard(Mutex<Vec<Option<String>>>);
        impl wezterm_term::Clipboard for Clipboard {
            fn set_contents(
                &self,
                _: wezterm_term::ClipboardSelection,
                value: Option<String>,
            ) -> anyhow::Result<()> {
                self.0.lock().unwrap().push(value);
                Ok(())
            }
        }
        let (writer, mut received) = mpsc::unbounded_channel();
        let mut term = create(Default::default(), writer.clone());
        let sink = Arc::new(Clipboard::default());
        let clipboard: Arc<dyn wezterm_term::Clipboard> = sink.clone();
        term.set_clipboard(&clipboard);
        let events = crate::events::Events::default();
        let (notices, mut pending) = mpsc::channel(16);
        let player = crate::options::PlayerOptions {
            remote_sounds: Some(true),
            sync_assets: Some(true),
            ..Default::default()
        };
        term.set_notification_handler(Box::new(crate::sound::Alerts {
            events: events.clone(),
            notices,
            player: Arc::new(std::sync::RwLock::new(player)),
        }));
        let effects = b"\x07\x1b]52;c;eA==\x07\x1b]440;1;play;shot\x07\x1b]440;1;pack;v1\x07\x1b]777;notify;title;test\x07";
        term.advance_bytes(effects);
        assert_eq!(sink.0.lock().unwrap().len(), 1);
        assert!(
            events
                .since(0)
                .events
                .iter()
                .any(|e| e.kind == "notification")
        );
        assert!(matches!(
            pending.try_recv(),
            Ok(crate::sound::Notice::Play(_))
        ));
        assert!(matches!(
            pending.try_recv(),
            Ok(crate::sound::Notice::Play(_))
        ));
        assert!(matches!(
            pending.try_recv(),
            Ok(crate::sound::Notice::Pack(_))
        ));
        let cursor = events.since(0).next;

        enter_herdr_viewer(&mut term, &writer, &events);
        term.reset_connection();
        for _ in 0..2 {
            // Reproduce both a whole repaint and OSCs split across DATA frames.
            for byte in effects {
                term.advance_bytes([*byte]);
            }
            term.advance_bytes(b"\x1b]52;c;?\x07");
        }
        term.advance_bytes(b"visible\x1b]2;viewer title\x07\x1b[5n");
        term.send_paste("typed").unwrap();
        term.advance_bytes(b"\x1b[5n");
        assert!(text(&term).contains("visible"));
        assert_eq!(term.get_title(), "viewer title");
        let updates = events.since(cursor).events;
        assert!(updates.iter().any(|event| event.kind == "title"));
        assert!(updates.iter().all(|event| event.kind == "title"));
        assert_eq!(sink.0.lock().unwrap().len(), 1);
        assert!(pending.try_recv().is_err());
        assert!(matches!(received.try_recv(), Ok(TransportInput::Automatic(v)) if v == b"\x1b[0n"));
        assert!(matches!(received.try_recv(), Ok(TransportInput::Data(v)) if v == b"typed"));
        assert!(matches!(received.try_recv(), Ok(TransportInput::Automatic(v)) if v == b"\x1b[0n"));
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn kitty_modifiers_safety_keys_shifted_identity_and_composed_text() {
        for flags in [1, 3, 8, 11, 15, 31] {
            let (mut terminal, _) = engine();
            terminal.advance_bytes(format!("\x1b[>{flags}u"));
            let (writer, mut output) = mpsc::unbounded_channel();
            for key in [KeyCode::Enter, KeyCode::Tab, KeyCode::Backspace] {
                send_key_event(
                    &mut terminal,
                    &writer,
                    key,
                    KeyModifiers::NONE,
                    false,
                    false,
                )
                .unwrap();
                if flags & 8 != 0 && flags & 2 != 0 {
                    assert!(!receive(&mut output).is_empty());
                } else {
                    assert!(output.try_recv().is_err());
                }
            }
            send_key_event(
                &mut terminal,
                &writer,
                KeyCode::LeftShift,
                KeyModifiers::SHIFT,
                true,
                false,
            )
            .unwrap();
            if flags & 8 != 0 {
                assert_eq!(receive(&mut output), b"\x1b[57441;2u");
            } else {
                assert!(output.try_recv().is_err());
            }
        }
        let (mut terminal, _) = engine();
        terminal.advance_bytes(b"\x1b[>31u");
        let (writer, mut output) = mpsc::unbounded_channel();
        for (down, repeat, expected) in [
            (true, false, "\x1b[49:33;2;33u"),
            (true, true, "\x1b[49:33;2:2;33u"),
            (false, false, "\x1b[49:33;2:3u"),
        ] {
            send_key_event_detailed(
                &mut terminal,
                &writer,
                KeyCode::Char('!'),
                KeyModifiers::SHIFT,
                down,
                repeat,
                &KeyDetails {
                    base: Some('1'),
                    ..KeyDetails::default()
                },
            )
            .unwrap();
            assert_eq!(receive(&mut output), expected.as_bytes());
        }
        for (down, repeat, expected) in [
            (true, false, "\x1b[101;1;101:769u"),
            (true, true, "\x1b[101;1:2;101:769u"),
            (false, false, "\x1b[101;1:3u"),
        ] {
            send_key_event_detailed(
                &mut terminal,
                &writer,
                KeyCode::Char('e'),
                KeyModifiers::NONE,
                down,
                repeat,
                &KeyDetails {
                    base: Some('e'),
                    text: Some("e\u{301}".into()),
                    ..KeyDetails::default()
                },
            )
            .unwrap();
            assert_eq!(receive(&mut output), expected.as_bytes());
        }
    }
    #[test]
    fn reconnect_moves_primary_text_and_images_into_history_before_new_output() {
        let (mut terminal, _) = engine();
        terminal
            .advance_bytes(b"old session\x1b_Ga=T,f=32,s=1,v=1,c=1,r=1,i=7,C=1,q=2;/wAA/w==\x1b\\");
        terminal.reset_connection();
        let fresh = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        assert!(
            fresh
                .rows
                .iter()
                .all(|line| line.as_str().trim().is_empty())
        );
        terminal.advance_bytes(b"new session");
        assert!(text(&terminal).starts_with("new session"));
        let old = Snapshot::capture(&terminal, None, 2, usize::MAX, String::new(), 0);
        assert!(
            old.rows
                .iter()
                .any(|line| line.as_str().contains("old session"))
        );
        terminal.advance_bytes(b"\x1b_Ga=d,d=I,i=7\x1b\\");
        assert_eq!(terminal.image_cache_bytes(), 0);
    }
    fn engine() -> (Terminal, mpsc::UnboundedReceiver<TransportInput>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            create(
                TerminalSize {
                    pixel_width: 800,
                    pixel_height: 480,
                    dpi: 96,
                    ..TerminalSize::default()
                },
                tx,
            ),
            rx,
        )
    }
    pub(super) fn receive(rx: &mut mpsc::UnboundedReceiver<TransportInput>) -> Vec<u8> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        match runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap()
        }) {
            TransportInput::Data(bytes) => bytes,
            _ => panic!("unexpected resize"),
        }
    }

    #[test]
    fn title_stack_restores_mixed_scopes_and_preserves_the_shell_title_after_ncurses() {
        let (mut term, _) = engine();
        term.advance_bytes(b"\x1b]0;shell\x07\x1b[?1049h\x1b[22;0;0t\x1b]2;editor\x07");
        assert!(term.is_alt_screen_active());
        assert_eq!(term.get_title(), "editor");
        term.advance_bytes(b"\x1b[?1049l\x1b[23;0;0t");
        assert!(!term.is_alt_screen_active());
        assert_eq!(term.get_title(), "shell");
        term.advance_bytes(b"\x1b]2;next command\x07");
        assert_eq!(term.get_title(), "next command");

        // The inner entry saved only its icon. The window comes from the
        // preceding entry when the application asks to restore both labels.
        term.advance_bytes(b"\x1b[22;2t\x1b]2;inner\x07\x1b]1;tools\x07\x1b[22;1t");
        term.advance_bytes(b"\x1b]0;transient\x07\x1b[23;0t");
        assert_eq!(term.get_title(), "tools");
        term.advance_bytes(b"\x1b]1;\x07");
        assert_eq!(term.get_title(), "next command");
    }

    #[test]
    fn application_title_distinguishes_initial_literal_restored_and_reset_window_titles() {
        let (mut term, _) = engine();
        let capture = |term: &Terminal| Snapshot::capture(term, None, 0, 0, String::new(), 0);
        let initial = capture(&term);
        assert_eq!(initial.title, "wezterm");
        assert_eq!(initial.application_title(), None);
        term.advance_bytes(b"\x1b[22;2;10t\x1b]2;wezterm\x07");
        let explicit = capture(&term);
        assert_eq!(explicit.application_title(), Some("wezterm"));
        assert!(!explicit.same_presentation(&initial));
        term.advance_bytes(b"\x1b[22;2t\x1b[23;2;10t");
        assert_eq!(capture(&term).application_title(), None);
        term.advance_bytes(b"\x1b[23;2t");
        assert_eq!(capture(&term).application_title(), Some("wezterm"));

        term.advance_bytes(b"\x1b]1;icon only\x07");
        let icon = capture(&term);
        assert_eq!(icon.title, "icon only");
        assert_eq!(icon.application_title(), Some("wezterm"));
        term.advance_bytes(b"\x1b]2;\x07");
        let empty = capture(&term);
        assert_eq!(empty.title, "icon only");
        assert_eq!(empty.application_title(), None);
        term.advance_bytes(b"\x1b]0;shell\x07\x1b[22;0t\x1bc\x1b[23;0t");
        let reset = capture(&term);
        assert_eq!(reset.title, "shell"); // Existing raw RIS title behavior.
        assert_eq!(reset.application_title(), None);
        term.advance_bytes(b"\x1b]0;new shell\x07");
        assert_eq!(capture(&term).application_title(), Some("new shell"));
        term.reset_connection();
        assert_eq!(capture(&term).application_title(), None);
    }

    #[test]
    fn application_title_reuses_a_bounded_single_line_without_changing_the_raw_title() {
        let (mut term, _) = engine();
        let raw = format!(
            "  shell\u{2028}雪\u{2029}\u{202e}job\u{2066}  {}",
            "é".repeat(500)
        );
        term.advance_bytes(format!("\x1b]2;{raw}\x07"));
        let first = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
        assert_eq!(first.title, raw);
        let caption = first.application_title().unwrap();
        assert!(caption.starts_with("shell 雪 job é"));
        assert!(caption.chars().count() <= 200);
        assert!(!caption.contains(['\u{2028}', '\u{2029}', '\u{202e}', '\u{2066}']));
        term.advance_bytes(b"ordinary output");
        term.advance_bytes(format!("\x1b]2;{raw}\x07"));
        let second = Snapshot::capture(&term, Some(&first), 2, 0, String::new(), 0);
        assert!(Arc::ptr_eq(
            first.application_title.as_ref().unwrap(),
            second.application_title.as_ref().unwrap()
        ));
        term.advance_bytes("\x1b]0;\u{202e}\u{2066}\u{2028}  \x07");
        assert_eq!(
            Snapshot::capture(&term, None, 3, 0, String::new(), 0).application_title(),
            None
        );
    }

    #[test]
    fn inbox_command_link_refreshes_on_commit_and_expiration() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (mut term, events) = watched_terminal();
        let frame = |term: &Terminal, generation| {
            let mut frame = Snapshot::capture(term, None, 0, 0, String::new(), 0);
            frame.connection_generation = generation;
            frame
        };
        let store = FrameStore::new(frame(&term, 7));
        let wakes = Arc::new(AtomicUsize::new(0));
        let inbox = crate::events::Inbox::new({
            let wakes = wakes.clone();
            move || {
                wakes.fetch_add(1, Ordering::Relaxed);
            }
        });
        store.watch_window(Some((&inbox, 42, false)));
        events.connect_inbox(42, "fixture", &inbox);
        events.command_stream(Some(7));
        assert!(events.set_command_watch(true));
        term.advance_bytes(b"\x1b]133;C\x07first result\x1b]133;D;17\x07");
        let notice = &inbox.rows()[0];
        let crate::events::NoticeTarget::Pane { pane: 42, sequence } = notice.target else {
            panic!("expected a pane notice");
        };
        let link = inbox.command_link(42, sequence).unwrap();
        assert_eq!(link.generation, 7);
        let first = term.command_history().front().unwrap();
        assert_eq!(link.id, first.id);
        assert_eq!(first.output.as_deref(), Ok("first result"));
        assert!(!store.load().has_command(link));
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        inbox.acknowledge_wake();

        store.publish(frame(&term, 7));
        assert!(store.load().has_command(link));
        assert_eq!(wakes.load(Ordering::Relaxed), 2);
        inbox.acknowledge_wake();

        term.advance_bytes(b"\x1b]133;C\x07next result\x1b]133;D;0\x07");
        store.publish(frame(&term, 7));
        assert!(store.load().has_command(link));
        assert_eq!(wakes.load(Ordering::Relaxed), 2);

        // Even a still-retained ID is invalid when the connection changes.
        store.publish(frame(&term, 8));
        assert!(!store.load().has_command(link));
        assert_eq!(wakes.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn application_title_refreshes_only_the_subscribed_window_after_publication() {
        let (mut term, _) = engine();
        let frame = |term: &Terminal| Snapshot::capture(term, None, 0, 0, String::new(), 0);
        let store = FrameStore::new(frame(&term));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let observe = |name| {
            let store = store.clone();
            let observed = observed.clone();
            crate::events::Inbox::new(move || {
                observed
                    .lock()
                    .unwrap()
                    .push((name, store.load().application_title().map(str::to_owned)));
            })
        };
        let first = observe("first");
        let second = observe("second");
        store.watch_window(Some((&first, 1, true)));
        term.advance_bytes(b"\x1b]1;background icon\x07");
        store.publish(frame(&term));
        assert!(observed.lock().unwrap().is_empty());
        term.advance_bytes(b"\x1b]2;building\x07");
        store.publish(frame(&term));
        first.acknowledge_wake();
        term.advance_bytes(b"more output\x1b]2;  building  \x07");
        store.publish(frame(&term));
        store.watch_window(Some((&second, 1, true)));
        term.advance_bytes(b"\x1bc");
        store.publish(frame(&term));
        store.watch_window(None);
        term.advance_bytes(b"\x1b]0;unwatched\x07");
        store.publish(frame(&term));
        assert_eq!(
            *observed.lock().unwrap(),
            [("first", Some("building".to_owned())), ("second", None)]
        );
    }

    #[test]
    fn indexed_titles_and_stack_depth_belong_to_the_requesting_connection() {
        let (mut term, mut out) = engine();
        let (mut sibling, mut other) = engine();
        for i in 0..12 {
            term.advance_bytes(format!("\x1b]2;title {i}\x07\x1b[22;2t"));
        }
        term.advance_bytes(b"\x1b[#S");
        assert_eq!(receive(&mut out), b"\x1b[12;10#S");
        term.advance_bytes(b"\x1b[23;2;1t");
        assert_eq!(term.get_title(), "title 10");
        term.advance_bytes(b"\x1b[23;2t");
        assert_eq!(term.get_title(), "title 11");

        sibling.advance_bytes(b"\x1b]2;sibling\x07\x1b[23;2;1t\x1b[23;2t\x1b[#S");
        assert_eq!(sibling.get_title(), "sibling");
        assert_eq!(receive(&mut other), b"\x1b[0;10#S");
        for reconnect in [false, true] {
            term.advance_bytes(b"\x1b]0;old shell\x07\x1b[22;0;10t\x1b[22;0t");
            if reconnect {
                term.reset_connection();
            } else {
                term.advance_bytes(b"\x1bc");
            }
            term.advance_bytes(b"\x1b]0;new shell\x07\x1b[23;0;10t\x1b[23;0t\x1b[#S");
            assert_eq!(term.get_title(), "new shell");
            assert_eq!(receive(&mut out), b"\x1b[0;10#S");
        }
    }
    #[test]
    fn in_band_resize_reports_committed_cells_and_pixels_when_subscribed() {
        let (mut term, mut out) = engine();
        let (mut neighbor, mut other_out) = engine();
        term.advance_bytes(b"\x1b[?2048$p");
        assert_eq!(receive(&mut out), b"\x1b[?2048;2$y");
        for _ in 0..2 {
            term.advance_bytes(b"\x1b[?2048h");
            assert_eq!(receive(&mut out), b"\x1b[48;24;80;480;800t");
        }
        let mut size = TerminalSize {
            rows: 36,
            cols: 120,
            pixel_height: 864,
            pixel_width: 1440,
            dpi: 120,
        };
        term.advance_bytes(b"\x1b[?1049h");
        term.resize(size);
        assert_eq!(receive(&mut out), b"\x1b[48;36;120;864;1440t");
        neighbor.resize(size);
        neighbor.advance_bytes(b"\x1b[?2048$p");
        assert_eq!(receive(&mut other_out), b"\x1b[?2048;2$y");

        // Neither a repeated size nor a DPI change that keeps the text area
        // identical should precede the query reply with another report.
        term.resize(size);
        size.dpi = 144;
        term.resize(size);
        term.advance_bytes(b"\x1b[?2048$p");
        assert_eq!(receive(&mut out), b"\x1b[?2048;1$y");

        size.pixel_width = 0;
        term.resize(size);
        assert_eq!(receive(&mut out), b"\x1b[48;36;120;864;0t");
        size.pixel_height = 0;
        term.resize(size);
        assert_eq!(receive(&mut out), b"\x1b[48;36;120;0;0t");
        // Screen storage always has at least one cell, even if geometry isn't
        // yet measurable. Report the committed grid rather than the raw request.
        size.cols = 0;
        size.rows = 0;
        term.resize(size);
        assert_eq!(receive(&mut out), b"\x1b[48;1;1;0;0t");
        term.resize(size);
        term.advance_bytes(b"\x1b[?1049l\x1b[?2048$p");
        assert_eq!(receive(&mut out), b"\x1b[?2048;1$y");
    }

    #[test]
    fn in_band_resize_subscription_obeys_disable_reset_and_disconnect() {
        let (mut term, mut out) = engine();
        term.advance_bytes(b"\x1b[?2048h\x1b[!p\x1b[?2048$p");
        assert_eq!(receive(&mut out), b"\x1b[48;24;80;480;800t");
        // DECSTR resets a limited set of VT controls. Like focus and color
        // subscriptions, this subscription lasts until DECRST, RIS or disconnect.
        assert_eq!(receive(&mut out), b"\x1b[?2048;1$y");
        term.advance_bytes(b"\x1b[?2048l");
        let size = TerminalSize::default();
        term.resize(size);
        term.advance_bytes(b"\x1b[?2048$p");
        assert_eq!(receive(&mut out), b"\x1b[?2048;2$y");
        for reset in [
            |t: &mut Terminal| t.advance_bytes(b"\x1bc"),
            |t: &mut Terminal| t.clear_resize_subscription(),
            |t: &mut Terminal| t.reset_connection(),
        ] {
            term.advance_bytes(b"\x1b[?2048h");
            assert_eq!(receive(&mut out), b"\x1b[48;24;80;0;0t");
            reset(&mut term);
            term.resize(TerminalSize { cols: 60, ..size });
            term.resize(size);
            term.advance_bytes(b"\x1b[?2048$p");
            assert_eq!(receive(&mut out), b"\x1b[?2048;2$y");
        }
    }

    #[test]
    fn in_band_resize_uses_the_automatic_reply_channel_on_a_framed_viewer() {
        let (mut term, _ordinary) = engine();
        let (tx, mut viewer) = mpsc::unbounded_channel();
        enter_herdr_viewer(&mut term, &tx, &crate::events::Events::default());
        term.advance_bytes(b"\x1b[?2048h");
        term.resize(TerminalSize::default());
        for expected in [b"\x1b[48;24;80;480;800t".as_slice(), b"\x1b[48;24;80;0;0t"] {
            match viewer.try_recv().unwrap() {
                TransportInput::Automatic(bytes) => assert_eq!(bytes, expected),
                _ => panic!("a size report was routed as user input"),
            }
        }
        assert!(viewer.try_recv().is_err());
    }

    #[test]
    fn reverse_screen_reports_state_across_screen_switches_and_resets() {
        let (mut terminal, mut replies) = engine();
        for (request, answer) in [
            ("", 2),
            ("\x1b[?5h", 1),
            ("\x1b[?5h", 1),
            ("\x1b[?1049h", 1),
            ("\x1b[?1049l", 1),
            ("\x1b[?5l", 2),
            ("\x1b[?5h\x1bc", 2),
        ] {
            terminal.advance_bytes(request.as_bytes());
            terminal.advance_bytes(b"\x1b[?5$p");
            assert_eq!(
                receive(&mut replies),
                format!("\x1b[?5;{answer}$y").as_bytes()
            );
        }
        terminal.advance_bytes(b"\x1b[?5h");
        terminal.reset_connection();
        terminal.advance_bytes(b"\x1b[?5$p");
        assert_eq!(receive(&mut replies), b"\x1b[?5;2$y");
    }

    #[test]
    fn reverse_screen_preserves_the_dynamic_colors_reported_to_applications() {
        let (mut terminal, mut replies) = engine();
        let query = b"\x1b]10;?\x07";
        terminal.advance_bytes(b"\x1b]10;#13579b\x07\x1b]11;#421735\x07");
        terminal.advance_bytes(query);
        let normal_reply = receive(&mut replies);
        terminal.advance_bytes(b"\x1b[?5h");
        terminal.advance_bytes(query);
        assert_eq!(receive(&mut replies), normal_reply);
        terminal.advance_bytes(b"\x1b]10;#eeddaa\x07");
        let palette = terminal.palette();
        let capture = |term: &Terminal| Snapshot::capture(term, None, 0, 0, String::new(), 0);
        assert_eq!(capture(&terminal).palette.background, palette.foreground);
        assert_eq!(capture(&terminal).palette.foreground, palette.background);
        terminal.advance_bytes(b"\x1b[?5l");
        assert_eq!(capture(&terminal).palette, palette);
    }

    #[test]
    fn cursor_blink_mode_preserves_shape_and_reports_the_latest_request() {
        use termwiz::surface::CursorShape::*;
        let (mut terminal, mut replies) = engine();
        for (request, expected, answer) in [
            ("", Default, 2),
            ("\x1b[?12h", BlinkingBlock, 1),
            ("\x1b[?12l", SteadyBlock, 2),
            ("\x1b[4 q\x1b[?12h", BlinkingUnderline, 1),
            ("\x1b[?12l", SteadyUnderline, 2),
            ("\x1b[5 q", BlinkingBar, 1),
            ("\x1b[?12l", SteadyBar, 2),
            ("\x1b[?12h", BlinkingBar, 1),
            ("\x1b[2 q", SteadyBlock, 2),
            ("\x1b[1 q\x1b[?25l", BlinkingBlock, 1),
            ("\x1b[0 q", Default, 2),
            ("\x1b[?12h\x1bc", Default, 2),
        ] {
            terminal.advance_bytes(request.as_bytes());
            terminal.advance_bytes(b"\x1b[?12$p");
            assert_eq!(terminal.cursor_pos().shape, expected);
            assert_eq!(
                receive(&mut replies),
                format!("\x1b[?12;{answer}$y").as_bytes()
            );
        }
        terminal.advance_bytes(b"\x1b[?12h");
        terminal.reset_connection();
        terminal.advance_bytes(b"\x1b[?12$p");
        assert_eq!(receive(&mut replies), b"\x1b[?12;2$y");
    }

    #[test]
    fn default_cursor_shape_is_inherited_by_cursor_reset_but_respects_application_overrides() {
        use termwiz::surface::CursorShape::*;
        let (mut terminal, mut replies) = engine();
        assert!(terminal.set_default_cursor_shape(SteadyBar));
        assert_eq!(terminal.cursor_pos().shape, SteadyBar);
        terminal.advance_bytes(b"\x1b[?12h");
        assert_eq!(terminal.cursor_pos().shape, BlinkingBar);
        terminal.advance_bytes(b"\x1b[?12l");
        assert_eq!(terminal.cursor_pos().shape, SteadyBar);

        terminal.advance_bytes(b"\x1b[2 q");
        assert!(!terminal.set_default_cursor_shape(BlinkingUnderline));
        assert_eq!(terminal.cursor_pos().shape, SteadyBlock);
        terminal.advance_bytes(b"\x1b[0 q\x1b[?12$p");
        assert_eq!(terminal.cursor_pos().shape, BlinkingUnderline);
        assert_eq!(receive(&mut replies), b"\x1b[?12;1$y");

        terminal.advance_bytes(b"\x1b7\x1b[6 q\x1b8");
        assert_eq!(terminal.cursor_pos().shape, BlinkingUnderline);
        terminal.advance_bytes(b"\x1b[2 q\x1bc");
        assert_eq!(terminal.cursor_pos().shape, BlinkingUnderline);
        terminal.advance_bytes(b"\x1b[6 q");
        terminal.reset_connection();
        assert_eq!(terminal.cursor_pos().shape, BlinkingUnderline);
    }

    #[test]
    fn iterm_cursor_guide_requests_update_the_snapshot_and_reset_with_the_session() {
        let (mut terminal, mut replies) = engine();
        let capture = |terminal: &Terminal, old: Option<&Snapshot>| {
            Snapshot::capture(terminal, old, 1, 0, String::new(), 0)
        };
        let before = capture(&terminal, None);
        assert!(!before.application_cursor_guide);
        terminal.advance_bytes(b"\x1b]1337;HighlightCursorLine=yes\x07");
        let enabled = capture(&terminal, Some(&before));
        assert!(enabled.application_cursor_guide);
        assert!(!enabled.same_presentation(&before));
        terminal.advance_bytes(b"\x1b]1337;HighlightCursorLine=invalid\x07");
        assert!(capture(&terminal, Some(&enabled)).same_presentation(&enabled));
        terminal.advance_bytes(b"\x1b]1337;HighlightCursorLine=no\x1b\\");
        assert!(!capture(&terminal, None).application_cursor_guide);
        terminal.advance_bytes(b"\x1b]1337;HighlightCursorLine=yes\x07\x1bc");
        assert!(!capture(&terminal, None).application_cursor_guide);
        terminal.advance_bytes(b"\x1b]1337;HighlightCursorLine=yes\x07");
        terminal.reset_connection();
        assert!(!capture(&terminal, None).application_cursor_guide);
        assert!(replies.try_recv().is_err());
    }
    fn text(t: &Terminal) -> String {
        let s = t.screen();
        s.lines_in_phys_range(s.phys_row(0)..s.phys_row(0) + s.physical_rows)
            .iter()
            .map(|l| l.as_str().trim_end().to_owned())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn dec_selective_erase_keeps_fields_and_uses_the_requested_line_or_display_extent() {
        let (mut term, _) = engine();
        term.advance_bytes(b"a\x1b[1\"qA\x1b[0mB\x1b[2\"qc\r\n");
        term.advance_bytes(b"d\x1b[1\"qCD\x1b[\"qe\r\nplain");
        term.advance_bytes(b"\x1b[2;2H\x1b[?1J");
        assert!(text(&term).starts_with(" AB\n CDe\nplain\n"));
        term.advance_bytes(b"\x1b[2;4H\x1b[?K");
        assert!(text(&term).starts_with(" AB\n CD\nplain\n"));
        term.advance_bytes(b"\x1b[2;1H\x1b[?J");
        assert!(text(&term).starts_with(" AB\n CD\n\n"));
        term.advance_bytes(b"\x1b[1;1HX\x1b[1;1H\x1b[?1K");
        assert!(text(&term).starts_with(" AB\n CD\n\n"));
        term.advance_bytes(b"\x1b[2K\x1b[?2J");
        assert!(text(&term).starts_with("\n CD\n\n"));
    }

    #[test]
    fn dec_selective_rectangle_preserves_wide_protected_glyphs_and_cell_rendition() {
        let (mut term, mut replies) = engine();
        term.advance_bytes("\x1b#6\x1b[1\"q界\x1b[0\"q\x1b[4:3;44m語\x1b[0mZ");
        term.advance_bytes(b"\x1b[1;2;1;2${\x1b[1;4;1;4${\x1b[6n");
        assert_eq!(receive(&mut replies), b"\x1b[1;6R");
        let frame = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
        let row = &frame.rows[0];
        assert!(row.is_double_width());
        assert_eq!(row.as_str().trim_end(), "界  Z");
        assert!(row.get_cell(0).unwrap().attrs().protected());
        for col in 2..4 {
            let cell = row.get_cell(col).unwrap();
            assert_eq!(cell.attrs().underline(), wezterm_term::Underline::Curly);
            assert_eq!(
                cell.attrs().background(),
                wezterm_term::color::ColorAttribute::PaletteIndex(4)
            );
        }
    }

    #[test]
    fn dec_character_protection_is_saved_with_the_cursor_and_has_a_status_reply() {
        let (mut term, mut replies) = engine();
        term.advance_bytes(b"\x1b[1\"q\x1b7\x1b[2\"q\x1bP$q\"q\x1b\\");
        assert_eq!(receive(&mut replies), b"\x1bP1$r0\"q\x1b\\");
        term.advance_bytes(b"\x1b8\x1b[9\"q\x1bP$q\"q\x1b\\");
        assert_eq!(receive(&mut replies), b"\x1bP1$r1\"q\x1b\\");
        // Ordinary erasure inserts unprotected blanks even while the pen is protected.
        term.advance_bytes(b"AB\r\x1b[X");
        let frame = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
        assert!(!frame.rows[0].get_cell(0).unwrap().attrs().protected());
        assert!(frame.rows[0].get_cell(1).unwrap().attrs().protected());
        term.advance_bytes(b"\x1b[!p\x1bP$q\"q\x1b\\");
        assert_eq!(receive(&mut replies), b"\x1bP1$r0\"q\x1b\\");
    }

    #[test]
    fn dec_selective_protection_follows_the_wrapped_cells_between_screen_edits() {
        let (tx, _) = mpsc::unbounded_channel();
        let mut term = create(
            TerminalSize {
                cols: 8,
                rows: 5,
                ..Default::default()
            },
            tx,
        );
        term.advance_bytes(b"..\x1b[1\"qprotected\x1b[0\"q..");
        let size = TerminalSize {
            cols: 16,
            ..term.get_size()
        };
        term.resize(size);
        term.advance_bytes(b"\x1b[?1049hdiscard\x1b[?2J\x1b[?1049l");
        term.advance_bytes(b"\x1b[1;1H\x1b[@\x1b[?2J");
        assert!(text(&term).starts_with("   protected\n"));
    }

    #[test]
    fn dec_rectangle_fill_uses_the_pen_and_ordinary_erase_clears_protection() {
        let (mut term, mut replies) = engine();
        term.advance_bytes(b"ABCDEF\r\nabcdef\r\n012345\x1b[4;9H");
        term.advance_bytes(b"\x1b[1;4:3;44m\x1b[1\"q\x1b[42;1;2;2;5$x");
        term.advance_bytes(b"\x1b[2;3;3;4$z\x1b[6n");
        assert_eq!(receive(&mut replies), b"\x1b[4;9R");
        assert!(text(&term).starts_with("A****F\na*  *f\n01  45\n"));

        let frame = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
        let fill = frame.rows[1].get_cell(1).unwrap();
        assert!(fill.attrs().protected());
        assert_eq!(fill.attrs().underline(), wezterm_term::Underline::Curly);
        assert_eq!(
            fill.attrs().background(),
            wezterm_term::color::ColorAttribute::PaletteIndex(4)
        );
        let blank = frame.rows[1].get_cell(2).unwrap();
        assert!(!blank.attrs().protected());
        assert_eq!(blank.attrs().underline(), wezterm_term::Underline::None);
        assert_eq!(
            blank.attrs().background(),
            wezterm_term::color::ColorAttribute::Default
        );
        // The active protection setting still applies to text after the edit.
        term.advance_bytes(b"Z\x1b[?2J");
        assert!(text(&term).starts_with(" ****\n *  *\n\n        Z\n"));
    }

    #[test]
    fn dec_rectangle_edit_coordinates_follow_origin_and_handle_invalid_or_default_borders() {
        let (tx, _) = mpsc::unbounded_channel();
        let mut term = create(
            TerminalSize {
                cols: 6,
                rows: 3,
                ..Default::default()
            },
            tx,
        );
        term.advance_bytes(b"ABCDEF\r\nabcdef\r\n012345");
        term.advance_bytes(b"\x1b[?69h\x1b[2;5s\x1b[2;3r\x1b[?6h\x1b[46;1;1;99;99$x");
        assert_eq!(text(&term), "ABCDEF\na....f\n0....5");
        term.advance_bytes(b"\x1b[?6l\x1b[100;100;99;99$z\x1b[127;1;1;3;6$x");
        assert_eq!(text(&term), "ABCDEF\na....f\n0....5");
        term.advance_bytes(b"\x1b[2;5$z");
        assert_eq!(text(&term), "ABCDEF\na...\n0...");
    }

    #[test]
    fn dec_rectangle_fill_translates_graphic_characters_and_keeps_line_rendition() {
        let (mut term, _) = engine();
        term.advance_bytes("\x1b#6A界Z\x1b(0\x1b[113;1;3;1;3$x\x1b(B\x1b[163;1;5;1;5$x");
        let frame = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
        assert_eq!(frame.rows[0].as_str().trim_end(), "A ─Z£");
        assert!(frame.rows[0].is_double_width());
        for col in [2, 4] {
            assert_eq!(frame.rows[0].get_cell(col).unwrap().width(), 1);
        }
    }

    #[test]
    fn dec_rectangle_copy_reads_overlapping_rows_and_columns_before_overwriting() {
        let (mut term, mut replies) = engine();
        term.advance_bytes(b"ABCDEFGHIJ\r\n0123456789\r\nxxxxxxxxxx\x1b[4;8H");
        term.advance_bytes(b"\x1b[1;2;2;5;1;2;4;1$v");
        assert!(text(&term).starts_with("ABCDEFGHIJ\n012BCDE789\nxxx1234xxx\n"));
        term.advance_bytes(b"\x1b[2;4;3;7;1;1;1;1$v\x1b[1;1;1;4;1;1;3;1$v\x1b[6n");
        assert!(text(&term).starts_with("BCBCDEGHIJ\n1234CDE789\nxxx1234xxx\n"));
        assert_eq!(receive(&mut replies), b"\x1b[4;8R");
    }

    #[test]
    fn dec_rectangle_copy_keeps_cell_style_and_clips_wide_graphemes_at_either_edge() {
        let (tx, _) = mpsc::unbounded_channel();
        let mut term = create(
            TerminalSize {
                cols: 8,
                rows: 5,
                ..Default::default()
            },
            tx,
        );
        term.advance_bytes("A\x1b[1\"q\x1b[44m界\x1b[0m\x1b[0\"qB\x1b[2;1H\x1b#6zzzz");
        term.advance_bytes(b"\x1b[1;2;1;3;1;2;3;1$v\x1b[1;2;1;3;1;3;8;1$v\x1b[1;3;1;4;1;4;1;1$v");
        let frame = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
        assert!(frame.rows[1].is_double_width());
        assert_eq!(frame.rows[1].as_str().trim_end(), "zz界");
        assert_eq!(frame.rows[2].as_str().trim_end(), "");
        assert_eq!(frame.rows[3].as_str().trim_end(), " B");
        for (row, col) in [(1, 2), (2, 7), (3, 0)] {
            let cell = frame.rows[row].get_cell(col).unwrap();
            assert!(cell.attrs().protected());
            assert_eq!(
                cell.attrs().background(),
                wezterm_term::color::ColorAttribute::PaletteIndex(4)
            );
        }
    }

    #[test]
    fn dec_rectangle_copy_uses_origin_defaults_and_the_active_page() {
        let (tx, _) = mpsc::unbounded_channel();
        let mut term = create(
            TerminalSize {
                cols: 6,
                rows: 4,
                ..Default::default()
            },
            tx,
        );
        term.advance_bytes(b"primary\x1b[?1049hABCDEF\r\nabcdef\r\n012345\r\nuvwxyz");
        term.advance_bytes(b"\x1b[?69h\x1b[2;5s\x1b[2;3r\x1b[?6h");
        // Pages above the single active page clamp to page 1. Zero and omitted
        // parameters take their usual defaults.
        term.advance_bytes(b"\x1b[0;0;1;;7;2;2;5$v");
        assert_eq!(text(&term), "ABCDEF\nabcdef\n01bcd5\nuvwxyz");
        term.advance_bytes(b"\x1b[1;1;2;4;1;9;9;1$v\x1b[2;3;1;1;1;1;1;1$v");
        assert_eq!(text(&term), "ABCDEF\nabcdef\n01bcd5\nuvwxyz");
        term.advance_bytes(b"\x1b[?1049l");
        assert!(text(&term).starts_with("primar\ny\n"));
    }

    #[test]
    fn dec_range_attributes_follow_stream_then_rectangle_without_changing_the_pen() {
        let (mut term, _) = engine();
        term.advance_bytes(b"ABCDEF\r\nabcdef\r\n012345\x1b[4;8H\x1b[3;31m");
        term.advance_bytes(b"\x1b[1;4;3;2;1;4$r");
        let first = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
        for (row, styled) in [(0, 3..6), (1, 0..6), (2, 0..2)] {
            for col in 0..6 {
                let cell = first.rows[row].get_cell(col).unwrap();
                assert_eq!(
                    cell.attrs().intensity() == wezterm_term::Intensity::Bold,
                    styled.contains(&col)
                );
                assert_eq!(
                    cell.attrs().underline() == wezterm_term::Underline::Single,
                    styled.contains(&col)
                );
            }
        }
        term.advance_bytes(b"\x1b[2*x\x1b[2;2;3;4;1;7$t");
        let toggled = Snapshot::capture(&term, Some(&first), 2, 0, String::new(), 0);
        for row in [1, 2] {
            for col in 0..6 {
                let before = first.rows[row].get_cell(col).unwrap();
                let after = toggled.rows[row].get_cell(col).unwrap();
                let selected = (1..4).contains(&col);
                assert_eq!(after.attrs().reverse(), selected);
                assert_eq!(
                    after.attrs().intensity() == wezterm_term::Intensity::Bold,
                    (before.attrs().intensity() == wezterm_term::Intensity::Bold) ^ selected
                );
                assert_eq!(after.attrs().underline(), before.attrs().underline());
            }
        }
        assert!(text(&term).starts_with("ABCDEF\nabcdef\n012345\n"));
        assert_eq!((term.cursor_pos().x, term.cursor_pos().y), (7, 3));
        assert!(term.pen().italic());
        assert_eq!(
            term.pen().foreground(),
            wezterm_term::color::ColorAttribute::PaletteIndex(1)
        );
    }

    #[test]
    fn dec_rectangle_attributes_restyle_complete_glyphs_and_extend_into_blanks() {
        let (mut term, _) = engine();
        term.advance_bytes("\x1b[1\"q\x1b[44;4:3m界\x1b[0m\x1b[0\"q!");
        term.advance_bytes(b"\x1b[1;4;1;5;4$r");
        let stream = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
        assert!(stream.rows[0].get_cell(4).is_none());
        term.advance_bytes(b"\x1b[2*x\x1b[1;2;1;5;0;4;7$r\x1b[1;2;1;5;7;7$t");
        let rect = Snapshot::capture(&term, Some(&stream), 2, 0, String::new(), 0);
        let wide = rect.rows[0].get_cell(0).unwrap();
        assert_eq!(wide.str(), "界");
        assert_eq!(wide.width(), 2);
        assert!(wide.attrs().protected());
        assert_eq!(
            wide.attrs().background(),
            wezterm_term::color::ColorAttribute::PaletteIndex(4)
        );
        for col in [0, 2, 3, 4] {
            let cell = rect.rows[0].get_cell(col).unwrap();
            assert_eq!(cell.attrs().underline(), wezterm_term::Underline::Single);
            assert!(cell.attrs().reverse());
        }
    }

    #[test]
    fn dec_attribute_extent_is_queryable_and_resettable() {
        let (mut term, mut replies) = engine();
        for (command, extent) in [
            ("", 0),
            ("\x1b[2*x\x1b[9*x", 2),
            ("\x1b[1*x", 1),
            ("\x1b[2*x\x1b[!p", 0),
            ("\x1b[2*x\x1bc", 0),
        ] {
            term.advance_bytes(format!("{command}\x1bP$q*x\x1b\\"));
            assert_eq!(
                receive(&mut replies),
                format!("\x1bP1$r{extent}*x\x1b\\").as_bytes()
            );
        }
    }

    #[test]
    fn dec_column_edits_shift_the_scrolling_region_and_keep_the_cursor() {
        let (tx, mut replies) = mpsc::unbounded_channel();
        let mut term = create(
            TerminalSize {
                cols: 8,
                rows: 5,
                ..Default::default()
            },
            tx,
        );
        term.advance_bytes(b"ABCDEFGH\r\nabcdefgh\r\n12345678\r\nijklmnop\r\nIJKLMNOP");
        term.advance_bytes(b"\x1b[?69h\x1b[2;7s\x1b[2;4r\x1b[3;3H\x1b[44m\x1b[2'}");
        assert_eq!(
            text(&term),
            "ABCDEFGH\nab  cdeh\n12  3458\nij  klmp\nIJKLMNOP"
        );
        term.advance_bytes(b"\x1b[3'~\x1b[6n");
        assert_eq!(
            text(&term),
            "ABCDEFGH\nabde   h\n1245   8\nijlm   p\nIJKLMNOP"
        );
        assert_eq!(receive(&mut replies), b"\x1b[3;3R");
        let frame = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
        assert_eq!(
            frame.rows[1].get_cell(4).unwrap().attrs().background(),
            wezterm_term::color::ColorAttribute::Default
        );
        // A cursor above the top margin or to the left of the region has no effect.
        term.advance_bytes(b"\x1b[1;3H\x1b['}\x1b[3;1H\x1b['~");
        assert_eq!(
            text(&term),
            "ABCDEFGH\nabde   h\n1245   8\nijlm   p\nIJKLMNOP"
        );
        term.advance_bytes(b"\x1b[3;3H\x1b[4294967295'~");
        assert_eq!(
            text(&term),
            "ABCDEFGH\nab     h\n12     8\nij     p\nIJKLMNOP"
        );
    }

    #[test]
    fn dec_column_edits_clip_wide_glyphs_and_respect_each_rows_rendition() {
        let (tx, _) = mpsc::unbounded_channel();
        let mut term = create(
            TerminalSize {
                cols: 8,
                rows: 3,
                ..Default::default()
            },
            tx,
        );
        term.advance_bytes("A界B語Z\x1b[2;1H\x1b#6m界n");
        term.advance_bytes(b"\x1b[1;3H\x1b[0'}");
        let frame = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
        assert_eq!(frame.rows[0].as_str().trim_end(), "A   B語Z");
        assert_eq!(frame.rows[1].as_str().trim_end(), "m");
        assert!(frame.rows[1].is_double_width());
        assert_eq!(frame.rows[0].get_cell(5).unwrap().width(), 2);
    }

    #[test]
    fn horizontal_scroll_starts_at_the_left_margin_and_preserves_the_cursor() {
        let (tx, mut replies) = mpsc::unbounded_channel();
        let mut term = create(
            TerminalSize {
                cols: 8,
                rows: 4,
                ..Default::default()
            },
            tx,
        );
        term.advance_bytes(b"ABCDEFGH\r\nabcdefgh\r\n12345678\r\nIJKLMNOP");
        term.advance_bytes(b"\x1b[?69h\x1b[3;6s\x1b[2;3r\x1b[3;5H\x1b[2 @\x1b[6n");
        assert_eq!(text(&term), "ABCDEFGH\nabef  gh\n1256  78\nIJKLMNOP");
        assert_eq!(receive(&mut replies), b"\x1b[3;5R");
        term.advance_bytes(b"\x1b[ A\x1b[6n");
        assert_eq!(text(&term), "ABCDEFGH\nab ef gh\n12 56 78\nIJKLMNOP");
        assert_eq!(receive(&mut replies), b"\x1b[3;5R");
        term.advance_bytes(b"\x1b[0 @");
        assert_eq!(text(&term), "ABCDEFGH\nabef  gh\n1256  78\nIJKLMNOP");
        term.advance_bytes(b"\x1b[1;5H\x1b[ A\x1b[3;1H\x1b[ @");
        assert_eq!(text(&term), "ABCDEFGH\nabef  gh\n1256  78\nIJKLMNOP");
        term.advance_bytes(b"\x1b[3;5H\x1b[1;2 A");
        assert_eq!(text(&term), "ABCDEFGH\nabef  gh\n1256  78\nIJKLMNOP");
        term.advance_bytes(b"\x1b[4294967295 A");
        assert_eq!(text(&term), "ABCDEFGH\nab    gh\n12    78\nIJKLMNOP");
    }

    #[test]
    fn dec_horizontal_index_moves_the_cursor_and_scrolls_at_horizontal_margins() {
        let (tx, mut replies) = mpsc::unbounded_channel();
        let mut term = create(
            TerminalSize {
                cols: 8,
                rows: 4,
                ..Default::default()
            },
            tx,
        );
        term.advance_bytes(b"ABCDEFGH\r\nabcdefgh\r\n12345678\r\nIJKLMNOP");
        term.advance_bytes(b"\x1b[?69h\x1b[3;6s\x1b[2;3r\x1b[2;3H\x1b6\x1b[6n");
        assert_eq!(text(&term), "ABCDEFGH\nab cdegh\n12 34578\nIJKLMNOP");
        assert_eq!(receive(&mut replies), b"\x1b[2;3R");
        term.advance_bytes(b"\x1b[3;6H\x1b9\x1b[6n");
        assert_eq!(text(&term), "ABCDEFGH\nabcde gh\n12345 78\nIJKLMNOP");
        assert_eq!(receive(&mut replies), b"\x1b[3;6R");
        for (bytes, position) in [
            (b"\x1b[2;4H\x1b9\x1b[6n".as_slice(), b"\x1b[2;5R".as_slice()),
            (b"\x1b6\x1b[6n", b"\x1b[2;4R"),
            (b"\x1b[1;2H\x1b9\x1b6\x1b[6n", b"\x1b[1;3R"),
            (b"\x1b[3;7H\x1b9\x1b9\x1b[6n", b"\x1b[3;8R"),
        ] {
            term.advance_bytes(bytes);
            assert_eq!(receive(&mut replies), position);
        }
        assert_eq!(text(&term), "ABCDEFGH\nabcde gh\n12345 78\nIJKLMNOP");
    }

    #[test]
    fn dec_forward_index_honors_double_width_and_clears_pending_wrap() {
        let (tx, _) = mpsc::unbounded_channel();
        let mut term = create(
            TerminalSize {
                cols: 8,
                rows: 3,
                ..Default::default()
            },
            tx,
        );
        term.advance_bytes(b"\x1b[2;1Habcdefgh\x1b[1;1H\x1b#61234\x1b9Z");
        assert_eq!(text(&term), "234Z\nbcdefgh\n");
        assert_eq!((term.cursor_pos().x, term.cursor_pos().y), (3, 0));
        assert!(Snapshot::capture(&term, None, 1, 0, String::new(), 0).rows[0].is_double_width());
    }

    #[test]
    fn dec_line_width_controls_cursor_addressing_and_auto_wrap() {
        let (tx, mut replies) = mpsc::unbounded_channel();
        let mut term = create(
            TerminalSize {
                cols: 11,
                rows: 4,
                ..Default::default()
            },
            tx,
        );

        // The full-width row contains a wide grapheme straddling the new edge.
        term.advance_bytes("ABCD界GHIJK\x1b#6\x1b[6n");
        let doubled = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
        assert_eq!(doubled.rows[0].as_str().trim_end(), "ABCD");
        assert!(doubled.rows[0].is_double_width());
        assert_eq!(doubled.cell_map(0).unwrap().logical_cols(), 5);
        assert_eq!(doubled.visual_cursor_column(), 8);
        assert_eq!(receive(&mut replies), b"\x1b[1;5R");

        term.advance_bytes(b"\r123456\x1b[11G\x1b[A\x1b[6n\r\t\x1b[6n");
        assert_eq!(receive(&mut replies), b"\x1b[1;5R");
        assert_eq!(receive(&mut replies), b"\x1b[1;5R");
        let wrapped = Snapshot::capture(&term, Some(&doubled), 2, 0, String::new(), 0);
        assert_eq!(wrapped.rows[0].as_str().trim_end(), "12345");
        assert_eq!(wrapped.rows[1].as_str().trim_end(), "6");

        term.advance_bytes(b"\x1b#5\x1b[1;11HX");
        let restored = Snapshot::capture(&term, Some(&wrapped), 3, 0, String::new(), 0);
        assert_eq!(restored.rows[0].as_str().trim_end(), "12345     X");
        assert!(!restored.rows[0].is_double_width());
        assert_eq!(restored.cell_map(0).unwrap().logical_cols(), 11);

        term.advance_bytes("\x1b[3;1H\x1b#6abc界\x1b[3;1H\x1b[@\x1b#5");
        let edited = Snapshot::capture(&term, None, 4, 0, String::new(), 0);
        assert_eq!(edited.rows[2].as_str().trim_end(), " abc");
    }

    #[test]
    fn dec_line_renditions_reset_while_horizontal_margin_mode_is_enabled() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut term = create(
            TerminalSize {
                cols: 10,
                rows: 4,
                ..Default::default()
            },
            tx,
        );
        term.advance_bytes(b"\x1b#312345\r\n\x1b#412345");
        let pair = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
        assert!(pair.rows[0].is_double_height_top());
        assert!(pair.rows[1].is_double_height_bottom());

        term.advance_bytes(b"\x1b[?69hZ\x1b#6\x1b[3;1H\x1b#3\x1b[4;1H\x1b#4");
        let margins = Snapshot::capture(&term, Some(&pair), 2, 0, String::new(), 0);
        assert!(margins.cell_maps().iter().all(|map| map.width_scale() == 1));
        assert_eq!(margins.rows[0].as_str().trim_end(), "12345");
        assert_eq!(margins.rows[1].as_str().trim_end(), "1234Z");
        assert!(margins.rows[2].as_str().trim_end().is_empty());

        term.advance_bytes(b"\x1b[?69l\x1b#6");
        let normal = Snapshot::capture(&term, Some(&margins), 3, 0, String::new(), 0);
        assert!(normal.rows[3].is_double_width());
    }

    #[test]
    fn dec_line_halves_survive_resize_and_keep_their_own_text() {
        for alternate in [false, true] {
            let size = TerminalSize {
                cols: 12,
                rows: 6,
                ..Default::default()
            };
            let (tx, _rx) = mpsc::unbounded_channel();
            let mut term = create(size, tx);
            if alternate {
                term.advance_bytes(b"\x1b[?1049h");
            }
            term.advance_bytes(b"abcdefghijkl_\x1b[2;1H\x1b#3TopABC\r\n\x1b#4BotDEF");
            term.resize(TerminalSize {
                cols: 8,
                rows: 8,
                ..size
            });
            let screen = term.screen();
            let lines: Vec<_> = screen.physical_lines().collect();
            let top = lines
                .iter()
                .position(|line| line.is_double_height_top())
                .unwrap();
            assert!(lines[top + 1].is_double_height_bottom());
            assert_eq!(lines[top].as_str().trim_end(), "TopA");
            assert_eq!(lines[top + 1].as_str().trim_end(), "BotD");
            assert_eq!(term.cursor_pos().x, 3);

            term.advance_bytes(b"\x1b[S");
            let visible = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
            let top = visible
                .rows
                .iter()
                .position(|line| line.is_double_height_top())
                .unwrap();
            assert!(visible.rows[top + 1].is_double_height_bottom());
        }
    }

    #[test]
    fn cursor_style_status_string_round_trips_every_shape_and_tracks_mode_12() {
        let (mut terminal, mut replies) = engine();
        let query = b"\x1bP$q q\x1b\\";
        terminal.advance_bytes(query);
        assert_eq!(receive(&mut replies), b"\x1bP1$r0 q\x1b\\");
        for selector in 0..=6 {
            terminal.advance_bytes(format!("\x1b[{selector} q"));
            for &byte in query {
                terminal.advance_bytes([byte]);
            }
            assert_eq!(
                receive(&mut replies),
                format!("\x1bP1$r{selector} q\x1b\\").as_bytes()
            );
        }
        for (mode, expected) in [("h", 5), ("l", 6)] {
            terminal.advance_bytes(format!("\x1b[?12{mode}"));
            terminal.advance_bytes(query);
            assert_eq!(
                receive(&mut replies),
                format!("\x1bP1$r{expected} q\x1b\\").as_bytes()
            );
        }
        terminal.advance_bytes(b"\x1bP$q\"p\x1b\\");
        assert_eq!(receive(&mut replies), b"\x1bP1$r65;1\"p\x1b\\");
        terminal.advance_bytes(b"\x1bP$qunknown\x1b\\");
        assert_eq!(receive(&mut replies), b"\x1bP0$r\x1b\\");
    }

    #[test]
    fn pixel_mouse_leave_clears_motion_coalescing_and_keeps_the_button_held() {
        use wezterm_term::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let (mut terminal, mut replies) = engine();
        terminal.advance_bytes(b"\x1b[?1003h\x1b[?1006h");
        terminal.mouse_left(KeyModifiers::NONE).unwrap();
        assert!(replies.try_recv().is_err());
        terminal.advance_bytes(b"\x1b[?1016h");

        let motion = MouseEvent {
            kind: MouseEventKind::Move,
            x: 0,
            y: 0,
            x_pixel_offset: 2,
            y_pixel_offset: 3,
            button: MouseButton::None,
            modifiers: KeyModifiers::NONE,
        };
        terminal.mouse_event(motion).unwrap();
        assert_eq!(receive(&mut replies), b"\x1b[<35;3;4M");
        terminal.mouse_event(motion).unwrap();
        assert!(replies.try_recv().is_err());
        terminal
            .mouse_left(KeyModifiers::CTRL | KeyModifiers::ALT)
            .unwrap();
        assert_eq!(receive(&mut replies), b"\x1b[<312;1;1M");
        terminal.mouse_event(motion).unwrap();
        assert_eq!(receive(&mut replies), b"\x1b[<35;3;4M");

        let press = MouseEvent {
            kind: MouseEventKind::Press,
            button: MouseButton::Left,
            ..motion
        };
        terminal.mouse_event(press).unwrap();
        assert_eq!(receive(&mut replies), b"\x1b[<0;3;4M");
        terminal.mouse_left(KeyModifiers::NONE).unwrap();
        assert_eq!(receive(&mut replies), b"\x1b[<288;1;1M");
        terminal.mouse_event(motion).unwrap();
        assert_eq!(receive(&mut replies), b"\x1b[<32;3;4M");
        terminal.advance_bytes(b"\x1b[?1016l");
        terminal.mouse_left(KeyModifiers::NONE).unwrap();
        assert!(replies.try_recv().is_err());
    }

    #[test]
    fn tab_stop_controls_edit_the_shared_table_and_restore_defaults() {
        let (mut terminal, mut replies) = engine();
        let size = terminal.get_size();
        terminal.resize(TerminalSize {
            cols: 32,
            rows: 4,
            ..size
        });
        terminal.advance_bytes(b"\x1bP2$t3/11/29\x1b\\\x1b[2;6H\x1b[W\x1b[2$w");
        assert_eq!(receive(&mut replies), b"\x1bP2$u3/6/11/29\x1b\\");
        assert_eq!((terminal.cursor_pos().x, terminal.cursor_pos().y), (5, 1));
        terminal.advance_bytes(b"\x1b[2W\x1b[2$w");
        assert_eq!(receive(&mut replies), b"\x1bP2$u3/11/29\x1b\\");

        terminal.advance_bytes(b"\x1b[5;1;0W\x1b[2$w");
        assert_eq!(receive(&mut replies), b"\x1bP2$u6\x1b\\");
        terminal.advance_bytes(b"\x1b[5;99W\x1b[2$w");
        assert_eq!(receive(&mut replies), b"\x1bP2$u6\x1b\\");
        terminal.advance_bytes(b"\x1b[9G\x1b[5;W\x1b[2$w");
        assert_eq!(receive(&mut replies), b"\x1bP2$u9\x1b\\");
        terminal.advance_bytes(b"\x1b[4W\x1b[2$w");
        assert_eq!(receive(&mut replies), b"\x1bP2$u\x1b\\");
        terminal.advance_bytes(b"\x1b[0W\x1b[5g\x1b[2$w");
        assert_eq!(receive(&mut replies), b"\x1bP2$u\x1b\\");

        terminal.advance_bytes(b"\x1bP2$t3/27\x1b\\");
        terminal.resize(TerminalSize {
            cols: 7,
            rows: 4,
            ..size
        });
        terminal.advance_bytes(b"\x1b[?5W\x1b[2$w");
        assert_eq!(receive(&mut replies), b"\x1bP2$u\x1b\\");
        terminal.resize(TerminalSize {
            cols: 33,
            rows: 4,
            ..size
        });
        terminal.advance_bytes(b"\x1b[2$w");
        assert_eq!(receive(&mut replies), b"\x1bP2$u9/17/25/33\x1b\\");
    }

    #[test]
    fn tab_stop_moves_are_bounded_by_the_line_and_origin_margins() {
        let (mut terminal, _) = engine();
        let size = terminal.get_size();
        terminal.resize(TerminalSize {
            cols: 32,
            rows: 4,
            ..size
        });
        terminal.advance_bytes(b"\x1bP2$t3/7/11/17/25\x1b\\\x1b[?69h\x1b[5;19s\x1b[?6h");
        for (control, column) in [
            (b"\x1b[2I".as_slice(), 11),
            (b"\x1b[Z", 7),
            (b"\x1b[Z", 5),
            (b"\x1b[4294967295I", 19),
            (b"\x1b[4294967295Z", 5),
        ] {
            terminal.advance_bytes(control);
            assert_eq!(terminal.cursor_pos().x, column - 1);
            assert_eq!(terminal.cursor_pos().y, 0);
        }
        terminal.advance_bytes(b"\x1b[?6l\x1b[?69l\x1b#6\x1b[4294967295I");
        assert_eq!(terminal.cursor_pos().x, 15);
        terminal.advance_bytes(b"\x1b[4294967295Z");
        assert_eq!(terminal.cursor_pos().x, 0);
    }

    #[test]
    fn tab_stop_report_round_trips_custom_and_empty_tables() {
        let (mut terminal, mut replies) = engine();
        let query = b"\x1b[2$w";
        let size = terminal.get_size();
        terminal.resize(TerminalSize {
            cols: 24,
            rows: 4,
            ..size
        });
        terminal.advance_bytes(query);
        assert_eq!(receive(&mut replies), b"\x1bP2$u1/9/17\x1b\\");
        terminal.advance_bytes(b"\x1b[3g\x1b[4G\x1bH\x1b[12G\x1bH");
        terminal.advance_bytes(query);
        let saved = receive(&mut replies);
        assert_eq!(saved, b"\x1bP2$u4/12\x1b\\");
        assert_eq!(terminal.cursor_pos().x, 11);

        terminal.advance_bytes(b"\x1b[3g");
        terminal.advance_bytes(query);
        assert_eq!(receive(&mut replies), b"\x1bP2$u\x1b\\");
        let mut restore = saved;
        restore[4] = b't';
        for byte in restore {
            terminal.advance_bytes([byte]);
        }
        terminal.advance_bytes(b"\x1b[1G\t");
        assert_eq!(terminal.cursor_pos().x, 3);
        terminal.advance_bytes(b"\x1b[I");
        assert_eq!(terminal.cursor_pos().x, 11);
        terminal.advance_bytes(b"\x1b[Z");
        assert_eq!(terminal.cursor_pos().x, 3);

        terminal.advance_bytes(b"\x1bP2$t\x9c\x1b[2$w");
        assert_eq!(receive(&mut replies), b"\x1bP2$u\x1b\\");
        terminal.advance_bytes(b"\x1bc\x1b[2$w");
        assert_eq!(receive(&mut replies), b"\x1bP2$u1/9/17\x1b\\");
    }

    #[test]
    fn tab_stop_restore_ignores_cancelled_malformed_and_oversize_strings() {
        let (mut terminal, mut replies) = engine();
        terminal.advance_bytes(b"\x1bP2$t3/8\x1b\\");
        for rejected in [
            b"\x1bP2$t5/0/7\x1b\\".as_slice(),
            b"\x1bP2$t5/oops/7\x1b\\",
            b"\x1bP2$t5\x18",
            b"\x1bP2$t5\x1a",
            b"\x1bP2$t5\x1b\x1b\\",
            b"\x1bP2$t5\x1b[1G",
        ] {
            terminal.advance_bytes(rejected);
            terminal.advance_bytes(b"\x1b[2$w");
            assert_eq!(receive(&mut replies), b"\x1bP2$u3/8\x1b\\");
        }
        terminal.advance_bytes(format!("\x1bP2$t{}4\x1b\\\x1b[2$w", "5/".repeat(4200)));
        assert_eq!(receive(&mut replies), b"\x1bP2$u3/8\x1b\\");
        terminal.advance_bytes(b"\x1bP2$t7/2/7\x9c\x1b[2$w");
        assert_eq!(receive(&mut replies), b"\x1bP2$u2/7\x1b\\");
    }

    #[test]
    fn vim_undercurl_probe_reports_the_rendition_at_the_query() {
        let (mut terminal, mut replies) = engine();
        // Vim's term.c sends the reset after the query in the same write.
        terminal.advance_bytes(b"\x1b[0m\x1b[4:3m\x1bP$qm\x1b\\\x1b[0m");
        let reply = receive(&mut replies);
        assert!(
            sgr_parameters(&reply)
                .split(';')
                .any(|param| param == "4:3")
        );
        assert_eq!(terminal.pen(), CellAttributes::default());
        terminal.advance_bytes(b"\x1bP$qm\x1b\\");
        assert_eq!(receive(&mut replies), b"\x1bP1$r0m\x1b\\");
    }

    fn sgr_parameters(reply: &[u8]) -> &str {
        let response = std::str::from_utf8(reply).unwrap();
        let params = response
            .strip_prefix("\x1bP1$r")
            .and_then(|report| report.strip_suffix("m\x1b\\"))
            .expect("expected a valid, ST-terminated SGR status string");
        assert!(
            params
                .bytes()
                .all(|c| c.is_ascii_digit() || matches!(c, b':' | b';'))
        );
        params
    }

    #[test]
    fn rendition_reports_restore_the_complete_pen_on_another_terminal() {
        let (mut source, mut replies) = engine();
        let (mut restored, _) = engine();
        // Change one pen in sequence, including partial resets and all three
        // color channels. The report must restore the pen even over prior styles.
        for settings in [
            "0",
            "1;3;4:3;5;7;8;9;53;73;38:2::1:2:3;48:2::4:5:6;58:2::7:8:9",
            "2;21;6;74;38;5;200;48;5;17;58;5;245",
            "22;23;28;39;59",
            "24;25;27;29;49;55;75",
            "31;104;4;58:5:2",
            "97;45;4:4",
            "4:5;38:6::11:12:13:91;48:6::14:15:16:125;58:6::17:18:19:159",
            "4:0;59;39;49",
        ] {
            source.advance_bytes(format!("\x1b[{settings}m"));
            let expected = source.pen();
            source.advance_bytes(b"\x1bP$qm\x1b\\");
            let reply = receive(&mut replies);
            let params = sgr_parameters(&reply);
            for starting in ["0", "0;1;3;4:2;6;7;8;9;53;73;92;106;58;5;99"] {
                restored.advance_bytes(format!("\x1b[{starting}m\x1b[{params}m"));
                assert_eq!(
                    restored.pen(),
                    expected,
                    "input {settings}, report {params}, starting {starting}"
                );
            }
        }
    }

    #[test]
    fn fragmented_rendition_query_only_emits_a_reply_and_keeps_local_metadata() {
        let (writer, mut output) = mpsc::unbounded_channel();
        let mut terminal = create(TerminalSize::default(), writer.clone());
        // The framed viewer distinguishes automatic replies from user input.
        enter_herdr_viewer(&mut terminal, &writer, &crate::events::Events::default());
        terminal
            .advance_bytes(b"sample\x1b]133;B\x07\x1b[3m\x1b]8;;https://example.com/private\x1b\\");
        let original_pen = terminal.pen();
        assert!(original_pen.hyperlink().is_some());
        assert_ne!(
            original_pen.semantic_type(),
            CellAttributes::default().semantic_type()
        );
        let before = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        for &byte in b"\x1bP$qm" {
            terminal.advance_bytes([byte]);
            assert!(output.try_recv().is_err(), "query is still incomplete");
        }
        terminal.advance_bytes([0x1b]);
        terminal.advance_bytes(b"\\");
        let TransportInput::Automatic(reply) = output.try_recv().unwrap() else {
            panic!("an SGR report must use the terminal reply transport");
        };
        let params = sgr_parameters(&reply);
        let (mut other, _) = engine();
        other.advance_bytes(format!("\x1b[{params}m"));
        let mut expected = CellAttributes::default();
        expected.set_italic(true);
        assert_eq!(other.pen(), expected);
        assert_eq!(terminal.pen(), original_pen);
        assert!(before.same_presentation(&Snapshot::capture(
            &terminal,
            Some(&before),
            2,
            0,
            String::new(),
            0
        )));
        terminal.advance_bytes(b"\x1bP$q!m\x1b\\");
        assert!(
            matches!(output.try_recv(), Ok(TransportInput::Automatic(bytes)) if bytes == b"\x1bP0$r\x1b\\")
        );
        assert!(output.try_recv().is_err(), "no extra replies or user input");
    }

    #[test]
    fn parses_fragmented_utf8_and_control_sequences() {
        let (mut t, _rx) = engine();
        for byte in "\x1b[2J\x1b[Hhello 世界\x1b[31m red\x1b[0m".as_bytes() {
            t.advance_bytes([*byte]);
        }
        assert!(text(&t).starts_with("hello 世界 red"));
        let frame = Snapshot::capture(&t, None, 1, 0, String::new(), 0);
        let red = frame.rows[0]
            .visible_cells()
            .find(|c| c.str() == "r")
            .unwrap();
        assert_ne!(
            red.attrs().foreground(),
            wezterm_term::color::ColorAttribute::Default
        );
    }

    #[test]
    fn alternate_screen_restores_primary_and_resize_reflows() {
        let (mut t, _rx) = engine();
        t.advance_bytes(b"primary\x1b[?1049halternate");
        assert!(t.is_alt_screen_active());
        t.advance_bytes(b"\x1b[?1049l");
        assert!(text(&t).starts_with("primary"));
        t.resize(TerminalSize {
            cols: 4,
            ..TerminalSize::default()
        });
        assert!(text(&t).contains("prim\nary"));
    }

    #[test]
    fn resizing_preserves_cursor_at_start_of_hard_newline_row() {
        for cols in [40, 100] {
            let (mut terminal, _rx) = engine();
            terminal.advance_bytes(b"remote ready\r\n");
            let size = TerminalSize {
                cols,
                ..terminal.get_size()
            };
            terminal.resize(size);
            assert_eq!((terminal.cursor_pos().x, terminal.cursor_pos().y), (0, 1));
            terminal.advance_bytes(b"x");
            assert!(text(&terminal).starts_with("remote ready\nx"));
        }
    }

    #[test]
    fn snapshot_cache_tracks_stable_rows_during_scrolling() {
        let (mut terminal, _rx) = engine();
        let output = (0..30).map(|n| format!("line {n}\r\n")).collect::<String>();
        terminal.advance_bytes(output);
        let old = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        terminal.advance_bytes(b"next\r\n");
        let new = Snapshot::capture(&terminal, Some(&old), 2, 0, String::new(), 0);
        assert_eq!(old.rows[1].as_str(), new.rows[0].as_str());
        assert!(Arc::ptr_eq(&old.rows[1], &new.rows[0]));
    }

    #[test]
    fn answers_queries_and_encodes_negotiated_keyboard() {
        let (mut t, mut rx) = engine();
        t.advance_bytes(b"\x1b[6n");
        assert_eq!(receive(&mut rx), b"\x1b[1;1R");
        t.advance_bytes(b"\x1b[>1u");
        let (writer, mut encoded) = mpsc::unbounded_channel();
        send_key(
            &mut t,
            &writer,
            KeyCode::Char('a'),
            KeyModifiers::CTRL,
            true,
        )
        .unwrap();
        assert_eq!(receive(&mut encoded), b"\x1b[97;5u");
    }

    #[test]
    fn da1_reports_osc52_write_support_after_policy_changes() {
        use crate::clipboard::{RemoteClipboard, RemoteClipboardAccess, RemoteClipboardPolicy::*};

        let (mut terminal, mut replies) = engine();
        let access = Arc::new(RemoteClipboardAccess::new(Disabled));
        let clipboard: Arc<dyn wezterm_term::Clipboard> = Arc::new(RemoteClipboard(access.clone()));
        terminal.set_clipboard(&clipboard);
        for (policy, report) in [
            (Disabled, b"\x1b[?65;4;6;18;22c".as_slice()),
            (AnyPane, b"\x1b[?65;4;6;18;22;52c".as_slice()),
            (ForegroundPane, b"\x1b[?65;4;6;18;22;52c".as_slice()),
            (Disabled, b"\x1b[?65;4;6;18;22c".as_slice()),
        ] {
            access.set_policy(policy);
            terminal.advance_bytes(b"\x1b[c");
            assert_eq!(receive(&mut replies), report);
        }
        access.set_policy(AnyPane);
        terminal.clear_clipboard();
        terminal.advance_bytes(b"\x1b[0c");
        assert_eq!(receive(&mut replies), b"\x1b[?65;4;6;18;22c");
    }

    #[test]
    fn identifies_sixel_and_version_and_encodes_repeat_release_and_reconnect_reset() {
        let (mut t, mut rx) = engine();
        t.advance_bytes(b"\x1b[c");
        assert_eq!(receive(&mut rx), b"\x1b[?65;4;6;18;22c");
        t.advance_bytes(b"\x1b[>0q");
        assert_eq!(
            receive(&mut rx),
            format!("\x1bP>|w11term {}\x1b\\", env!("CARGO_PKG_VERSION")).as_bytes()
        );
        t.advance_bytes(b"retained\x1b[?1049h\x1b[?1000h\x1b[?2004h\x1b[>11u");
        let (writer, mut encoded) = mpsc::unbounded_channel();
        for (down, repeat, expected) in [
            (true, false, "\x1b[97;1u"),
            (true, true, "\x1b[97;1:2u"),
            (false, false, "\x1b[97;1:3u"),
        ] {
            send_key_event(
                &mut t,
                &writer,
                KeyCode::Char('a'),
                KeyModifiers::NONE,
                down,
                repeat,
            )
            .unwrap();
            assert_eq!(receive(&mut encoded), expected.as_bytes());
        }
        t.advance_bytes(b"\x1b]2;unfinished");
        t.reset_connection();
        assert!(!t.is_alt_screen_active());
        assert!(!t.mouse_tracking_enabled());
        assert!(!t.bracketed_paste_enabled());
        assert!(!text(&t).contains("retained"));
        let kept = Snapshot::capture(&t, None, 1, usize::MAX, String::new(), 0);
        assert!(
            kept.rows
                .iter()
                .any(|line| line.as_str().contains("retained"))
        );
        t.advance_bytes(b"fresh");
        assert!(text(&t).contains("fresh"));
        send_key_event(
            &mut t,
            &writer,
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            true,
            true,
        )
        .unwrap();
        assert_eq!(receive(&mut rx), b"a");
    }

    #[test]
    fn encoding_a_confirmed_clipboard_paste_uses_the_existing_wire_format_without_writing() {
        let (mut terminal, mut rx) = engine();
        let text = "one\n界\x1b[201~\r\ntwo";
        for (mode, expected) in [
            (b"\x1b[?2004l".as_slice(), "one\r界\rtwo"),
            (
                b"\x1b[?2004h".as_slice(),
                "\x1b[200~one\n界\r\ntwo\x1b[201~",
            ),
        ] {
            terminal.advance_bytes(mode);
            assert_eq!(terminal.encode_paste(text), expected);
            assert!(rx.try_recv().is_err());
            terminal.send_paste(text).unwrap();
            assert_eq!(receive(&mut rx), expected.as_bytes());
        }
    }

    #[test]
    fn bracketed_paste_and_graphics_are_preserved_in_model() {
        let (mut t, mut rx) = engine();
        t.advance_bytes(b"\x1b[?2004h");
        assert!(
            t.bracketed_paste_enabled(),
            "bracketed paste mode was not enabled"
        );
        t.send_paste("one\ntwo").unwrap();
        let bytes = receive(&mut rx);
        assert!(bytes.starts_with(b"\x1b[200~"));
        assert!(bytes.ends_with(b"\x1b[201~"));
        t.advance_bytes(b"\x1b_Ga=T,f=32,s=1,v=1,i=1,c=1,r=1,q=2;/wAA/w==\x1b\\");
        let frame = Snapshot::capture(&t, None, 1, 0, String::new(), 0);
        assert!(
            frame
                .rows
                .iter()
                .flat_map(|l| l.visible_cells())
                .any(|c| c.attrs().images().is_some())
        );
    }

    #[test]
    fn snapshot_reuses_image_rows_and_freezes_their_actual_tile_allocations() {
        use wezterm_term::image::{ImageCell, ImageData, ImageDataType, TextureCoordinate};
        let (mut t, _) = engine();
        t.advance_bytes(b"tile\r\ntext");
        let source = || {
            Arc::new(ImageData::with_data(ImageDataType::new_single_frame(
                1,
                1,
                vec![1, 2, 3, 255],
            )))
        };
        let attach = |t: &mut Terminal, data| {
            t.screen_mut().line_mut(0).cells_mut_for_attr_changes_only()[0]
                .attrs_mut()
                .set_image(Box::new(ImageCell::new(
                    TextureCoordinate::new_f32(0., 0.),
                    TextureCoordinate::new_f32(1., 1.),
                    data,
                )));
        };
        let original = source();
        attach(&mut t, original.clone());
        let first = Snapshot::capture(&t, None, 1, 0, String::new(), 0);
        let replacement = source();
        assert!(!Arc::ptr_eq(&original, &replacement));
        attach(&mut t, replacement);
        let same = Snapshot::capture(&t, Some(&first), 2, 0, String::new(), 0);
        assert!(Arc::ptr_eq(&first.rows[0], &same.rows[0]));
        assert!(Arc::ptr_eq(&first.rows[1], &same.rows[1]));
        assert!(std::ptr::eq(
            first.image_data(&original),
            same.image_data(&original)
        ));
        assert!(first.same_presentation(&same));

        t.screen_mut().line_mut(0).cells_mut_for_attr_changes_only()[0]
            .attrs_mut()
            .clear_images();
        let cleared = Snapshot::capture(&t, Some(&same), 3, 0, String::new(), 0);
        assert!(cleared.images.is_empty());
        assert!(cleared.image_rows.is_empty());
        assert!(!same.same_presentation(&cleared));
        assert!(Arc::ptr_eq(&same.rows[1], &cleared.rows[1]));
    }

    #[test]
    fn deleting_a_placement_after_narrowing_removes_reflowed_image_cells() {
        let (mut t, _) = engine();
        t.advance_bytes(b"\x1b[2;71H\x1b_Ga=T,i=7,p=1,f=32,s=1,v=1,c=2,r=2,q=2;/wAA/w==\x1b\\");
        let size = t.get_size();
        t.resize(TerminalSize {
            cols: 40,
            pixel_width: 400,
            ..size
        });
        let before = Snapshot::capture(&t, None, 1, 0, String::new(), 0);
        assert!(
            before
                .rows
                .iter()
                .flat_map(|l| l.visible_cells())
                .any(|c| c.attrs().images().is_some()),
            "placement disappeared during reflow"
        );
        t.advance_bytes(b"\x1b_Ga=d,d=I,i=7,q=2\x1b\\");
        let snapshot = Snapshot::capture(&t, None, 1, 0, String::new(), 0);
        assert!(
            !snapshot
                .rows
                .iter()
                .flat_map(|l| l.visible_cells())
                .any(|c| c.attrs().images().is_some()),
            "stale image cells survived deletion after reflow"
        );
    }
}
