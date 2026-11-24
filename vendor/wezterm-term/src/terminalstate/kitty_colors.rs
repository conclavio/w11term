use super::palette::{Assignment, Assignments, Slot};
use super::*;
use crate::color::SrgbaTuple;
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine as _};
use wezterm_escape_parser::csi::PaletteStack;

fn slot(key: &str) -> Option<Slot> {
    Some(match key {
        "foreground" => Slot::Foreground,
        "background" => Slot::Background,
        "cursor" => Slot::CursorBackground,
        "cursor_text" => Slot::CursorForeground,
        "selection_background" => Slot::SelectionBackground,
        "selection_foreground" => Slot::SelectionForeground,
        "visual_bell" => Slot::VisualBell,
        index if index.bytes().all(|b| b.is_ascii_digit()) => Slot::Indexed(index.parse().ok()?),
        _ => return None,
    })
}

fn fraction(text: &str) -> Option<f32> {
    let value: f64 = text.parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    Some(value.clamp(0.0, 1.0) as f32)
}

fn parse_color(text: &str) -> Option<SrgbaTuple> {
    let (color, alpha) = match text.split_once('@') {
        Some((color, alpha)) => (color, fraction(alpha)?),
        None => (text, 1.0),
    };
    let mut color = if let Some(components) = color.strip_prefix("rgbi:") {
        let mut components = components.split('/');
        let red = fraction(components.next()?)?;
        let green = fraction(components.next()?)?;
        let blue = fraction(components.next()?)?;
        if components.next().is_some() {
            return None;
        }
        SrgbaTuple(red, green, blue, 1.0)
    } else if color.starts_with('#') || color.starts_with("rgb:") {
        color.parse().ok()?
    } else {
        SrgbaTuple::from_named(color)?
    };
    color.3 = alpha;
    Some(color)
}

fn with_cursor_border(slot: Slot) -> impl Iterator<Item = Slot> {
    let border = (slot == Slot::CursorBackground).then_some(Slot::CursorBorder);
    std::iter::once(slot).chain(border)
}

impl TerminalState {
    pub(super) fn kitty_palette_stack(&mut self, parts: &[Vec<u8>]) -> bool {
        let selector = match parts {
            [selector] => selector,
            [selector, value] if value.is_empty() => selector,
            _ => return false,
        };
        let operation = match selector.as_slice() {
            b"30001" => PaletteStack::Push(Vec::new()),
            b"30101" => PaletteStack::Pop(Vec::new()),
            _ => return false,
        };
        self.perform_palette_stack(operation);
        true
    }

    pub(super) fn perform_kitty_colors(&mut self, body: &[u8]) {
        let Ok(body) = std::str::from_utf8(body) else {
            return;
        };
        self.collect_palette_edits();
        let mut reply = String::new();
        let mut pending: Option<Assignments> = None;
        let mut defaults = None;
        for record in body.split(';') {
            let (key, value) = match record.split_once('=') {
                Some((key, value)) => (key, Some(value)),
                None => (record, None),
            };
            // `unknown` is reserved for replies. SSH peers that echo replies
            // back must not produce a new response each time.
            if key.is_empty() || key == "unknown" {
                continue;
            }
            let Some(slot) = slot(key) else {
                write!(reply, ";unknown={}", STANDARD_NO_PAD.encode(key)).ok();
                continue;
            };
            match value {
                Some("?") => {
                    write!(reply, ";{key}=").ok();
                    let assignments = pending.as_ref().unwrap_or(&self.palette.assigned);
                    if let Some(color) = self.kitty_color(slot, assignments, &mut defaults) {
                        let byte = |channel: f32| (channel * 255.0).round() as u8;
                        let r = byte(color.0);
                        let g = byte(color.1);
                        let b = byte(color.2);
                        write!(reply, "rgb:{r:02x}/{g:02x}/{b:02x}").ok();
                        if color.3 != 1.0 {
                            write!(reply, "@{}", color.3).ok();
                        }
                    }
                }
                None => {
                    let assignments = pending.get_or_insert_with(|| self.palette.assigned.clone());
                    for slot in with_cursor_border(slot) {
                        assignments.remove(&slot);
                    }
                }
                Some(value) => {
                    let assignment = if value.is_empty() {
                        match slot {
                            Slot::Foreground | Slot::Background | Slot::Indexed(_) => None,
                            _ => Some(Assignment::Automatic),
                        }
                    } else {
                        parse_color(value).map(Assignment::Fixed)
                    };
                    if let Some(assignment) = assignment {
                        let assignments =
                            pending.get_or_insert_with(|| self.palette.assigned.clone());
                        for slot in with_cursor_border(slot) {
                            assignments.insert(slot, assignment);
                        }
                    }
                }
            }
        }
        if let Some(assignments) = pending.filter(|assigned| *assigned != self.palette.assigned) {
            self.palette.assigned = assignments;
            self.rebuild_palette();
            self.palette_did_change();
        }
        if !reply.is_empty() {
            write!(self.writer, "\x1b]21{reply}\x1b\\").ok();
            self.writer.flush().ok();
        }
    }

    fn kitty_color(
        &self,
        slot: Slot,
        assignments: &Assignments,
        defaults: &mut Option<ColorPalette>,
    ) -> Option<SrgbaTuple> {
        if let Some(value) = assignments.get(&slot) {
            return value.fixed();
        }
        match slot {
            // The default selection and bell use the embedding's UI effects.
            Slot::SelectionForeground | Slot::SelectionBackground | Slot::VisualBell => None,
            _ => slot
                .field(defaults.get_or_insert_with(|| self.config.color_palette()))
                .copied(),
        }
    }
}
