use serde_json::{Value, json};
use termwiz::input::KeyboardEncoding;
use wezterm_term::{MouseEncoding, TerminalState};

/// Live text-area geometry and parser modes, including changes awaiting presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalModes {
    pub cols: usize,
    pub rows: usize,
    pub pixel_width: usize,
    pub pixel_height: usize,
    pub dpi: u32,
    pub alternate: bool,
    pub mouse_click: bool,
    pub mouse_drag: bool,
    pub mouse_motion: bool,
    pub mouse_encoding: MouseEncoding,
    pub keyboard: KeyboardEncoding,
    pub bracketed_paste: bool,
    pub synchronized: bool,
    pub in_band_resize: bool,
}

impl TerminalModes {
    pub(crate) fn read(terminal: &TerminalState) -> Self {
        let size = terminal.get_size();
        Self {
            cols: size.cols,
            rows: size.rows,
            pixel_width: size.pixel_width,
            pixel_height: size.pixel_height,
            dpi: size.dpi,
            alternate: terminal.is_alt_screen_active(),
            mouse_click: terminal.mouse_tracking_enabled(),
            mouse_drag: terminal.button_event_mouse_enabled(),
            mouse_motion: terminal.any_event_mouse_enabled(),
            mouse_encoding: terminal.get_mouse_encoding(),
            keyboard: terminal.effective_keyboard_encoding(),
            bracketed_paste: terminal.bracketed_paste_enabled(),
            synchronized: terminal.synchronized_output(),
            in_band_resize: terminal.in_band_resize_enabled(),
        }
    }

    pub(crate) fn as_json(&self) -> Value {
        let mouse = match self.mouse_encoding {
            MouseEncoding::X10 => "x10",
            MouseEncoding::Utf8 => "utf8",
            MouseEncoding::SGR => "sgr",
            MouseEncoding::SgrPixels => "sgr-pixels",
        };
        let (keyboard, flags) = match self.keyboard {
            KeyboardEncoding::Xterm => ("xterm", 0),
            KeyboardEncoding::CsiU => ("csi-u", 0),
            KeyboardEncoding::Win32 => ("win32", 0),
            KeyboardEncoding::Kitty(flags) => ("kitty", flags.bits()),
        };
        json!({
            "cols": self.cols,
            "rows": self.rows,
            "pixel_width": self.pixel_width,
            "pixel_height": self.pixel_height,
            "dpi": self.dpi,
            "alternate_screen": self.alternate,
            "bracketed_paste": self.bracketed_paste,
            "synchronized_output": self.synchronized,
            "in_band_resize": self.in_band_resize,
            "mouse": {
                "click": self.mouse_click,
                "drag": self.mouse_drag,
                "motion": self.mouse_motion,
                "encoding": mouse,
            },
            "keyboard": { "encoding": keyboard, "flags": flags },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use wezterm_term::{Terminal, TerminalConfiguration, TerminalSize, color::ColorPalette};

    fn terminal() -> Terminal {
        let (input, _) = tokio::sync::mpsc::unbounded_channel();
        crate::terminal::create(
            TerminalSize {
                cols: 80,
                rows: 24,
                pixel_width: 800,
                pixel_height: 480,
                dpi: 96,
            },
            input,
        )
    }

    #[test]
    fn inspection_observes_committed_pixels_dpi_and_subscription_without_cell_changes() {
        let mut terminal = terminal();
        let baseline = TerminalModes::read(&terminal);
        assert_eq!(
            baseline.as_json(),
            json!({
                "cols": 80, "rows": 24, "pixel_width": 800, "pixel_height": 480, "dpi": 96,
                "alternate_screen": false, "bracketed_paste": false,
                "synchronized_output": false, "in_band_resize": false,
                "mouse": { "click": false, "drag": false, "motion": false, "encoding": "x10" },
                "keyboard": { "encoding": "xterm", "flags": 0 }
            })
        );
        terminal.advance_bytes(b"\x1b[?2048h\x1b[?2026h");
        let subscribed = TerminalModes::read(&terminal);
        assert_eq!(
            subscribed,
            TerminalModes {
                in_band_resize: true,
                synchronized: true,
                ..baseline
            }
        );

        let mut size = terminal.get_size();
        size.pixel_width += 1;
        size.pixel_height += 2;
        terminal.resize(size);
        let resized = TerminalModes::read(&terminal);
        assert_eq!(
            resized,
            TerminalModes {
                pixel_width: 801,
                pixel_height: 482,
                ..subscribed
            }
        );
        size.dpi = 144;
        terminal.resize(size);
        assert_eq!(
            TerminalModes::read(&terminal),
            TerminalModes {
                dpi: 144,
                ..resized
            }
        );
        terminal.advance_bytes(b"\x1b[?2048l");
        let observed = TerminalModes::read(&terminal).as_json();
        assert_eq!(observed["in_band_resize"], false);
        assert_eq!(observed["synchronized_output"], true);
        assert_eq!(observed["dpi"], 144);
    }

    #[test]
    fn inspection_names_active_mouse_and_keyboard_encodings_and_reports_kitty_flags() {
        #[derive(Debug)]
        struct CsiU;
        impl TerminalConfiguration for CsiU {
            fn enable_csi_u_key_encoding(&self) -> bool {
                true
            }
            fn color_palette(&self) -> ColorPalette {
                ColorPalette::default()
            }
        }
        let mut terminal = terminal();
        terminal.advance_bytes(b"\x1b[?1049h\x1b[?2004h\x1b[?1000;1002;1003h\x1b[>19u");
        for (mode, encoding) in [(1005, "utf8"), (1006, "sgr"), (1016, "sgr-pixels")] {
            terminal.advance_bytes(format!("\x1b[?{mode}h").as_bytes());
            let state = TerminalModes::read(&terminal).as_json();
            assert_eq!(state["alternate_screen"], true);
            assert_eq!(state["bracketed_paste"], true);
            assert_eq!(
                state["mouse"],
                json!({
                    "click": true, "drag": true, "motion": true, "encoding": encoding
                })
            );
            assert_eq!(
                state["keyboard"],
                json!({ "encoding": "kitty", "flags": 19 })
            );
        }
        terminal.advance_bytes(b"\x1b[<u\x1b[?9001h");
        assert_eq!(
            TerminalModes::read(&terminal).as_json()["keyboard"],
            json!({ "encoding": "win32", "flags": 0 })
        );
        terminal.set_config(Arc::new(CsiU));
        terminal.advance_bytes(b"\x1b[?9001l");
        assert_eq!(
            TerminalModes::read(&terminal).as_json()["keyboard"],
            json!({ "encoding": "csi-u", "flags": 0 })
        );
    }
}
