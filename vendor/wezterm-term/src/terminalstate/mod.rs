// The range_plus_one lint can't see when the LHS is not compatible with
// and inclusive range
#![allow(clippy::range_plus_one)]

use super::*;
use crate::color::{ColorPalette, RgbColor};
use crate::config::{BidiMode, NewlineCanon};
use log::debug;
use num_traits::ToPrimitive;
use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::io::{BufWriter, Write};
use std::num::NonZeroUsize;
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use terminfo::{Database, Value};
use termwiz::input::KeyboardEncoding;
use url::Url;
use wezterm_bidi::ParagraphDirectionHint;
use wezterm_cell::image::ImageData;
use wezterm_cell::UnicodeVersion;
use wezterm_escape_parser::csi::{
    Cursor, CursorStyle, CursorTabulationControl, DecPrivateMode, DecPrivateModeCode, Device, Edit,
    EraseInDisplay, EraseInLine, Mode, Sgr, TabulationClear, TerminalMode, TerminalModeCode,
    Window, XtSmGraphics, XtSmGraphicsAction, XtSmGraphicsItem, XtSmGraphicsStatus,
    XtermKeyModifierResource,
};
use wezterm_escape_parser::{OneBased, OperatingSystemCommand, CSI};
use wezterm_surface::{CursorShape, CursorVisibility, SequenceNo};

mod area_attributes;
mod column_edit;
mod image;
mod iterm;
mod keyboard;
mod kitty;
mod kitty_colors;
mod mouse;
mod palette;
pub(crate) mod performer;
mod pointer;
mod program_status;
mod rectangle;
mod rendition;
mod selective_erase;
mod sgr_stack;
mod sixel;
mod title;
use crate::terminalstate::image::*;
use crate::terminalstate::kitty::*;
pub use pointer::PointerShape;
pub use program_status::{ProgramBlockKind, ProgramState, ProgramStatus};

#[derive(Default)]
struct SavedPrivateModes {
    flags: HashMap<u16, bool>,
    alternate_screen: Option<bool>,
    mouse_reporting: Option<(bool, bool, bool)>,
    mouse_encoding: Option<MouseEncoding>,
}

lazy_static::lazy_static! {
    static ref DB: Database = {
        let data = include_bytes!("../../data/wezterm");
        Database::from_buffer(&data[..]).unwrap()
    };
}

/// Tracks the horizontal tab stops for a terminal screen.
/// Tab stops are stored as a boolean vector, one entry per column.
pub(crate) struct TabStop {
    tabs: Vec<bool>,
    tab_width: usize,
}

/// A character set selectable through the DEC SCS escape sequences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharSet {
    /// The US ASCII character set (`B`).
    Ascii,
    /// The United Kingdom character set (`A`).
    Uk,
    /// DEC Special Graphics / line drawing (`0`).
    DecLineDrawing,
}

/// The mouse reporting encoding negotiated through DEC private mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEncoding {
    X10,
    Utf8,
    SGR,
    SgrPixels,
}

impl TabStop {
    fn new(screen_width: usize, tab_width: usize) -> Self {
        let mut tabs = Vec::with_capacity(screen_width);

        for i in 0..screen_width {
            tabs.push((i % tab_width) == 0);
        }
        Self { tabs, tab_width }
    }

    fn set_tab_stop(&mut self, col: usize) {
        self.tabs[col] = true;
    }

    fn columns(&self, width: usize) -> impl Iterator<Item = usize> + '_ {
        self.tabs
            .iter()
            .take(width)
            .enumerate()
            .filter_map(|(index, &stop)| stop.then_some(index + 1))
    }

    fn restore(&mut self, report: &[u8]) {
        let Ok(report) = std::str::from_utf8(report) else {
            return;
        };
        let mut tabs = vec![false; self.tabs.len()];
        for field in report.split_terminator('/') {
            if !field.bytes().all(|byte| byte.is_ascii_digit()) {
                return;
            }
            let Some(index) = field
                .parse::<usize>()
                .ok()
                .and_then(|column| column.checked_sub(1))
            else {
                return;
            };
            if let Some(stop) = tabs.get_mut(index) {
                *stop = true;
            }
        }
        self.tabs = tabs;
    }

    fn clear_tab_stop(&mut self, col: usize) {
        if let Some(stop) = self.tabs.get_mut(col) {
            *stop = false;
        }
    }

    fn clear_all_tab_stops(&mut self) {
        self.tabs.fill(false);
    }

    fn reset_tab_stops(&mut self) {
        for (index, stop) in self.tabs.iter_mut().enumerate() {
            *stop = index != 0 && index % self.tab_width == 0;
        }
    }

    fn backward(&self, column: usize, left: usize, skip: usize) -> usize {
        self.tabs
            .iter()
            .take(column)
            .enumerate()
            .skip(left)
            .rev()
            .filter_map(|(index, &stop)| stop.then_some(index))
            .nth(skip)
            .unwrap_or(left)
    }

    fn forward(&self, column: usize, right: usize, skip: usize) -> usize {
        self.tabs
            .iter()
            .take(right + 1)
            .enumerate()
            .skip(column + 1)
            .filter_map(|(index, &stop)| stop.then_some(index))
            .nth(skip)
            .unwrap_or(right)
    }

    /// Respond to the terminal resizing.
    /// If the screen got bigger, we need to expand the tab stops
    /// into the new columns with the appropriate width.
    fn resize(&mut self, screen_width: usize) {
        let current = self.tabs.len();
        if screen_width > current {
            for i in current..screen_width {
                self.tabs.push((i % self.tab_width) == 0);
            }
        }
    }

    fn clear(&mut self, to_clear: TabulationClear, col: usize, log_unknown_escape_sequences: bool) {
        match to_clear {
            TabulationClear::ClearCharacterTabStopAtActivePosition => self.clear_tab_stop(col),
            // Preserve VT100 behavior for TBC 2. Stops are shared across the screen.
            TabulationClear::ClearAllCharacterTabStops | TabulationClear::ClearAllTabStops => {
                self.clear_all_tab_stops();
            }
            _ => {
                if log_unknown_escape_sequences {
                    log::warn!("unhandled TabulationClear {:?}", to_clear);
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SavedCursor {
    position: CursorPosition,
    wrap_next: bool,
    pen: CellAttributes,
    dec_origin_mode: bool,
    g0_charset: CharSet,
    g1_charset: CharSet,
}

struct ScreenOrAlt {
    /// The primary screen + scrollback
    screen: Screen,
    /// The alternate screen; no scrollback
    alt_screen: Screen,
    /// Tells us which screen is active
    alt_screen_is_active: bool,
}

impl Deref for ScreenOrAlt {
    type Target = Screen;

    fn deref(&self) -> &Screen {
        if self.alt_screen_is_active {
            &self.alt_screen
        } else {
            &self.screen
        }
    }
}

impl DerefMut for ScreenOrAlt {
    fn deref_mut(&mut self) -> &mut Screen {
        if self.alt_screen_is_active {
            &mut self.alt_screen
        } else {
            &mut self.screen
        }
    }
}

impl ScreenOrAlt {
    pub fn new(
        size: TerminalSize,
        config: &Arc<dyn TerminalConfiguration>,
        seqno: SequenceNo,
        bidi_mode: BidiMode,
    ) -> Self {
        let screen = Screen::new(size, config, true, seqno, bidi_mode);
        let alt_screen = Screen::new(size, config, false, seqno, bidi_mode);

        Self {
            screen,
            alt_screen,
            alt_screen_is_active: false,
        }
    }

    pub fn resize(
        &mut self,
        size: TerminalSize,
        cursor_main: CursorPosition,
        cursor_alt: CursorPosition,
        seqno: SequenceNo,
        is_conpty: bool,
    ) -> (CursorPosition, CursorPosition) {
        let cursor_main = self.screen.resize(size, cursor_main, seqno, is_conpty);
        let cursor_alt = self.alt_screen.resize(size, cursor_alt, seqno, is_conpty);
        (cursor_main, cursor_alt)
    }

    pub fn activate_alt_screen(&mut self, seqno: SequenceNo) {
        if !self.alt_screen_is_active {
            self.alt_screen.invalidate_visible_rows();
        }
        self.alt_screen_is_active = true;
        self.dirty_top_phys_rows(seqno);
    }

    pub fn activate_primary_screen(&mut self, seqno: SequenceNo) {
        if self.alt_screen_is_active {
            self.screen.invalidate_visible_rows();
        }
        self.alt_screen_is_active = false;
        self.dirty_top_phys_rows(seqno);
    }

    // When switching between alt and primary screen, we implicitly change
    // the content associated with StableRowIndex 0..num_rows.  The muxer
    // use case needs to know to invalidate its cache, so we mark those rows
    // as dirty.
    fn dirty_top_phys_rows(&mut self, seqno: SequenceNo) {
        let num_rows = self.screen.physical_rows;
        for line_idx in 0..num_rows {
            self.screen
                .line_mut(line_idx)
                .update_last_change_seqno(seqno);
        }
    }

    pub fn is_alt_screen_active(&self) -> bool {
        self.alt_screen_is_active
    }

    pub fn saved_cursor(&mut self) -> &mut Option<SavedCursor> {
        if self.alt_screen_is_active {
            &mut self.alt_screen.saved_cursor
        } else {
            &mut self.screen.saved_cursor
        }
    }

    pub fn full_reset(&mut self) {
        self.screen.full_reset();
        self.alt_screen.full_reset();
    }
}

/// Frozen visible text and optional exit status from shell integration markers.
#[derive(Debug)]
pub struct CompletedCommand {
    pub id: u64,
    pub command: Option<String>,
    pub exit_code: Option<i32>,
    pub output: Result<String, &'static str>,
}

impl CompletedCommand {
    fn text_bytes(&self) -> usize {
        self.command.as_ref().map_or(0, String::len) + self.output.as_ref().map_or(0, String::len)
    }
}

/// Location of the last non-blank text printed into the primary screen.
#[derive(Clone, Copy)]
pub struct PrimaryPrint {
    pub seqno: SequenceNo,
    pub row_epoch: u64,
    pub row: StableRowIndex,
    pub end_col: usize,
}

#[derive(Clone, Copy)]
enum ConEmuDirectoryReports {
    Drive,
    Wsl,
}

#[derive(Default)]
enum VscodeCommand {
    #[default]
    Idle,
    Pending(Option<String>),
    Running,
}

/// Manages the state for the terminal
pub struct TerminalState {
    config: Arc<dyn TerminalConfiguration>,

    screen: ScreenOrAlt,
    /// The current set of attributes in effect for the next
    /// attempt to print to the display
    pen: CellAttributes,
    /// The current cursor position, relative to the top left
    /// of the screen.  0-based index.
    cursor: CursorPosition,
    default_cursor_shape: CursorShape,
    application_cursor_guide: bool,

    /// if true, implicitly move to the next line on the next
    /// printed character
    wrap_next: bool,

    clear_semantic_attribute_on_newline: bool,

    /// If true, writing a character inserts a new cell
    insert: bool,

    /// When set, received chars auto-wrap to the next line when the cursor reaches the right border.
    /// Otherwise received chars continuously replace the last char on the right.
    /// note: This is set by default, because it's more useful this way.
    /// https://vt100.net/docs/vt510-rm/DECAWM.html
    dec_auto_wrap: bool,

    /// Reverse Wraparound Mode
    reverse_wraparound_mode: bool,

    /// Reverse video mode
    reverse_video_mode: bool,

    /// https://vt100.net/docs/vt510-rm/DECOM.html
    /// When OriginMode is enabled, cursor is constrained to the
    /// scroll region and its position is relative to the scroll
    /// region.
    dec_origin_mode: bool,

    /// The scroll region
    top_and_bottom_margins: Range<VisibleRowIndex>,
    /// The inner horizontal region between DECSLRM left/right margins.
    /// `start` is the left margin column, `end` is one past the right margin column.
    /// Only effective when `left_and_right_margin_mode` is enabled.
    left_and_right_margins: Range<usize>,
    left_and_right_margin_mode: bool,

    /// When set, modifies the sequence of bytes sent for keys
    /// designated as cursor keys.  This includes various navigation
    /// keys.  The code in key_down() is responsible for interpreting this.
    application_cursor_keys: bool,
    modify_other_keys: u8,
    format_other_keys_csi_u: bool,

    dec_ansi_mode: bool,

    /// https://vt100.net/dec/ek-vt38t-ug-001.pdf#page=132 has a
    /// discussion on what sixel dispay mode (DECSDM) does.
    sixel_display_mode: bool,
    use_private_color_registers_for_each_graphic: bool,

    /// Graphics mode color register map.
    color_map: HashMap<u16, RgbColor>,

    /// When set, modifies the sequence of bytes sent for keys
    /// in the numeric keypad portion of the keyboard.
    application_keypad: bool,

    /// When set, pasting the clipboard should bracket the data with
    /// designated marker characters.
    bracketed_paste: bool,

    /// Movement events enabled
    any_event_mouse: bool,
    focus_tracking: bool,
    report_color_scheme_changes: bool,
    report_text_area_resize: bool,
    /// X10 (legacy), SGR, and SGR-Pixels style mouse tracking and
    /// reporting is enabled
    mouse_encoding: MouseEncoding,
    mouse_tracking: bool,
    /// Button events enabled
    button_event_mouse: bool,
    current_mouse_buttons: Vec<MouseButton>,
    last_mouse_move: Option<MouseEvent>,
    cursor_visible: bool,

    keyboard_encoding: KeyboardEncoding,
    /// Support for US, UK, and DEC Special Graphics
    g0_charset: CharSet,
    g1_charset: CharSet,
    shift_out: bool,

    newline_mode: bool,

    tabs: TabStop,

    /// The terminal title string (OSC 2)
    title: title::WindowTitle,
    /// The icon title string (OSC 1)
    icon_title: Option<String>,
    saved_titles: title::TitleStack,
    progress: Progress,
    program_statuses: program_status::ProgramStatuses,
    // OSC 9;4 preserves the previous value while the bar is indeterminate.
    progress_value: u8,

    palette: palette::PaletteOverrides,
    palette_stack: palette::PaletteStack,

    pixel_width: usize,
    pixel_height: usize,
    dpi: u32,

    clipboard: Option<Arc<dyn Clipboard>>,
    device_control_handler: Option<Box<dyn DeviceControlHandler>>,
    alert_handler: Option<Box<dyn AlertHandler>>,
    download_handler: Option<Arc<dyn DownloadHandler>>,

    // Keep the wire authority, since URL normalization maps file://localhost
    // to an empty host. The SSH client must validate the reported destination.
    current_dir: Option<(Url, Arc<str>)>,
    conemu_directory_reports: Option<ConEmuDirectoryReports>,
    iterm_directory_peer: Option<iterm::DirectoryPeer>,
    command_history: Arc<VecDeque<Arc<CompletedCommand>>>,
    next_command_id: u64,
    capturing_shell_input: bool,
    track_primary_prints: bool,
    last_primary_print: Option<PrimaryPrint>,
    command_label: Option<String>,
    vscode_command: VscodeCommand,

    term_program: String,
    term_version: String,

    writer: BufWriter<Box<dyn std::io::Write + Send>>,
    parser_reply_writer: Option<BufWriter<Box<dyn std::io::Write + Send>>>,

    image_cache: lru::LruCache<[u8; 32], Arc<ImageData>>,
    sixel_scrolls_right: bool,

    user_vars: HashMap<String, String>,

    kitty_img: KittyImageState,
    inactive_kitty_img: KittyImageState,
    kitty_animation_time: Option<std::time::Instant>,
    seqno: SequenceNo,

    synchronized_output: bool,
    unscrolled_rows: usize,
    saved_private_modes: SavedPrivateModes,
    sgr_stack: sgr_stack::SgrStack,
    pointer_shapes: [pointer::Stack; 2],
    attribute_extent: u8,

    /// The unicode version that is in effect
    unicode_version: UnicodeVersion,
    unicode_version_stack: Vec<UnicodeVersionStackEntry>,

    enable_conpty_quirks: bool,
    /// On Windows, the ConPTY layer emits an OSC sequence to
    /// set the title shortly after it starts up.
    /// We don't want that, so we use this flag to remember
    /// whether we want to skip it or not.
    suppress_initial_title_change: bool,

    accumulating_title: Option<String>,

    /// seqno when we last lost focus
    lost_focus_seqno: SequenceNo,
    /// seqno when we last emitted Alert::OutputSinceFocusLost
    lost_focus_alerted_seqno: SequenceNo,
    focused: bool,

    /// True if lines should be marked as bidi-enabled, and thus
    /// have the renderer apply the bidi algorithm.
    /// true is equivalent to "implicit" bidi mode as described in
    /// <https://terminal-wg.pages.freedesktop.org/bidi/recommendation/basic-modes.html>
    /// If none, then the default value specified by the config is used.
    bidi_enabled: Option<bool>,
    /// When set, specifies the bidi direction information that should be
    /// applied to lines.
    /// If none, then the default value specified by the config is used.
    bidi_hint: Option<ParagraphDirectionHint>,
}

#[derive(Debug)]
struct UnicodeVersionStackEntry {
    vers: UnicodeVersion,
    label: Option<String>,
}

fn default_color_map() -> HashMap<u16, RgbColor> {
    let mut color_map = HashMap::new();
    // Match colors to the VT340 color table:
    // https://github.com/hackerb9/vt340test/blob/main/colormap/showcolortable.png
    for (idx, r, g, b) in [
        (0, 0, 0, 0),
        (1, 0x33, 0x33, 0xcc),
        (2, 0xcc, 0x23, 0x23),
        (3, 0x33, 0xcc, 0x33),
        (4, 0xcc, 0x33, 0xcc),
        (5, 0x33, 0xcc, 0xcc),
        (6, 0xcc, 0xcc, 0xcc),
        (7, 0x77, 0x77, 0x77),
        (8, 0x44, 0x44, 0x44),
        (9, 0x56, 0x56, 0x99),
        (10, 0x99, 0x44, 0x44),
        (11, 0x56, 0x99, 0x56),
        (12, 0x99, 0x56, 0x99),
        (13, 0x56, 0x99, 0x99),
        (14, 0x99, 0x99, 0x56),
        (15, 0xcc, 0xcc, 0xcc),
    ] {
        color_map.insert(idx, RgbColor::new_8bpc(r, g, b));
    }
    color_map
}

/// This struct implements a writer that sends the data across
/// to another thread so that the write side of the terminal
/// processing never blocks.
///
/// This is important for example when processing large pastes into
/// vim.  In that scenario, we can fill up the data pending
/// on vim's input buffer, while it is busy trying to send
/// output to the terminal.  A deadlock is reached because
/// send_paste blocks on the writer, but it is unable to make
/// progress until we're able to read the output from vim.
///
/// We either need input or output to be non-blocking.
/// Output seems safest because we want to be able to exert
/// back-pressure when there is a lot of data to read,
/// and we're in control of the write side, which represents
/// input from the interactive user, or pastes.
struct ThreadedWriter {
    sender: Sender<WriterMessage>,
}

enum WriterMessage {
    Data(Vec<u8>),
    Flush,
}

impl ThreadedWriter {
    fn new(mut writer: Box<dyn std::io::Write + Send>) -> Self {
        let (sender, receiver) = channel::<WriterMessage>();

        std::thread::spawn(move || {
            while let Ok(msg) = receiver.recv() {
                match msg {
                    WriterMessage::Data(buf) => {
                        if writer.write(&buf).is_err() {
                            break;
                        }
                    }
                    WriterMessage::Flush => {
                        if writer.flush().is_err() {
                            break;
                        }
                    }
                }
            }
        });

        Self { sender }
    }
}

impl std::io::Write for ThreadedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.sender
            .send(WriterMessage::Data(buf.to_vec()))
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::BrokenPipe, err))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.sender
            .send(WriterMessage::Flush)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::BrokenPipe, err))?;
        Ok(())
    }
}

