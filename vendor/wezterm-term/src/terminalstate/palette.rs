use super::*;
use crate::color::SrgbaTuple;
use wezterm_escape_parser::csi::PaletteStack as PaletteOperation;
use wezterm_escape_parser::osc::DynamicColorNumber;

const STACK_CAPACITY: i64 = 10;

type Assignments = HashMap<Slot, SrgbaTuple>;

#[derive(Default)]
pub(super) struct PaletteStack {
    slots: Vec<Option<Assignments>>,
    position: usize,
    high_water: usize,
}

impl PaletteStack {
    fn push(&mut self, parameter: Option<i64>, colors: &Assignments) {
        let index = match parameter {
            None | Some(0) => self.position,
            Some(slot @ 1..=STACK_CAPACITY) => slot as usize - 1,
            _ => return,
        };
        if index >= STACK_CAPACITY as usize {
            return;
        }
        if index >= self.slots.len() {
            self.slots.resize_with(index + 1, || None);
        }
        self.slots[index]
            .get_or_insert_with(HashMap::new)
            .clone_from(colors);
        // xterm numbers slots from one and retains them after a pop. A bare
        // push can grow the high-water mark. An explicit zero rewinds the
        // position after writing the next slot.
        self.position = match parameter {
            Some(0) => 0,
            _ => index + 1,
        };
        if parameter.is_none() {
            self.high_water = self.high_water.max(self.position);
        }
    }

    fn pop(&mut self, parameter: Option<i64>) -> Option<usize> {
        let index = match parameter {
            None | Some(0) => self.position.checked_sub(1)?,
            Some(slot @ 1..=STACK_CAPACITY) => slot as usize - 1,
            _ => return None,
        };
        self.slots.get(index)?.as_ref()?;
        self.position = index;
        Some(index)
    }

    fn restore(&mut self, params: &[Option<i64>], colors: &mut Assignments) -> bool {
        let mut last = None;
        for &param in params {
            if let Some(index) = self.pop(param) {
                last = Some(index);
            }
        }
        let Some(saved) = last.and_then(|index| self.slots[index].as_ref()) else {
            return false;
        };
        if *colors == *saved {
            return false;
        }
        colors.clone_from(saved);
        true
    }
}

fn with_default(params: &[Option<i64>]) -> &[Option<i64>] {
    if params.is_empty() {
        &[None]
    } else {
        params
    }
}

/// Keep the resolved palette cheap to snapshot. The assignment table distinguishes
/// an application's deliberate choice of the current default from an inherited slot.
#[derive(Default)]
pub(super) struct PaletteOverrides {
    pub(super) effective: Option<ColorPalette>,
    pub(super) assigned: Assignments,
    pub(super) borrowed_mutably: bool,
}

#[derive(Clone, Copy, Eq, PartialEq, Hash)]
pub(super) enum Slot {
    Indexed(u8),
    Foreground,
    Background,
    CursorBackground,
    CursorForeground,
    CursorBorder,
    SelectionBackground,
    SelectionForeground,
    ScrollbarThumb,
    Split,
}

impl Slot {
    pub(super) fn dynamic(number: DynamicColorNumber) -> Option<Self> {
        Some(match number {
            DynamicColorNumber::TextForegroundColor => Self::Foreground,
            DynamicColorNumber::TextBackgroundColor => Self::Background,
            DynamicColorNumber::TextCursorColor => Self::CursorBackground,
            DynamicColorNumber::HighlightBackgroundColor => Self::SelectionBackground,
            DynamicColorNumber::HighlightForegroundColor => Self::SelectionForeground,
            _ => return None,
        })
    }

    pub(super) fn field(self, palette: &mut ColorPalette) -> &mut SrgbaTuple {
        match self {
            Self::Indexed(index) => &mut palette.colors.0[index as usize],
            Self::Foreground => &mut palette.foreground,
            Self::Background => &mut palette.background,
            Self::CursorBackground => &mut palette.cursor_bg,
            Self::CursorForeground => &mut palette.cursor_fg,
            Self::CursorBorder => &mut palette.cursor_border,
            Self::SelectionBackground => &mut palette.selection_bg,
            Self::SelectionForeground => &mut palette.selection_fg,
            Self::ScrollbarThumb => &mut palette.scrollbar_thumb,
            Self::Split => &mut palette.split,
        }
    }
}

impl TerminalState {
    pub(super) fn perform_palette_stack(&mut self, operation: PaletteOperation) {
        match operation {
            PaletteOperation::Push(params) => {
                self.collect_palette_edits();
                for &param in with_default(&params) {
                    self.palette_stack.push(param, &self.palette.assigned);
                }
            }
            PaletteOperation::Pop(params) => {
                self.collect_palette_edits();
                if self
                    .palette_stack
                    .restore(with_default(&params), &mut self.palette.assigned)
                {
                    self.rebuild_palette();
                    self.palette_did_change();
                }
            }
            PaletteOperation::Report => {
                write!(
                    self.writer,
                    "\x1b[?{};{}#Q",
                    self.palette_stack.position, self.palette_stack.high_water
                )
                .ok();
                self.writer.flush().ok();
            }
        }
    }

    pub(super) fn materialize_palette(&mut self) -> &mut ColorPalette {
        let config = &self.config;
        self.palette
            .effective
            .get_or_insert_with(|| config.color_palette())
    }

    pub(super) fn set_palette_slot(&mut self, slot: Slot, color: SrgbaTuple) {
        self.collect_palette_edits();
        *slot.field(self.materialize_palette()) = color;
        self.palette.assigned.insert(slot, color);
    }

    pub(super) fn reset_palette_slots(&mut self, slots: impl Iterator<Item = Slot>) {
        self.collect_palette_edits();
        let mut changed = false;
        for slot in slots {
            changed |= self.palette.assigned.remove(&slot).is_some();
        }
        if changed {
            self.rebuild_palette();
            self.palette_did_change();
        }
    }

    /// A raw palette_mut borrow can edit fields outside OSC, such as the scrollbar.
    /// Reconcile it before the next palette mutation, never while painting or querying.
    pub(super) fn collect_palette_edits(&mut self) {
        if !std::mem::take(&mut self.palette.borrowed_mutably) {
            return;
        }
        let current = self
            .palette
            .effective
            .as_mut()
            .expect("palette_mut materialized the palette");
        let mut base = self.config.color_palette();
        for slot in (0..=u8::MAX).map(Slot::Indexed).chain([
            Slot::Foreground,
            Slot::Background,
            Slot::CursorForeground,
            Slot::CursorBackground,
            Slot::CursorBorder,
            Slot::SelectionForeground,
            Slot::SelectionBackground,
            Slot::ScrollbarThumb,
            Slot::Split,
        ]) {
            let value = *slot.field(current);
            let default = *slot.field(&mut base);
            if self.palette.assigned.get(&slot).copied().unwrap_or(default) != value {
                if value == default {
                    self.palette.assigned.remove(&slot);
                } else {
                    self.palette.assigned.insert(slot, value);
                }
            }
        }
    }

    pub(super) fn rebuild_palette(&mut self) {
        self.palette.effective = if self.palette.assigned.is_empty() {
            None
        } else {
            let mut merged = self.config.color_palette();
            for (&slot, &value) in &self.palette.assigned {
                *slot.field(&mut merged) = value;
            }
            Some(merged)
        };
    }
}
