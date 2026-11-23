use wezterm_cell::CellAttributes;
use wezterm_escape_parser::csi::{Intensity, Underline};

const MAX_DEPTH: usize = 10;

bitflags::bitflags! {
    struct SgrMask: u16 {
        const BOLD = 1 << 0;
        const FAINT = 1 << 1;
        const ITALIC = 1 << 2;
        const UNDERLINE = 1 << 3;
        const BLINK = 1 << 4;
        const INVERSE = 1 << 5;
        const INVISIBLE = 1 << 6;
        const STRIKE = 1 << 7;
        const DOUBLE_UNDERLINE = 1 << 8;
        const FOREGROUND = 1 << 9;
        const BACKGROUND = 1 << 10;
        const UNDERLINE_COLOR = 1 << 11;
        const OVERLINE = 1 << 12;
        const VERTICAL_ALIGN = 1 << 13;
    }
}

impl SgrMask {
    fn from_parameters(parameters: &[i64]) -> Self {
        if parameters.is_empty() {
            return Self::all();
        }
        let mut mask = Self::empty();
        for value in parameters {
            mask.insert(match value {
                1 => Self::BOLD,
                2 => Self::FAINT,
                3 => Self::ITALIC,
                4 => Self::UNDERLINE,
                5 => Self::BLINK,
                7 => Self::INVERSE,
                8 => Self::INVISIBLE,
                9 => Self::STRIKE,
                21 => Self::DOUBLE_UNDERLINE,
                10 | 30 => Self::FOREGROUND,
                11 | 31 => Self::BACKGROUND,
                _ => continue,
            });
        }
        mask
    }

    fn intensity(value: Intensity) -> Self {
        match value {
            Intensity::Bold => Self::BOLD,
            Intensity::Half => Self::FAINT,
            Intensity::Normal => Self::empty(),
        }
    }

    fn underline(value: Underline) -> Self {
        match value {
            Underline::None => Self::empty(),
            Underline::Double => Self::DOUBLE_UNDERLINE,
            _ => Self::UNDERLINE,
        }
    }

    fn restore(self, pen: &mut CellAttributes, saved: &CellAttributes) {
        let intensity = saved.intensity();
        if self.intersects(Self::intensity(intensity)) {
            pen.set_intensity(intensity);
        } else if self.intersects(Self::intensity(pen.intensity())) {
            pen.set_intensity(Intensity::Normal);
        }
        let underline = saved.underline();
        if self.intersects(Self::underline(underline)) {
            pen.set_underline(underline);
        } else if self.intersects(Self::underline(pen.underline())) {
            pen.set_underline(Underline::None);
        }

        if self.contains(Self::ITALIC) {
            pen.set_italic(saved.italic());
        }
        if self.contains(Self::BLINK) {
            pen.set_blink(saved.blink());
        }
        if self.contains(Self::INVERSE) {
            pen.set_reverse(saved.reverse());
        }
        if self.contains(Self::INVISIBLE) {
            pen.set_invisible(saved.invisible());
        }
        if self.contains(Self::STRIKE) {
            pen.set_strikethrough(saved.strikethrough());
        }
        if self.contains(Self::FOREGROUND) {
            pen.set_foreground(saved.foreground());
        }
        if self.contains(Self::BACKGROUND) {
            pen.set_background(saved.background());
        }
        if self.contains(Self::UNDERLINE_COLOR) {
            pen.set_underline_color(saved.underline_color());
        }
        if self.contains(Self::OVERLINE) {
            pen.set_overline(saved.overline());
        }
        if self.contains(Self::VERTICAL_ALIGN) {
            pen.set_vertical_align(saved.vertical_align());
        }
    }
}

#[derive(Default)]
pub(super) struct SgrStack {
    levels: Vec<(SgrMask, CellAttributes)>,
    overflow: usize,
}

impl SgrStack {
    pub(super) fn push(&mut self, parameters: &[i64], pen: &CellAttributes) {
        if self.levels.len() == MAX_DEPTH {
            // Pair excess pushes with their pops while retaining ten renditions.
            self.overflow = self.overflow.saturating_add(1);
            return;
        }
        let mask = SgrMask::from_parameters(parameters);
        let mut saved = CellAttributes::default();
        mask.restore(&mut saved, pen);
        self.levels.push((mask, saved));
    }

    pub(super) fn pop(&mut self, pen: &mut CellAttributes) {
        if self.overflow > 0 {
            self.overflow -= 1;
        } else if let Some((mask, saved)) = self.levels.pop() {
            mask.restore(pen, &saved);
        }
    }
}