impl TerminalState {
    pub(crate) fn reset_connection(&mut self) {
        let mut primary = self.screen.screen.clone();
        let rows = primary.physical_rows;
        primary.scroll_up(
            &(0..rows as i64),
            rows,
            self.seqno + 1,
            CellAttributes::default(),
            self.config.bidi_mode(),
        );
        primary.full_reset();
        self.increment_seqno();
        {
            let mut performer = performer::Performer::new(self);
            performer.perform(wezterm_escape_parser::Action::Esc(
                wezterm_escape_parser::Esc::Code(wezterm_escape_parser::EscCode::FullReset),
            ));
        }
        self.kitty_img = Default::default();
        self.inactive_kitty_img = Default::default();
        self.image_cache.clear();
        self.sgr_stack = Default::default();
        self.palette_stack = Default::default();
        self.screen.screen = primary;
        self.activate_screen(false);
    }

    /// Retained CPU image cache bytes (live screen/scrollback references are separate).
    pub fn image_cache_bytes(&self) -> usize {
        self.image_cache.iter().map(|(_, image)| image.len()).sum()
    }

    /// Constructs the terminal state.
    /// You generally want the `Terminal` struct rather than this one;
    /// Terminal contains and dereferences to `TerminalState`.
    pub fn new(
        size: TerminalSize,
        config: Arc<dyn TerminalConfiguration>,
        term_program: &str,
        term_version: &str,
        writer: Box<dyn std::io::Write + Send>,
    ) -> TerminalState {
        Self::with_nonblocking_writer(
            size,
            config,
            term_program,
            term_version,
            Box::new(ThreadedWriter::new(writer)),
        )
    }

    pub(crate) fn with_nonblocking_writer(
        size: TerminalSize,
        config: Arc<dyn TerminalConfiguration>,
        term_program: &str,
        term_version: &str,
        writer: Box<dyn std::io::Write + Send>,
    ) -> TerminalState {
        let seqno = 1;
        let screen = ScreenOrAlt::new(size, &config, seqno, config.bidi_mode());

        let color_map = default_color_map();

        let unicode_version = config.unicode_version();

        TerminalState {
            config,
            screen,
            pen: CellAttributes::default(),
            cursor: CursorPosition::default(),
            default_cursor_shape: CursorShape::Default,
            application_cursor_guide: false,
            top_and_bottom_margins: 0..size.rows as VisibleRowIndex,
            left_and_right_margins: 0..size.cols,
            left_and_right_margin_mode: false,
            wrap_next: false,
            clear_semantic_attribute_on_newline: false,
            // We default auto wrap to true even though the default for
            // a dec terminal is false, because it is more useful this way.
            dec_auto_wrap: true,
            reverse_wraparound_mode: false,
            reverse_video_mode: false,
            dec_origin_mode: false,
            insert: false,
            application_cursor_keys: false,
            modify_other_keys: 0,
            format_other_keys_csi_u: false,
            dec_ansi_mode: false,
            sixel_display_mode: false,
            use_private_color_registers_for_each_graphic: false,
            color_map,
            application_keypad: false,
            bracketed_paste: false,
            focus_tracking: false,
            report_color_scheme_changes: false,
            report_text_area_resize: false,
            mouse_encoding: MouseEncoding::X10,
            keyboard_encoding: KeyboardEncoding::Xterm,
            sixel_scrolls_right: false,
            any_event_mouse: false,
            button_event_mouse: false,
            mouse_tracking: false,
            last_mouse_move: None,
            cursor_visible: true,
            g0_charset: CharSet::Ascii,
            g1_charset: CharSet::Ascii,
            shift_out: false,
            newline_mode: false,
            current_mouse_buttons: vec![],
            tabs: TabStop::new(size.cols, 8),
            title: Default::default(),
            icon_title: None,
            saved_titles: Default::default(),
            palette: Default::default(),
            palette_stack: Default::default(),
            pixel_height: size.pixel_height,
            pixel_width: size.pixel_width,
            dpi: size.dpi,
            clipboard: None,
            device_control_handler: None,
            alert_handler: None,
            download_handler: None,
            current_dir: None,
            conemu_directory_reports: None,
            iterm_directory_peer: None,
            command_history: Arc::default(),
            next_command_id: 1,
            capturing_shell_input: false,
            track_primary_prints: false,
            last_primary_print: None,
            command_label: None,
            vscode_command: VscodeCommand::Idle,
            term_program: term_program.to_string(),
            term_version: term_version.to_string(),
            writer: BufWriter::new(writer),
            parser_reply_writer: None,
            image_cache: lru::LruCache::new(NonZeroUsize::new(16).unwrap()),
            user_vars: HashMap::new(),
            kitty_img: Default::default(),
            inactive_kitty_img: Default::default(),
            kitty_animation_time: None,
            synchronized_output: false,
            unscrolled_rows: 0,
            saved_private_modes: SavedPrivateModes::default(),
            sgr_stack: Default::default(),
            pointer_shapes: Default::default(),
            attribute_extent: 0,
            seqno,
            unicode_version,
            unicode_version_stack: vec![],
            suppress_initial_title_change: false,
            enable_conpty_quirks: false,
            accumulating_title: None,
            lost_focus_seqno: seqno,
            lost_focus_alerted_seqno: seqno,
            focused: true,
            bidi_enabled: None,
            bidi_hint: None,
            progress: Progress::default(),
            program_statuses: Default::default(),
            progress_value: 0,
        }
    }

    pub fn enable_conpty_quirks(&mut self) {
        self.enable_conpty_quirks = true;
        self.suppress_initial_title_change = true;
    }

    pub fn current_seqno(&self) -> SequenceNo {
        self.seqno
    }

    pub fn set_track_primary_prints(&mut self, track: bool) {
        self.track_primary_prints = track;
        self.last_primary_print = None;
    }

    pub fn last_primary_print(&self) -> Option<PrimaryPrint> {
        self.last_primary_print
    }

    pub fn synchronized_output(&self) -> bool {
        self.synchronized_output
    }

    pub fn in_band_resize_enabled(&self) -> bool {
        self.report_text_area_resize
    }

    /// Used by the embedding's watchdog; does not block parsing or input.
    pub fn end_synchronized_output(&mut self) {
        self.synchronized_output = false;
    }

    pub fn increment_seqno(&mut self) {
        self.seqno += 1;
    }

    /// Cumulative rows pulled out of scrollback by reverse scrolling.
    pub fn unscrolled_rows(&self) -> usize {
        self.unscrolled_rows
    }

    /// The program that requested notifications no longer owns this connection.
    pub fn clear_color_scheme_subscription(&mut self) {
        self.report_color_scheme_changes = false;
    }

    /// Stop asynchronous size reports when the program that requested them disconnects.
    pub fn clear_resize_subscription(&mut self) {
        self.report_text_area_resize = false;
    }

    pub fn set_config(&mut self, config: Arc<dyn TerminalConfiguration>) {
        let notify = self.report_color_scheme_changes
            && (self.config.prefers_light_colors() != config.prefers_light_colors()
                || self.config.color_palette() != config.color_palette());
        self.collect_palette_edits();
        self.config = config;
        self.rebuild_palette();
        if notify {
            // Config changes run outside parser dispatch. A framed viewer has a
            // distinct reply channel so this notification cannot become a keystroke.
            let reply = self
                .parser_reply_writer
                .as_mut()
                .unwrap_or(&mut self.writer);
            Self::write_color_scheme_report(reply, self.config.prefers_light_colors());
        }
    }

    fn write_color_scheme_report(writer: &mut impl std::io::Write, light: bool) {
        let report = if light {
            b"\x1b[?997;2n"
        } else {
            b"\x1b[?997;1n"
        };
        writer.write_all(report).ok();
        writer.flush().ok();
    }

    pub fn get_config(&self) -> Arc<dyn TerminalConfiguration> {
        Arc::clone(&self.config)
    }

    pub fn set_clipboard(&mut self, clipboard: &Arc<dyn Clipboard>) {
        self.clipboard.replace(Arc::clone(clipboard));
    }

    pub fn clear_clipboard(&mut self) {
        self.clipboard = None;
    }

    /// Install separate, nonblocking input and reply sinks for a framed viewer.
    /// Both sinks run on the parser thread to preserve order between writes.
    pub fn set_nonblocking_viewer_writers(
        &mut self,
        input: Box<dyn std::io::Write + Send>,
        reply: Box<dyn std::io::Write + Send>,
    ) {
        self.writer = BufWriter::new(input);
        self.parser_reply_writer = Some(BufWriter::new(reply));
    }

    pub(crate) fn swap_parser_writer(&mut self) {
        if let Some(reply) = &mut self.parser_reply_writer {
            std::mem::swap(&mut self.writer, reply);
        }
    }

    pub fn set_device_control_handler(&mut self, handler: Box<dyn DeviceControlHandler>) {
        self.device_control_handler.replace(handler);
    }

    pub fn set_notification_handler(&mut self, handler: Box<dyn AlertHandler>) {
        self.alert_handler.replace(handler);
    }

    /// Replace the input/reply sink when the host launches a new transport.
    /// Bytes still queued on the old writer remain bound to the old sink.
    pub fn replace_writer(&mut self, writer: Box<dyn std::io::Write + Send>) {
        self.replace_nonblocking_writer(Box::new(ThreadedWriter::new(writer)));
    }

    /// Replace a queue-backed sink without another thread reordering writes
    /// relative to resize requests on the same transport queue.
    pub fn replace_nonblocking_writer(&mut self, writer: Box<dyn std::io::Write + Send>) {
        self.writer = BufWriter::new(writer);
        self.parser_reply_writer = None;
    }

    pub fn set_download_handler(&mut self, handler: &Arc<dyn DownloadHandler>) {
        self.download_handler.replace(handler.clone());
    }

    /// Returns the title text associated with the terminal session.
    /// The title can be changed by the application using a number
    /// of escape sequences:
    /// OSC 2 is used to set the window title.
    /// OSC 1 is used to set the "icon title", which some terminal
    /// emulators interpret as a shorter title string for use when
    /// showing the tab title.
    /// Here in wezterm the terminalstate is isolated from other
    /// tabs; we process escape sequences without knowledge of other
    /// tabs, so we maintain both title strings here.
    /// The gui layer doesn't currently have a concept of what the
    /// overall window title should be beyond the title for the
    /// active tab with some decoration about the number of tabs.
    /// Shell toolkits such as oh-my-zsh prefer OSC 1 titles for
    /// abbreviated information.
    /// What we do here is prefer to return the OSC 1 icon title
    /// if it is set, otherwise return the OSC 2 window title.
    pub fn get_title(&self) -> &str {
        self.icon_title.as_deref().unwrap_or(&self.title.raw)
    }

    /// The window title supplied by the application, independently of OSC 1
    /// icon names. Full reset and a saved initial title clear its provenance.
    pub fn application_window_title(&self) -> Option<&Arc<str>> {
        self.title.from_application.then_some(&self.title.raw)
    }

    pub fn get_progress(&self) -> Progress {
        if self.program_statuses.seen_report {
            Progress::None
        } else {
            self.progress
        }
    }

