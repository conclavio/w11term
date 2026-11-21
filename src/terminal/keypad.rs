//! Kitty gives keypad functions distinct identities even when Windows supplies
//! the same logical key (Enter, Left, Delete, etc.) as the main keyboard.
use std::fmt::Write;
use wezterm_input_types::{KeyboardLedStatus, KittyKeyboardFlags as Flags};
use wezterm_term::{KeyCode, KeyModifiers as Mods};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum KeypadKey {
    Digit0 = 57399,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    Decimal,
    Divide,
    Multiply,
    Subtract,
    Add,
    Enter,
    Equal,
    Separator,
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Insert,
    Delete,
    Begin,
}

impl KeypadKey {
    /// None delegates to the regular logical-key encoder when keypad
    /// disambiguation is off. An empty result suppresses an unrequested release.
    pub(super) fn encode(
        self,
        flags: Flags,
        logical: KeyCode,
        modifiers: Mods,
        details: &super::KeyDetails,
        down: bool,
        repeat: bool,
    ) -> Option<String> {
        if !flags
            .intersects(Flags::DISAMBIGUATE_ESCAPE_CODES | Flags::REPORT_ALL_KEYS_AS_ESCAPE_CODES)
            && self != Self::Begin
        {
            return None;
        }
        let events = flags.contains(Flags::REPORT_EVENT_TYPES);
        if !down && !events {
            return Some(String::new());
        }
        let mut character = [0; 4];
        let text = if down && !modifiers.intersects(Mods::CTRL | Mods::ALT | Mods::SUPER) {
            details
                .text
                .as_deref()
                .or_else(|| match logical {
                    KeyCode::Char(c) if !c.is_control() => Some(&*c.encode_utf8(&mut character)),
                    _ => None,
                })
                .filter(|s| !s.is_empty() && !s.chars().any(char::is_control))
        } else {
            None
        };
        let all = flags.contains(Flags::REPORT_ALL_KEYS_AS_ESCAPE_CODES);
        if !all && let Some(text) = text {
            return Some(text.to_owned());
        }
        let modifier = 1
            + u8::from(modifiers.contains(Mods::SHIFT))
            + 2 * u8::from(modifiers.contains(Mods::ALT))
            + 4 * u8::from(modifiers.contains(Mods::CTRL))
            + 8 * u8::from(modifiers.contains(Mods::SUPER))
            + 64 * u8::from(details.leds.contains(KeyboardLedStatus::CAPS_LOCK))
            + 128 * u8::from(details.leds.contains(KeyboardLedStatus::NUM_LOCK));
        let event = if events && !down {
            ":3"
        } else if events && repeat {
            ":2"
        } else {
            ""
        };
        let associated = text.filter(|_| all && flags.contains(Flags::REPORT_ASSOCIATED_TEXT));
        let mut wire = String::from("\x1b[");
        if self != Self::Begin {
            write!(wire, "{}", self as u16).unwrap();
        } else if modifier != 1 || !event.is_empty() {
            wire.push('1');
        }
        if modifier != 1 || !event.is_empty() {
            write!(wire, ";{modifier}{event}").unwrap();
        } else if associated.is_some() {
            wire.push(';');
        }
        if let Some(text) = associated {
            for (index, c) in text.chars().enumerate() {
                wire.push(if index == 0 { ';' } else { ':' });
                write!(wire, "{}", c as u32).unwrap();
            }
        }
        wire.push(if self == Self::Begin { 'E' } else { 'u' });
        Some(wire)
    }
}
