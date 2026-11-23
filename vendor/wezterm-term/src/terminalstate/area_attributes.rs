use super::rectangle::rectangle_axis;
use super::*;
use wezterm_cell::{Blink, Intensity, Underline};
use wezterm_surface::line::Line;

bitflags::bitflags! {
    #[derive(Default)]
    struct Attributes: u8 {
        const BOLD = 1 << 0;
        const UNDERLINE = 1 << 1;
        const BLINK = 1 << 2;
        const INVERSE = 1 << 3;
        const INVISIBLE = 1 << 4;
        const DEC = Self::BOLD.bits | Self::UNDERLINE.bits | Self::BLINK.bits | Self::INVERSE.bits;
    }
}

#[derive(Default)]
struct AttributeEdit {
    set: Attributes,
    clear: Attributes,
    toggle: Attributes,
}

impl AttributeEdit {
    fn new(parameters: &[i64], reverse: bool) -> Self {
        let mut edit = Self::default();
        for &param in parameters {
            let (selected, enabled) = match param {
                0 => (Attributes::DEC, false),
                1 => (Attributes::BOLD, true),
                4 => (Attributes::UNDERLINE, true),
                5 => (Attributes::BLINK, true),
                7 => (Attributes::INVERSE, true),
                8 => (Attributes::INVISIBLE, true),
                22 if !reverse => (Attributes::BOLD, false),
                24 if !reverse => (Attributes::UNDERLINE, false),
                25 if !reverse => (Attributes::BLINK, false),
                27 if !reverse => (Attributes::INVERSE, false),
                28 if !reverse => (Attributes::INVISIBLE, false),
                _ => continue,
            };
            if reverse {
                edit.toggle ^= selected;
            } else {
                edit.set.remove(selected);
                edit.clear.remove(selected);
                if enabled {
                    edit.set.insert(selected);
                } else {
                    edit.clear.insert(selected);
                }
            }
        }
        edit
    }

    fn apply(&self, attrs: &mut CellAttributes) -> bool {
        use self::Attributes as A;
        let mut before = A::empty();
        before.set(A::BOLD, attrs.intensity() == Intensity::Bold);
        before.set(A::UNDERLINE, attrs.underline() != Underline::None);
        before.set(A::BLINK, attrs.blink() != Blink::None);
        before.set(A::INVERSE, attrs.reverse());
        before.set(A::INVISIBLE, attrs.invisible());
        let after = ((before | self.set) & !self.clear) ^ self.toggle;
        let mut changed = before ^ after;
        if self.set.contains(A::UNDERLINE) && attrs.underline() != Underline::Single {
            changed.insert(A::UNDERLINE);
        }
        if self.set.contains(A::BLINK) && attrs.blink() != Blink::Slow {
            changed.insert(A::BLINK);
        }
        if changed.contains(A::BOLD) {
            attrs.set_intensity(if after.contains(A::BOLD) {
                Intensity::Bold
            } else {
                Intensity::Normal
            });
        }
        if changed.contains(A::UNDERLINE) {
            attrs.set_underline(if after.contains(A::UNDERLINE) {
                Underline::Single
            } else {
                Underline::None
            });
        }
        if changed.contains(A::BLINK) {
            attrs.set_blink(if after.contains(A::BLINK) {
                Blink::Slow
            } else {
                Blink::None
            });
        }
        if changed.contains(A::INVERSE) {
            attrs.set_reverse(after.contains(A::INVERSE));
        }
        if changed.contains(A::INVISIBLE) {
            attrs.set_invisible(after.contains(A::INVISIBLE));
        }
        !changed.is_empty()
    }
}

impl TerminalState {
    pub(super) fn edit_area_attributes(
        &mut self,
        [top, left, bottom, right]: [OneBased; 4],
        parameters: &[i64],
        reverse: bool,
    ) {
        let edit = AttributeEdit::new(parameters, reverse);
        if (edit.set | edit.clear | edit.toggle).is_empty() {
            return;
        }
        let (row_limits, col_limits) = self.rectangle_limits();
        let rows = rectangle_axis(top, bottom, row_limits);
        if rows.is_empty() || col_limits.is_empty() {
            return;
        }
        let rectangle = self.attribute_extent == 2;
        if rectangle && left.as_one_based() > right.as_one_based() {
            return;
        }
        let first = rectangle_axis(left, left, col_limits.clone()).start;
        let last = rectangle_axis(right, right, col_limits).end;
        let seqno = self.seqno;
        let screen = self.screen_mut();
        for row in rows.clone() {
            let row_index = row as VisibleRowIndex;
            let width = screen.logical_cols(row_index);
            let start = if rectangle || row == rows.start {
                first
            } else {
                0
            };
            let end = if rectangle || row + 1 == rows.end {
                last.min(width)
            } else {
                width
            };
            if start < end {
                let physical = screen.phys_row(row_index);
                edit_line(
                    screen.line_mut(physical),
                    start..end,
                    rectangle,
                    &edit,
                    seqno,
                );
            }
        }
    }
}

fn edit_line(
    line: &mut Line,
    columns: Range<usize>,
    rectangle: bool,
    edit: &AttributeEdit,
    seqno: SequenceNo,
) {
    if rectangle && columns.end > line.len() && edit.apply(&mut CellAttributes::blank()) {
        line.resize(columns.end, seqno);
    }
    let end = columns.end.min(line.len());
    if columns.start >= end {
        return;
    }
    let mut actual = columns.start..end;
    // A selected continuation changes the whole grapheme's style, including
    // its head. Mutating attributes directly keeps each cell's image tiles.
    for cell in line
        .visible_cells()
        .take_while(|cell| cell.cell_index() < end)
    {
        let start = cell.cell_index();
        let next = start + cell.width().max(1);
        if next > columns.start {
            actual.start = actual.start.min(start);
            actual.end = actual.end.max(next);
        }
    }
    actual.end = actual.end.min(line.len());
    let mut changed = false;
    for cell in &mut line.cells_mut_for_attr_changes_only()[actual] {
        changed |= edit.apply(cell.attrs_mut());
    }
    if changed {
        line.update_last_change_seqno(seqno);
    }
}