    pub fn program_status(&self) -> Option<&Arc<ProgramStatus>> {
        self.program_statuses.summary.as_ref()
    }

    /// The attached process has ended. Finished and failed results stay visible.
    pub fn retire_running_program_statuses(&mut self) {
        self.program_statuses.process_ended();
    }

    /// Returns the current working directory associated with the
    /// terminal session.  The working directory can be changed by
    /// the applicaiton using the OSC 7 escape sequence.
    pub fn get_current_dir(&self) -> Option<&Url> {
        self.current_dir.as_ref().map(|(url, _)| url)
    }

    pub fn reported_current_dir(&self) -> Option<&Arc<str>> {
        self.current_dir.as_ref().map(|(_, raw)| raw)
    }

    /// A local Windows pseudoconsole may report drive paths using OSC 9;9.
    pub fn enable_conemu_directory_reports(&mut self) {
        self.conemu_directory_reports = Some(ConEmuDirectoryReports::Drive);
    }

    /// A local WSL pseudoconsole may also report a WSL share path.
    pub fn enable_wsl_directory_reports(&mut self) {
        self.conemu_directory_reports = Some(ConEmuDirectoryReports::Wsl);
    }

    fn set_reported_current_dir(&mut self, report: Option<(Url, Arc<str>)>) {
        self.current_dir = report;
        if let Some(handler) = self.alert_handler.as_mut() {
            handler.alert(Alert::CurrentWorkingDirectoryChanged);
        }
    }

    pub fn last_command(&self) -> Option<&Arc<CompletedCommand>> {
        self.command_history.back()
    }

    pub fn command_history(&self) -> &Arc<VecDeque<Arc<CompletedCommand>>> {
        &self.command_history
    }

    fn shell_column(&self) -> usize {
        if self.wrap_next {
            self.screen().physical_cols
        } else {
            self.cursor.x
        }
    }

    /// The cursor is still at the primary screen's last reported input boundary.
    pub fn at_shell_input_start(&self) -> bool {
        !self.is_alt_screen_active()
            && self.capturing_shell_input
            && self.screen.command_start
                == Some((
                    self.screen().visible_row_to_stable_row(self.cursor.y),
                    self.shell_column(),
                ))
    }

    fn mark_shell_position(&mut self) {
        if !self.is_alt_screen_active() {
            self.screen.command_start = Some((
                self.screen.visible_row_to_stable_row(self.cursor.y),
                self.shell_column(),
            ));
        }
    }

    fn begin_command_input(&mut self) {
        if !self.is_alt_screen_active() {
            self.command_label = None;
            self.capturing_shell_input = true;
            self.mark_shell_position();
        }
    }

    fn begin_command_output(&mut self, reported_command: Option<String>) {
        if !self.is_alt_screen_active() {
            self.command_label = reported_command.or_else(|| {
                if !self.capturing_shell_input {
                    return None;
                }
                let col = self.shell_column();
                self.screen
                    .take_command_text(self.cursor.y, col, 4096)
                    .ok()
                    .and_then(|mut text| {
                        text.truncate(text.trim_end_matches(['\r', '\n']).len());
                        (!text.is_empty()).then_some(text)
                    })
            });
            self.capturing_shell_input = false;
            self.mark_shell_position();
        }
    }

    fn next_shell_prompt(&mut self) {
        if !self.is_alt_screen_active() {
            if !self.capturing_shell_input && self.screen.command_start.is_some() {
                self.finish_command_output(None);
            }
            self.capturing_shell_input = false;
            self.screen.command_start = None;
            self.command_label = None;
        }
    }

    fn finish_command_output(&mut self, exit_code: Option<i32>) {
        if !self.is_alt_screen_active() {
            if self.capturing_shell_input {
                self.screen.command_start = None;
                self.capturing_shell_input = false;
            }
            self.notify_live_shell_output(true);
            let col = self.shell_column();
            self.screen
                .remember_command_output(self.next_command_id, (self.cursor.y, col));
            let output = self
                .screen
                .take_command_text(self.cursor.y, col, 1024 * 1024);
            let command = Arc::new(CompletedCommand {
                id: self.next_command_id,
                command: self.command_label.take(),
                exit_code,
                output,
            });
            self.next_command_id += 1;
            let history = Arc::make_mut(&mut self.command_history);
            history.push_back(command);
            let mut bytes: usize = history.iter().map(|entry| entry.text_bytes()).sum();
            while history.len() > 64 || bytes > 4 * 1024 * 1024 {
                let old = history.pop_front().unwrap();
                bytes -= old.text_bytes();
                self.screen.forget_command_output(old.id);
            }
        }
    }

    pub fn notify_running_shell_output(&mut self) {
        self.notify_live_shell_output(false);
    }

    fn notify_live_shell_output(&mut self, finishing: bool) {
        if self.is_alt_screen_active() || self.capturing_shell_input {
            return;
        }
        let col = self.shell_column();
        let (Some(output), Some(handler)) = (
            self.screen
                .live_command_output(self.cursor.y, col, finishing),
            self.alert_handler.as_mut(),
        ) else {
            return;
        };
        handler.live_shell_output(self.next_command_id, self.command_label.as_deref(), output);
    }

    pub fn command_output_row(&self, id: u64) -> Option<StableRowIndex> {
        if self.is_alt_screen_active() {
            return None;
        }
        self.screen.command_output_row(id)
    }

    /// The command whose output began above this row and still covers it.
    pub fn command_context(&self, row: StableRowIndex) -> Option<(StableRowIndex, &str)> {
        if self.is_alt_screen_active() {
            return None;
        }
        let screen = self.screen();
        if let (false, Some((start, _)), Some(label)) = (
            self.capturing_shell_input,
            screen.command_start,
            self.command_label.as_deref(),
        ) {
            if start < row
                && screen.stable_row_to_phys(start).is_some()
                && (row, 0)
                    < (
                        screen.visible_row_to_stable_row(self.cursor.y),
                        self.shell_column(),
                    )
            {
                return Some((start, label));
            }
        }
        let (id, start) = screen.command_output_covering(row)?;
        let index = self
            .command_history
            .binary_search_by_key(&id, |entry| entry.id)
            .ok()?;
        Some((start, self.command_history[index].command.as_deref()?))
    }

    /// Returns a copy of the palette.
    /// By default we don't keep a copy in the terminal state,
    /// preferring to take the config values from the users
    /// config file and updating to changes live.
    /// However, if they have used dynamic color scheme escape
    /// sequences we'll fork a copy of the palette at that time
    /// so that we can start tracking those changes.
    pub fn palette(&self) -> ColorPalette {
        let mut palette = self
            .palette
            .effective
            .as_ref()
            .cloned()
            .unwrap_or_else(|| self.config.color_palette());
        self.palette.resolve_cursor_colors(&mut palette);
        palette
    }

    /// Edit the effective palette directly. Changes made through this reference
    /// are reconciled before a subsequent scheme change or OSC mutation.
    pub fn palette_mut(&mut self) -> &mut ColorPalette {
        self.palette.borrowed_mutably = true;
        self.materialize_palette()
    }

    /// Release a directly edited palette after restoring the configured colors.
    /// Explicit OSC assignments still belong to the application until it resets them.
    pub fn implicit_palette_reset_if_same_as_configured(&mut self) {
        self.collect_palette_edits();
        if self.palette.assigned.is_empty() {
            self.palette.effective = None;
        }
    }

    /// Returns a reference to the active screen (either the primary or
    /// the alternate screen).
    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    /// Returns a mutable reference to the active screen (either the primary or
    /// the alternate screen).
    pub fn screen_mut(&mut self) -> &mut Screen {
        &mut self.screen
    }

    fn set_clipboard_contents(
        &self,
        selection: ClipboardSelection,
        text: Option<String>,
    ) -> anyhow::Result<()> {
        if let Some(clip) = self.clipboard.as_ref() {
            clip.set_contents(selection, text)?;
        }
        Ok(())
    }

    pub fn erase_scrollback_and_viewport(&mut self) {
        // Since we may be called outside of perform_actions,
        // we need to ensure that we increment the seqno in
        // order to correctly invalidate the display
        self.increment_seqno();
        self.erase_in_display(EraseInDisplay::EraseScrollback);

        let row_index = self.screen.phys_row(self.cursor.y);
        let rows = self.screen.lines_in_phys_range(row_index..row_index + 1);

        self.erase_in_display(EraseInDisplay::EraseDisplay);

        self.screen.placement_rows_changed();
        for (idx, row) in rows.into_iter().enumerate() {
            *self.screen.line_mut(idx) = row;
        }

        self.cursor.y = 0;
    }

    /// Discards the scrollback, leaving only the data that is present
    /// in the viewport.
    pub fn erase_scrollback(&mut self) {
        // Since we may be called outside of perform_actions,
        // we need to ensure that we increment the seqno in
        // order to correctly invalidate the display
        self.increment_seqno();
        self.screen_mut().erase_scrollback();
    }

    /// Returns true if the associated application has enabled any of the
    /// supported mouse reporting modes.
    /// This is useful for the hosting GUI application to decide how best
    /// to dispatch mouse events to the terminal.
    pub fn is_mouse_grabbed(&self) -> bool {
        self.mouse_tracking || self.button_event_mouse || self.any_event_mouse
    }

    fn activate_screen(&mut self, alternate: bool) {
        if self.screen.is_alt_screen_active() != alternate {
            std::mem::swap(&mut self.kitty_img, &mut self.inactive_kitty_img);
            if let Some(now) = self.kitty_animation_time {
                self.advance_kitty_animations(now);
            }
        }
        if alternate {
            self.screen.activate_alt_screen(self.seqno);
        } else {
            self.screen.activate_primary_screen(self.seqno);
        }
    }

    /// Returns true if the alternate screen (secondary screen buffer) is currently active.
    /// This is useful for the hosting GUI application to decide how best
    /// to handle operations like scrollback that only apply to the primary screen.
    pub fn is_alt_screen_active(&self) -> bool {
        self.screen.is_alt_screen_active()
    }

    /// Returns true if the associated application has enabled basic mouse
    /// tracking mode (DEC mode 1000).
    /// This is useful for the hosting GUI application to decide how best
    /// to dispatch mouse events to the terminal.
    pub fn mouse_tracking_enabled(&self) -> bool {
        self.mouse_tracking
    }

    /// Returns true if the associated application has enabled button-event mouse
    /// tracking mode (DEC mode 1002), which reports motion events while a button is held.
    /// This is useful for the hosting GUI application to decide how best
    /// to dispatch mouse events to the terminal.
    pub fn button_event_mouse_enabled(&self) -> bool {
        self.button_event_mouse
    }

    /// Returns true if the associated application has enabled any-event mouse
    /// tracking mode (DEC mode 1003), which reports all motion events regardless of button state.
    /// This is useful for the hosting GUI application to decide how best
    /// to dispatch mouse events to the terminal.
    pub fn any_event_mouse_enabled(&self) -> bool {
        self.any_event_mouse
    }

    /// Returns the current mouse reporting encoding selected by the associated application.
    pub fn get_mouse_encoding(&self) -> MouseEncoding {
        self.mouse_encoding
    }

    /// Returns true if the associated application has enabled focus tracking mode
    /// (DEC mode 1004), which causes focus-in and focus-out events to be reported.
    pub fn focus_tracking_enabled(&self) -> bool {
        self.focus_tracking
    }

    /// Returns true if the associated application has enabled application cursor keys mode
    /// (DEC mode 1), which causes cursor keys to send application sequences instead of ANSI ones.
    pub fn application_cursor_keys_enabled(&self) -> bool {
        self.application_cursor_keys
    }

    /// Returns true if the associated application has enabled the special encoding of the numeric
    /// keypad portion of the keyboard.
    pub fn application_keypad_enabled(&self) -> bool {
        self.application_keypad
    }

    /// Returns true if DECAWM auto-wrap mode (DEC mode 7) is enabled, which causes
    /// cursor to wrap to next line when it reaches right border.
    pub fn dec_auto_wrap_enabled(&self) -> bool {
        self.dec_auto_wrap
    }

    /// The DECSTBM scroll region, as inclusive-start/exclusive-end visible rows.
    pub fn get_top_and_bottom_margins(&self) -> Range<VisibleRowIndex> {
        self.top_and_bottom_margins.clone()
    }

    /// Returns true if DEC left/right margin mode (DECLRMM) is enabled.
    /// Use [`Self::get_left_and_right_margins`] to get those margin values.
    pub fn left_and_right_margin_mode_enabled(&self) -> bool {
        self.left_and_right_margin_mode
    }

    /// The inner horizontal region between the DECSLRM margins.
    /// `start` is the left margin column, `end` is one past the right margin column.
    /// Only effective when [`Self::left_and_right_margin_mode_enabled`] is true.
    pub fn get_left_and_right_margins(&self) -> Range<usize> {
        self.left_and_right_margins.clone()
    }

    /// Returns true if DEC origin mode (DECOM) is enabled.
    pub fn dec_origin_mode_enabled(&self) -> bool {
        self.dec_origin_mode
    }

    /// Returns true if insert mode (IRM) is enabled.
    pub fn insert_mode_enabled(&self) -> bool {
        self.insert
    }

    /// Returns true if shift-out is enabled: G1 charset is active for display.
    /// Reset by shift-in, which restores G0 charset.
    pub fn shift_out_enabled(&self) -> bool {
        self.shift_out
    }

    /// The Character Set Selection (SCS) charset designated for G0.
    pub fn g0_charset(&self) -> CharSet {
        self.g0_charset
    }

    /// The Character Set Selection (SCS) charset designated for G1.
    pub fn g1_charset(&self) -> CharSet {
        self.g1_charset
    }

    /// Tab stops by column: `true` where a stop is set.
    /// Length tracks the screen width, so index is the column.
    pub fn tab_stops_by_column(&self) -> Vec<bool> {
        self.tabs.tabs.clone()
    }

    /// Returns true if the associated application has enabled
    /// bracketed paste mode, which can be helpful to the hosting
    /// GUI application to decide about fragmenting a large paste.
    pub fn bracketed_paste_enabled(&self) -> bool {
        self.bracketed_paste
    }

    /// Advise the terminal about a change in its focus state
    pub fn focus_changed(&mut self, focused: bool) {
        if focused == self.focused {
            return;
        }
        if !focused {
            // notify app of release of buttons
            let buttons = self.current_mouse_buttons.clone();
            for b in buttons {
                self.mouse_event(MouseEvent {
                    kind: MouseEventKind::Release,
                    button: b,
                    modifiers: KeyModifiers::NONE,
                    x: 0,
                    y: 0,
                    x_pixel_offset: 0,
                    y_pixel_offset: 0,
                })
                .ok();
            }
        }
        if self.focus_tracking {
            write!(self.writer, "{}{}", CSI, if focused { "I" } else { "O" }).ok();
            self.writer.flush().ok();
        }
        self.set_focus_without_reporting(focused);
    }

    /// Keep local focus current while no interactive peer can receive events.
    pub fn set_focus_without_reporting(&mut self, focused: bool) {
        if focused == self.focused {
            return;
        }
        self.focused = focused;
        if !focused {
            self.lost_focus_seqno = self.seqno;
        }
    }

    /// Returns true if there is new output since the terminal
    /// lost focus
    pub fn has_unseen_output(&self) -> bool {
        !self.focused && self.seqno > self.lost_focus_seqno
    }

