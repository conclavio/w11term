use crate::options::ResolvedColorScheme;
use wezterm_term::color::{ColorPalette, SrgbaTuple};

fn rgb(value: u32) -> SrgbaTuple {
    let [_, red, green, blue] = value.to_be_bytes();
    (red, green, blue).into()
}

pub(super) fn palette(scheme: ResolvedColorScheme) -> ColorPalette {
    // One Half colors in ANSI order, from Windows Terminal's defaults.json.
    // https://github.com/microsoft/terminal/blob/main/src/cascadia/TerminalSettingsModel/defaults.json
    let (foreground, background, cursor, ansi) = match scheme {
        ResolvedColorScheme::Default => {
            return ColorPalette {
                foreground: SrgbaTuple(0.86, 0.88, 0.91, 1.0),
                background: SrgbaTuple(0.055, 0.063, 0.078, 1.0),
                cursor_bg: SrgbaTuple(0.48, 0.79, 0.98, 1.0),
                ..ColorPalette::default()
            };
        }
        ResolvedColorScheme::OneHalfDark => (
            0xdcdfe4,
            0x282c34,
            0xffffff,
            [
                0x282c34, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xdcdfe4,
                0x5a6374, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xdcdfe4,
            ],
        ),
        ResolvedColorScheme::OneHalfLight => (
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
    if scheme == ResolvedColorScheme::OneHalfLight {
        palette.selection_bg = rgb(0x383a42);
        palette.selection_fg = rgb(background);
    }
    palette
}

#[cfg(test)]
mod tests {
    use super::{ColorPalette, SrgbaTuple, palette, rgb};
    use crate::options::ResolvedColorScheme as ColorScheme;
    use crate::terminal::{Snapshot, TransportInput, create_with_palette, set_color_scheme};
    use tokio::sync::mpsc;
    use wezterm_term::Terminal;

    fn engine(scheme: ColorScheme) -> (Terminal, mpsc::UnboundedReceiver<TransportInput>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            create_with_palette(Default::default(), tx, Default::default(), scheme),
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
    fn theme_reports_follow_the_selected_scheme_and_only_notify_subscribers() {
        use crate::terminal::tests::receive;
        let (mut term, mut replies) = engine(ColorScheme::OneHalfLight);
        let change = |t: &mut Terminal, scheme| {
            crate::terminal::set_color_scheme(t, Default::default(), scheme);
        };
        const LIGHT: &[u8] = b"\x1b[?997;2n";
        const DARK: &[u8] = b"\x1b[?997;1n";
        // Configuration changes, remote colors and echoed replies before a query stay silent.
        change(&mut term, ColorScheme::Default);
        term.advance_bytes(b"\x1b[?997;2n\x1b]11;#ffffff\x07\x1b[?5h\x1b[?996");
        term.advance_bytes(b"n");
        assert_eq!(receive(&mut replies), DARK);
        term.advance_bytes(b"\x1b[?2031$p");
        assert_eq!(receive(&mut replies), b"\x1b[?2031;2$y");
        term.advance_bytes(b"\x1b[?2031h\x1b[?2031$p");
        assert_eq!(receive(&mut replies), b"\x1b[?2031;1$y");
        // A different dark palette is a real palette update. Reapplying it is silent.
        change(&mut term, ColorScheme::OneHalfDark);
        assert_eq!(receive(&mut replies), DARK);
        change(&mut term, ColorScheme::OneHalfDark);
        term.advance_bytes(b"\x1b]11;#010203\x07\x1b[?5l\x1b[?1049h");
        change(&mut term, ColorScheme::OneHalfLight);
        assert_eq!(receive(&mut replies), LIGHT);
        term.advance_bytes(b"\x1b[?2031l");
        change(&mut term, ColorScheme::OneHalfDark);
        term.advance_bytes(b"\x1b[?2031$p");
        assert_eq!(receive(&mut replies), b"\x1b[?2031;2$y");
        term.advance_bytes(b"\x1b[?2031h\x1bc\x1b[?2031$p");
        assert_eq!(receive(&mut replies), b"\x1b[?2031;2$y");
        term.advance_bytes(b"\x1b[?2031h");
        term.reset_connection();
        term.advance_bytes(b"\x1b[?2031$p\x1b[?996n\x1b[?2999$p");
        assert_eq!(receive(&mut replies), b"\x1b[?2031;2$y");
        assert_eq!(receive(&mut replies), DARK);
        assert_eq!(receive(&mut replies), b"\x1b[?2999;0$y");
    }

    #[test]
    fn framed_viewer_sends_requested_and_unsolicited_theme_reports_as_automatic_input() {
        let (mut term, _original) = engine(ColorScheme::OneHalfDark);
        let (writer, mut viewer) = mpsc::unbounded_channel();
        crate::terminal::enter_herdr_viewer(&mut term, &writer, &Default::default());
        term.advance_bytes(b"\x1b[?2031h\x1b[?996n");
        crate::terminal::set_color_scheme(&mut term, Default::default(), ColorScheme::OneHalfLight);
        for expected in [b"\x1b[?997;1n", b"\x1b[?997;2n"] {
            let TransportInput::Automatic(bytes) = viewer.try_recv().unwrap() else {
                panic!("theme report was sent as user input");
            };
            assert_eq!(bytes, expected);
        }
        assert!(viewer.try_recv().is_err());
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

    #[test]
    fn live_switch_preserves_same_as_base_writes_and_rebases_queried_slots() {
        let (mut term, mut replies) = engine(ColorScheme::OneHalfDark);
        for (slot, old) in [("10", 0xdcdfe4), ("12", 0xffffff), ("4;2", 0x98c379)] {
            assert_query(&mut term, &mut replies, slot, old);
        }
        // The effective palette is identical to the base, but the application
        // explicitly owns these two slots until OSC 111 and OSC 104;1.
        term.advance_bytes(b"\x1b]11;#282c34\x07\x1b]4;1;#e06c75\x07\x1b[?5h");
        term.implicit_palette_reset_if_same_as_configured();
        set_color_scheme(&mut term, Default::default(), ColorScheme::OneHalfLight);
        for (slot, value) in [
            ("11", 0x282c34),
            ("4;1", 0xe06c75),
            ("10", 0x383a42),
            ("12", 0x4f525d),
            ("4;2", 0x50a14f),
        ] {
            assert_query(&mut term, &mut replies, slot, value);
        }
        let snapshot = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
        assert_eq!(snapshot.palette.foreground, rgb(0x282c34));
        assert_eq!(snapshot.palette.background, rgb(0x383a42));
        term.advance_bytes(b"\x1b]111\x07\x1b]104;1\x07");
        assert_eq!(term.palette(), palette(ColorScheme::OneHalfLight));
        set_color_scheme(&mut term, Default::default(), ColorScheme::OneHalfDark);
        assert_eq!(term.palette(), palette(ColorScheme::OneHalfDark));
    }

    #[test]
    fn live_switch_preserves_independent_dynamic_slots_until_their_resets() {
        let (mut term, mut replies) = engine(ColorScheme::OneHalfDark);
        term.advance_bytes(concat!(
            "\x1b]10;#112233;#445566;#778899\x07",
            "\x1b]17;#123456\x07\x1b]19;#abcdef\x07",
            "\x1b]4;15;#001122;245;#334455\x07",
        ));
        set_color_scheme(&mut term, Default::default(), ColorScheme::OneHalfLight);
        for (slot, value) in [
            ("10", 0x112233),
            ("11", 0x445566),
            ("12", 0x778899),
            ("17", 0x123456),
            ("19", 0xabcdef),
            ("4;15", 0x001122),
            ("4;245", 0x334455),
        ] {
            assert_query(&mut term, &mut replies, slot, value);
        }
        let light = palette(ColorScheme::OneHalfLight);
        assert_eq!(term.palette().cursor_border, rgb(0x778899));
        assert_eq!(term.palette().cursor_fg, light.cursor_fg);
        term.advance_bytes(b"\x1b]104\x07\x1b]112\x07\x1b]117\x07");
        assert_eq!(term.palette().colors, light.colors);
        assert_eq!(term.palette().cursor_border, light.cursor_border);
        for (slot, value) in [
            ("12", 0x4f525d),
            ("17", 0x383a42),
            ("19", 0xabcdef),
            ("10", 0x112233),
        ] {
            assert_query(&mut term, &mut replies, slot, value);
        }
        term.advance_bytes(b"\x1b]110\x07\x1b]111\x07\x1b]119\x07");
        assert_eq!(term.palette(), light);
        for reconnect in [false, true] {
            term.advance_bytes(b"\x1b]11;#010203\x07");
            if reconnect {
                term.reset_connection()
            } else {
                term.advance_bytes(b"\x1bc")
            }
            assert_eq!(term.palette(), light);
        }
    }

    #[test]
    fn direct_palette_edits_survive_a_switch_without_pinning_unedited_fields() {
        let (mut term, _) = engine(ColorScheme::OneHalfDark);
        let dark = palette(ColorScheme::OneHalfDark);
        let light = palette(ColorScheme::OneHalfLight);
        term.palette_mut().split = rgb(0x123456);
        term.palette_mut().colors.0[2] = light.colors.0[2];
        term.advance_bytes(b"\x1b]4;1;#e06c75\x07");
        set_color_scheme(&mut term, Default::default(), ColorScheme::OneHalfLight);
        assert_eq!(term.palette().split, rgb(0x123456));
        assert_eq!(term.palette().foreground, light.foreground);
        set_color_scheme(&mut term, Default::default(), ColorScheme::OneHalfDark);
        assert_eq!(term.palette().colors.0[2], light.colors.0[2]);
        term.palette_mut().colors.0[2] = dark.colors.0[2];
        term.palette_mut().split = dark.split;
        term.implicit_palette_reset_if_same_as_configured();
        set_color_scheme(&mut term, Default::default(), ColorScheme::OneHalfLight);
        assert_eq!(
            term.palette().colors.0[1],
            dark.colors.0[1],
            "explicit OSC assignment"
        );
        assert_eq!(term.palette().colors.0[2], light.colors.0[2]);
        assert_eq!(term.palette().split, light.split);
    }

    #[test]
    fn xterm_color_stack_restores_dynamic_and_indexed_overrides_over_the_live_theme() {
        let (mut term, mut replies) = engine(ColorScheme::OneHalfDark);
        term.advance_bytes(concat!(
            "\x1b]11;#282c34\x07\x1b]4;245;#123456\x07",
            "\x1b[#P\x1b[?1049h\x1b]10;#223344;#556677;#8899aa\x07",
            "\x1b]17;#abcdef\x07\x1b]19;#102030\x07\x1b]4;1;#334455\x07",
            "\x1b[#P\x1b]111\x07\x1b]112\x07\x1b]104\x07\x1b[?1049l",
        ));
        set_color_scheme(&mut term, Default::default(), ColorScheme::OneHalfLight);
        term.advance_bytes(b"\x1b[#Q");
        for (slot, value) in [
            ("10", 0x223344),
            ("11", 0x556677),
            ("12", 0x8899aa),
            ("17", 0xabcdef),
            ("19", 0x102030),
            ("4;1", 0x334455),
            ("4;245", 0x123456),
        ] {
            assert_query(&mut term, &mut replies, slot, value);
        }
        assert_eq!(term.palette().cursor_border, rgb(0x8899aa));
        term.advance_bytes(b"\x1b[0#Q");
        let mut restored = palette(ColorScheme::OneHalfLight);
        // The explicit write matched the original dark theme, so it still owns
        // the background slot. Unset slots inherit the newly selected theme.
        restored.background = rgb(0x282c34);
        restored.colors.0[245] = rgb(0x123456);
        assert_eq!(term.palette(), restored);
        term.advance_bytes(b"\x1b[#R");
        assert_eq!(
            crate::terminal::tests::receive(&mut replies),
            b"\x1b[?0;2#Q"
        );
    }

    #[test]
    fn xterm_color_stack_numbered_slots_persist_after_pop_and_transport_reset_clears_them() {
        let (mut term, mut replies) = engine(ColorScheme::OneHalfDark);
        let report = |term: &mut Terminal, replies: &mut _, expected: &[u8]| {
            term.advance_bytes(b"\x1b[#R");
            assert_eq!(crate::terminal::tests::receive(replies), expected);
        };
        report(&mut term, &mut replies, b"\x1b[?0;0#Q");
        // Feed a numbered save one byte at a time, as with packet boundaries.
        term.advance_bytes(b"\x1b]11;#112233\x07");
        for byte in b"\x1b[2;10#P" {
            term.advance_bytes([*byte]);
        }
        report(&mut term, &mut replies, b"\x1b[?10;0#Q");
        term.advance_bytes(b"\x1b]11;#445566\x07\x1b[#P\x1b[11;999;6#Q");
        report(&mut term, &mut replies, b"\x1b[?10;0#Q");
        assert_eq!(term.palette().background, rgb(0x445566));
        term.advance_bytes(b"\x1b[10;2#Q");
        report(&mut term, &mut replies, b"\x1b[?1;0#Q");
        assert_eq!(term.palette().background, rgb(0x112233));
        term.advance_bytes(b"\x1bc\x1b[10#Q");
        report(&mut term, &mut replies, b"\x1b[?9;0#Q");
        assert_eq!(term.palette().background, rgb(0x112233));
        term.reset_connection();
        term.advance_bytes(b"\x1b[10;2#Q");
        report(&mut term, &mut replies, b"\x1b[?0;0#Q");
        assert_eq!(term.palette(), palette(ColorScheme::OneHalfDark));
    }

    #[test]
    fn xterm_color_stack_distinguishes_an_omitted_parameter_from_zero_and_ignores_replies() {
        let (mut term, mut replies) = engine(ColorScheme::OneHalfDark);
        term.advance_bytes(b"\x1b]11;#102030\x07\x1b[999;#P\x1b]11;#abcdef\x07\x1b[0#P");
        term.advance_bytes(b"\x1b[?2;1#Q\x1b[1:2#Q\x1b[#Q\x1b[#R");
        assert_eq!(
            crate::terminal::tests::receive(&mut replies),
            b"\x1b[?0;1#Q"
        );
        assert_eq!(term.palette().background, rgb(0xabcdef));
        term.advance_bytes(b"\x1b[1#Q");
        assert_eq!(term.palette().background, rgb(0x102030));
        term.advance_bytes(b"\x1b[2#Q");
        assert_eq!(term.palette().background, rgb(0xabcdef));
    }
}
