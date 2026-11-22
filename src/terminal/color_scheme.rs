use crate::options::ColorScheme;
use wezterm_term::color::{ColorPalette, SrgbaTuple};

fn rgb(value: u32) -> SrgbaTuple {
    let [_, red, green, blue] = value.to_be_bytes();
    (red, green, blue).into()
}

pub(super) fn palette(scheme: ColorScheme) -> ColorPalette {
    // One Half colors in ANSI order, from Windows Terminal's defaults.json.
    // https://github.com/microsoft/terminal/blob/main/src/cascadia/TerminalSettingsModel/defaults.json
    let (foreground, background, cursor, ansi) = match scheme {
        ColorScheme::Default => {
            return ColorPalette {
                foreground: SrgbaTuple(0.86, 0.88, 0.91, 1.0),
                background: SrgbaTuple(0.055, 0.063, 0.078, 1.0),
                cursor_bg: SrgbaTuple(0.48, 0.79, 0.98, 1.0),
                ..ColorPalette::default()
            };
        }
        ColorScheme::OneHalfDark => (
            0xdcdfe4,
            0x282c34,
            0xffffff,
            [
                0x282c34, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xdcdfe4,
                0x5a6374, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xdcdfe4,
            ],
        ),
        ColorScheme::OneHalfLight => (
            0x383a42,
            0xfafafa,
            0x4f525d,
            [
                0x383a42, 0xe45649, 0x50a14f, 0xc18301, 0x0184bc, 0xa626a4, 0x0997b3, 0xfafafa,
                0x4f525d, 0xdf6c75, 0x98c379, 0xe4c07a, 0x61afef, 0xc577dd, 0x56b5c1, 0xffffff,
            ],
        ),
    };
    let mut palette = ColorPalette {
        foreground: rgb(foreground),
        background: rgb(background),
        cursor_bg: rgb(cursor),
        cursor_border: rgb(cursor),
        cursor_fg: rgb(background),
        ..ColorPalette::default()
    };
    palette.colors.0[..16].copy_from_slice(&ansi.map(rgb));
    if scheme == ColorScheme::OneHalfLight {
        palette.selection_bg = rgb(0x383a42);
        palette.selection_fg = rgb(background);
    }
    palette
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{Snapshot, TransportInput, create_with_profile};
    use tokio::sync::mpsc;
    use wezterm_term::Terminal;

    fn engine(scheme: ColorScheme) -> (Terminal, mpsc::UnboundedReceiver<TransportInput>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            create_with_profile(Default::default(), tx, Default::default(), scheme),
            rx,
        )
    }

    fn assert_query(
        term: &mut Terminal,
        replies: &mut mpsc::UnboundedReceiver<TransportInput>,
        slot: &str,
        expected: u32,
    ) {
        term.advance_bytes(format!("\x1b]{slot};?\x1b\\"));
        let reply = crate::terminal::tests::receive(replies);
        let [_, r, g, b] = expected.to_be_bytes();
        assert_eq!(
            reply,
            format!("\x1b]{slot};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}\x1b\\")
                .as_bytes()
        );
    }

    #[test]
    fn named_profiles_keep_the_color_cube_and_publish_their_palette_on_the_first_frame() {
        assert_eq!(
            palette(ColorScheme::Default),
            ColorPalette {
                foreground: SrgbaTuple(0.86, 0.88, 0.91, 1.0),
                background: SrgbaTuple(0.055, 0.063, 0.078, 1.0),
                cursor_bg: SrgbaTuple(0.48, 0.79, 0.98, 1.0),
                ..ColorPalette::default()
            }
        );
        for (scheme, fg, bg, cursor, red, bright_blue) in [
            (
                ColorScheme::OneHalfDark,
                0xdcdfe4,
                0x282c34,
                0xffffff,
                0xe06c75,
                0x61afef,
            ),
            (
                ColorScheme::OneHalfLight,
                0x383a42,
                0xfafafa,
                0x4f525d,
                0xe45649,
                0x61afef,
            ),
        ] {
            let (mut term, mut replies) = engine(scheme);
            let frame = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
            assert_eq!(frame.palette, palette(scheme));
            assert_eq!(
                frame.palette.colors.0[16..],
                ColorPalette::default().colors.0[16..]
            );
            for (slot, value) in [
                ("10", fg),
                ("11", bg),
                ("12", cursor),
                ("4;1", red),
                ("4;12", bright_blue),
            ] {
                assert_query(&mut term, &mut replies, slot, value);
            }
        }
    }

    #[test]
    fn named_profile_indexed_resets_preserve_dynamic_colors_and_reverse_only_changes_display() {
        let (mut term, mut replies) = engine(ColorScheme::OneHalfLight);
        term.advance_bytes(concat!(
            "\x1b]10;#123456\x07\x1b]11;#abcdef\x07\x1b]12;#345678\x07",
            "\x1b]4;1;#112233;2;#445566;245;#778899\x07\x1b[?5h",
        ));
        term.advance_bytes(b"\x1b]104;1\x07");
        for (slot, value) in [("4;1", 0xe45649), ("4;2", 0x445566), ("4;245", 0x778899)] {
            assert_query(&mut term, &mut replies, slot, value);
        }
        term.advance_bytes(b"\x1b]104\x07");
        for (slot, value) in [
            ("4;2", 0x50a14f),
            ("10", 0x123456),
            ("11", 0xabcdef),
            ("12", 0x345678),
        ] {
            assert_query(&mut term, &mut replies, slot, value);
        }
        assert_eq!(
            term.palette().colors.0[245],
            ColorPalette::default().colors.0[245]
        );
        let reversed = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
        assert_eq!(reversed.palette.background, term.palette().foreground);
        assert_eq!(reversed.palette.foreground, term.palette().background);
        assert_eq!(reversed.palette.cursor_bg, rgb(0x345678));
        // The stored colors remain the application's originals under DECSCNM.
        term.advance_bytes(b"\x1b]111\x07");
        assert_query(&mut term, &mut replies, "11", 0xfafafa);
        assert_query(&mut term, &mut replies, "10", 0x123456);
        term.advance_bytes(b"\x1b]110\x07\x1b]112\x07\x1b[?5l");
        assert_eq!(term.palette(), palette(ColorScheme::OneHalfLight));
    }

    #[test]
    fn named_profile_ris_and_reconnect_restore_only_the_owning_terminal() {
        let (mut light, _) = engine(ColorScheme::OneHalfLight);
        let (mut dark, _) = engine(ColorScheme::OneHalfDark);
        dark.advance_bytes(b"\x1b]4;9;#551122\x07\x1b]11;#224411\x07");
        let dark_override = dark.palette();
        for reconnect in [false, true] {
            light.advance_bytes(b"\x1b[?5h\x1b]4;9;#771199\x07\x1b]11;#334455\x07");
            if reconnect {
                light.reset_connection();
            } else {
                light.advance_bytes(b"\x1bc");
            }
            assert!(!light.get_reverse_video());
            assert_eq!(light.palette(), palette(ColorScheme::OneHalfLight));
            assert_eq!(dark.palette(), dark_override);
        }
    }
}