    pub(crate) fn trigger_unseen_output_notif(&mut self) {
        if self.has_unseen_output() {
            // We want to avoid over-notifying about output events,
            // so here we gate the notification to the case where
            // we have lost the focus more recently than the last
            // time we notified about it
            if self.lost_focus_seqno > self.lost_focus_alerted_seqno {
                self.lost_focus_alerted_seqno = self.seqno;
                if let Some(handler) = self.alert_handler.as_mut() {
                    handler.alert(Alert::OutputSinceFocusLost);
                }
            }
        }
    }

    /// Encode text to send to the terminal as a paste.
    /// If bracketed paste mode is enabled, the paste is enclosed
    /// in the bracketing, otherwise it is fed to the writer as-is.
    /// De-fang the text by removing any embedded bracketed paste
    /// sequence that may be present.
    pub fn encode_paste(&self, text: &str) -> String {
        let mut buf = String::new();
        if self.bracketed_paste {
            buf.push_str("\x1b[200~");
        }

        let canon = if self.bracketed_paste {
            NewlineCanon::None
        } else {
            self.config.canonicalize_pasted_newlines()
        };

        let canon = canon.canonicalize(text);
        let de_fanged = canon.replace("\x1b[200~", "").replace("\x1b[201~", "");
        buf.push_str(&de_fanged);

        if self.bracketed_paste {
            buf.push_str("\x1b[201~");
        }

        buf
    }

    pub fn send_paste(&mut self, text: &str) -> Result<(), Error> {
        let buf = self.encode_paste(text);
        self.writer.write_all(buf.as_bytes())?;
        self.writer.flush()?;
        Ok(())
    }

    /// Informs the terminal that the viewport of the window has resized to the
    /// specified dimensions.
    /// We need to resize both the primary and alt screens, adjusting
    /// the cursor positions of both accordingly.
    pub fn resize(&mut self, size: TerminalSize) {
        let previous = self
            .report_text_area_resize
            .then(|| self.text_area_dimensions());
        self.increment_seqno();
        let (cursor_main, cursor_alt) = if self.screen.alt_screen_is_active {
            (
                self.screen
                    .screen
                    .saved_cursor
                    .as_ref()
                    .map(|s| s.position)
                    .unwrap_or_else(CursorPosition::default),
                self.cursor,
            )
        } else {
            (
                self.cursor,
                self.screen
                    .alt_screen
                    .saved_cursor
                    .as_ref()
                    .map(|s| s.position)
                    .unwrap_or_else(CursorPosition::default),
            )
        };

        let (adjusted_cursor_main, adjusted_cursor_alt) = self.screen.resize(
            size,
            cursor_main,
            cursor_alt,
            self.seqno,
            self.enable_conpty_quirks,
        );
        self.top_and_bottom_margins = 0..size.rows as i64;
        self.left_and_right_margins = 0..size.cols;
        self.pixel_height = size.pixel_height;
        self.pixel_width = size.pixel_width;
        self.dpi = size.dpi;
        self.tabs.resize(size.cols);

        if self.screen.alt_screen_is_active {
            self.set_cursor_pos(
                &Position::Absolute(adjusted_cursor_alt.x as i64),
                &Position::Absolute(adjusted_cursor_alt.y),
            );

            if let Some(saved) = self.screen.screen.saved_cursor.as_mut() {
                saved.position.x = adjusted_cursor_main.x;
                saved.position.y = adjusted_cursor_main.y;
                saved.position.seqno = self.seqno;
                saved.wrap_next = false;
            }
        } else {
            self.set_cursor_pos(
                &Position::Absolute(adjusted_cursor_main.x as i64),
                &Position::Absolute(adjusted_cursor_main.y),
            );
            if let Some(saved) = self.screen.alt_screen.saved_cursor.as_mut() {
                saved.position.x = adjusted_cursor_alt.x;
                saved.position.y = adjusted_cursor_alt.y;
                saved.position.seqno = self.seqno;
                saved.wrap_next = false;
            }
        }

        if let Some(previous) = previous {
            let current = self.text_area_dimensions();
            if current != previous {
                // Resizes happen outside parser dispatch. Preserve the separate
                // automatic-reply channel on a framed session.
                let reply = self
                    .parser_reply_writer
                    .as_mut()
                    .unwrap_or(&mut self.writer);
                Self::write_resize_report(reply, current);
            }
        }
    }

    /// Mode 2048 uses height before width, first in cells and then in pixels.
    fn text_area_dimensions(&self) -> [usize; 4] {
        let size = self.get_size();
        [size.rows, size.cols, size.pixel_height, size.pixel_width]
    }

    fn write_resize_report(writer: &mut impl Write, [rows, cols, height, width]: [usize; 4]) {
        write!(writer, "\x1b[48;{rows};{cols};{height};{width}t").ok();
        writer.flush().ok();
    }

    pub fn get_size(&self) -> TerminalSize {
        let screen = self.screen();
        TerminalSize {
            dpi: self.dpi,
            pixel_width: self.pixel_width,
            pixel_height: self.pixel_height,
            rows: screen.physical_rows,
            cols: screen.physical_cols,
        }
    }

    fn palette_did_change(&mut self) {
        self.make_all_lines_dirty();
        if let Some(handler) = self.alert_handler.as_mut() {
            handler.alert(Alert::PaletteChanged);
        }
    }

    /// When dealing with selection, mark a range of lines as dirty
    pub fn make_all_lines_dirty(&mut self) {
        let seqno = self.seqno;
        let screen = self.screen_mut();
        screen.for_each_phys_line_mut(|_, line| {
            line.update_last_change_seqno(seqno);
        });
    }

    /// Returns the 0-based cursor position relative to the top left of
    /// the visible screen
    pub fn cursor_pos(&self) -> CursorPosition {
        CursorPosition {
            x: self.cursor.x,
            y: self.cursor.y,
            shape: self.effective_cursor_shape(),
            visibility: if self.cursor_visible {
                CursorVisibility::Visible
            } else {
                CursorVisibility::Hidden
            },
            seqno: self.cursor.seqno,
        }
    }

    fn effective_cursor_shape(&self) -> CursorShape {
        if self.cursor.shape == CursorShape::Default {
            self.default_cursor_shape
        } else {
            self.cursor.shape
        }
    }

    /// Set the local fallback for a cursor with the default DECSCUSR style.
    pub fn set_default_cursor_shape(&mut self, shape: CursorShape) -> bool {
        let previous = self.effective_cursor_shape();
        self.default_cursor_shape = shape;
        previous != self.effective_cursor_shape()
    }

    pub fn application_cursor_guide(&self) -> bool {
        self.application_cursor_guide
    }

    /// Returns the current cell attributes of the screen
    pub fn pen(&self) -> CellAttributes {
        self.pen.clone()
    }

    pub fn user_vars(&self) -> &HashMap<String, String> {
        &self.user_vars
    }

    fn clear_semantic_attribute_due_to_movement(&mut self) {
        if self.clear_semantic_attribute_on_newline {
            self.clear_semantic_attribute_on_newline = false;
            self.pen.set_semantic_type(SemanticType::default());
        }
    }

    /// Limit the cursor to the addressable columns of a DEC-sized row.
    fn constrain_line_column(&self, x: usize, y: VisibleRowIndex) -> usize {
        if self.screen().line_width_scale(y) == 1 {
            x
        } else {
            x.min(self.screen().logical_cols(y) - 1)
        }
    }

    /// Set the cursor position in visible screen coordinates.
    fn set_cursor_position_absolute(&mut self, x: usize, y: VisibleRowIndex) {
        if self.cursor.y != y {
            self.clear_semantic_attribute_due_to_movement();
        }
        self.cursor.y = y;
        self.cursor.x = self.constrain_line_column(x, y);
        self.cursor.seqno = self.seqno;
        self.wrap_next = false;
    }

    /// Sets the cursor position. x and y are 0-based and relative to the
    /// top left of the visible screen.
    fn set_cursor_pos(&mut self, x: &Position, y: &Position) {
        let x = match *x {
            Position::Relative(x) => (self.cursor.x as i64 + x)
                .min(
                    if self.dec_origin_mode {
                        self.left_and_right_margins.end
                    } else {
                        self.screen().physical_cols
                    } as i64
                        - 1,
                )
                .max(0),
            Position::Absolute(x) => (x + if self.dec_origin_mode {
                self.left_and_right_margins.start
            } else {
                0
            } as i64)
                .min(
                    if self.dec_origin_mode {
                        self.left_and_right_margins.end
                    } else {
                        // We allow 1 extra for the cursor x position
                        // to account for some resize/rewrap scenarios
                        // where we don't want to forget that the
                        // cursor belongs to a wrapped line
                        self.screen().physical_cols + 1
                    } as i64
                        - 1,
                )
                .max(0),
        };

        let y = match *y {
            Position::Relative(y) => (self.cursor.y + y)
                .min(
                    if self.dec_origin_mode {
                        self.top_and_bottom_margins.end
                    } else {
                        self.screen().physical_rows as i64
                    } - 1,
                )
                .max(0),
            Position::Absolute(y) => (y + if self.dec_origin_mode {
                self.top_and_bottom_margins.start
            } else {
                0
            })
            .min(
                if self.dec_origin_mode {
                    self.top_and_bottom_margins.end
                } else {
                    self.screen().physical_rows as i64
                } - 1,
            )
            .max(0),
        };

        self.set_cursor_position_absolute(x as usize, y);
    }

    fn scroll_up(&mut self, num_rows: usize) {
        let seqno = self.seqno;
        let blank_attr = self.pen.clone_sgr_only();
        let top_and_bottom_margins = self.top_and_bottom_margins.clone();
        let left_and_right_margins = self.left_and_right_margins.clone();
        let bidi_mode = self.get_bidi_mode();
        let count =
            num_rows.min((top_and_bottom_margins.end - top_and_bottom_margins.start) as usize);
        let graphics = self.prepare_kitty_margin_scroll(
            &top_and_bottom_margins,
            &left_and_right_margins,
            -(count as VisibleRowIndex),
        );
        self.screen_mut().scroll_up_within_margins(
            &top_and_bottom_margins,
            &left_and_right_margins,
            num_rows,
            seqno,
            blank_attr,
            bidi_mode,
        );
        self.finish_kitty_tile_update(graphics);
        self.cursor.x = self.constrain_line_column(self.cursor.x, self.cursor.y);
    }

    fn scroll_down(&mut self, num_rows: usize) {
        let seqno = self.seqno;
        let blank_attr = self.pen.clone_sgr_only();
        let top_and_bottom_margins = self.top_and_bottom_margins.clone();
        let left_and_right_margins = self.left_and_right_margins.clone();
        let bidi_mode = self.get_bidi_mode();
        let count =
            num_rows.min((top_and_bottom_margins.end - top_and_bottom_margins.start) as usize);
        let graphics = self.prepare_kitty_margin_scroll(
            &top_and_bottom_margins,
            &left_and_right_margins,
            count as VisibleRowIndex,
        );
        self.screen_mut().scroll_down_within_margins(
            &top_and_bottom_margins,
            &left_and_right_margins,
            num_rows,
            seqno,
            blank_attr,
            bidi_mode,
        );
        self.finish_kitty_tile_update(graphics);
        self.cursor.x = self.constrain_line_column(self.cursor.x, self.cursor.y);
    }

    fn scroll_down_from_history(&mut self, num_rows: usize) {
        let screen = self.screen();
        let history = if !self.is_alt_screen_active()
            && self.top_and_bottom_margins.start == 0
            && self.left_and_right_margins == (0..screen.physical_cols)
        {
            screen.phys_row(0)
        } else {
            0
        };
        let count = num_rows.min(screen.physical_rows.max(history));
        let restored = count.min(history);
        if restored != 0 {
            let bottom = self.top_and_bottom_margins.end;
            let seqno = self.seqno;
            self.screen_mut()
                .restore_scrollback_rows(bottom, restored, seqno);
            self.unscrolled_rows = self.unscrolled_rows.wrapping_add(restored);
        }
        if count > restored {
            self.scroll_down(count - restored);
        } else if restored != 0 {
            self.cursor.x = self.constrain_line_column(self.cursor.x, self.cursor.y);
        }
    }

    /// Defined by FinalTermSemanticPrompt; a fresh-line is a NOP if the
    /// cursor is already at the left margin, otherwise it is the same as
    /// a new line.
    fn fresh_line(&mut self) {
        if self.cursor.x == self.left_and_right_margins.start {
            return;
        }
        self.new_line(true);
    }

    fn new_line(&mut self, move_to_first_column: bool) {
        let x = if move_to_first_column {
            self.left_and_right_margins.start
        } else {
            self.cursor.x
        };
        let y = self.cursor.y;
        let y = if y == self.top_and_bottom_margins.end - 1 {
            self.scroll_up(1);
            y
        } else {
            y + 1
        };
        self.set_cursor_pos(&Position::Absolute(x as i64), &Position::Absolute(y as i64));
    }

    /// Moves the cursor down one line in the same column.
    /// If the cursor is at the bottom margin, the page scrolls up.
    fn c1_index(&mut self) {
        if self.left_and_right_margins.contains(&self.cursor.x) {
            if self.cursor.y == self.top_and_bottom_margins.end - 1 {
                self.scroll_up(1);
            } else {
                self.set_cursor_pos(&Position::Relative(0), &Position::Relative(1));
            }
        }
    }

    /// Moves the cursor to the first position on the next line.
    /// If the cursor is at the bottom margin, the page scrolls up.
    fn c1_nel(&mut self) {
        let y_clamp = if self.top_and_bottom_margins.contains(&self.cursor.y) {
            self.top_and_bottom_margins.end - 1
        } else {
            self.screen().physical_rows as VisibleRowIndex - 1
        };

        if self.left_and_right_margins.contains(&self.cursor.x) {
            if self.cursor.y == self.top_and_bottom_margins.end - 1 {
                self.scroll_up(1);
                self.set_cursor_position_absolute(self.left_and_right_margins.start, self.cursor.y);
            } else {
                self.set_cursor_position_absolute(
                    self.left_and_right_margins.start,
                    (self.cursor.y + 1).min(y_clamp),
                );
            }
        } else {
            // When outside left/right margins, NEL moves but does not scroll
            self.set_cursor_position_absolute(
                if self.cursor.x < self.left_and_right_margins.start {
                    self.cursor.x
                } else {
                    self.left_and_right_margins.start
                },
                (self.cursor.y + 1).min(y_clamp),
            );
        }
    }

    /// Sets a horizontal tab stop at the column where the cursor is.
    fn c1_hts(&mut self) {
        self.tabs.set_tab_stop(self.cursor.x);
    }

    /// Moves the cursor to the next tab stop. If there are no more tab stops,
    /// the cursor moves to the right margin. HT does not cause text to auto
    /// wrap.
    fn c0_horizontal_tab(&mut self) {
        self.horizontal_tabs_forward(0);
    }

    fn horizontal_tabs_forward(&mut self, skip: usize) {
        let right = self
            .left_and_right_margins
            .end
            .min(self.screen().logical_cols(self.cursor.y))
            - 1;
        self.cursor.x = self.tabs.forward(self.cursor.x, right, skip);
        self.cursor.seqno = self.seqno;
    }

    /// Move the cursor up 1 line.  If the position is at the top scroll margin,
    /// scroll the region down.
    fn c1_reverse_index(&mut self) {
        if self.left_and_right_margins.contains(&self.cursor.x) {
            if self.cursor.y == self.top_and_bottom_margins.start {
                self.scroll_down(1);
            } else {
                self.set_cursor_pos(&Position::Relative(0), &Position::Relative(-1));
            }
        }
    }

