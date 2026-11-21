use termwiz::input::KeyboardEncoding;
use wezterm_term::{MouseEncoding, Terminal};

/// Live parser state for the inspector, independent of committed screen pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalModes {
    pub cols: usize,
    pub rows: usize,
    pub alternate: bool,
    pub mouse_click: bool,
    pub mouse_drag: bool,
    pub mouse_motion: bool,
    pub mouse_encoding: MouseEncoding,
    pub keyboard: KeyboardEncoding,
    pub bracketed_paste: bool,
    pub synchronized: bool,
}

impl TerminalModes {
    pub(crate) fn read(terminal: &Terminal) -> Self {
        let size = terminal.get_size();
        Self {
            cols: size.cols,
            rows: size.rows,
            alternate: terminal.is_alt_screen_active(),
            mouse_click: terminal.mouse_tracking_enabled(),
            mouse_drag: terminal.button_event_mouse_enabled(),
            mouse_motion: terminal.any_event_mouse_enabled(),
            mouse_encoding: terminal.get_mouse_encoding(),
            keyboard: terminal.get_keyboard_encoding(),
            bracketed_paste: terminal.bracketed_paste_enabled(),
            synchronized: terminal.synchronized_output(),
        }
    }
}
