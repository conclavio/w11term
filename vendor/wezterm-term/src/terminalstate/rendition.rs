use crate::{line_width_scale, TerminalState};
use std::fmt::Write as _;
use std::io::Write as _;
use std::ops::Range;
use wezterm_cell::color::{ColorAttribute, ColorSpec};
use wezterm_cell::CellAttributes;
use wezterm_escape_parser::csi::{Blink, Intensity, Sgr, Underline, VerticalAlign};
use wezterm_escape_parser::OneBased;

/// Attributes that have an SGR representation.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct Rendition {
    intensity: Intensity,
    italic: bool,
    underline: Underline,
    blink: Blink,
    inverse: bool,
    invisible: bool,
    strike: bool,
    overline: bool,
    align: VerticalAlign,
    foreground: ColorAttribute,
    background: ColorAttribute,
    underline_color: ColorAttribute,
}

impl From<&CellAttributes> for Rendition {
    fn from(attrs: &CellAttributes) -> Self {
        Self {
            intensity: attrs.intensity(),
            italic: attrs.italic(),
            underline: attrs.underline(),
            blink: attrs.blink(),
            inverse: attrs.reverse(),
            invisible: attrs.invisible(),
            strike: attrs.strikethrough(),
            overline: attrs.overline(),
            align: attrs.vertical_align(),
            foreground: attrs.foreground(),
            background: attrs.background(),
            underline_color: attrs.underline_color(),
        }
    }
}

impl Default for Rendition {
    fn default() -> Self {
        Self::from(&CellAttributes::blank())
    }
}

impl Rendition {
    fn retain_common(&mut self, other: Self) {
        fn shared<T: PartialEq>(a: T, b: T, default: T) -> T {
            if a == b {
                a
            } else {
                default
            }
        }
        self.intensity = shared(self.intensity, other.intensity, Intensity::Normal);
        self.italic &= other.italic;
        self.underline = shared(self.underline, other.underline, Underline::None);
        self.blink = shared(self.blink, other.blink, Blink::None);
        self.inverse &= other.inverse;
        self.invisible &= other.invisible;
        self.strike &= other.strike;
        self.overline &= other.overline;
        self.align = shared(self.align, other.align, VerticalAlign::BaseLine);
        self.foreground = shared(self.foreground, other.foreground, ColorAttribute::Default);
        self.background = shared(self.background, other.background, ColorAttribute::Default);
        self.underline_color = shared(
            self.underline_color,
            other.underline_color,
            ColorAttribute::Default,
        );
    }

    /// Format a restorable SGR parameter list starting with a reset.
    pub(super) fn parameters(self) -> String {
        fn color(value: ColorAttribute) -> Option<ColorSpec> {
            match value {
                ColorAttribute::Default => None,
                ColorAttribute::PaletteIndex(index) => Some(ColorSpec::PaletteIndex(index)),
                ColorAttribute::TrueColorWithDefaultFallback(rgb)
                | ColorAttribute::TrueColorWithPaletteFallback(rgb, _) => {
                    Some(ColorSpec::TrueColor(rgb))
                }
            }
        }

        let mut params = String::from("0");
        for setting in [
            (self.intensity != Intensity::Normal).then_some(Sgr::Intensity(self.intensity)),
            self.italic.then_some(Sgr::Italic(true)),
            (self.underline != Underline::None).then_some(Sgr::Underline(self.underline)),
            (self.blink != Blink::None).then_some(Sgr::Blink(self.blink)),
            self.inverse.then_some(Sgr::Inverse(true)),
            self.invisible.then_some(Sgr::Invisible(true)),
            self.strike.then_some(Sgr::StrikeThrough(true)),
            self.overline.then_some(Sgr::Overline(true)),
            (self.align != VerticalAlign::BaseLine).then_some(Sgr::VerticalAlign(self.align)),
            color(self.foreground).map(Sgr::Foreground),
            color(self.background).map(Sgr::Background),
            color(self.underline_color).map(Sgr::UnderlineColor),
        ]
        .iter()
        .flatten()
        {
            write!(params, ";{setting}").expect("writing to a String is infallible");
            // Keep exactly one final byte for the combined reply.
            let final_byte = params.pop();
            debug_assert_eq!(final_byte, Some('m'));
        }
        params
    }
}

impl TerminalState {
    pub(super) fn report_rectangular_sgr(
        &mut self,
        top: OneBased,
        left: OneBased,
        bottom: OneBased,
        right: OneBased,
    ) {
        let (rows, cols) = self.screen_rectangle(top, left, bottom, right);
        let shared = self.shared_rendition(rows, cols);
        write!(self.writer, "\x1b[{}m", shared.parameters()).ok();
        self.writer.flush().ok();
    }

    fn shared_rendition(&self, rows: Range<usize>, cols: Range<usize>) -> Rendition {
        let plain = Rendition::default();
        if rows.is_empty() || cols.is_empty() {
            return plain;
        }
        let screen = self.screen();
        let first = screen.phys_row(rows.start as i64);
        let mut shared = None;
        for line in screen.physical_lines().skip(first).take(rows.len()) {
            let line_cols = (screen.physical_cols / line_width_scale(line)).max(1);
            if cols.end > line.len().min(line_cols) {
                return plain;
            }
            let mut next = cols.start;
            for cell in line
                .visible_cells()
                .take_while(|cell| cell.cell_index() < cols.end)
            {
                let start = cell.cell_index();
                let end = start + cell.width();
                if end <= cols.start {
                    continue;
                }
                if start > next {
                    return plain;
                }
                // A wide grapheme supplies the style of every column it occupies.
                let style = Rendition::from(cell.attrs());
                let current = shared.get_or_insert(style);
                current.retain_common(style);
                if *current == plain {
                    return plain;
                }
                next = end;
            }
            if next < cols.end {
                return plain;
            }
        }
        shared.unwrap_or_default()
    }
}