    fn set_hyperlink(&mut self, link: Option<Hyperlink>) {
        self.pen.set_hyperlink(match link {
            Some(hyperlink) => Some(Arc::new(hyperlink)),
            None => None,
        });
    }

    /// <https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h4-Device-Control-functions:DCS-plus-q-Pt-ST.F95>
    /// XTGETTCAP
    fn xt_get_tcap(&mut self, names: Vec<String>) {
        let mut res = String::new();

        for name in &names {
            res.push_str("\x1bP");

            let encoded_name = hex::encode_upper(&name);
            match name.as_str() {
                "TN" | "name" => {
                    res.push_str("1+r");
                    res.push_str(&encoded_name);
                    res.push('=');

                    let encoded_val = hex::encode_upper(&self.term_program);
                    res.push_str(&encoded_val);
                }

                "Co" | "colors" => {
                    res.push_str("1+r");
                    res.push_str(&encoded_name);
                    res.push('=');
                    let encoded_val = hex::encode_upper("256");
                    res.push_str(&encoded_val);
                }

                "RGB" => {
                    res.push_str("1+r");
                    res.push_str(&encoded_name);
                    res.push('=');
                    let encoded_val = hex::encode_upper("8/8/8");
                    res.push_str(&encoded_val);
                }

                _ => {
                    if let Some(value) = DB.raw(name) {
                        res.push_str("1+r");
                        res.push_str(&encoded_name);
                        res.push('=');
                        let value = match value {
                            Value::True => hex::encode_upper("1"),
                            Value::Number(n) => hex::encode_upper(&n.to_string()),
                            Value::String(s) => hex::encode_upper(s),
                        };
                        res.push_str(&value);
                    } else {
                        log::trace!("xt_get_tcap: unknown name {}", name);
                        res.push_str("0+r");
                        res.push_str(&encoded_name);
                    }
                }
            }
            res.push_str("\x1b\\");
        }

        log::trace!(
            "XTGETTCAP {:?} responding with {}",
            names,
            res.escape_debug()
        );
        self.writer.write_all(res.as_bytes()).ok();
        self.writer.flush().ok();
    }

    fn perform_device(&mut self, dev: Device) {
        match dev {
            Device::DeviceAttributes(a) => {
                if self.config.log_unknown_escape_sequences() {
                    log::warn!("unhandled: {:?}", a);
                }
            }
            Device::SoftReset => {
                self.pointer_shapes = Default::default();
                if let Some(handler) = self.alert_handler.as_mut() {
                    handler.terminal_reset();
                }
                // TODO: see https://vt100.net/docs/vt510-rm/DECSTR.html
                self.pen = CellAttributes::default();
                self.attribute_extent = 0;
                self.insert = false;
                self.dec_origin_mode = false;
                // Note that xterm deviates from the documented DECSTR
                // setting for dec_auto_wrap, so we do too
                self.dec_auto_wrap = true;
                self.application_cursor_keys = false;
                self.modify_other_keys = 0;
                self.format_other_keys_csi_u = false;
                self.application_keypad = false;
                self.top_and_bottom_margins = 0..self.screen().physical_rows as i64;
                self.left_and_right_margins = 0..self.screen().physical_cols;
                self.left_and_right_margin_mode = false;
                self.activate_screen(true);
                self.reset_kitty_images();
                self.screen.saved_cursor().take();
                self.activate_screen(false);
                self.screen.saved_cursor().take();
                self.reset_kitty_images();

                self.reverse_wraparound_mode = false;
                self.reverse_video_mode = false;
                self.bidi_enabled.take();
                self.bidi_hint.take();

                self.g0_charset = CharSet::Ascii;
                self.g1_charset = CharSet::Ascii;
            }
            Device::RequestPrimaryDeviceAttributes => {
                let mut ident = "\x1b[?65".to_string(); // Vt500
                ident.push_str(";4"); // Sixel graphics
                ident.push_str(";6"); // Selective erase
                ident.push_str(";18"); // windowing extensions
                ident.push_str(";22"); // ANSI color, vt525
                if self
                    .clipboard
                    .as_ref()
                    .is_some_and(|clip| clip.writes_enabled())
                {
                    ident.push_str(";52"); // OSC 52 clipboard writes
                }
                ident.push('c');

                self.writer.write(ident.as_bytes()).ok();
                self.writer.flush().ok();
            }
            Device::RequestSecondaryDeviceAttributes => {
                // Response is: Pp ; Pv ; Pc
                // Where Pp=1 means vt220
                // and Pv is the firmware version.
                // Pc is always 0.
                // Because our default TERM is xterm, the firmware
                // version will be considered to be equialent to xterm's
                // patch levels, with the following effects:
                // pv < 95 -> ttymouse=xterm
                // pv >= 95 < 277 -> ttymouse=xterm2
                // pv >= 277 -> ttymouse=sgr
                // pv >= 279 - xterm will probe for additional device settings.
                self.writer.write(b"\x1b[>1;277;0c").ok();
                self.writer.flush().ok();
            }
            Device::RequestTertiaryDeviceAttributes => {
                self.writer
                    .write(format!("\x1bP!|00000000{}", ST).as_bytes())
                    .ok();
                self.writer.flush().ok();
            }
            Device::RequestTerminalNameAndVersion => {
                self.writer.write(DCS.as_bytes()).ok();
                self.writer
                    .write(
                        format!(">|{} {}{}", self.term_program, self.term_version, ST).as_bytes(),
                    )
                    .ok();
                self.writer.flush().ok();
            }
            Device::RequestTerminalParameters(a) => {
                self.writer
                    .write(format!("\x1b[{};1;1;128;128;1;0x", a + 2).as_bytes())
                    .ok();
                self.writer.flush().ok();
            }
            Device::StatusReport => {
                self.writer.write(b"\x1b[0n").ok();
                self.writer.flush().ok();
            }
            Device::XtSmGraphics(g) => {
                let response = if matches!(g.item, XtSmGraphicsItem::Unspecified(_)) {
                    XtSmGraphics {
                        item: g.item,
                        action_or_status: XtSmGraphicsStatus::InvalidItem.to_i64(),
                        value: vec![],
                    }
                } else {
                    match g.action() {
                        None | Some(XtSmGraphicsAction::SetToValue) => XtSmGraphics {
                            item: g.item,
                            action_or_status: XtSmGraphicsStatus::InvalidAction.to_i64(),
                            value: vec![],
                        },
                        Some(XtSmGraphicsAction::ResetToDefault) => XtSmGraphics {
                            item: g.item,
                            action_or_status: XtSmGraphicsStatus::Success.to_i64(),
                            value: vec![],
                        },
                        Some(XtSmGraphicsAction::ReadMaximumAllowedValue)
                        | Some(XtSmGraphicsAction::ReadAttribute) => match g.item {
                            XtSmGraphicsItem::Unspecified(_) => unreachable!("checked above"),
                            XtSmGraphicsItem::NumberOfColorRegisters => XtSmGraphics {
                                item: g.item,
                                action_or_status: XtSmGraphicsStatus::Success.to_i64(),
                                value: vec![65536],
                            },
                            XtSmGraphicsItem::RegisGraphicsGeometry
                            | XtSmGraphicsItem::SixelGraphicsGeometry => XtSmGraphics {
                                item: g.item,
                                action_or_status: XtSmGraphicsStatus::Success.to_i64(),
                                value: vec![self.pixel_width as i64, self.pixel_height as i64],
                            },
                        },
                    }
                };

                let dev = Device::XtSmGraphics(response);

                write!(self.writer, "\x1b[{}", dev).ok();
                self.writer.flush().ok();
            }
        }
    }

    /// Indicates that mode is permanently enabled
    fn decqrm_response_permanent(&mut self, mode: Mode) {
        let (is_dec, number) = match &mode {
            Mode::QueryDecPrivateMode(code) => (true, Self::private_mode_number(code)),
            Mode::QueryMode(TerminalMode::Code(code)) => (false, code.to_u16().unwrap()),
            Mode::QueryMode(TerminalMode::Unspecified(code)) => (false, *code),
            _ => unreachable!(),
        };

        let prefix = if is_dec { "?" } else { "" };

        write!(self.writer, "\x1b[{prefix}{number};3$y").ok();
        self.writer.flush().ok();
    }

    fn decqrm_response(&mut self, mode: Mode, recognized: bool, enabled: bool) {
        let (is_dec, number) = match &mode {
            Mode::QueryDecPrivateMode(code) => (true, Self::private_mode_number(code)),
            Mode::QueryMode(TerminalMode::Code(code)) => (false, code.to_u16().unwrap()),
            Mode::QueryMode(TerminalMode::Unspecified(code)) => (false, *code),
            _ => unreachable!(),
        };

        let prefix = if is_dec { "?" } else { "" };

        let status = if recognized {
            if enabled {
                1 // set
            } else {
                2 // reset
            }
        } else {
            0
        };

        log::trace!("{:?} -> recognized={} status={}", mode, recognized, status);
        write!(self.writer, "\x1b[{}{};{}$y", prefix, number, status).ok();
        self.writer.flush().ok();
    }

    fn private_mode_flag(&self, mode: &DecPrivateMode) -> Option<bool> {
        use DecPrivateModeCode::*;
        let flag = match mode {
            DecPrivateMode::Unspecified(2031) => self.report_color_scheme_changes,
            DecPrivateMode::Code(code) => match code {
                InBandResize => self.report_text_area_resize,
                StartBlinkingCursor => self.effective_cursor_shape().is_blinking(),
                Win32InputMode => self.keyboard_encoding == KeyboardEncoding::Win32,
                ReverseWraparound => self.reverse_wraparound_mode,
                LeftRightMarginMode => self.left_and_right_margin_mode,
                AutoWrap => self.dec_auto_wrap,
                OriginMode => self.dec_origin_mode,
                UsePrivateColorRegistersForEachGraphic => {
                    self.use_private_color_registers_for_each_graphic
                }
                SynchronizedOutput => self.synchronized_output,
                ReverseVideo => self.reverse_video_mode,
                BracketedPaste => self.bracketed_paste,
                ApplicationCursorKeys => self.application_cursor_keys,
                SixelDisplayMode => self.sixel_display_mode,
                DecAnsiMode => self.dec_ansi_mode,
                ShowCursor => self.cursor_visible,
                MouseTracking => self.mouse_tracking,
                ButtonEventMouse => self.button_event_mouse,
                AnyEventMouse => self.any_event_mouse,
                FocusTracking => self.focus_tracking,
                SGRMouse => matches!(self.mouse_encoding, MouseEncoding::SGR),
                SGRPixelsMouse => matches!(self.mouse_encoding, MouseEncoding::SgrPixels),
                Utf8Mouse => matches!(self.mouse_encoding, MouseEncoding::Utf8),
                SixelScrollsRight => self.sixel_scrolls_right,
                EnableAlternateScreen
                | OptEnableAlternateScreen
                | ClearAndEnableAlternateScreen => self.screen.is_alt_screen_active(),
                _ => return None,
            },
            DecPrivateMode::Unspecified(_) => return None,
        };
        Some(flag)
    }

    fn private_mode_number(mode: &DecPrivateMode) -> u16 {
        match mode {
            DecPrivateMode::Code(code) => code.to_u16().unwrap(),
            DecPrivateMode::Unspecified(number) => *number,
        }
    }

    // XTSAVE/XTRESTORE use one saved value per setting, with aliases for
    // the alternate buffer, mouse reporting level and mouse encoding.
    fn save_private_mode(&mut self, mode: DecPrivateMode) {
        use DecPrivateMode::Code;
        use DecPrivateModeCode::*;
        match mode {
            Code(
                EnableAlternateScreen | OptEnableAlternateScreen | ClearAndEnableAlternateScreen,
            ) => {
                self.saved_private_modes.alternate_screen =
                    Some(self.screen.is_alt_screen_active());
            }
            Code(MouseTracking | HighlightMouseTracking | ButtonEventMouse | AnyEventMouse) => {
                self.saved_private_modes.mouse_reporting = Some((
                    self.mouse_tracking,
                    self.button_event_mouse,
                    self.any_event_mouse,
                ));
            }
            Code(Utf8Mouse | SGRMouse | SGRPixelsMouse) => {
                self.saved_private_modes.mouse_encoding = Some(self.mouse_encoding);
            }
            Code(SaveCursor) => self.dec_save_cursor(),
            mode => {
                if let Some(value) = self.private_mode_flag(&mode) {
                    self.saved_private_modes
                        .flags
                        .insert(Self::private_mode_number(&mode), value);
                }
            }
        }
    }

    fn restore_private_mode(&mut self, mode: DecPrivateMode) {
        use DecPrivateMode::Code;
        use DecPrivateModeCode::*;
        match mode {
            Code(
                EnableAlternateScreen | OptEnableAlternateScreen | ClearAndEnableAlternateScreen,
            ) => {
                // Preserve the contents of both buffers when restoring the selector.
                if let Some(alternate) = self.saved_private_modes.alternate_screen {
                    if alternate != self.screen.is_alt_screen_active() {
                        self.activate_screen(alternate);
                    }
                }
            }
            Code(MouseTracking | HighlightMouseTracking | ButtonEventMouse | AnyEventMouse) => {
                if let Some((press, drag, motion)) = self.saved_private_modes.mouse_reporting {
                    (
                        self.mouse_tracking,
                        self.button_event_mouse,
                        self.any_event_mouse,
                    ) = (press, drag, motion);
                    self.last_mouse_move.take();
                }
            }
            Code(Utf8Mouse | SGRMouse | SGRPixelsMouse) => {
                if let Some(encoding) = self.saved_private_modes.mouse_encoding {
                    self.mouse_encoding = encoding;
                    self.last_mouse_move.take();
                }
            }
            Code(SaveCursor) => self.dec_restore_cursor(),
            mode => {
                if let Some(&enabled) = self
                    .saved_private_modes
                    .flags
                    .get(&Self::private_mode_number(&mode))
                {
                    self.perform_csi_mode(if enabled {
                        Mode::SetDecPrivateMode(mode)
                    } else {
                        Mode::ResetDecPrivateMode(mode)
                    });
                }
            }
        }
    }

