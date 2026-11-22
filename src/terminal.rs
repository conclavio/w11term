//! The terminal engine is independent of Reactor, rendering, and SSH.
use crate::options::{ColorScheme, ScrollbackLines};
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use wezterm_term::color::{ColorPalette, SrgbaTuple};
use wezterm_term::config::NewlineCanon;
use wezterm_term::{
    CellAttributes, CursorPosition, Line, Terminal, TerminalConfiguration, TerminalSize,
};

mod color_scheme;
mod export;
mod kitty;
mod retained_rows;
pub use retained_rows::RetainedRows;
mod scrollback_view;
pub use scrollback_view::{ScrollbackTarget, ScrollbackView};
#[cfg(test)]
mod scrollback_tests;
pub(crate) use export::export_text;
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
    scheme: ColorScheme,
}

impl Config {
    fn new(scrollback_lines: ScrollbackLines, scheme: ColorScheme) -> Self {
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
        self.scheme == ColorScheme::OneHalfLight
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
    Data(Vec<u8>),
    /// Parser-generated reply on a viewer transport that distinguishes it from user input.
    Automatic(Vec<u8>),
    Raw(Vec<u8>, tokio::sync::oneshot::Sender<Result<(), String>>),
    /// A GUI clipboard write is valid only for the shell that was
    /// active when the paste was requested.
    Clipboard {
        connection: u64,
        bytes: Vec<u8>,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    Resize(TerminalSize),
}

pub(crate) const STALE_PASTE: &str =
    "The pane connection changed. Paste again to use the new connection.";

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
    Terminal::new(
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
    scheme: ColorScheme,
) {
    terminal.set_config(Arc::new(Config::new(scrollback_lines, scheme)));
}

pub(crate) fn replace_transport(
    terminal: &mut Terminal,
    writer: mpsc::UnboundedSender<TransportInput>,
) {
    terminal.replace_writer(Box::new(ReplyWriter {
        sender: writer,
        message: TransportInput::Data,
    }));
}

/// Herdr can send a screen reconstruction on attach or while a viewer falls behind.
/// Screen paint and subsequent live bytes share the same stream.
pub(crate) fn enter_herdr_viewer(
    terminal: &mut Terminal,
    writer: &mpsc::UnboundedSender<TransportInput>,
    events: &crate::events::Events,
) {
    struct ViewerAlerts(crate::events::Events);
    impl wezterm_term::AlertHandler for ViewerAlerts {
        fn alert(&mut self, alert: wezterm_term::Alert) {
            if let wezterm_term::Alert::WindowTitleChanged(title) = alert {
                self.0.push("title", serde_json::json!({"title": title}));
            }
        }
    }
    events.command_stream(false);
    terminal.clear_clipboard();
    terminal.set_notification_handler(Box::new(ViewerAlerts(events.clone())));
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
    if let termwiz::input::KeyboardEncoding::Kitty(flags) = terminal.get_keyboard_encoding() {
        use wezterm_input_types::{KeyCode as K, KeyEvent, KeyboardLedStatus, Modifiers};
        use wezterm_term::KeyCode as L;
        let all = flags
            .contains(wezterm_input_types::KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES);
        let reports = flags.contains(wezterm_input_types::KittyKeyboardFlags::REPORT_EVENT_TYPES);
        if let Some(native) = kitty::NativeKey::from_event(flags, key, details) {
            let wire = native.encode(flags, key, modifiers, details, down, repeat);
            if !wire.is_empty() {
                writer.send(TransportInput::Data(wire.into_bytes()))?;
            }
            return Ok(());
        }
        let safety_key = matches!(
            key,
            L::Enter | L::Tab | L::Backspace | L::Char('\r' | '\t' | '\x08')
        );
        if safety_key && !all && !down {
            return Ok(());
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
                writer.send(TransportInput::Data(
                    format!("\x1b[{code};{modifier}{event}u").into_bytes(),
                ))?;
            }
            return Ok(());
        }
        if let Some(text) = &details.text
            && !all
        {
            if down {
                writer.send(TransportInput::Data(text.as_bytes().to_vec()))?;
            }
            return Ok(());
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
            _ => {
                return if down {
                    terminal.key_down(key, modifiers)
                } else {
                    terminal.key_up(key, modifiers)
                };
            }
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
            let reports =
                flags.contains(wezterm_input_types::KittyKeyboardFlags::REPORT_EVENT_TYPES);
            if !down && !reports {
                return Ok(());
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
            writer.send(TransportInput::Data(
                format!("\x1b[3;{modifier}{event}~").into_bytes(),
            ))?;
            return Ok(());
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
        if !encoded.is_empty() {
            writer.send(TransportInput::Data(encoded.into_bytes()))?;
        }
        Ok(())
    } else if down {
        if let Some(text) = &details.text {
            writer.send(TransportInput::Data(text.as_bytes().to_vec()))?;
            return Ok(());
        }
        terminal.key_down(key, modifiers)
    } else {
        terminal.key_up(key, modifiers)
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub generation: u64,
    pub rows: Vec<Arc<Line>>,
    /// Whether the last visible cell continues onto the following physical row.
    row_wraps: Vec<bool>,
    pub palette: ColorPalette,
    pub cursor: CursorPosition,
    pub cursor_visible: bool,
    pub cols: usize,
    pub scroll_offset: usize,
    pub title: String,
    pub command_history: Arc<std::collections::VecDeque<Arc<wezterm_term::CompletedCommand>>>,
    pub reported_directory: Option<Arc<str>>,
    pub status: String,
    pub bytes_received: u64,
    pub first_stable_row: isize,
    pub oldest_stable_row: isize,
    row_epoch: u64,
    history_clear_count: u64,
    /// Most recent successful local history clear. The floor is the first row
    /// retained at the time of that clear, even if new output has since arrived.
    pub scrollback_clear_generation: u64,
    pub scrollback_clear_floor: Option<isize>,
    pub alternate: bool,
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

impl Snapshot {
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
        // DECSCNM exchanges the display defaults. Leave the underlying OSC colors
        // and indexed palette intact so queries and later resets see the originals.
        let mut palette = terminal.palette();
        if terminal.get_reverse_video() {
            std::mem::swap(&mut palette.foreground, &mut palette.background);
        }
        Self {
            generation,
            rows,
            row_wraps,
            palette,
            cursor,
            cursor_visible: offset == 0
                && cursor.visibility == termwiz::surface::CursorVisibility::Visible,
            cols: screen.physical_cols,
            scroll_offset: offset,
            title: terminal.get_title().to_owned(),
            command_history: terminal.command_history().clone(),
            reported_directory: terminal.reported_current_dir().cloned(),
            status,
            bytes_received,
            first_stable_row,
            oldest_stable_row: screen.phys_to_stable_row_index(0),
            row_epoch: screen.row_epoch(),
            history_clear_count: screen.history_clear_count(),
            scrollback_clear_generation: 0,
            scrollback_clear_floor: None,
            alternate,
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
            .map_or(self.cursor.x, |map| map.visual(self.cursor.x))
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
            && self.cursor_visible == old.cursor_visible
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
            && self.title == old.title
            && Arc::ptr_eq(&self.command_history, &old.command_history)
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

/// Only an immutable frame crosses onto the UI thread. Output is never queued as frames.
mod modes;
pub use modes::TerminalModes;

#[derive(Clone, Debug)]
pub struct FrameStore(Arc<(Mutex<ViewState>, Condvar, tokio::sync::Notify)>);

#[derive(Debug)]
struct ViewState {
    frame: Arc<Snapshot>,
    inspection: Option<TerminalModes>,
}

impl FrameStore {
    pub fn new(frame: Snapshot) -> Self {
        Self(Arc::new((
            Mutex::new(ViewState {
                frame: Arc::new(frame),
                inspection: None,
            }),
            Condvar::new(),
            tokio::sync::Notify::new(),
        )))
    }
    pub fn load(&self) -> Arc<Snapshot> {
        self.0.0.lock().unwrap().frame.clone()
    }
    pub fn publish(&self, frame: Snapshot) {
        self.0.0.lock().unwrap().frame = Arc::new(frame);
        self.0.1.notify_all();
        self.0.2.notify_waiters();
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
    fn command_watch_fires_once_for_the_first_paired_primary_command_in_a_batch() {
        let (mut term, events) = watched_terminal();
        events.command_stream(true);
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
        events.command_stream(true);
        term.advance_bytes(b"\x1b]133;C;aid=shell\x07private output\x1b[3J");
        assert!(events.set_command_watch(true));
        for b in b"\x1b]133;C;aid=shell\x07\x1b]133;C;aid=nested\x07\x1b]133;D;5;aid=nested\x1b\\\x1b]133;A;aid=nested\x07\x1b[?1049h\x1b]133;D;9;aid=shell\x07\x1b[?1049l\x1b]133;N;aid=shell\x1b\\\x1b]133;D;8;aid=shell\x07" {
            term.advance_bytes([*b]);
        }
        assert_eq!(
            completions(&events),
            [serde_json::json!({"exit_code":null})]
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
    }

    #[test]
    fn command_watch_cancels_on_reset_disconnect_and_viewer_transition() {
        let (mut term, events) = watched_terminal();
        assert!(!events.set_command_watch(true));
        term.advance_bytes(b"\x1b]133;C\x07historical output");
        events.command_stream(true);
        assert!(events.set_command_watch(true));
        term.advance_bytes(b"\x1b]133;D;11\x07");
        assert_eq!(events.command_watch(), Some(true));
        for reset in [b"\x1bc".as_slice(), b"\x1b[!p", b"\x1b]133;B\x07"] {
            events.set_command_watch(true);
            term.advance_bytes(b"\x1b]133;C\x07");
            term.advance_bytes(reset);
            assert_eq!(events.command_watch(), Some(false));
            term.advance_bytes(b"\x1b]133;D;42\x07");
        }
        events.set_command_watch(true);
        term.advance_bytes(b"\x1b]133;C\x07");
        events.command_stream(false);
        assert_eq!(events.command_watch(), None);
        term.reset_connection();
        events.command_stream(true);
        term.advance_bytes(b"\x1b]133;D;2\x07\x1b]133;C\x07");
        assert!(events.set_command_watch(true));
        assert!(events.set_command_watch(false));
        term.advance_bytes(b"\x1b]133;D;4\x07");
        assert!(events.set_command_watch(true));
        enter_herdr_viewer(&mut term, &mpsc::unbounded_channel().0, &events);
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
    fn command_history_is_bounded_and_frozen_across_eviction_and_reset() {
        let (mut term, _) = engine();
        term.advance_bytes(b"\x1b]133;B\x07cancelled\x1b]133;A\x07");
        assert!(term.command_history().is_empty());
        term.advance_bytes(b"\x1b]133;B\x07echo first\r\n\x1b]133;C\x07first\x1b]133;D;0\x07");
        let frozen = term.command_history().clone();
        for _ in 0..64 {
            term.advance_bytes(b"\x1b]133;C\x07ok\x1b]133;D;1\x07");
        }
        assert_eq!(term.command_history().len(), 64);
        assert_eq!(term.command_history().front().unwrap().id, 2);
        assert_eq!(frozen[0].command.as_deref(), Some("echo first"));
        assert_eq!(frozen[0].output.as_deref(), Ok("first"));
        term.reset_connection();
        assert!(term.command_history().is_empty());
        term.advance_bytes(b"\x1b]133;C\x07new\x1b]133;D\x07");
        assert_eq!(term.command_history()[0].id, 66);
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
        events.command_stream(true);
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
    fn text(t: &Terminal) -> String {
        let s = t.screen();
        s.lines_in_phys_range(s.phys_row(0)..s.phys_row(0) + s.physical_rows)
            .iter()
            .map(|l| l.as_str().trim_end().to_owned())
            .collect::<Vec<_>>()
            .join("\n")
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
    fn identifies_sixel_and_version_and_encodes_repeat_release_and_reconnect_reset() {
        let (mut t, mut rx) = engine();
        t.advance_bytes(b"\x1b[c");
        assert_eq!(receive(&mut rx), b"\x1b[?65;4;6;18;22;52c");
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
