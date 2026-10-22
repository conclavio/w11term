//! The terminal engine is independent of Reactor, rendering, and SSH.
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
struct Config;

impl TerminalConfiguration for Config {
    fn color_palette(&self) -> ColorPalette {
        ColorPalette {
            foreground: SrgbaTuple(0.86, 0.88, 0.91, 1.0),
            background: SrgbaTuple(0.055, 0.063, 0.078, 1.0),
            cursor_bg: SrgbaTuple(0.48, 0.79, 0.98, 1.0),
            ..ColorPalette::default()
        }
    }

    fn scrollback_size(&self) -> usize {
        10_000
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
    Raw(Vec<u8>, tokio::sync::oneshot::Sender<Result<(), String>>),
    Resize(TerminalSize),
}

struct ReplyWriter(mpsc::UnboundedSender<TransportInput>);

impl Write for ReplyWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .send(TransportInput::Data(bytes.to_vec()))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "session closed"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn create(size: TerminalSize, writer: mpsc::UnboundedSender<TransportInput>) -> Terminal {
    Terminal::new(
        size,
        Arc::new(Config),
        "w11term",
        env!("CARGO_PKG_VERSION"),
        Box::new(ReplyWriter(writer)),
    )
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
    if let termwiz::input::KeyboardEncoding::Kitty(flags) = terminal.get_keyboard_encoding() {
        use wezterm_input_types::{KeyCode as K, KeyEvent, KeyboardLedStatus, Modifiers};
        use wezterm_term::KeyCode as L;
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
        if !encoded.is_empty() {
            writer.send(TransportInput::Data(encoded.into_bytes()))?;
        }
        Ok(())
    } else if down {
        terminal.key_down(key, modifiers)
    } else {
        terminal.key_up(key, modifiers)
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub generation: u64,
    pub rows: Vec<Arc<Line>>,
    pub palette: ColorPalette,
    pub cursor: CursorPosition,
    pub cursor_visible: bool,
    pub cols: usize,
    pub scroll_offset: usize,
    pub title: String,
    pub status: String,
    pub bytes_received: u64,
    pub first_stable_row: isize,
    pub alternate: bool,
    pub mouse_grabbed: bool,
    pub search: Option<crate::search::SearchSnapshot>,
    pub prompt: Option<crate::prompts::Prompt>,
    pub connection_generation: u64,
    pub size: TerminalSize,
    images: HashMap<usize, FrozenImage>,
}

#[derive(Clone, Debug)]
struct FrozenImage {
    hash: [u8; 32],
    data: Arc<wezterm_term::image::ImageDataType>,
}

impl Snapshot {
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
        let lines = screen.lines_in_phys_range(start..start + screen.physical_rows);
        let rows: Vec<Arc<Line>> = lines
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                let stable = first_stable_row + index as isize;
                let old_row = previous
                    .filter(|old| old.alternate == alternate)
                    .and_then(|old| {
                        usize::try_from(stable - old.first_stable_row)
                            .ok()
                            .and_then(|i| old.rows.get(i))
                    });
                if let Some(old) = old_row
                    && old.current_seqno() == line.current_seqno()
                {
                    return Arc::clone(old);
                }
                Arc::new(line)
            })
            .collect();
        let mut images = HashMap::new();
        for line in &rows {
            for cell in line.visible_cells() {
                for tile in cell.attrs().images().into_iter().flatten() {
                    let source = tile.image_data();
                    let identity = Arc::as_ptr(source) as usize;
                    if images.contains_key(&identity) {
                        continue;
                    }
                    let guard = source.data();
                    use wezterm_term::image::ImageDataType;
                    let hash = match &*guard {
                        ImageDataType::Rgba8 { hash, .. } => *hash,
                        ImageDataType::AnimRgba8 { hashes, .. } if !hashes.is_empty() => hashes[0],
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
                                } if !frames.is_empty() && !hashes.is_empty() => {
                                    ImageDataType::Rgba8 {
                                        width: *width,
                                        height: *height,
                                        data: frames[0].clone(),
                                        hash: hashes[0],
                                    }
                                }
                                data => data.clone(),
                            };
                            FrozenImage {
                                hash,
                                data: Arc::new(data),
                            }
                        });
                    images.insert(identity, frozen);
                }
            }
        }
        let cursor = terminal.cursor_pos();
        Self {
            generation,
            rows,
            palette: terminal.palette(),
            cursor,
            cursor_visible: offset == 0
                && cursor.visibility == termwiz::surface::CursorVisibility::Visible,
            cols: screen.physical_cols,
            scroll_offset: offset,
            title: terminal.get_title().to_owned(),
            status,
            bytes_received,
            first_stable_row,
            alternate,
            mouse_grabbed: terminal.is_mouse_grabbed(),
            search: None,
            prompt: None,
            connection_generation: 0,
            size: terminal.get_size(),
            images,
        }
    }

    /// Pixels referenced by a presentation never change underneath the renderer.
    pub fn image_data(
        &self,
        source: &Arc<wezterm_term::image::ImageData>,
    ) -> &wezterm_term::image::ImageDataType {
        &self.images[&(Arc::as_ptr(source) as usize)].data
    }

    pub fn same_presentation(&self, old: &Self) -> bool {
        self.size == old.size
            && self.first_stable_row == old.first_stable_row
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
#[derive(Clone, Debug)]
pub struct FrameStore(Arc<(Mutex<Arc<Snapshot>>, Condvar, tokio::sync::Notify)>);

impl FrameStore {
    pub fn new(frame: Snapshot) -> Self {
        Self(Arc::new((
            Mutex::new(Arc::new(frame)),
            Condvar::new(),
            tokio::sync::Notify::new(),
        )))
    }
    pub fn load(&self) -> Arc<Snapshot> {
        Arc::clone(&self.0.0.lock().unwrap())
    }
    pub fn publish(&self, frame: Snapshot) {
        *self.0.0.lock().unwrap() = Arc::new(frame);
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
    pub fn wait_for_change(&self, generation: u64, cancelled: impl Fn() -> bool) {
        let mut frame = self.0.0.lock().unwrap();
        while frame.generation == generation && !cancelled() {
            frame = self
                .0
                .1
                .wait_timeout(frame, Duration::from_millis(250))
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
                        if !down && !report {
                            assert!(rx.try_recv().is_err());
                            continue;
                        }
                        let event = if !report {
                            ""
                        } else if !down {
                            ":3"
                        } else if repeat {
                            ":2"
                        } else {
                            ""
                        };
                        let expected = if key == KeyCode::Backspace {
                            if modifiers == KeyModifiers::NONE
                                && flags & 8 == 0
                                && down
                                && !(repeat && report)
                            {
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
    fn receive(rx: &mut mpsc::UnboundedReceiver<TransportInput>) -> Vec<u8> {
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
    fn text(t: &Terminal) -> String {
        let s = t.screen();
        s.lines_in_phys_range(s.phys_row(0)..s.phys_row(0) + s.physical_rows)
            .iter()
            .map(|l| l.as_str().trim_end().to_owned())
            .collect::<Vec<_>>()
            .join("\n")
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
        assert!(text(&t).starts_with("retained"));
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