    fn perform_csi_mode(&mut self, mode: Mode) {
        match mode {
            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::InBandResize)) => {
                self.report_text_area_resize = true;
                let dimensions = self.text_area_dimensions();
                Self::write_resize_report(&mut self.writer, dimensions);
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::InBandResize)) => {
                self.clear_resize_subscription();
            }
            Mode::SetDecPrivateMode(DecPrivateMode::Unspecified(2031)) => {
                self.report_color_scheme_changes = true;
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Unspecified(2031)) => {
                self.report_color_scheme_changes = false;
            }
            Mode::SetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::StartBlinkingCursor,
            ))
            | Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::StartBlinkingCursor,
            )) => {
                use CursorShape::*;
                let blinking = matches!(mode, Mode::SetDecPrivateMode(_));
                self.cursor.shape = match (self.effective_cursor_shape(), blinking) {
                    (BlinkingBar | SteadyBar, true) => BlinkingBar,
                    (BlinkingBar | SteadyBar, false) => SteadyBar,
                    (BlinkingUnderline | SteadyUnderline, true) => BlinkingUnderline,
                    (BlinkingUnderline | SteadyUnderline, false) => SteadyUnderline,
                    (Default | BlinkingBlock | SteadyBlock, true) => BlinkingBlock,
                    (BlinkingBlock | SteadyBlock, false) => SteadyBlock,
                    (Default, false) => Default,
                };
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::AutoRepeat))
            | Mode::ResetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::AutoRepeat)) => {
                // We leave key repeat to the GUI layer prefs
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::Win32InputMode)) => {
                self.keyboard_encoding = KeyboardEncoding::Win32;
            }

            Mode::ResetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::Win32InputMode)) => {
                self.keyboard_encoding = KeyboardEncoding::Xterm;
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::ReverseWraparound,
            )) => {
                self.reverse_wraparound_mode = true;
            }

            Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::ReverseWraparound,
            )) => {
                self.reverse_wraparound_mode = false;
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::LeftRightMarginMode,
            )) => {
                if !self.left_and_right_margin_mode {
                    if self.screen().line_width_scale(self.cursor.y) > 1 {
                        self.wrap_next = false;
                    }
                    // DECLRMM restores the live pages to single-width, single-height.
                    self.screen.screen.reset_visible_line_renditions(self.seqno);
                    self.screen
                        .alt_screen
                        .reset_visible_line_renditions(self.seqno);
                }
                self.left_and_right_margin_mode = true;
            }

            Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::LeftRightMarginMode,
            )) => {
                self.left_and_right_margin_mode = false;
                self.left_and_right_margins = 0..self.screen().physical_cols;
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::GraphemeClustering,
            ))
            | Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::GraphemeClustering,
            )) => {
                // Permanently enabled
            }

            Mode::QueryDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::GraphemeClustering,
            )) => {
                self.decqrm_response_permanent(mode);
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::SaveCursor)) => {
                self.dec_save_cursor();
            }

            Mode::ResetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::SaveCursor)) => {
                self.dec_restore_cursor();
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::AutoWrap)) => {
                self.dec_auto_wrap = true;
            }

            Mode::ResetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::AutoWrap)) => {
                self.dec_auto_wrap = false;
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::OriginMode)) => {
                self.dec_origin_mode = true;
                self.set_cursor_pos(&Position::Absolute(0), &Position::Absolute(0));
            }

            Mode::ResetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::OriginMode)) => {
                self.dec_origin_mode = false;
                self.set_cursor_pos(&Position::Absolute(0), &Position::Absolute(0));
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::UsePrivateColorRegistersForEachGraphic,
            )) => {
                self.use_private_color_registers_for_each_graphic = true;
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::UsePrivateColorRegistersForEachGraphic,
            )) => {
                self.use_private_color_registers_for_each_graphic = false;
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::SynchronizedOutput,
            )) => {
                self.synchronized_output = true;
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::SynchronizedOutput,
            )) => {
                self.synchronized_output = false;
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::SmoothScroll))
            | Mode::ResetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::SmoothScroll)) => {
                // We always output at our "best" rate
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::ReverseVideo)) => {
                // Turn on reverse video for all of the lines on the
                // display.
                self.reverse_video_mode = true;
            }

            Mode::ResetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::ReverseVideo)) => {
                // Turn off reverse video for all of the lines on the
                // display.
                self.reverse_video_mode = false;
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::Select132Columns))
            | Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::Select132Columns,
            )) => {
                // Note: we don't support 132 column mode so we treat
                // both set/reset as the same and we're really just here
                // for the other side effects of this sequence
                // https://vt100.net/docs/vt510-rm/DECCOLM.html

                self.top_and_bottom_margins = 0..self.screen().physical_rows as i64;
                self.left_and_right_margins = 0..self.screen().physical_cols;
                self.set_cursor_pos(&Position::Absolute(0), &Position::Absolute(0));
                self.erase_in_display(EraseInDisplay::EraseDisplay);
            }
            Mode::QueryDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::Select132Columns,
            )) => {
                self.decqrm_response(mode, true, false);
            }

            Mode::SetMode(TerminalMode::Code(TerminalModeCode::BiDirectionalSupportMode)) => {
                self.bidi_enabled.replace(true);
            }
            Mode::ResetMode(TerminalMode::Code(TerminalModeCode::BiDirectionalSupportMode)) => {
                self.bidi_enabled.replace(false);
            }
            Mode::QueryMode(TerminalMode::Code(TerminalModeCode::BiDirectionalSupportMode)) => {
                self.decqrm_response(
                    mode,
                    true,
                    self.bidi_enabled
                        .unwrap_or_else(|| self.config.bidi_mode().enabled),
                );
            }

            Mode::SetMode(TerminalMode::Code(TerminalModeCode::Insert)) => {
                self.insert = true;
            }
            Mode::ResetMode(TerminalMode::Code(TerminalModeCode::Insert)) => {
                self.insert = false;
            }
            Mode::QueryMode(TerminalMode::Code(TerminalModeCode::Insert)) => {
                self.decqrm_response(mode, true, self.insert);
            }

            Mode::SetMode(TerminalMode::Code(TerminalModeCode::AutomaticNewline)) => {
                self.newline_mode = true;
            }
            Mode::ResetMode(TerminalMode::Code(TerminalModeCode::AutomaticNewline)) => {
                self.newline_mode = false;
            }
            Mode::QueryMode(TerminalMode::Code(TerminalModeCode::AutomaticNewline)) => {
                self.decqrm_response(mode, true, self.newline_mode);
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::BracketedPaste)) => {
                self.bracketed_paste = true;
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::BracketedPaste)) => {
                self.bracketed_paste = false;
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::OptEnableAlternateScreen,
            ))
            | Mode::SetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::EnableAlternateScreen,
            )) => {
                if !self.screen.is_alt_screen_active() {
                    self.activate_screen(true);
                    self.pen = CellAttributes::default();
                }
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::OptEnableAlternateScreen,
            )) => {
                if self.screen.is_alt_screen_active() {
                    self.pen = CellAttributes::default();
                    self.erase_in_display(EraseInDisplay::EraseDisplay);
                    self.kitty_img = Default::default();
                    self.activate_screen(false);
                }
            }

            Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::EnableAlternateScreen,
            )) => {
                if self.screen.is_alt_screen_active() {
                    self.activate_screen(false);
                    self.pen = CellAttributes::default();
                }
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::ApplicationCursorKeys,
            )) => {
                self.application_cursor_keys = true;
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::ApplicationCursorKeys,
            )) => {
                self.application_cursor_keys = false;
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::SixelDisplayMode)) => {
                self.sixel_display_mode = true;
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::SixelDisplayMode,
            )) => {
                self.sixel_display_mode = false;
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::DecAnsiMode)) => {
                self.dec_ansi_mode = true;
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::DecAnsiMode)) => {
                self.dec_ansi_mode = false;
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::ShowCursor)) => {
                self.cursor_visible = true;
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::ShowCursor)) => {
                self.cursor_visible = false;
            }
            Mode::SetMode(TerminalMode::Code(TerminalModeCode::ShowCursor)) => {
                self.cursor_visible = true;
            }
            Mode::ResetMode(TerminalMode::Code(TerminalModeCode::ShowCursor)) => {
                self.cursor_visible = false;
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::MouseTracking)) => {
                self.mouse_tracking = true;
                self.last_mouse_move.take();
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::MouseTracking)) => {
                self.mouse_tracking = false;
                self.last_mouse_move.take();
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::HighlightMouseTracking,
            ))
            | Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::HighlightMouseTracking,
            )) => {}

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::ButtonEventMouse)) => {
                self.button_event_mouse = true;
                self.last_mouse_move.take();
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::ButtonEventMouse,
            )) => {
                self.button_event_mouse = false;
                self.last_mouse_move.take();
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::AnyEventMouse)) => {
                self.any_event_mouse = true;
                self.last_mouse_move.take();
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::AnyEventMouse)) => {
                self.any_event_mouse = false;
                self.last_mouse_move.take();
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::FocusTracking)) => {
                self.focus_tracking = true;
                self.last_mouse_move.take();
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::FocusTracking)) => {
                self.focus_tracking = false;
                self.last_mouse_move.take();
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::SGRMouse)) => {
                self.mouse_encoding = MouseEncoding::SGR;
                self.last_mouse_move.take();
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::SGRMouse)) => {
                self.mouse_encoding = MouseEncoding::X10;
                self.last_mouse_move.take();
            }
            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::SGRPixelsMouse)) => {
                self.mouse_encoding = MouseEncoding::SgrPixels;
                self.last_mouse_move.take();
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::SGRPixelsMouse)) => {
                self.mouse_encoding = MouseEncoding::X10;
                self.last_mouse_move.take();
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::Utf8Mouse)) => {
                self.mouse_encoding = MouseEncoding::Utf8;
                self.last_mouse_move.take();
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::Utf8Mouse)) => {
                self.mouse_encoding = MouseEncoding::X10;
                self.last_mouse_move.take();
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::SixelScrollsRight,
            )) => {
                self.sixel_scrolls_right = true;
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::SixelScrollsRight,
            )) => {
                self.sixel_scrolls_right = false;
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::ClearAndEnableAlternateScreen,
            )) => {
                if !self.screen.is_alt_screen_active() {
                    self.dec_save_cursor();
                    self.activate_screen(true);
                    self.kitty_img = Default::default();
                    self.set_cursor_pos(&Position::Absolute(0), &Position::Absolute(0));
                    self.pen = CellAttributes::default();
                    self.erase_in_display(EraseInDisplay::EraseDisplay);
                }
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::ClearAndEnableAlternateScreen,
            )) => {
                if self.screen.is_alt_screen_active() {
                    self.activate_screen(false);
                    self.dec_restore_cursor();
                }
            }
            Mode::SaveDecPrivateMode(mode) => self.save_private_mode(mode),
            Mode::RestoreDecPrivateMode(mode) => self.restore_private_mode(mode),

            Mode::SetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::MinTTYApplicationEscapeKeyMode,
            ))
            | Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::MinTTYApplicationEscapeKeyMode,
            )) => {}

            Mode::SetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::XTermMetaSendsEscape,
            ))
            | Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::XTermMetaSendsEscape,
            )) => {}

            Mode::SetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::XTermAltSendsEscape,
            ))
            | Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::XTermAltSendsEscape,
            )) => {}

            Mode::SetDecPrivateMode(DecPrivateMode::Unspecified(_))
            | Mode::ResetDecPrivateMode(DecPrivateMode::Unspecified(_)) => {
                if self.config.log_unknown_escape_sequences() {
                    log::warn!("unhandled DecPrivateMode {:?}", mode);
                }
            }

            mode @ Mode::SetMode(_) | mode @ Mode::ResetMode(_) => {
                if self.config.log_unknown_escape_sequences() {
                    log::warn!("unhandled {:?}", mode);
                }
            }

            Mode::XtermKeyMode {
                resource: XtermKeyModifierResource::OtherKeys,
                value,
            } => {
                let level = value.unwrap_or(0);
                if (0..=3).contains(&level) {
                    self.modify_other_keys = level as u8;
                }
            }

            Mode::XtermOtherKeys(operation) => self.perform_xterm_other_keys(operation),

            Mode::XtermKeyMode { resource, value } => {
                if self.config.log_unknown_escape_sequences() {
                    log::warn!("unhandled XtermKeyMode {:?} {:?}", resource, value);
                }
            }

            Mode::QueryDecPrivateMode(ref private_mode) => {
                let value = self.private_mode_flag(private_mode);
                self.decqrm_response(mode, value.is_some(), value.unwrap_or(false));
            }
            Mode::QueryMode(_) => self.decqrm_response(mode, false, false),
        }
    }

    fn checksum_rectangle(&mut self, left: u32, top: u32, right: u32, bottom: u32) -> u16 {
        let y_origin = if self.dec_origin_mode {
            self.top_and_bottom_margins.start
        } else {
            0
        } as u32;
        let x_origin = if self.dec_origin_mode {
            self.left_and_right_margins.start
        } else {
            0
        };
        let screen = self.screen_mut();
        let mut checksum = 0;
        /*
        debug!(
            "checksum left={} top={} right={} bottom={}",
            left as usize + x_origin,
            top + y_origin,
            right as usize + x_origin,
            bottom + y_origin
        );
        */

        for y in top..=bottom {
            let line_idx = screen.phys_row(VisibleRowIndex::from(y_origin + y));
            let line = screen.line_mut(line_idx);
            for cell in line.visible_cells().skip(x_origin + left as usize) {
                if cell.cell_index() > x_origin + right as usize {
                    break;
                }

                let ch = cell.str().chars().next().unwrap() as u32;
                // debug!("y={} col={} ch={:x} cell={:?}", y + y_origin, col, ch, cell);

                checksum += u16::from(ch as u8);
            }
        }

        // Treat uninitialized cells as spaces.
        // The concept of uninitialized cells in wezterm is not the same as that on VT520 or that
        // on xterm, so, to prevent a lot of noise in esctest, treat them as spaces, at least when
        // asking for the checksum of a single cell (which is what esctest does).
        // See: https://github.com/wezterm/wezterm/pull/4565
        if checksum == 0 {
            32u16
        } else {
            checksum
        }
    }

    fn perform_csi_window(&mut self, window: Window) {
        match window {
            Window::ReportTextAreaSizeCells => {
                let screen = self.screen();
                let height = Some(screen.physical_rows as i64);
                let width = Some(screen.physical_cols as i64);

                let response = Box::new(Window::ResizeWindowCells { width, height });
                write!(self.writer, "{}", CSI::Window(response)).ok();
                self.writer.flush().ok();
            }

            Window::ReportCellSizePixels => {
                let screen = self.screen();
                let height = screen.physical_rows;
                let width = screen.physical_cols;
                let response = Box::new(Window::ReportCellSizePixelsResponse {
                    width: Some((self.pixel_width / width) as i64),
                    height: Some((self.pixel_height / height) as i64),
                });
                write!(self.writer, "{}", CSI::Window(response)).ok();
                self.writer.flush().ok();
            }

            Window::ReportTextAreaSizePixels => {
                let response = Box::new(Window::ResizeWindowPixels {
                    width: Some(self.pixel_width as i64),
                    height: Some(self.pixel_height as i64),
                });
                write!(self.writer, "{}", CSI::Window(response)).ok();
                self.writer.flush().ok();
            }

            Window::ReportWindowTitle => {
                if self.config.enable_title_reporting() {
                    write!(
                        self.writer,
                        "{}",
                        OperatingSystemCommand::SetWindowTitleSun(self.title.raw.to_string())
                    )
                    .ok();
                    self.writer.flush().ok();
                }
            }

            Window::ReportRectangularSgr {
                top,
                left,
                bottom,
                right,
            } => {
                self.report_rectangular_sgr(top, left, bottom, right);
            }
            Window::SelectiveEraseRectangle {
                top,
                left,
                bottom,
                right,
            } => {
                let (rows, cols) = self.screen_rectangle(top, left, bottom, right);
                for row in rows {
                    self.selectively_erase_line(row as VisibleRowIndex, cols.clone());
                }
            }
            Window::EraseRectangle {
                top,
                left,
                bottom,
                right,
            } => {
                let area = self.screen_rectangle(top, left, bottom, right);
                self.paint_rectangle(area, &Cell::blank());
            }
            Window::CopyRectangle {
                source,
                destination,
                ..
            } => {
                // Only the active buffer is addressable, so all page numbers
                // resolve to its sole page per the DEC out-of-range rule.
                self.copy_rectangle(source, destination);
            }
            Window::AttributeExtent(extent) => self.attribute_extent = extent,
            Window::EditAreaAttributes {
                area,
                attributes,
                reverse,
            } => {
                self.edit_area_attributes(area, &attributes, reverse);
            }
            Window::FillRectangle {
                character,
                top,
                left,
                bottom,
                right,
            } => {
                let area = self.screen_rectangle(top, left, bottom, right);
                let cell = self.rectangle_fill_cell(character);
                self.paint_rectangle(area, &cell);
            }
            Window::ChecksumRectangularArea {
                request_id,
                top,
                left,
                bottom,
                right,
                ..
            } => {
                if self.config.enable_checksum_rectangular_area() {
                    let checksum = self.checksum_rectangle(
                        left.as_zero_based(),
                        top.as_zero_based(),
                        right.as_zero_based(),
                        bottom.as_zero_based(),
                    );
                    write!(self.writer, "\x1bP{}!~{:04x}\x1b\\", request_id, checksum).ok();
                    self.writer.flush().ok();
                }
            }
            Window::ResizeWindowCells { .. } => {
                // We don't allow the application to change the window size; that's
                // up to the user!
            }
            Window::Iconify | Window::DeIconify => {}
            Window::PushIconAndWindowTitle => self.save_titles(title::Scope::Both, None),
            Window::PushIconTitle => self.save_titles(title::Scope::Icon, None),
            Window::PushWindowTitle => self.save_titles(title::Scope::Window, None),
            Window::PopIconAndWindowTitle => self.restore_titles(title::Scope::Both, None),
            Window::PopIconTitle => self.restore_titles(title::Scope::Icon, None),
            Window::PopWindowTitle => self.restore_titles(title::Scope::Window, None),
            Window::SaveTitleToSlot { scope, slot } => self.save_titles(scope, Some(slot)),
            Window::RestoreTitleFromSlot { scope, slot } => self.restore_titles(scope, Some(slot)),
            Window::ReportTitleStackPosition => self.report_title_stack_position(),

            _ => {
                if self.config.log_unknown_escape_sequences() {
                    log::warn!("unhandled Window CSI {:?}", window);
                }
            }
        }
    }

    fn archive_display(&mut self) {
        let rows = self.screen().physical_rows;
        if !self.is_alt_screen_active()
            && self.top_and_bottom_margins == (0..rows as VisibleRowIndex)
        {
            let screen = self.screen();
            let blank = CellAttributes::blank();
            let last_cell_row = screen
                .physical_lines()
                .rev()
                .take(rows)
                .position(|line| {
                    !line.is_single_width()
                        || line
                            .visible_cells()
                            .any(|cell| cell.str() != " " || cell.attrs() != &blank)
                })
                .map_or(0, |from_bottom| rows - from_bottom);
            let count = last_cell_row.max(self.archive_kitty_relative_rows());
            if count != 0 {
                let seqno = self.seqno;
                let pen = self.pen.clone_sgr_only();
                let bidi_mode = self.get_bidi_mode();
                self.screen_mut().scroll_up(
                    &(0..rows as VisibleRowIndex),
                    count,
                    seqno,
                    pen,
                    bidi_mode,
                );
            }
        }
        self.erase_in_display(EraseInDisplay::EraseDisplay);
    }

    fn erase_in_display(&mut self, erase: EraseInDisplay) {
        let seqno = self.seqno;
        let cy = self.cursor.y;
        let pen = self.pen.clone_sgr_only();
        let rows = self.screen().physical_rows as VisibleRowIndex;
        let col_range = 0..self.screen().physical_cols;
        let row_range = match erase {
            EraseInDisplay::EraseToEndOfDisplay => {
                self.perform_csi_edit(Edit::EraseInLine(EraseInLine::EraseToEndOfLine));
                cy + 1..rows
            }
            EraseInDisplay::EraseToStartOfDisplay => {
                self.perform_csi_edit(Edit::EraseInLine(EraseInLine::EraseToStartOfLine));
                0..cy
            }
            EraseInDisplay::EraseDisplay => {
                self.clear_visible_kitty_images();
                0..rows
            }
            EraseInDisplay::EraseScrollback => {
                self.screen_mut().erase_scrollback();
                return;
            }
        };

        {
            let preserve_kitty = erase != EraseInDisplay::EraseDisplay;
            for y in row_range {
                self.erase_line_text(y, col_range.clone(), &pen, preserve_kitty);
                let screen = self.screen_mut();
                let line_idx = screen.phys_row(y);
                screen.line_mut(line_idx).set_single_width(seqno);
            }
        }
    }

    fn erase_line_columns(&self, erase: EraseInLine) -> Range<usize> {
        let col = self.cursor.x;
        let width = self.screen().logical_cols(self.cursor.y);
        match erase {
            // In pending-wrap state the cursor is effectively past the last column.
            EraseInLine::EraseToEndOfLine => col + usize::from(self.wrap_next)..width,
            EraseInLine::EraseToStartOfLine => 0..col + 1,
            EraseInLine::EraseLine => 0..width,
        }
    }

    fn erase_line_text(
        &mut self,
        y: VisibleRowIndex,
        cols: Range<usize>,
        pen: &CellAttributes,
        preserve_kitty: bool,
    ) {
        let seqno = self.seqno;
        let bidi_mode = self.get_bidi_mode();
        let keep_placements = preserve_kitty && self.kitty_img.has_physical_placements();
        let screen = self.screen_mut();
        let row = screen.phys_row(y);
        screen.discard_erased_output(row, &cols);
        let mut saved = Vec::new();
        if keep_placements {
            let line = screen.line_mut(row);
            // Wide glyph continuations can hold their own image tiles. Take
            // the raw cells when the line has graphics or a wide character.
            if line
                .visible_cells()
                .any(|cell| cell.width() > 1 || cell.attrs().images().is_some())
            {
                let cells = line.cells_mut_for_attr_changes_only();
                for (x, cell) in cells.iter().enumerate().take(cols.end).skip(cols.start) {
                    for tile in cell.attrs().images().into_iter().flatten() {
                        if tile.has_placement_id() {
                            saved.push((x, tile));
                        }
                    }
                }
            }
        }
        screen.clear_line(y, cols, pen, seqno, bidi_mode);
        if let Some((last, _)) = saved.last() {
            let line = screen.line_mut(row);
            if *last >= line.len() {
                line.resize(*last + 1, seqno);
            }
            let cells = line.cells_mut_for_attr_changes_only();
            for (x, tile) in saved {
                cells[x].attrs_mut().attach_image(Box::new(tile));
            }
        }
    }

    fn get_bidi_mode(&self) -> BidiMode {
        let mut mode = self.config.bidi_mode();
        if let Some(enabled) = &self.bidi_enabled {
            mode.enabled = *enabled;
        }
        if let Some(hint) = &self.bidi_hint {
            mode.hint = *hint;
        }
        mode
    }

    fn perform_csi_edit(&mut self, edit: Edit) {
        let seqno = self.seqno;
        match edit {
            Edit::InsertColumns(n) => self.edit_columns(n, column_edit::Shift::Right),
            Edit::DeleteColumns(n) => self.edit_columns(n, column_edit::Shift::Left),
            Edit::DeleteCharacter(n) => {
                let y = self.cursor.y;
                let x = self.cursor.x;

                if x >= self.left_and_right_margins.start && x < self.left_and_right_margins.end {
                    let right_margin = self
                        .left_and_right_margins
                        .end
                        .min(self.screen().logical_cols(y));
                    let limit = (x + n as usize).min(right_margin);

                    let blank_attr = self.pen.clone_sgr_only();
                    let graphics =
                        self.prepare_kitty_text_edit(&(y..y + 1), &(x..right_margin), false);
                    let screen = self.screen_mut();
                    for _ in x..limit as usize {
                        screen.erase_cell(x, y, right_margin, seqno, blank_attr.clone());
                    }
                    self.finish_kitty_tile_update(graphics);
                }
            }
            Edit::DeleteLine(n) => {
                if self.top_and_bottom_margins.contains(&self.cursor.y)
                    && self.left_and_right_margins.contains(&self.cursor.x)
                {
                    let top_and_bottom_margins = self.cursor.y..self.top_and_bottom_margins.end;
                    let left_and_right_margins = self.left_and_right_margins.clone();
                    let blank_attr = self.pen.clone_sgr_only();
                    let bidi_mode = self.get_bidi_mode();
                    let graphics = self.prepare_kitty_text_edit(
                        &top_and_bottom_margins,
                        &left_and_right_margins,
                        true,
                    );
                    let margin_rows = top_and_bottom_margins.end - top_and_bottom_margins.start;
                    let count = (n as usize).min(margin_rows as usize);
                    let removed = self.cursor.y..self.cursor.y + count as VisibleRowIndex;
                    self.screen_mut().erase_multicell_text(
                        removed,
                        left_and_right_margins.clone(),
                        seqno,
                    );
                    self.screen_mut().scroll_up_within_margins(
                        &top_and_bottom_margins,
                        &left_and_right_margins,
                        n as usize,
                        seqno,
                        blank_attr,
                        bidi_mode,
                    );
                    self.finish_kitty_tile_update(graphics);
                }
            }
            Edit::EraseCharacter(n) => {
                let y = self.cursor.y;
                let x = self.cursor.x;
                let limit = (x + n as usize).min(self.screen().logical_cols(y));
                {
                    let blank = Cell::blank_with_attrs(self.pen.clone_sgr_only());
                    let screen = self.screen_mut();
                    for x in x..limit as usize {
                        screen.set_cell(x, y, &blank, seqno);
                    }
                }
            }

            Edit::EraseInLine(erase) => {
                let pen = self.pen.clone_sgr_only();
                self.erase_line_text(self.cursor.y, self.erase_line_columns(erase), &pen, true);
            }
            Edit::SelectiveEraseInLine(erase) => {
                self.selectively_erase_line(self.cursor.y, self.erase_line_columns(erase));
            }
            Edit::SelectiveEraseInDisplay(erase) => self.selectively_erase_display(erase),
            Edit::CharacterProtection(protected) => {
                self.pen.set_protected(protected);
            }
            Edit::InsertCharacter(n) => {
                // https://vt100.net/docs/vt510-rm/ICH.html
                // The ICH sequence inserts Pn blank characters with the normal character
                // attribute. The cursor remains at the beginning of the blank characters. Text
                // between the cursor and right margin moves to the right. Characters scrolled past
                // the right margin are lost. ICH has no effect outside the scrolling margins.

                let y = self.cursor.y;
                let x = self.cursor.x;
                if self.top_and_bottom_margins.contains(&y)
                    && self.left_and_right_margins.contains(&x)
                {
                    let margin = self
                        .left_and_right_margins
                        .end
                        .min(self.screen().logical_cols(y));
                    let graphics = self.prepare_kitty_text_edit(&(y..y + 1), &(x..margin), false);
                    let screen = self.screen_mut();
                    for _ in 0..n as usize {
                        screen.insert_cell(x, y, margin, seqno);
                    }
                    self.finish_kitty_tile_update(graphics);
                }
            }
            Edit::InsertLine(n) => {
                if self.top_and_bottom_margins.contains(&self.cursor.y)
                    && self.left_and_right_margins.contains(&self.cursor.x)
                {
                    let bidi_mode = self.get_bidi_mode();
                    let top_and_bottom_margins = self.cursor.y..self.top_and_bottom_margins.end;
                    let left_and_right_margins = self.left_and_right_margins.clone();
                    let blank_attr = self.pen.clone_sgr_only();
                    let graphics = self.prepare_kitty_text_edit(
                        &top_and_bottom_margins,
                        &left_and_right_margins,
                        false,
                    );
                    self.screen_mut().scroll_down_within_margins(
                        &top_and_bottom_margins,
                        &left_and_right_margins,
                        n as usize,
                        seqno,
                        blank_attr,
                        bidi_mode,
                    );
                    self.finish_kitty_tile_update(graphics);
                }
            }
            Edit::ScrollDown(n) => self.scroll_down(n as usize),
            Edit::ScrollDownFromHistory(n) => self.scroll_down_from_history(n as usize),
            Edit::ScrollUp(n) => self.scroll_up(n as usize),
            Edit::ScrollLeft(n) => self.scroll_columns(n, column_edit::Shift::Left),
            Edit::ScrollRight(n) => self.scroll_columns(n, column_edit::Shift::Right),
            Edit::EraseInDisplay(erase) => self.erase_in_display(erase),
            Edit::ArchiveDisplay => self.archive_display(),
            Edit::Repeat(n) => {
                let mut y = self.cursor.y;
                let mut x = self.cursor.x;
                let left_and_right_margins = self.left_and_right_margins.clone();
                let top_and_bottom_margins = self.top_and_bottom_margins.clone();

                // Resolve the source cell.  It may be a double-wide character.
                let cell = {
                    let screen = self.screen_mut();
                    let to_copy = x.saturating_sub(1);
                    let line_idx = screen.phys_row(y);
                    let line = screen.line_mut(line_idx);

                    match line.cells_mut().get(to_copy).cloned() {
                        None => Cell::blank(),
                        Some(candidate) => {
                            if candidate.str() == " " && to_copy > 0 {
                                // It's a blank.  It may be the second part of
                                // a double-wide pair; look ahead of it.
                                let prior = &line.cells_mut()[to_copy - 1];
                                if prior.width() > 1 {
                                    prior.clone()
                                } else {
                                    candidate
                                }
                            } else {
                                candidate
                            }
                        }
                    }
                };

                if cell.attrs().multicell_row().is_some() {
                    return;
                }
                for _ in 0..n {
                    let right = left_and_right_margins
                        .end
                        .min(self.screen().logical_cols(y));
                    x = self.constrain_line_column(x, y);
                    let sized = self.screen().line_width_scale(y) > 1;
                    let width = if sized { cell.width() } else { 1 };
                    if !sized || x.saturating_add(width) <= right {
                        self.screen_mut().set_cell(x, y, &cell, seqno);
                    }
                    x += width;
                    if x >= right {
                        x = left_and_right_margins.start;
                        if y == top_and_bottom_margins.end - 1 {
                            self.scroll_up(1);
                        } else {
                            y += 1;
                            if y > top_and_bottom_margins.end - 1 {
                                y = top_and_bottom_margins.end;
                            }
                        }
                    }
                }
                self.cursor.x = self.constrain_line_column(x, y);
                self.cursor.y = y;
            }
        }
    }

    /// https://vt100.net/docs/vt510-rm/DECSLRM.html
    fn set_left_and_right_margins(&mut self, left: OneBased, right: OneBased) {
        // The terminal only recognizes this control function if vertical split
        // screen mode (DECLRMM) is set.
        if self.left_and_right_margin_mode {
            let cols = self.screen().physical_cols as u32;
            let left = left.as_zero_based().min(cols - 1).max(0) as usize;
            let right = right.as_zero_based().min(cols - 1).max(0) as usize;

            // The value of the left margin (Pl) must be less than the right margin (Pr).
            if left >= right {
                return;
            }

            // The minimum size of the scrolling region is two columns per DEC,
            // but xterm allows 1.
            /*
            if right - left < 2 {
                return;
            }
            */

            self.left_and_right_margins = left..right + 1;

            // DECSLRM moves the cursor to column 1, line 1 of the page.
            self.set_cursor_pos(&Position::Absolute(0), &Position::Absolute(0));
        }
    }

    fn perform_csi_cursor(&mut self, cursor: Cursor) {
        let seqno = self.seqno;
        match cursor {
            Cursor::SetTopAndBottomMargins { top, bottom } => {
                let rows = self.screen().physical_rows;
                let top = i64::from(top.as_zero_based()).min(rows as i64 - 1).max(0);
                let bottom = i64::from(bottom.as_zero_based())
                    .min(rows as i64 - 1)
                    .max(0);
                if top >= bottom {
                    return;
                }
                self.top_and_bottom_margins = top..bottom + 1;
                self.set_cursor_pos(&Position::Absolute(0), &Position::Absolute(0));
                log::debug!(
                    "SetTopAndBottomMargins {:?} (and move cursor to top left: {:?})",
                    self.top_and_bottom_margins,
                    self.cursor
                );
            }

            Cursor::SetLeftAndRightMargins { left, right } => {
                self.set_left_and_right_margins(left, right);
            }

            Cursor::ForwardTabulation(n) => {
                if let Some(skip) = n.checked_sub(1) {
                    self.horizontal_tabs_forward(skip as usize);
                }
            }
            Cursor::BackwardTabulation(n) => {
                if let Some(skip) = n.checked_sub(1) {
                    let left = if self.dec_origin_mode {
                        self.left_and_right_margins.start
                    } else {
                        0
                    };
                    let column = self.tabs.backward(self.cursor.x, left, skip as usize);
                    self.set_cursor_position_absolute(column, self.cursor.y);
                }
            }

            Cursor::TabulationClear(to_clear) => {
                self.tabs.clear(
                    to_clear,
                    self.cursor.x,
                    self.config.log_unknown_escape_sequences(),
                );
            }

            Cursor::RequestTabStops => {
                let mut reply = format!("{DCS}2$u");
                let mut separator = "";
                for column in self.tabs.columns(self.screen().physical_cols) {
                    write!(reply, "{separator}{column}").ok();
                    separator = "/";
                }
                reply.push_str(ST);
                self.writer.write_all(reply.as_bytes()).ok();
                self.writer.flush().ok();
            }
            Cursor::ResetTabStops => self.tabs.reset_tab_stops(),
            Cursor::TabulationControl(control) => match control {
                CursorTabulationControl::SetCharacterTabStopAtActivePosition => self.c1_hts(),
                CursorTabulationControl::ClearCharacterTabStopAtActivePosition => {
                    self.tabs.clear_tab_stop(self.cursor.x);
                }
                CursorTabulationControl::ClearAllCharacterTabStopsAtActiveLine
                | CursorTabulationControl::ClearAllCharacterTabStops => {
                    // Character stops are shared across the screen.
                    self.tabs.clear_all_tab_stops();
                }
                _ => {}
            },
            Cursor::LineTabulation(_) => {}

            Cursor::Left(_n) => {
                // https://vt100.net/docs/vt510-rm/CUB.html
                unreachable!("Actually handled in Performer::csi_dispatch by rewriting as ControlCode::Backspace");
            }

            Cursor::Right(n) => {
                // https://vt100.net/docs/vt510-rm/CUF.html
                let cols = self.screen().physical_cols;
                let new_x = if self.cursor.x >= self.left_and_right_margins.end {
                    // outside the margin, so allow movement to screen edge
                    (self.cursor.x + n as usize).min(cols - 1)
                } else {
                    // Else constrain to margin
                    (self.cursor.x + n as usize).min(self.left_and_right_margins.end - 1)
                };

                self.cursor.x = self.constrain_line_column(new_x, self.cursor.y);
                self.cursor.seqno = seqno;
                self.wrap_next = false;
            }

            Cursor::Up(n) => {
                // https://vt100.net/docs/vt510-rm/CUU.html

                let candidate = self.cursor.y.saturating_sub(i64::from(n));
                let new_y = if self.cursor.y < self.top_and_bottom_margins.start {
                    // above the top margin, so allow movement to
                    // top of screen
                    candidate
                } else {
                    // Else constrain to top margin
                    if candidate < self.top_and_bottom_margins.start {
                        self.top_and_bottom_margins.start
                    } else {
                        candidate
                    }
                };

                let new_y = new_y.max(0);

                self.cursor.y = new_y;
                self.cursor.x = self.constrain_line_column(self.cursor.x, new_y);
                self.cursor.seqno = seqno;
                self.wrap_next = false;
            }
            Cursor::Down(n) => {
                // https://vt100.net/docs/vt510-rm/CUD.html
                let rows = self.screen().physical_rows;
                let new_y = if self.cursor.y >= self.top_and_bottom_margins.end {
                    // below the bottom margin, so allow movement to
                    // bottom of screen
                    (self.cursor.y + i64::from(n)).min(rows as i64 - 1)
                } else {
                    // Else constrain to bottom margin
                    (self.cursor.y + i64::from(n)).min(self.top_and_bottom_margins.end - 1)
                };

                self.cursor.y = new_y;
                self.cursor.x = self.constrain_line_column(self.cursor.x, new_y);
                self.cursor.seqno = seqno;
                self.wrap_next = false;
            }

            Cursor::CharacterAndLinePosition { line, col } | Cursor::Position { line, col } => self
                .set_cursor_pos(
                    &Position::Absolute(i64::from(col.as_zero_based())),
                    &Position::Absolute(i64::from(line.as_zero_based())),
                ),
            Cursor::CharacterAbsolute(col) => self.set_cursor_pos(
                &Position::Absolute(i64::from(col.as_zero_based())),
                &Position::Relative(0),
            ),

            Cursor::CharacterPositionAbsolute(col) => {
                let col = col.as_zero_based() as usize;
                let col = if self.dec_origin_mode {
                    col + self.left_and_right_margins.start
                } else {
                    col
                };
                self.cursor.x = col.min(self.screen().logical_cols(self.cursor.y) - 1);
                self.cursor.seqno = seqno;
                self.wrap_next = false;
            }

            Cursor::CharacterPositionBackward(col) => self.set_cursor_pos(
                &Position::Relative(-(i64::from(col))),
                &Position::Relative(0),
            ),
            Cursor::CharacterPositionForward(col) => {
                self.set_cursor_pos(&Position::Relative(i64::from(col)), &Position::Relative(0))
            }
            Cursor::LinePositionAbsolute(line) => self.set_cursor_pos(
                &Position::Relative(0),
                &Position::Absolute((i64::from(line)).saturating_sub(1)),
            ),
            Cursor::LinePositionBackward(line) => self.set_cursor_pos(
                &Position::Relative(0),
                &Position::Relative(-(i64::from(line))),
            ),
            Cursor::LinePositionForward(line) => {
                self.set_cursor_pos(&Position::Relative(0), &Position::Relative(i64::from(line)))
            }
            Cursor::NextLine(n) => {
                // https://vt100.net/docs/vt510-rm/CNL.html
                let rows = self.screen().physical_rows;
                let new_y = if self.cursor.y >= self.top_and_bottom_margins.end {
                    // below the bottom margin, so allow movement to
                    // bottom of screen
                    (self.cursor.y + i64::from(n)).min(rows as i64 - 1)
                } else {
                    // Else constrain to bottom margin
                    (self.cursor.y + i64::from(n)).min(self.top_and_bottom_margins.end - 1)
                };

                self.cursor.y = new_y;
                self.cursor.x =
                    self.constrain_line_column(self.left_and_right_margins.start, new_y);
                self.cursor.seqno = seqno;
                self.wrap_next = false;
            }
            Cursor::PrecedingLine(n) => {
                // https://vt100.net/docs/vt510-rm/CPL.html
                let candidate = self.cursor.y.saturating_sub(i64::from(n));
                let new_y = if self.cursor.y < self.top_and_bottom_margins.start {
                    // above the top margin, so allow movement to
                    // top of screen
                    candidate
                } else {
                    // Else constrain to top margin
                    if candidate < self.top_and_bottom_margins.start {
                        self.top_and_bottom_margins.start
                    } else {
                        candidate
                    }
                };

                let new_y = new_y.max(0);

                self.cursor.y = new_y;
                self.cursor.x =
                    self.constrain_line_column(self.left_and_right_margins.start, new_y);
                self.cursor.seqno = seqno;
                self.wrap_next = false;
            }

            Cursor::ActivePositionReport { .. } => {
                // This is really a response from the terminal, and
                // we don't need to process it as a terminal command
            }
            Cursor::RequestActivePositionReport => {
                let line = OneBased::from_zero_based(
                    (self.cursor.y.saturating_sub(if self.dec_origin_mode {
                        self.top_and_bottom_margins.start
                    } else {
                        0
                    })) as u32,
                );
                let col = OneBased::from_zero_based(
                    (self
                        .cursor
                        .x
                        .min(self.screen().logical_cols(self.cursor.y) - 1)
                        .saturating_sub(if self.dec_origin_mode {
                            self.left_and_right_margins.start
                        } else {
                            0
                        })) as u32,
                );
                let report = CSI::Cursor(Cursor::ActivePositionReport { line, col });
                write!(self.writer, "{}", report).ok();
                self.writer.flush().ok();
            }
            Cursor::SaveCursor => {
                // The `CSI s` SaveCursor sequence is ambiguous with DECSLRM
                // with default parameters.  To resolve the ambiguity, DECSLRM
                // is recognized if DECLRMM mode is active which we do here
                // where we have the context!
                if self.left_and_right_margin_mode {
                    // https://vt100.net/docs/vt510-rm/DECSLRM.html
                    self.set_left_and_right_margins(
                        OneBased::new(1),
                        OneBased::new(self.screen().physical_cols as u32 + 1),
                    );
                } else {
                    self.dec_save_cursor();
                }
            }
            Cursor::RestoreCursor => self.dec_restore_cursor(),
            Cursor::CursorStyle(style) => {
                self.cursor.shape = match style {
                    CursorStyle::Default => CursorShape::Default,
                    CursorStyle::BlinkingBlock => CursorShape::BlinkingBlock,
                    CursorStyle::SteadyBlock => CursorShape::SteadyBlock,
                    CursorStyle::BlinkingUnderline => CursorShape::BlinkingUnderline,
                    CursorStyle::SteadyUnderline => CursorShape::SteadyUnderline,
                    CursorStyle::BlinkingBar => CursorShape::BlinkingBar,
                    CursorStyle::SteadyBar => CursorShape::SteadyBar,
                };
                log::debug!("Cursor shape is now {:?}", self.cursor.shape);
            }
        }
    }

    /// https://vt100.net/docs/vt510-rm/DECSC.html
    fn dec_save_cursor(&mut self) {
        let saved = SavedCursor {
            position: self.cursor,
            wrap_next: self.wrap_next,
            pen: self.pen.clone(),
            dec_origin_mode: self.dec_origin_mode,
            g0_charset: self.g0_charset,
            g1_charset: self.g1_charset,
        };
        debug!(
            "saving cursor {:?} is_alt={}",
            saved,
            self.screen.is_alt_screen_active()
        );
        *self.screen.saved_cursor() = Some(saved);
    }

    /// https://vt100.net/docs/vt510-rm/DECRC.html
    fn dec_restore_cursor(&mut self) {
        let saved = self
            .screen
            .saved_cursor()
            .clone()
            .unwrap_or_else(|| SavedCursor {
                position: CursorPosition::default(),
                wrap_next: false,
                pen: Default::default(),
                dec_origin_mode: false,
                g0_charset: CharSet::Ascii,
                g1_charset: CharSet::Ascii,
            });
        debug!(
            "restore cursor {:?} is_alt={}",
            saved,
            self.screen.is_alt_screen_active()
        );
        let x = saved.position.x;
        let y = saved.position.y;
        // Disable origin mode so that we can set the cursor position directly
        self.dec_origin_mode = false;
        self.set_cursor_pos(&Position::Absolute(x as i64), &Position::Absolute(y));
        self.cursor.shape = saved.position.shape;
        self.wrap_next = saved.wrap_next;
        self.pen = saved.pen;
        self.dec_origin_mode = saved.dec_origin_mode;
        self.g0_charset = saved.g0_charset;
        self.g1_charset = saved.g1_charset;
        self.shift_out = false;
        self.newline_mode = false;
    }

    fn perform_csi_sgr(&mut self, sgr: Sgr) {
        debug!("{:?}", sgr);
        match sgr {
            Sgr::Reset => {
                let link = self.pen.hyperlink().map(Arc::clone);
                let semantic_type = self.pen.semantic_type();
                let protected = self.pen.protected();
                self.pen = CellAttributes::default();
                self.pen.set_hyperlink(link);
                self.pen.set_semantic_type(semantic_type);
                self.pen.set_protected(protected);
            }
            Sgr::Intensity(intensity) => {
                self.pen.set_intensity(intensity);
            }
            Sgr::Underline(underline) => {
                self.pen.set_underline(underline);
            }
            Sgr::Overline(overline) => {
                self.pen.set_overline(overline);
            }
            Sgr::VerticalAlign(align) => {
                self.pen.set_vertical_align(align);
            }
            Sgr::Blink(blink) => {
                self.pen.set_blink(blink);
            }
            Sgr::Italic(italic) => {
                self.pen.set_italic(italic);
            }
            Sgr::Inverse(inverse) => {
                self.pen.set_reverse(inverse);
            }
            Sgr::Invisible(invis) => {
                self.pen.set_invisible(invis);
            }
            Sgr::StrikeThrough(strike) => {
                self.pen.set_strikethrough(strike);
            }
            Sgr::Foreground(col) => {
                self.pen.set_foreground(col);
            }
            Sgr::Background(col) => {
                self.pen.set_background(col);
            }
            Sgr::UnderlineColor(col) => {
                self.pen.set_underline_color(col);
            }
            Sgr::Font(_) => {}
        }
    }

    /// Computes the set of `SemanticZone`s for the current terminal screen.
    /// Semantic zones are contiguous runs of cells that have the same
    /// `SemanticType` (Prompt, Input, Output).
    /// Due to the way that the terminal clears the screen, the raw, literal
    /// set of zones is overly fragmented by blanks.  This method will ignore
    /// trailing Output regions when computing the SemanticZone bounds.
    ///
    /// By default, all screen data is of type Output.  The shell needs to
    /// employ OSC 133 escapes to markup its output.
    pub fn get_semantic_zones(&mut self) -> anyhow::Result<Vec<SemanticZone>> {
        let screen = self.screen_mut();

        let mut current_zone: Option<SemanticZone> = None;
        let mut zones = vec![];

        let first_stable_row = screen.phys_to_stable_row_index(0);
        screen.for_each_phys_line_mut(|idx, line| {
            let stable_row = first_stable_row + idx as StableRowIndex;

            for zone_range in line.semantic_zone_ranges() {
                let new_zone = match current_zone.as_ref() {
                    None => true,
                    Some(zone) => zone.semantic_type != zone_range.semantic_type,
                };

                if new_zone {
                    if let Some(zone) = current_zone.take() {
                        zones.push(zone);
                    }

                    current_zone.replace(SemanticZone {
                        start_x: zone_range.range.start as usize,
                        start_y: stable_row,
                        end_x: zone_range.range.end as usize,
                        end_y: stable_row,
                        semantic_type: zone_range.semantic_type,
                    });
                }

                if let Some(zone) = current_zone.as_mut() {
                    zone.end_x = zone_range.range.end as usize;
                    zone.end_y = stable_row;
                }
            }
        });
        if let Some(zone) = current_zone.take() {
            zones.push(zone);
        }

        Ok(zones)
    }

    #[inline]
    pub fn get_reverse_video(&self) -> bool {
        self.reverse_video_mode
    }

    pub fn get_keyboard_encoding(&self) -> KeyboardEncoding {
        self.screen()
            .keyboard_stack
            .last()
            .copied()
            .unwrap_or(self.keyboard_encoding)
    }
}
