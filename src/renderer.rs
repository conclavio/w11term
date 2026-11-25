use crate::{
    cursor_blink::{CursorBlink, CursorCell},
    native_drawing::{GpuImage, NativeDrawing, Typography},
};
use std::collections::{HashMap, HashSet};
use std::{cell::RefCell, ops::Range, rc::Rc, time::Instant};
use termwiz::surface::CursorShape;
use w11term::{
    appearance::CursorGuide,
    interaction::Selection,
    terminal::{Rgba, Snapshot, colors, rgba},
};
use wezterm_term::{
    CellAttributes, FractionalTextScale, Intensity, Underline,
    image::{ImageCell, ImageDataType},
};
use windows_canvas::{
    Brush, ColorF, DrawContext, DrawingSession, Rect, TextFormat, TextLayout, Vector2, WordWrapping,
};

pub const PADDING: f32 = 12.0;
pub const STATUS_HEIGHT: f32 = 26.0;
const SELECTION_BG: Rgba = [0.25, 0.53, 0.90, 0.4];
#[cfg(test)]
const FONT_SIZE: f32 = 16.0;

#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub cell_width: f32,
    pub cell_height: f32,
    pub cols: usize,
    pub rows: usize,
    pub scale: f32,
}
impl Default for Metrics {
    fn default() -> Self {
        Self {
            cell_width: 9.6,
            cell_height: 21.0,
            cols: 80,
            rows: 24,
            scale: 1.0,
        }
    }
}

/// Renderer coordinates and Reactor pointer events are both expressed in pane-local DIPs.
pub struct CellHit {
    pub col: usize,
    pub row: usize,
    pub inside: bool,
}
impl Metrics {
    pub fn hit(self, x: f64, y: f64) -> CellHit {
        let col = ((x as f32 - PADDING) / self.cell_width).floor();
        let row = ((y as f32 - PADDING) / self.cell_height).floor();
        CellHit {
            col: (col as usize).min(self.cols.saturating_sub(1)),
            row: (row as usize).min(self.rows.saturating_sub(1)),
            inside: col >= 0.0 && row >= 0.0 && col < self.cols as f32 && row < self.rows as f32,
        }
    }
}
impl CellHit {
    pub fn point(&self, frame: &Snapshot) -> w11term::interaction::Point {
        w11term::interaction::Point {
            row: frame.first_stable_row + self.row as isize,
            col: frame
                .cell_map(self.row)
                .map_or(self.col, |map| map.logical_at_grid(self.col)),
        }
    }
}

/// First renderer: cached DirectWrite layouts + GPU-resident Direct2D images.
/// The engine/transport boundary allows an atlas renderer to replace this independently.
pub struct Renderer {
    pub live_cursor: Option<Rc<RefCell<CursorBlink>>>,
    pub cursor_guide: CursorGuide,
    pub read_only: bool,
    pub copy_cursor: Option<w11term::interaction::Point>,
    normal: TextFormat,
    bold: TextFormat,
    small: TextFormat,
    layouts: HashMap<(String, TextStyle), CachedLayout>,
    typography: Typography,
    ligatures: bool,
    images: HashMap<[u8; 32], CachedImage>,
    rejected_images: HashSet<[u8; 32]>,
    attempted_images: HashSet<[u8; 32]>,
    tile_bytes: usize,
    image_budget: usize,
    #[cfg(test)]
    image_attempts: usize,
    #[cfg(test)]
    image_uploads: usize,
    base_cell: (f32, f32),
    pub metrics: Metrics,
}

struct CachedImage {
    bitmap: GpuImage,
    width: u32,
    height: u32,
}
struct CachedLayout {
    layout: TextLayout,
    natural_width: f32,
}
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct TextStyle {
    bold: bool,
    italic: bool,
    rtl: bool,
}
struct TextRun<'a> {
    text: &'a str,
    style: TextStyle,
    cols: usize,
    shape: bool,
    explicit_width: bool,
    fraction: Option<FractionalTextScale>,
    text_scale: f32,
    scale: (f32, f32),
}

struct ShapedRun<'a> {
    layout: &'a TextLayout,
    origin: (f32, f32),
    scale: (f32, f32),
    ink: Rgba,
}

#[derive(Clone, Copy)]
struct GridRow {
    top: f32,
    cw: f32,
    ch: f32,
}

impl GridRow {
    fn rect(self, span: Range<usize>) -> Rect {
        Rect::from_xywh(
            PADDING + span.start as f32 * self.cw,
            self.top,
            span.len() as f32 * self.cw,
            self.ch,
        )
    }
}

impl ShapedRun<'_> {
    fn draw(
        &self,
        native: &NativeDrawing,
        row: GridRow,
        run: Range<usize>,
        selection: Option<(Rgba, &[Range<usize>])>,
    ) -> windows_canvas::Result<()> {
        let draw_original = || {
            native.text(
                self.layout,
                self.origin.0,
                self.origin.1,
                self.ink,
                self.scale,
            )
        };
        let Some((ink, spans)) = selection else {
            return draw_original();
        };
        // Clip the complete shaped run so a selection boundary never splits a
        // ligature or changes the shaping of neighboring glyphs.
        let mut next = run.start;
        for span in spans {
            let start = span.start.max(run.start);
            let end = span.end.min(run.end);
            if start >= end {
                continue;
            }
            if next < start {
                native.with_clip(row.rect(next..start), draw_original)?;
            }
            native.with_clip(row.rect(start..end), || {
                native.cursor_text(self.layout, self.origin, ink, self.scale)
            })?;
            next = end;
        }
        if next == run.start {
            draw_original()?;
        } else if next < run.end {
            native.with_clip(row.rect(next..run.end), draw_original)?;
        }
        Ok(())
    }
}

fn selection_span(
    selection: &Selection,
    frame: &Snapshot,
    row: usize,
    cols: usize,
) -> Option<Range<usize>> {
    let mut span = selection.span(frame.first_stable_row + row as isize, cols)?;
    for cell in frame.rows[row].visible_cells() {
        let start = cell.cell_index();
        let end = start + cell.width();
        if start < span.start && end > span.start {
            span.start = start;
        }
        if start < span.end && end > span.end {
            span.end = end;
        }
    }
    Some(span)
}

#[derive(Clone, Copy)]
struct VisibleCursor {
    row: usize,
    col: usize,
    shape: CursorShape,
}

#[derive(Clone, Copy, PartialEq)]
enum CursorDisplay {
    Live,
    Static,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum RunKind {
    Ascii,
    Script,
}

fn run_kind(cell: &wezterm_term::CellRef<'_>, ligatures: bool, rtl: bool) -> Option<RunKind> {
    if cell.attrs().explicit_width() || cell.attrs().fractional_text_scale().is_some() {
        return None;
    }
    let text = cell.str();
    if text.is_ascii() {
        return (ligatures && cell.width() == 1 && text.len() == 1).then_some(RunKind::Ascii);
    }
    // Frame and block glyphs need an exact cell advance. Connected scripts
    // need their neighbors even when programming ligatures are disabled.
    let first = text.chars().next()?;
    if cell.width() != 1
        && (!first.is_alphabetic()
            || wezterm_term::grapheme_column_width(&text[..first.len_utf8()], None) > 1)
    {
        // Keep intrinsically wide characters and emoji anchored individually.
        // Indic conjuncts can occupy two cells even though the first base is narrow.
        return None;
    }
    if matches!(first, '\u{2500}'..='\u{259f}' | '\u{10eeee}') {
        return None;
    }
    if !rtl
        && matches!(
            wezterm_bidi::bidi_class_for_char(first),
            wezterm_bidi::BidiClass::RightToLeft | wezterm_bidi::BidiClass::ArabicLetter
        )
    {
        // Explicit visual-order mode: preserve the application's cell positions.
        return None;
    }
    Some(RunKind::Script)
}

fn run_extent(
    cells: &[wezterm_term::CellRef<'_>],
    map: &w11term::text_layout::CellMap,
    ligatures: bool,
) -> (usize, Option<RunKind>) {
    let first = &cells[0];
    let rtl = map.rtl(first.cell_index());
    let Some(kind) = run_kind(first, ligatures, rtl) else {
        return (1, None);
    };
    let mut count = 1;
    for pair in cells.windows(2) {
        let (prev, next) = (&pair[0], &pair[1]);
        let col = next.cell_index();
        let prev_visual = map.visual(prev.cell_index());
        let next_visual = map.visual(col);
        let adjacent = if rtl {
            next_visual + next.width() == prev_visual
        } else {
            prev_visual + prev.width() == next_visual
        };
        if !adjacent
            || col != prev.cell_index() + prev.width()
            || next.attrs() != first.attrs()
            || map.rtl(col) != rtl
            || run_kind(next, ligatures, rtl) != Some(kind)
        {
            break;
        }
        count += 1;
    }
    (count, Some(kind))
}

/// Both halves use the same double-height baseline and are clipped to their own row.
fn glyph_row(line: &wezterm_term::Line, top: f32, cell_height: f32) -> (f32, f32) {
    if line.is_double_height_top() {
        (top, 2.0)
    } else if line.is_double_height_bottom() {
        (top - cell_height, 2.0)
    } else {
        (top, 1.0)
    }
}

fn text_glyph_geometry(
    attrs: &CellAttributes,
    mut top: f32,
    mut height: f32,
    ch: f32,
) -> (f32, f32) {
    if let Some(slice) = attrs.multicell_row() {
        top -= f32::from(slice.offset()) * ch;
        height = f32::from(slice.run().scale()) * ch;
    }
    attrs
        .fractional_text_scale()
        .map_or((top, height), |fraction| {
            let reduced_height = height * fraction.factor();
            (
                top + fraction.vertical_offset(height - reduced_height),
                reduced_height,
            )
        })
}

fn draw_ink_lines(
    ctx: &DrawingSession<'_>,
    brush: &Brush,
    cell: Rect,
    scale: f32,
    overline: bool,
    strike: bool,
) {
    let thickness = scale.round().max(1.0) / scale;
    let stripe = |top: f32| {
        let top = (top * scale).round() / scale;
        ctx.fill_rect(
            &Rect::from_xywh(cell.left, top, cell.right - cell.left, thickness),
            brush,
        );
    };
    if overline {
        stripe(cell.top);
    }
    if strike {
        stripe(cell.top + (cell.bottom - cell.top) * 0.5);
    }
}

fn draw_underline(
    ctx: &DrawingSession<'_>,
    brush: &Brush,
    style: Underline,
    cell: Rect,
    scale: f32,
) {
    let height = cell.bottom - cell.top;
    let stroke = (height / 19.0 * scale).round().max(1.0) / scale;
    let baseline = cell.bottom - 3.0 * stroke;
    let stripe =
        |left, top, width| ctx.fill_rect(&Rect::from_xywh(left, top, width, stroke), brush);
    match style {
        Underline::None => {}
        Underline::Single | Underline::Double => {
            stripe(cell.left, baseline, cell.right - cell.left);
            if style == Underline::Double {
                stripe(cell.left, baseline - 2.0 * stroke, cell.right - cell.left);
            }
        }
        Underline::Dotted | Underline::Dashed => {
            let ink = if style == Underline::Dotted {
                stroke
            } else {
                4.0 * stroke
            };
            let period = ink + 2.0 * stroke;
            // Phase is shared by the entire row, including fractional-DPI cells.
            let mut x = cell.left - (cell.left - PADDING).rem_euclid(period);
            while x < cell.right {
                let left = x.max(cell.left);
                let right = (x + ink).min(cell.right);
                if right > left {
                    stripe(left, baseline, right - left);
                }
                x += period;
            }
        }
        Underline::Curly => {
            let step = 2.0 * stroke;
            let wave = |x: f32| {
                let phase = ((x - PADDING) / step).rem_euclid(2.0);
                Vector2::new(x, baseline + (1.0 - (phase - 1.0).abs()) * 2.0 * stroke)
            };
            let mut x = cell.left;
            let mut boundary = x - (x - PADDING).rem_euclid(step) + step;
            while x < cell.right {
                let next = boundary.min(cell.right);
                ctx.draw_line(wave(x), wave(next), brush, stroke);
                x = next;
                boundary += step;
            }
        }
    }
}

impl Renderer {
    #[cfg(test)]
    pub fn new() -> windows_canvas::Result<Self> {
        Self::with_font("Cascadia Mono", true)
    }
    #[cfg(test)]
    pub fn with_font(family: &str, ligatures: bool) -> windows_canvas::Result<Self> {
        Self::with_size(family, ligatures, FONT_SIZE)
    }
    pub fn with_size(
        family: &str,
        ligatures: bool,
        font_size: f32,
    ) -> windows_canvas::Result<Self> {
        let normal = TextFormat::new(family, font_size)?.with_word_wrapping(WordWrapping::NoWrap);
        let bold =
            TextFormat::new_bold(family, font_size)?.with_word_wrapping(WordWrapping::NoWrap);
        let probe = TextLayout::new("M", &normal, 100.0, 100.0)?.metrics();
        let metrics = Metrics {
            cell_width: probe.width_including_trailing_whitespace,
            cell_height: probe.height.ceil(),
            ..Metrics::default()
        };
        Ok(Self {
            live_cursor: None,
            copy_cursor: None,
            cursor_guide: CursorGuide::Off,
            read_only: false,
            normal,
            bold,
            small: TextFormat::new("Segoe UI", 11.0)?.with_word_wrapping(WordWrapping::NoWrap),
            layouts: HashMap::new(),
            typography: Typography::new(family, ligatures)?,
            ligatures,
            images: HashMap::new(),
            rejected_images: HashSet::new(),
            attempted_images: HashSet::new(),
            tile_bytes: 0,
            image_budget: 32 * 1024 * 1024,
            #[cfg(test)]
            image_attempts: 0,
            #[cfg(test)]
            image_uploads: 0,
            metrics,
            base_cell: (probe.width_including_trailing_whitespace, probe.height),
        })
    }

    fn shape_text(
        &mut self,
        run: TextRun<'_>,
    ) -> windows_canvas::Result<Option<(&CachedLayout, (f32, f32))>> {
        let TextRun {
            text,
            style,
            cols,
            shape,
            explicit_width,
            fraction,
            text_scale,
            mut scale,
        } = run;
        if text.trim_end_matches(' ').is_empty() {
            return Ok(None);
        }
        let key = (text.to_owned(), style);
        if !self.layouts.contains_key(&key) {
            if self.layouts.len() >= 2048 {
                self.layouts.clear();
            }
            let layout = TextLayout::new(
                text,
                if style.bold { &self.bold } else { &self.normal },
                self.metrics.cell_width * cols as f32 + 4.0,
                self.metrics.cell_height,
            )?;
            self.typography.apply(&layout, style.italic, style.rtl)?;
            let natural_width = layout.metrics().width_including_trailing_whitespace;
            self.layouts.insert(
                key.clone(),
                CachedLayout {
                    layout,
                    natural_width,
                },
            );
        }
        let cached = &self.layouts[&key];
        // Anchor each shaped run to its occupied grid columns even when DirectWrite
        // chose a proportional fallback font or the grid was snapped to physical pixels.
        let factor = text_scale * fraction.map_or(1.0, FractionalTextScale::factor);
        if (shape || explicit_width || fraction.is_some()) && cached.natural_width > 0.0 {
            let fit = cols as f32 * self.metrics.cell_width / cached.natural_width;
            scale.0 *= if explicit_width || fraction.is_some() {
                fit.min(factor)
            } else {
                fit
            };
        } else {
            scale.0 *= factor;
        }
        scale.1 *= factor;
        Ok(Some((cached, scale)))
    }

    pub fn draw(
        &mut self,
        ctx: &DrawContext<'_>,
        frame: &Snapshot,
        selection: Option<&Selection>,
    ) -> windows_canvas::Result<Metrics> {
        self.paint(
            ctx,
            (ctx.width, ctx.height),
            ctx.device_changed(),
            frame,
            CursorDisplay::Live,
            selection,
        )
    }

    /// Draw the committed grid at its original geometry, then fit it into the thumbnail.
    /// Preview dimensions must never become a resize request for the PTY.
    pub fn draw_preview(
        &mut self,
        ctx: &DrawingSession<'_>,
        width: f32,
        height: f32,
        device_changed: bool,
        frame: &Snapshot,
    ) -> windows_canvas::Result<()> {
        if width <= 0.0 || height <= 0.0 {
            return Ok(());
        }
        let dpi = (NativeDrawing::new(ctx)?.dpi() / 96.0).max(0.5);
        let grid = self.measure(width, height, dpi);
        let source_w = PADDING * 2.0 + frame.cols as f32 * grid.cell_width;
        let source_h = PADDING * 2.0 + STATUS_HEIGHT + frame.rows.len() as f32 * grid.cell_height;
        let fit = (width / source_w).min(height / source_h).min(1.0);
        let previous = ctx.transform();
        let transform = windows_canvas::Matrix3x2 {
            m11: fit,
            m22: fit,
            m31: (width - source_w * fit) / 2.0,
            m32: (height - source_h * fit) / 2.0,
            ..Default::default()
        };
        ctx.set_transform(&(transform * previous));
        let result = self.draw_surface(ctx, source_w, source_h, device_changed, frame);
        ctx.set_transform(&previous);
        result.map(|_| ())
    }

    pub fn draw_hint_labels<'a>(
        &self,
        ctx: &DrawContext<'_>,
        frame: &Snapshot,
        hints: impl Iterator<Item = &'a w11term::hints::Hint>,
    ) -> windows_canvas::Result<()> {
        let brush = ctx.create_solid_brush(ColorF::WHITE)?;
        let Metrics {
            cell_width,
            cell_height,
            cols,
            rows,
            ..
        } = self.metrics;
        for hint in hints.filter(|h| h.row < rows) {
            let col = frame
                .cell_map(hint.row)
                .map_or(hint.col, |map| map.visual_grid(hint.col));
            if col >= cols {
                continue;
            }
            let width = cell_width * hint.label.len() as f32 + 4.0;
            let x = (PADDING + col as f32 * cell_width).min((ctx.width - width).max(0.0));
            let y = PADDING + hint.row as f32 * cell_height;
            let rect = Rect::from_xywh(x, y, width, cell_height);
            brush.set_color(ColorF::new(1.0, 0.84, 0.4, 1.0));
            ctx.fill_rect(&rect, &brush);
            brush.set_color(ColorF::new(0.06, 0.08, 0.11, 1.0));
            ctx.draw_text(&hint.label, &self.bold, &rect, &brush);
        }
        Ok(())
    }

    pub fn draw_preedit(
        &self,
        ctx: &DrawContext<'_>,
        frame: &Snapshot,
        text: &str,
        cursor: u32,
    ) -> windows_canvas::Result<f32> {
        let cw = self.metrics.cell_width;
        let ch = self.metrics.cell_height;
        if text.is_empty() || frame.cursor.y < 0 || frame.cursor.y as usize >= self.metrics.rows {
            return Ok(0.0);
        }
        let x = PADDING
            + frame
                .visual_cursor_column()
                .min(self.metrics.cols.saturating_sub(1)) as f32
                * cw;
        let y = PADDING + frame.cursor.y as f32 * ch;
        let layout = TextLayout::new(text, &self.normal, 1000000.0, ch)?;
        let width = layout
            .metrics()
            .width_including_trailing_whitespace
            .max(1.0);
        let cursor = layout.caret_bounds(cursor, false).left;
        let origin = x.min((ctx.width - PADDING - width).max(PADDING));
        // Scroll a long composition horizontally to keep its insertion point visible.
        let origin = origin.min(ctx.width - PADDING - 2.0 - cursor);
        let brush = ctx.create_solid_brush(ColorF::new(0.13, 0.16, 0.20, 1.0))?;
        ctx.fill_rect(&Rect::from_xywh(origin, y, width + 2.0, ch), &brush);
        brush.set_color(ColorF::new(0.97, 0.98, 1.0, 1.0));
        ctx.draw_text_layout(windows_canvas::Vector2 { x: origin, y }, &layout, &brush);
        brush.set_color(ColorF::new(0.44, 0.76, 1.0, 1.0));
        ctx.fill_rect(&Rect::from_xywh(origin, y + ch - 1.5, width, 1.5), &brush);
        ctx.fill_rect(&Rect::from_xywh(origin + cursor, y, 1.2, ch), &brush);
        Ok(origin + cursor - x)
    }

    pub fn draw_interaction(
        &self,
        ctx: &DrawContext<'_>,
        frame: &Snapshot,
        selection: Option<&Selection>,
        link: Option<&w11term::interaction::Link>,
    ) -> windows_canvas::Result<()> {
        let cw = self.metrics.cell_width;
        let ch = self.metrics.cell_height;
        let brush = ctx.create_solid_brush(color(SELECTION_BG))?;
        let highlight = |row: usize, span, top: f32, height: f32| {
            if let Some(map) = frame.cell_map(row) {
                for visual in map.visual_ranges(span) {
                    let end = visual.end.min(self.metrics.cols);
                    if visual.start < end {
                        ctx.fill_rect(
                            &Rect::from_xywh(
                                PADDING + visual.start as f32 * cw,
                                PADDING + row as f32 * ch + top,
                                (end - visual.start) as f32 * cw,
                                height,
                            ),
                            &brush,
                        );
                    }
                }
            }
        };
        if let Some(search) = &frame.search {
            for (selected, matched) in &search.visible {
                brush.set_color(if *selected {
                    ColorF::new(1.0, 0.48, 0.10, 0.65)
                } else {
                    ColorF::new(1.0, 0.85, 0.15, 0.3)
                });
                for (point, end) in &matched.spans {
                    let y = point.row - frame.first_stable_row;
                    if y >= 0 && y < self.metrics.rows as isize {
                        highlight(y as usize, point.col..*end, 0.0, ch);
                    }
                }
            }
        }
        brush.set_color(color(SELECTION_BG));
        if let Some(selection) = selection.filter(|s| {
            frame.selection_colors == Default::default()
                && s.alternate == frame.alternate
                && s.cols == frame.cols
        }) {
            for y in 0..frame.rows.len().min(self.metrics.rows) {
                if let Some(span) = selection_span(selection, frame, y, self.metrics.cols) {
                    highlight(y, span, 0.0, ch);
                }
            }
        }
        brush.set_color(ColorF::new(0.48, 0.79, 0.98, 1.0));
        if let Some(link) = link {
            for (row, span) in &link.spans {
                let y = *row - frame.first_stable_row;
                if y >= 0 && y < self.metrics.rows as isize {
                    highlight(y as usize, span.clone(), ch - 2.0, 1.0);
                }
            }
        }
        Ok(())
    }

    pub fn measure(&mut self, width: f32, height: f32, scale: f32) -> Metrics {
        self.metrics.scale = scale;
        self.metrics.cell_width = (self.base_cell.0 * scale).ceil() / scale;
        self.metrics.cell_height = (self.base_cell.1 * scale).ceil() / scale;
        self.metrics.cols = ((width - PADDING * 2.0) / self.metrics.cell_width + 0.001)
            .floor()
            .max(1.0) as usize;
        self.metrics.rows = ((height - PADDING * 2.0 - STATUS_HEIGHT) / self.metrics.cell_height
            + 0.001)
            .floor()
            .max(1.0) as usize;
        self.metrics
    }

    fn visible_cursor(&self, frame: &Snapshot, display: CursorDisplay) -> Option<VisibleCursor> {
        let (row, col, visible, shape) = self.copy_cursor.map_or_else(
            || {
                (
                    frame.cursor.y as isize,
                    frame.visual_cursor_column(),
                    frame.cursor_visible,
                    frame.cursor.shape,
                )
            },
            |point| {
                let row = point.row - frame.first_stable_row;
                let col = usize::try_from(row)
                    .ok()
                    .and_then(|r| frame.cell_map(r))
                    .map_or(point.col, |map| {
                        map.visual_grid(point.col.min(map.logical_cols().saturating_sub(1)))
                    });
                (row, col, true, CursorShape::SteadyBlock)
            },
        );
        let row = usize::try_from(row)
            .ok()
            .filter(|&r| visible && r < self.metrics.rows && col < self.metrics.cols);
        if display == CursorDisplay::Live
            && let Some(blink) = &self.live_cursor
        {
            let cell = row.filter(|_| shape.is_blinking()).map(|row| CursorCell {
                col,
                row,
                shape,
                stable_row: frame.first_stable_row.saturating_add(row as isize),
                alternate: frame.alternate,
                connection: frame.connection_generation,
            });
            if !blink.borrow_mut().visible(cell, Instant::now()) {
                return None;
            }
        }
        Some(VisibleCursor {
            row: row?,
            col,
            shape,
        })
    }

    fn draw_surface(
        &mut self,
        ctx: &DrawingSession<'_>,
        width: f32,
        height: f32,
        device_changed: bool,
        frame: &Snapshot,
    ) -> windows_canvas::Result<Metrics> {
        self.paint(
            ctx,
            (width, height),
            device_changed,
            frame,
            CursorDisplay::Static,
            None,
        )
    }

    fn paint(
        &mut self,
        ctx: &DrawingSession<'_>,
        (width, height): (f32, f32),
        device_changed: bool,
        frame: &Snapshot,
        display: CursorDisplay,
        selection: Option<&Selection>,
    ) -> windows_canvas::Result<Metrics> {
        let native = NativeDrawing::new(ctx)?;
        if device_changed {
            self.layouts.clear();
            self.images.clear();
            self.rejected_images.clear();
            self.tile_bytes = 0;
        }
        self.attempted_images.clear();
        let scale = (native.dpi() / 96.0).max(0.5);
        self.measure(width, height, scale);
        // Keep GPU ownership tied to the current visible model. Replacing an id,
        // deleting a placement, changing tabs, or scrolling releases obsolete textures.
        let mut live = HashSet::new();
        let mut tiles = Vec::new();
        for (row, line) in frame.rows.iter().take(self.metrics.rows).enumerate() {
            let map = frame.cell_map(row).expect("row belongs to the snapshot");
            let cols = map
                .logical_cols()
                .min((self.metrics.cols / map.width_scale()).max(1));
            for cell in line.visible_cells().take_while(|c| c.cell_index() < cols) {
                if let Some(images) = cell.attrs().images() {
                    for tile in images {
                        let data = tile.image_data();
                        let guard = frame.image_data(data);
                        live.insert(match guard {
                            ImageDataType::Rgba8 { hash, .. } => *hash,
                            ImageDataType::AnimRgba8 { hashes, .. } if !hashes.is_empty() => {
                                hashes[0]
                            }
                            _ => data.hash(),
                        });
                        tiles.push((row, cell.cell_index(), tile));
                    }
                }
            }
        }
        self.images.retain(|key, _| live.contains(key));
        self.rejected_images.retain(|key| live.contains(key));
        self.tile_bytes = self
            .images
            .values()
            .map(|i| i.width as usize * i.height as usize * 4)
            .sum();
        let cw = self.metrics.cell_width;
        let ch = self.metrics.cell_height;
        let background = rgba(frame.palette.background);
        ctx.clear(color(background));
        let brush = ctx.create_solid_brush(ColorF::WHITE)?;

        // Kitty places extremely negative images below explicit cell backgrounds.
        // Higher image IDs win an equal-z overlap, regardless of arrival order.
        tiles.sort_by_key(|(_, _, tile)| (tile.z_index(), tile.image_id()));
        let under_background = tiles.partition_point(|(_, _, tile)| tile.z_index() < i32::MIN / 2);
        let under_text = tiles.partition_point(|(_, _, tile)| tile.z_index() < 0);
        self.draw_images(&native, frame, &tiles[..under_background])?;
        for (y, line) in frame.rows.iter().take(self.metrics.rows).enumerate() {
            let map = frame.cell_map(y).expect("row belongs to the snapshot");
            for cell in line
                .visible_cells()
                .take_while(|cell| cell.cell_index() < map.logical_cols())
                .filter(|cell| map.visual_grid(cell.cell_index()) < self.metrics.cols)
            {
                let (_, bg) = colors(cell.attrs(), &frame.palette);
                if bg != background
                    || (under_background != 0
                        && (cell.attrs().background() != Default::default()
                            || cell.attrs().reverse()))
                {
                    brush.set_color(color(bg));
                    let col = map.visual_grid(cell.cell_index());
                    let cols = (cell.width() * map.width_scale()).min(self.metrics.cols - col);
                    ctx.fill_rect(
                        &Rect::from_xywh(
                            PADDING + col as f32 * cw,
                            PADDING + y as f32 * ch,
                            cw * cols as f32,
                            ch,
                        ),
                        &brush,
                    );
                }
            }
        }
        self.draw_images(&native, frame, &tiles[under_background..under_text])?;
        let focused = display == CursorDisplay::Live
            && self
                .live_cursor
                .as_ref()
                .is_some_and(|blink| blink.borrow().is_focused());
        if focused
            && self.copy_cursor.is_none()
            && frame.cursor_visible
            && self.cursor_guide.shown(frame.application_cursor_guide)
            && let Ok(row) = usize::try_from(frame.cursor.y)
            && row < self.metrics.rows.min(frame.rows.len())
        {
            let mut guide = rgba(frame.palette.foreground);
            guide[3] = 0.35;
            brush.set_color(color(guide));
            let bottom = ((PADDING + (row + 1) as f32 * ch) * scale).round();
            ctx.fill_rect(
                &Rect::from_xywh(
                    PADDING,
                    (bottom - 1.0) / scale,
                    frame.cols.min(self.metrics.cols) as f32 * cw,
                    1.0 / scale,
                ),
                &brush,
            );
        }
        let selection = selection.filter(|s| {
            frame.selection_colors != Default::default()
                && s.alternate == frame.alternate
                && s.cols == frame.cols
        });
        let selection_bg = frame.selection_colors.background.map_or(SELECTION_BG, rgba);
        let selection_fg = frame.selection_colors.foreground.map(rgba);
        let cursor = self.visible_cursor(frame, display);
        let cursor_slice = cursor.and_then(|cursor| {
            let map = frame.cell_map(cursor.row)?;
            let col = map.logical_at_grid(cursor.col);
            let line = frame.rows.get(cursor.row)?;
            if !line.has_multicell_text() {
                return None;
            }
            let cell = line
                .visible_cells()
                .take_while(|cell| cell.cell_index() <= col)
                .find(|cell| col < cell.cell_index() + cell.width())?;
            let slice = cell.attrs().multicell_row()?;
            Some((
                map.visual_grid(cell.cell_index()),
                cell.width(),
                cursor.row as isize - isize::from(slice.offset()),
                usize::from(slice.run().scale()),
            ))
        });
        let block = cursor.filter(|cursor| {
            self.copy_cursor.is_none()
                && focused
                && matches!(
                    cursor.shape,
                    CursorShape::Default | CursorShape::SteadyBlock | CursorShape::BlinkingBlock
                )
        });
        let mut block_extent = None;
        let mut block_decorations = (Underline::None, false, false, 0.0, 0.0);
        let mut cursor_ink = None;
        for (y, line) in frame.rows.iter().take(self.metrics.rows).enumerate() {
            let map = frame.cell_map(y).expect("row belongs to the snapshot");
            let block_col = block
                .filter(|cursor| cursor.row == y)
                .map(|cursor| cursor.col);
            let width_scale = map.width_scale();
            let row_cw = cw * width_scale as f32;
            let row_top = PADDING + y as f32 * ch;
            let row = GridRow {
                top: row_top,
                cw,
                ch,
            };
            let selected = selection.and_then(|s| selection_span(s, frame, y, self.metrics.cols));
            let visual_selection = selected
                .as_ref()
                .map_or_else(Vec::new, |span| map.visual_ranges(span.clone()));
            if !visual_selection.is_empty() {
                brush.set_color(color(selection_bg));
                for span in &visual_selection {
                    let end = span.end.min(self.metrics.cols);
                    if span.start < end {
                        ctx.fill_rect(&row.rect(span.start..end), &brush);
                    }
                }
            }
            let selected_ink = selection_fg
                .filter(|_| !visual_selection.is_empty())
                .map(|ink| (ink, visual_selection.as_slice()));
            let (glyph_top, height_scale) = glyph_row(line, row_top, ch);
            let glyph_height = ch * height_scale;
            let cells: Vec<_> = line
                .visible_cells()
                .take_while(|c| c.cell_index() < map.logical_cols())
                .filter(|c| map.visual_grid(c.cell_index()) < self.metrics.cols)
                .collect();
            if let Some(col) = block_col {
                // visible_cells omits the continuation of a wide grapheme.
                block_extent = Some((col, width_scale));
                let logical = map.logical_at_grid(col);
                let end = cells.partition_point(|cell| cell.cell_index() <= logical);
                if let Some(cell) = cells[..end].last()
                    && logical < cell.cell_index() + cell.width()
                {
                    block_extent = Some((
                        map.visual_grid(cell.cell_index()),
                        cell.width() * width_scale,
                    ));
                    if !cell.attrs().invisible() {
                        let attrs = cell.attrs();
                        let (top, height) = text_glyph_geometry(attrs, glyph_top, glyph_height, ch);
                        block_decorations = (
                            attrs.underline(),
                            attrs.overline(),
                            attrs.strikethrough(),
                            top,
                            height,
                        );
                    }
                }
            }
            let row_width = (map.logical_cols() * width_scale).min(self.metrics.cols);
            let mut draw_row = || -> windows_canvas::Result<()> {
                let mut next_run = 0;
                for (index, cell) in cells.iter().enumerate() {
                    let col = cell.cell_index();
                    let attrs = cell.attrs();
                    if attrs.invisible() {
                        continue;
                    }
                    let fraction = attrs.fractional_text_scale();
                    let slice = attrs.multicell_row();
                    let text_scale = slice.map_or(1.0, |slice| f32::from(slice.run().scale()));
                    let (glyph_top, glyph_height) =
                        text_glyph_geometry(attrs, glyph_top, glyph_height, ch);
                    let visual = map.visual(col);
                    let x = PADDING + visual as f32 * row_cw;
                    let (fg, _) = colors(attrs, &frame.palette);
                    let decoration_ink = selection_fg
                        .filter(|_| selected.as_ref().is_some_and(|span| span.contains(&col)));
                    // Kitty marks the image's placement in the text grid with U+10EEEE.
                    if index >= next_run && !cell.str().starts_with('\u{10eeee}') {
                        let rtl = map.rtl(col);
                        let (count, kind) = run_extent(&cells[index..], map, self.ligatures);
                        next_run = index + count;
                        let run_cells = &cells[index..next_run];
                        let mut joined = String::new();
                        let text = if run_cells.len() == 1 {
                            slice.map_or(cell.str(), |slice| slice.run().text())
                        } else {
                            for member in run_cells {
                                joined.push_str(member.str());
                            }
                            &joined
                        };
                        let last = run_cells.last().unwrap();
                        let mut cols = last.cell_index() + last.width() - col;
                        let left = visual.min(map.visual(last.cell_index()));
                        let text = if kind == Some(RunKind::Ascii) && !rtl {
                            let trimmed = text.trim_end_matches(' ');
                            cols -= text.len() - trimmed.len();
                            trimmed
                        } else {
                            text
                        };
                        let layout = self.shape_text(TextRun {
                            text,
                            style: TextStyle {
                                bold: attrs.intensity() == Intensity::Bold,
                                italic: attrs.italic(),
                                rtl,
                            },
                            cols,
                            shape: kind.is_some(),
                            explicit_width: attrs.explicit_width(),
                            fraction,
                            text_scale,
                            scale: (width_scale as f32, height_scale),
                        })?;
                        if let Some((cached, scale)) = layout {
                            let inset = fraction.map_or(0.0, |fraction| {
                                let extra = cols as f32 * row_cw - cached.natural_width * scale.0;
                                fraction.horizontal_offset(extra.max(0.0))
                            });
                            let origin = (PADDING + left as f32 * row_cw + inset, glyph_top);
                            let layout = &cached.layout;
                            let run = ShapedRun {
                                layout,
                                origin,
                                scale,
                                ink: fg,
                            };
                            let columns = left * width_scale..(left + cols) * width_scale;
                            if attrs.explicit_width() || fraction.is_some() {
                                native.with_clip(row.rect(columns.clone()), || {
                                    run.draw(&native, row, columns, selected_ink)
                                })?;
                            } else {
                                run.draw(&native, row, columns, selected_ink)?;
                            }
                            if block_col.is_some_and(|col| {
                                (left..left + cols).contains(&(col / width_scale))
                            }) {
                                cursor_ink = Some((layout.clone(), origin, scale));
                            }
                        }
                    }
                    if attrs.underline() != Underline::None
                        || attrs.overline()
                        || attrs.strikethrough()
                    {
                        let glyph_rect = Rect::from_xywh(
                            x,
                            glyph_top,
                            cell.width() as f32 * row_cw,
                            glyph_height,
                        );
                        let draw = || {
                            if attrs.underline() != Underline::None {
                                let underline = if attrs.underline_color()
                                    == wezterm_term::color::ColorAttribute::Default
                                {
                                    fg
                                } else {
                                    rgba(frame.palette.resolve_fg(attrs.underline_color()))
                                };
                                brush.set_color(color(decoration_ink.unwrap_or(underline)));
                                draw_underline(
                                    ctx,
                                    &brush,
                                    attrs.underline(),
                                    glyph_rect,
                                    self.metrics.scale,
                                );
                            }
                            if attrs.overline() || attrs.strikethrough() {
                                brush.set_color(color(decoration_ink.unwrap_or(fg)));
                                draw_ink_lines(
                                    ctx,
                                    &brush,
                                    glyph_rect,
                                    self.metrics.scale,
                                    attrs.overline(),
                                    attrs.strikethrough(),
                                );
                            }
                        };
                        if slice.is_some() {
                            let left = map.visual_grid(col);
                            native
                                .with_clip(row.rect(left..left + cell.width() * width_scale), draw);
                        } else {
                            draw();
                        }
                    }
                }
                Ok(())
            };
            if width_scale > 1 {
                let bounds = Rect::from_xywh(PADDING, row_top, row_width as f32 * cw, ch);
                native.with_clip(bounds, draw_row)?;
            } else {
                draw_row()?;
            }
        }
        self.draw_images(&native, frame, &tiles[under_text..])?;
        if let Some(cursor) = cursor {
            let width_scale = frame
                .cell_map(cursor.row)
                .map_or(1, |map| map.width_scale());
            let (left, cols, top, rows) = cursor_slice.unwrap_or_else(|| {
                let (left, cols) = block_extent.unwrap_or((cursor.col, width_scale));
                (left, cols, cursor.row as isize, 1)
            });
            let cols = cols.min(self.metrics.cols - left);
            let x = PADDING + left as f32 * cw;
            let y = PADDING + top as f32 * ch;
            let w = cols as f32 * cw;
            let h = rows as f32 * ch;
            let rect = Rect::from_xywh(x, y, w, h);
            let viewport = Rect::from_xywh(
                PADDING,
                PADDING,
                self.metrics.cols as f32 * cw,
                self.metrics.rows as f32 * ch,
            );
            brush.set_color(color(rgba(frame.palette.cursor_bg)));
            native.with_clip(viewport, || -> windows_canvas::Result<()> {
                match cursor.shape {
                    CursorShape::SteadyBar | CursorShape::BlinkingBar => {
                        ctx.fill_rect(&Rect::from_xywh(x, y, 2.0, h), &brush);
                        Ok(())
                    }
                    CursorShape::SteadyUnderline | CursorShape::BlinkingUnderline => {
                        ctx.fill_rect(&Rect::from_xywh(x, y + h - 2.0, w, 2.0), &brush);
                        Ok(())
                    }
                    _ if block_extent.is_some() => {
                        ctx.fill_rect(&rect, &brush);
                        let ink = block_ink(frame);
                        native.with_clip(rect, || -> windows_canvas::Result<()> {
                            if let Some((layout, origin, scale)) = cursor_ink {
                                native.cursor_text(&layout, origin, ink, scale)?;
                            }
                            let (underline, overline, strike, glyph_top, glyph_height) =
                                block_decorations;
                            if underline != Underline::None || overline || strike {
                                brush.set_color(color(ink));
                                let glyph_rect = Rect::from_xywh(x, glyph_top, w, glyph_height);
                                draw_underline(
                                    ctx,
                                    &brush,
                                    underline,
                                    glyph_rect,
                                    self.metrics.scale,
                                );
                                draw_ink_lines(
                                    ctx,
                                    &brush,
                                    glyph_rect,
                                    self.metrics.scale,
                                    overline,
                                    strike,
                                );
                            }
                            Ok(())
                        })
                    }
                    _ => {
                        ctx.draw_rect(&rect, &brush, 1.0);
                        Ok(())
                    }
                }
            })?;
        }
        brush.set_color(ColorF::new(0.09, 0.10, 0.12, 1.0));
        ctx.fill_rect(
            &Rect::from_xywh(0.0, height - STATUS_HEIGHT, width, STATUS_HEIGHT),
            &brush,
        );
        brush.set_color(ColorF::new(0.59, 0.64, 0.71, 1.0));
        let mut label = format!(
            "{}{}   ·   {} × {}   ·   {} KiB{}",
            if self.read_only {
                "Read-only   ·   "
            } else {
                ""
            },
            frame.status,
            self.metrics.cols,
            self.metrics.rows,
            frame.bytes_received / 1024,
            if frame.scroll_offset > 0 {
                "   ·   scrollback"
            } else {
                ""
            }
        );
        if let Some(code) = frame.command_history.back().and_then(|c| c.exit_code) {
            use std::fmt::Write as _;
            let _ = write!(label, "   ·   exit {code}");
        }
        ctx.draw_text(
            &label,
            &self.small,
            &Rect::from_xywh(
                PADDING,
                height - STATUS_HEIGHT + 5.0,
                (width - 2.0 * PADDING).max(1.0),
                STATUS_HEIGHT,
            ),
            &brush,
        );
        Ok(self.metrics)
    }

    fn draw_images(
        &mut self,
        native: &NativeDrawing,
        frame: &Snapshot,
        tiles: &[(usize, usize, ImageCell)],
    ) -> windows_canvas::Result<()> {
        for &(row, column, ref tile) in tiles {
            let data = tile.image_data();
            let guard = frame.image_data(data);
            let key = match guard {
                ImageDataType::Rgba8 { hash, .. } => *hash,
                ImageDataType::AnimRgba8 { hashes, .. } if !hashes.is_empty() => hashes[0],
                _ => data.hash(),
            };
            if !self.images.contains_key(&key) {
                if self.rejected_images.contains(&key)
                    || self.attempted_images.contains(&key)
                    || self.images.len() + self.rejected_images.len() >= 512
                    || self.attempted_images.len() >= 512
                {
                    continue;
                }
                self.attempted_images.insert(key);
                #[cfg(test)]
                {
                    self.image_attempts += 1;
                }
                let Some(cached) = self.admit_image(native, guard, key)? else {
                    continue;
                };
                self.tile_bytes += cached.width as usize * cached.height as usize * 4;
                self.images.insert(key, cached);
            }
            let cached = &self.images[&key];
            let width = cached.width;
            let height = cached.height;
            let tl = tile.top_left();
            let br = tile.bottom_right();
            let left = (tl.x.into_inner() * width as f32).clamp(0.0, width as f32);
            let top = (tl.y.into_inner() * height as f32).clamp(0.0, height as f32);
            let right = (br.x.into_inner() * width as f32).clamp(left, width as f32);
            let bottom = (br.y.into_inner() * height as f32).clamp(top, height as f32);
            if right == left || bottom == top {
                continue;
            }
            let (pl, pt, pr, pb) = tile.padding();
            let width_scale = frame.cell_map(row).map_or(1, |map| map.width_scale());
            let grid_col = column * width_scale;
            let cw = self.metrics.cell_width;
            let ch = self.metrics.cell_height;
            let tile_width = cw * width_scale.min(self.metrics.cols - grid_col) as f32;
            let dest = Rect::from_xywh(
                PADDING + grid_col as f32 * cw + pl as f32 / self.metrics.scale,
                PADDING + row as f32 * ch + pt as f32 / self.metrics.scale,
                (tile_width - (pl as f32 + pr as f32) / self.metrics.scale).max(0.0),
                (ch - (pt as f32 + pb as f32) / self.metrics.scale).max(0.0),
            );
            native.draw_image(&cached.bitmap, Rect::new(left, top, right, bottom), dest);
        }
        Ok(())
    }

    fn admit_image(
        &mut self,
        native: &NativeDrawing,
        data: &ImageDataType,
        key: [u8; 32],
    ) -> windows_canvas::Result<Option<CachedImage>> {
        // Only device loss escapes to Canvas. Malformed or unsupported content is
        // isolated to this image, including failures from the native upload API.
        let mut deferred = false;
        let result = (|| -> windows_canvas::Result<Option<CachedImage>> {
            let decoded;
            let (pixels, width, height) = match data {
                ImageDataType::Rgba8 {
                    data,
                    width,
                    height,
                    ..
                } => (data.as_slice(), *width, *height),
                ImageDataType::AnimRgba8 {
                    frames,
                    width,
                    height,
                    ..
                } if !frames.is_empty() => (frames[0].as_slice(), *width, *height),
                ImageDataType::EncodedFile(bytes) => {
                    let Ok(mut reader) =
                        image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()
                    else {
                        return Ok(None);
                    };
                    let mut limits = image::Limits::default();
                    limits.max_image_width = Some(4096);
                    limits.max_image_height = Some(4096);
                    limits.max_alloc = Some(32 * 1024 * 1024);
                    reader.limits(limits);
                    let Ok(image) = reader.decode() else {
                        return Ok(None);
                    };
                    decoded = image.to_rgba8();
                    (
                        decoded.as_raw().as_slice(),
                        decoded.width(),
                        decoded.height(),
                    )
                }
                _ => return Ok(None),
            };
            let Some(bytes) = (width as usize)
                .checked_mul(height as usize)
                .and_then(|n| n.checked_mul(4))
            else {
                return Ok(None);
            };
            // Bound this renderer's GPU cache. The engine owns its own image storage.
            if width == 0
                || height == 0
                || width > native.maximum_bitmap_size()
                || height > native.maximum_bitmap_size()
                || bytes != pixels.len()
                || bytes > self.image_budget
            {
                return Ok(None);
            }
            if self.tile_bytes + bytes > self.image_budget {
                // Retry admission next frame if other images leave the view.
                deferred = true;
                return Ok(None);
            }
            #[cfg(test)]
            {
                self.image_uploads += 1;
            }
            Ok(Some(CachedImage {
                bitmap: native.image(pixels, width, height)?,
                width,
                height,
            }))
        })();
        match result {
            Ok(Some(image)) => Ok(Some(image)),
            Err(error) if windows_canvas::is_device_lost(error.code()) => Err(error),
            other => {
                // Budget deferrals are per-frame, all other rejections persist
                // only while this content is visible on the same device.
                if !deferred || other.is_err() {
                    self.rejected_images.insert(key);
                }
                Ok(None)
            }
        }
    }
}

fn block_ink(frame: &Snapshot) -> [f32; 4] {
    let ink = rgba(frame.palette.cursor_fg);
    let pixel = |rgba: [f32; 4]| rgba.map(|v| (v * 255.0).round() as u8);
    if pixel(ink) != pixel(rgba(frame.palette.cursor_bg)) {
        return ink;
    }
    // OSC 12 can choose the same displayed color as the ink. Keep its fill and
    // use whichever of black or white has the higher contrast ratio.
    let light = if frame.palette.cursor_bg.to_linear().relative_luminance() < 0.179 {
        1.0
    } else {
        0.0
    };
    [light, light, light, 1.0]
}

fn color(c: [f32; 4]) -> ColorF {
    ColorF::new(c[0], c[1], c[2], c[3])
}

/// Uses the same drawing code as the visible canvas, on an independent offscreen device.
/// No HWND, swapchain capture, selection change, or window activation is involved.
#[cfg(test)]
pub fn screenshot(
    frame: &Snapshot,
    size: wezterm_term::TerminalSize,
    path: &std::path::Path,
) -> anyhow::Result<(u32, u32)> {
    screenshot_with_font(
        frame,
        size,
        path,
        &w11term::appearance::Appearance::default(),
    )
}

pub fn screenshot_with_font(
    frame: &Snapshot,
    size: wezterm_term::TerminalSize,
    path: &std::path::Path,
    appearance: &w11term::appearance::Appearance,
) -> anyhow::Result<(u32, u32)> {
    let grid = render_grid(frame, size, appearance)?;
    let dimensions = grid.dimensions();
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("screenshot path has no parent"))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    write_grid_png(file.as_file_mut(), grid)?;
    file.persist(path)?;
    Ok(dimensions)
}

pub fn write_grid_png(file: &mut std::fs::File, grid: image::RgbaImage) -> std::io::Result<()> {
    image::DynamicImage::ImageRgba8(grid)
        .write_to(file, image::ImageFormat::Png)
        .map_err(std::io::Error::other)
}

pub fn render_grid(
    frame: &Snapshot,
    size: wezterm_term::TerminalSize,
    appearance: &w11term::appearance::Appearance,
) -> anyhow::Result<image::RgbaImage> {
    use anyhow::ensure;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
    }
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe {
                CoUninitialize();
            }
        }
    }
    let _apartment = Apartment;
    let mut renderer = Renderer::with_size(
        &appearance.font_family,
        appearance.ligatures,
        appearance.terminal_font_size(),
    )?;
    let scale = (size.dpi as f32 / 96.0).max(0.5);
    let cw = (renderer.base_cell.0 * scale).ceil();
    let ch = (renderer.base_cell.1 * scale).ceil();
    let grid_width = (cw * size.cols as f32).round() as u32;
    let grid_height = (ch * size.rows as f32).round() as u32;
    let pad = (PADDING * scale).round() as u32;
    let width = grid_width + pad * 2;
    let height = grid_height + pad * 2 + (STATUS_HEIGHT * scale).ceil() as u32;
    ensure!(
        width > 0 && height > 0 && (width as u64) * (height as u64) <= 16 * 1024 * 1024,
        "screenshot exceeds 16 megapixels"
    );
    let device = windows_canvas::GpuDevice::new_or_warp()?;
    let target = device.create_render_target(width, height)?;
    target.draw(|ctx| {
        NativeDrawing::new(ctx)?.set_dpi(96.0 * scale);
        renderer.draw_surface(
            ctx,
            width as f32 / scale,
            height as f32 / scale,
            true,
            frame,
        )?;
        Ok(())
    })?;
    let mut pixels = target.read_pixels()?;
    // The opaque clear makes every pixel opaque; convert premultiplied BGRA to RGBA.
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    let image =
        image::RgbaImage::from_raw(width, height, pixels).expect("validated readback dimensions");
    Ok(image::imageops::crop_imm(&image, pad, pad, grid_width, grid_height).to_image())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Apartment;
    impl Apartment {
        fn new() -> Self {
            unsafe {
                windows::Win32::System::Com::CoInitializeEx(
                    None,
                    windows::Win32::System::Com::COINIT_MULTITHREADED,
                )
                .ok()
                .unwrap();
            }
            Self
        }
    }
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe {
                windows::Win32::System::Com::CoUninitialize();
            }
        }
    }

    #[test]
    fn osc_selection_recolors_only_the_selected_cells_of_shaped_runs() {
        use w11term::interaction::Point;

        let _com = Apartment::new();
        let mut renderer = Renderer::with_size("Cascadia Code", true, 18.0).unwrap();
        let device = windows_canvas::GpuDevice::new_or_warp().unwrap();
        let (width, height) = (360_u32, 180_u32);
        let target = device.create_render_target(width, height).unwrap();
        let size = (width as f32, height as f32);
        let metrics = renderer.measure(size.0, size.1, 1.0);
        let (cw, ch) = (metrics.cell_width as usize, metrics.cell_height as usize);
        let render = |renderer: &mut Renderer, frame: &Snapshot, selection: Option<&Selection>| {
            target
                .draw(|ctx| {
                    NativeDrawing::new(ctx)?.set_dpi(96.0);
                    renderer.paint(ctx, size, false, frame, CursorDisplay::Live, selection)?;
                    Ok(())
                })
                .unwrap();
            target.read_pixels().unwrap()
        };
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let mut terminal = w11term::terminal::create(
            wezterm_term::TerminalSize {
                cols: metrics.cols,
                rows: metrics.rows,
                ..Default::default()
            },
            tx,
        );
        terminal.advance_bytes("\x1b[?25l\x1b]11;#000000\x07\x1b]10;#ffffff\x07\x1b]17;#602000\x07\x1b]19;#00ff00\x07A->B界X\r\nسلام");
        let frame = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        let normal = render(&mut renderer, &frame, None);
        let mut reference = frame.clone();
        reference.palette.background = frame.selection_colors.background.unwrap();
        reference.palette.foreground = frame.selection_colors.foreground.unwrap();
        let recolored = render(&mut renderer, &reference, None);
        for (row, col, logical) in [(0, 2, 2..3), (0, 5, 4..6), (1, 1, 1..2)] {
            let mut selection = Selection::new(
                Point {
                    row: frame.first_stable_row + row as isize,
                    col,
                },
                false,
                false,
            );
            selection.observe(&frame);
            selection.active = true;
            let selected = render(&mut renderer, &frame, Some(&selection));
            let visual = frame.cell_map(row).unwrap().visual_ranges(logical);
            let mut seen_ink = false;
            let paper = [0, 32, 96, 255];
            for y in 0..2 * ch {
                for x in 0..metrics.cols * cw {
                    let i = ((PADDING as usize + y) * width as usize + PADDING as usize + x) * 4;
                    let inside =
                        y / ch == row && visual.iter().any(|span| span.contains(&(x / cw)));
                    let expected = if inside { &recolored } else { &normal };
                    assert_eq!(
                        &selected[i..i + 4],
                        &expected[i..i + 4],
                        "pixel {x},{y} with selection {row},{col}"
                    );
                    seen_ink |= inside && selected[i..i + 4] != paper;
                }
            }
            assert!(seen_ink);
        }
    }

    #[test]
    fn focused_block_recolors_the_original_shaped_run_only_inside_the_cursor() {
        let _com = Apartment::new();
        let mut renderer = Renderer::with_size("Cascadia Code", true, 18.0).unwrap();
        let blink = Rc::new(RefCell::new(CursorBlink::default()));
        let surface = windows_canvas::Invalidator::new();
        renderer.live_cursor = Some(blink.clone());
        let device = windows_canvas::GpuDevice::new_or_warp().unwrap();
        let (width, height) = (320_u32, 160_u32);
        let target = device.create_render_target(width, height).unwrap();
        let metrics = renderer.measure(width as f32, height as f32, 1.0);
        let (cw, ch) = (metrics.cell_width as usize, metrics.cell_height as usize);
        let render = |renderer: &mut Renderer, frame: &Snapshot, display| {
            target
                .draw(|ctx| {
                    NativeDrawing::new(ctx)?.set_dpi(96.0);
                    renderer.paint(
                        ctx,
                        (width as f32, height as f32),
                        false,
                        frame,
                        display,
                        None,
                    )?;
                    Ok(())
                })
                .unwrap();
            target.read_pixels().unwrap()
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut terminal = w11term::terminal::create(wezterm_term::TerminalSize::default(), tx);
        terminal.advance_bytes("A->B界X\x1b[4;9;53m \x1b[0m\r\nسلام");
        for (request, left, cells, paper) in [
            ("\x1b]12;#ff0000\x1b\\\x1b[1;3H", 2, 1, [0, 0, 255, 255]),
            (
                "\x1b]12;#0000ff\x1b\\\x1b[2 q\x1b[1;6H",
                4,
                2,
                [255, 0, 0, 255],
            ),
            ("\x1b[1 q\x1b[1;9H", 8, 1, [255, 0, 0, 255]),
            ("\x1b[1;8H", 7, 1, [255, 0, 0, 255]),
            ("\x1b[2;1H", 3, 1, [255, 0, 0, 255]),
            (
                "\x1b]11;#ffffff\x1b\\\x1b]12;rgb:0001/0000/0000\x1b\\\x1b[1;1H",
                0,
                1,
                [0, 0, 0, 255],
            ),
        ] {
            terminal.advance_bytes(request);
            let mut frame = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
            if paper != [0, 0, 0, 255] {
                frame.palette.cursor_fg = frame
                    .palette
                    .resolve_fg(wezterm_term::color::ColorAttribute::PaletteIndex(10));
            }
            let mut reference = frame.clone();
            reference.cursor_visible = false;
            let normal = render(&mut renderer, &reference, CursorDisplay::Static);
            // Paint the complete original text with the cursor colors. A clipped
            // cursor must sample exactly this glyph mask, including its neighbors'
            // influence on the arrow's shaping and on the wide character's metrics.
            reference.palette.background = frame.palette.cursor_bg;
            reference.palette.foreground = if paper == [0, 0, 0, 255] {
                // Black OSC 12 on a white theme collides with the default black cursor ink.
                frame.palette.background
            } else {
                frame.palette.cursor_fg
            };
            let recolored = render(&mut renderer, &reference, CursorDisplay::Static);
            blink.borrow_mut().set_focus(true, None, &surface);
            let focused = render(&mut renderer, &frame, CursorDisplay::Live);
            let mut ink = 0;
            for y in 0..2 * ch {
                for x in 0..10 * cw {
                    let i = ((PADDING as usize + y) * width as usize + PADDING as usize + x) * 4;
                    let inside = y / ch == frame.cursor.y as usize
                        && (left * cw..(left + cells) * cw).contains(&x);
                    let expected = if inside { &recolored } else { &normal };
                    assert_eq!(
                        &focused[i..i + 4],
                        &expected[i..i + 4],
                        "pixel {x},{y} at {request:?}"
                    );
                    if inside && focused[i..i + 4] != paper {
                        ink += 1;
                    }
                }
            }
            assert_eq!(
                ink > 0,
                left != 8,
                "ink on text, solid fill on a blank cell"
            );

            let capture = render(&mut renderer, &frame, CursorDisplay::Static);
            assert!(focused != capture, "live block and static outline");
            renderer.copy_cursor = Some(w11term::interaction::Point {
                col: frame.cursor.x,
                row: frame.first_stable_row + frame.cursor.y as isize,
            });
            assert!(
                render(&mut renderer, &frame, CursorDisplay::Live) == capture,
                "copy cursor"
            );
            renderer.copy_cursor = None;
            blink.borrow_mut().set_focus(false, None, &surface);
            assert!(
                render(&mut renderer, &frame, CursorDisplay::Live) == capture,
                "inactive cursor"
            );
        }
    }

    #[test]
    fn cursor_guide_is_one_physical_pixel_on_the_focused_live_row() {
        let _com = Apartment::new();
        let mut renderer = Renderer::with_size("Cascadia Mono", false, 16.0).unwrap();
        let blink = Rc::new(RefCell::new(CursorBlink::default()));
        let surface = windows_canvas::Invalidator::new();
        renderer.live_cursor = Some(blink.clone());
        blink.borrow_mut().set_focus(true, None, &surface);
        let (width, height) = (330_u32, 210_u32);
        let dpi = 120.0;
        let scale = dpi / 96.0;
        let device = windows_canvas::GpuDevice::new_or_warp().unwrap();
        let target = device.create_render_target(width, height).unwrap();
        let metrics = renderer.measure(width as f32 / scale, height as f32 / scale, scale);
        let render = |renderer: &mut Renderer, frame: &Snapshot, display| {
            target
                .draw(|ctx| {
                    NativeDrawing::new(ctx)?.set_dpi(dpi);
                    renderer.paint(
                        ctx,
                        (width as f32 / scale, height as f32 / scale),
                        false,
                        frame,
                        display,
                        None,
                    )?;
                    Ok(())
                })
                .unwrap();
            target.read_pixels().unwrap()
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut terminal = w11term::terminal::create(wezterm_term::TerminalSize::default(), tx);
        terminal.advance_bytes(b"\x1b[2;4H\x1b[6 q");
        let mut frame = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        let unmarked = render(&mut renderer, &frame, CursorDisplay::Live);
        renderer.cursor_guide = CursorGuide::FollowApplications;
        assert_eq!(render(&mut renderer, &frame, CursorDisplay::Live), unmarked);
        frame.application_cursor_guide = true;
        let marked = render(&mut renderer, &frame, CursorDisplay::Live);
        let changed_rows: Vec<_> = marked
            .chunks_exact(width as usize * 4)
            .zip(unmarked.chunks_exact(width as usize * 4))
            .enumerate()
            .filter_map(|(row, (marked, unmarked))| (marked != unmarked).then_some(row))
            .collect();
        assert_eq!(
            changed_rows,
            [((PADDING + 2.0 * metrics.cell_height) * scale).round() as usize - 1]
        );

        frame.application_cursor_guide = false;
        renderer.cursor_guide = CursorGuide::On;
        assert_eq!(render(&mut renderer, &frame, CursorDisplay::Live), marked);
        assert_eq!(
            render(&mut renderer, &frame, CursorDisplay::Static),
            unmarked
        );
        blink.borrow_mut().set_focus(false, None, &surface);
        assert_eq!(render(&mut renderer, &frame, CursorDisplay::Live), unmarked);
        blink.borrow_mut().set_focus(true, None, &surface);
        frame.cursor_visible = false;
        let hidden = render(&mut renderer, &frame, CursorDisplay::Live);
        renderer.cursor_guide = CursorGuide::Off;
        assert_eq!(render(&mut renderer, &frame, CursorDisplay::Live), hidden);
    }

    #[test]
    fn cursor_timer_only_runs_for_a_blinking_application_caret_in_the_painted_grid() {
        let _com = Apartment::new();
        let mut renderer = Renderer::new().unwrap();
        let blink = Rc::new(RefCell::new(CursorBlink::default()));
        blink
            .borrow_mut()
            .set_focus(true, Some(200), &windows_canvas::Invalidator::new());
        renderer.live_cursor = Some(blink.clone());
        let device = windows_canvas::GpuDevice::new_or_warp().unwrap();
        let target = device.create_render_target(260, 180).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut terminal = w11term::terminal::create(wezterm_term::TerminalSize::default(), tx);
        for (request, copy_mode, running) in [
            ("\x1b[5 q", false, true),
            ("\x1b[?25l", false, false),
            ("\x1b[?25h", false, true),
            ("\x1b[24;80H", false, false),
            ("\x1b[H", true, false),
            ("", false, true),
            ("\x1b[6 q", false, false),
        ] {
            terminal.advance_bytes(request.as_bytes());
            let frame = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
            renderer.copy_cursor = copy_mode.then_some(w11term::interaction::Point {
                row: frame.first_stable_row,
                col: 1,
            });
            target
                .draw(|ctx| {
                    renderer.paint(
                        ctx,
                        (260.0, 180.0),
                        false,
                        &frame,
                        CursorDisplay::Live,
                        None,
                    )?;
                    Ok(())
                })
                .unwrap();
            assert_eq!(blink.borrow().is_running(), running, "{request:?}");
        }
    }

    #[test]
    fn dec_line_renditions_scale_and_clip_glyphs_and_address_both_halves_of_a_cell() {
        let _com = Apartment::new();
        let mut renderer = Renderer::with_font("Cascadia Mono", false).unwrap();
        let device = windows_canvas::GpuDevice::new_or_warp().unwrap();
        let (width, height) = (360, 240);
        let target = device.create_render_target(width, height).unwrap();
        let reference = device.create_render_target(width, height).unwrap();
        let metrics = renderer.measure(width as f32, height as f32, 1.0);
        let (cw, ch) = (metrics.cell_width, metrics.cell_height);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let size = wezterm_term::TerminalSize {
            cols: metrics.cols,
            rows: metrics.rows,
            dpi: 96,
            ..Default::default()
        };
        let mut term = w11term::terminal::create(size, tx);
        term.advance_bytes(b"\x1b[?25l\x1b#3T\r\n\x1b#4T\r\n\x1b#6T\x1b[48;2;255;0;0m \x1b[0m");
        let frame = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
        target
            .draw(|ctx| {
                NativeDrawing::new(ctx)?.set_dpi(96.0);
                renderer.paint(
                    ctx,
                    (width as f32, height as f32),
                    false,
                    &frame,
                    CursorDisplay::Static,
                    None,
                )?;
                Ok(())
            })
            .unwrap();

        // Use one unclipped double-height glyph as a reference for the two halves.
        reference
            .draw(|ctx| {
                let native = NativeDrawing::new(ctx)?;
                native.set_dpi(96.0);
                ctx.clear(color(rgba(frame.palette.background)));
                let layout = TextLayout::new("T", &renderer.normal, cw + 4.0, ch)?;
                renderer.typography.apply(&layout, false, false)?;
                native.text(
                    &layout,
                    PADDING,
                    PADDING,
                    rgba(frame.palette.foreground),
                    (2.0, 2.0),
                )?;
                native.text(
                    &layout,
                    PADDING,
                    PADDING + 2.0 * ch,
                    rgba(frame.palette.foreground),
                    (2.0, 1.0),
                )
            })
            .unwrap();

        let pixels = target.read_pixels().unwrap();
        let expected = reference.read_pixels().unwrap();
        let pad = PADDING as usize;
        let (cw, ch) = (cw as usize, ch as usize);
        let pixel = |x, y| 4 * (y * width as usize + x);
        let background = &pixels[0..4];
        for row in 0..3 {
            let mut ink = 0;
            for y in pad + row * ch..pad + (row + 1) * ch {
                let span = pixel(pad, y)..pixel(pad + 2 * cw, y);
                assert_eq!(&pixels[span.clone()], &expected[span.clone()]);
                ink += pixels[span]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .filter(|p| *p != background)
                    .count();
            }
            assert!(ink > 0, "row {row} must contain part of the glyph");
        }
        let y = pad + 2 * ch;
        for col in [2, 3] {
            let start = pixel(pad + col * cw, y);
            assert_eq!(&pixels[start..start + 4], &[0, 0, 255, 255]);
            let hit = metrics.hit((pad + col * cw) as f64 + 0.5, y as f64 + 0.5);
            assert_eq!(hit.point(&frame).col, 1);
        }
        assert_eq!(
            frame.cell_map(2).unwrap().visual_ranges(1..2),
            std::slice::from_ref(&(2..4))
        );
        let mut copy = w11term::copy_mode::CopyMode::new(&frame);
        copy.navigate(w11term::copy_mode::CopyKey::End, &frame);
        assert_eq!(copy.cursor.col, frame.cols / 2 - 1);
    }

    #[test]
    fn complex_text_fixture_matches_native_word_shaping_and_decoration_masks() {
        let _com = Apartment::new();
        let mut renderer = Renderer::with_font("Cascadia Mono", false).unwrap();
        let device = windows_canvas::GpuDevice::new_or_warp().unwrap();
        let width = 480;
        let height = 410;
        let target = device.create_render_target(width, height).unwrap();
        let reference = device.create_render_target(width, height).unwrap();
        let size = wezterm_term::TerminalSize {
            cols: 32,
            rows: 12,
            dpi: 96,
            ..Default::default()
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut term = w11term::terminal::create(size, tx);
        term.advance_bytes(include_bytes!("../tests/fixtures/complex-text.vt"));
        let frame = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
        target
            .draw(|ctx| {
                NativeDrawing::new(ctx)?.set_dpi(96.0);
                renderer.draw_surface(ctx, width as f32, height as f32, false, &frame)?;
                Ok(())
            })
            .unwrap();
        let pixels = target.read_pixels().unwrap();
        let cw = renderer.metrics.cell_width as usize;
        let ch = renderer.metrics.cell_height as usize;
        let row_pixels = |buffer: &[u8], row: usize, cols: usize| -> Vec<[u8; 4]> {
            let mut out = Vec::new();
            for y in 0..ch {
                let start = (PADDING as usize + row * ch + y) * width as usize + PADDING as usize;
                out.extend_from_slice(
                    buffer[start * 4..(start + cw * cols) * 4]
                        .as_chunks::<4>()
                        .0,
                );
            }
            out
        };
        // The logical red Alef is the rightmost Hebrew letter in this mixed line.
        let top = row_pixels(&pixels, 0, 8);
        assert_eq!(top[5 * cw], [0, 0, 255, 255]);
        assert_ne!(top[cw], [0, 0, 255, 255]);

        // Build an independent native DirectWrite reference from whole logical words.
        // This tests the terminal's run assembly, contextual forms and placement
        // without baking machine-specific fallback fonts into a golden bitmap.
        reference
            .draw(|ctx| {
                let native = NativeDrawing::new(ctx)?;
                native.set_dpi(96.0);
                ctx.clear(color(rgba(frame.palette.background)));
                for (row, text, rtl) in [(1, "سلام", true), (2, "नमस्ते", false)] {
                    let grid = (wezterm_term::unicode_column_width(text, None) * cw) as f32;
                    let layout = TextLayout::new(text, &renderer.normal, grid + 4.0, ch as f32)?;
                    renderer.typography.apply(&layout, false, rtl)?;
                    let natural = layout.metrics().width_including_trailing_whitespace;
                    native.text(
                        &layout,
                        PADDING,
                        PADDING + (row * ch) as f32,
                        rgba(frame.palette.foreground),
                        (grid / natural, 1.0),
                    )?;
                }
                Ok(())
            })
            .unwrap();
        let expected = reference.read_pixels().unwrap();
        for row in 1..=2 {
            let actual = row_pixels(&pixels, row, 8);
            assert!(actual.iter().any(|p| p[1] > 100), "empty word on row {row}");
            assert!(
                actual == row_pixels(&expected, row, 8),
                "word on row {row} differs from DirectWrite reference: {:?}",
                frame.rows[row]
                    .visible_cells()
                    .map(|c| (c.str().to_owned(), c.width()))
                    .collect::<Vec<_>>()
            );
        }
        assert!(
            row_pixels(&pixels, 3, 7) != row_pixels(&pixels, 4, 7),
            "italic matched upright"
        );

        let mut masks = HashSet::new();
        for row in 5..10 {
            let samples = row_pixels(&pixels, row, 8);
            let mask: Vec<_> = samples.iter().map(|p| p[2] > 100 && p[1] < 64).collect();
            assert!(
                mask.iter().any(|bit| *bit),
                "missing underline on row {row}"
            );
            assert!(masks.insert(mask), "duplicate underline shape on row {row}");
        }
        let strike = row_pixels(&pixels, 10, 8);
        assert!(
            strike.iter().any(|p| p[1] > 160 && p[2] < 64),
            "strike lost its foreground color"
        );
        assert!(
            strike.iter().any(|p| p[2] > 160 && p[1] < 64),
            "underline lost its explicit color"
        );

        let overline = row_pixels(&pixels, 11, 9);
        let top = |col| overline[col * cw + cw / 2];
        let paper = overline[cw / 2 + 9 * cw]; // below the overlined space
        // A green space, a two-column grapheme, and another space share one stroke.
        assert!(overline[..4 * cw].iter().all(|&p| p == [0, 255, 0, 255]));
        assert_ne!(top(0), paper);
        assert_eq!(top(4), paper, "SGR 55 clears just the overline");
        assert!((0..ch).any(|y| overline[y * 9 * cw + 4 * cw + cw / 2] == [0, 0, 255, 255]));
        assert_eq!(top(5), paper, "concealed overline");
        assert_eq!(top(6), paper, "reset cell");
        assert_ne!(top(7), paper, "default foreground overline");
        assert_eq!(top(8), paper, "SGR 0 clears the overline");
    }

    #[test]
    fn oversized_and_malformed_images_are_isolated_and_rejections_expire() {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let _com = Apartment::new();
        let mut renderer = Renderer::new().unwrap();
        let device = windows_canvas::GpuDevice::new_or_warp().unwrap();
        let target = device.create_render_target(640, 360).unwrap();
        let size = wezterm_term::TerminalSize {
            cols: 40,
            rows: 12,
            pixel_width: 400,
            pixel_height: 240,
            dpi: 96,
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut terminal = w11term::terminal::create(size, tx);
        let mut maximum = 0;
        target
            .draw(|ctx| {
                maximum = NativeDrawing::new(ctx)?.maximum_bitmap_size();
                Ok(())
            })
            .unwrap();
        // Small even on devices whose bitmap limit is larger than 20,000.
        let width = maximum.saturating_add(1).min(20_000);
        terminal.advance_bytes(format!(
            "\x1b_Ga=T,i=7,p=1,f=32,s={width},v=1,c=4,r=2,q=2;{}\x1b\\\r\nhealthy",
            STANDARD.encode([255, 0, 0, 255].repeat(width as usize))
        ));
        let frame = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        assert!(
            frame
                .rows
                .iter()
                .any(|l| l.visible_cells().any(|c| c.attrs().images().is_some()))
        );
        for _ in 0..3 {
            target
                .draw(|ctx| {
                    renderer.draw_surface(ctx, 640.0, 360.0, false, &frame)?;
                    Ok(())
                })
                .unwrap();
        }
        assert_eq!(
            renderer.image_attempts, 1,
            "one admission per content/device"
        );
        if width > maximum {
            assert_eq!(renderer.image_uploads, 0);
            assert_eq!(renderer.rejected_images.len(), 1);
        }
        target
            .draw(|ctx| {
                let native = NativeDrawing::new(ctx)?;
                let malformed = ImageDataType::Rgba8 {
                    data: vec![0; 3],
                    width: 1,
                    height: 1,
                    hash: [1; 32],
                };
                assert!(
                    renderer
                        .admit_image(&native, &malformed, [1; 32])?
                        .is_none()
                );
                Ok(())
            })
            .unwrap();
        terminal.advance_bytes(b"\x1b_Ga=d,d=I,i=7,q=2\x1b\\");
        let frame = Snapshot::capture(&terminal, None, 2, 0, String::new(), 0);
        target
            .draw(|ctx| {
                renderer.draw_surface(ctx, 640.0, 360.0, false, &frame)?;
                Ok(())
            })
            .unwrap();
        assert!(renderer.rejected_images.is_empty());
        assert!(renderer.images.is_empty());
        assert!(
            target
                .read_pixels()
                .unwrap()
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[0] > 100 && p[1] > 100 && p[2] > 100),
            "text still renders"
        );
    }

    #[test]
    fn image_budget_defers_once_per_frame_without_evicting_visible_textures() {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let _com = Apartment::new();
        let mut renderer = Renderer::new().unwrap();
        renderer.image_budget = 256;
        let device = windows_canvas::GpuDevice::new_or_warp().unwrap();
        let target = device.create_render_target(640, 360).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut terminal = w11term::terminal::create(
            wezterm_term::TerminalSize {
                cols: 40,
                rows: 12,
                pixel_width: 400,
                pixel_height: 240,
                dpi: 96,
            },
            tx,
        );
        for (id, row, color) in [(1, 1, [255, 0, 0, 255]), (2, 4, [0, 255, 0, 255])] {
            terminal.advance_bytes(format!(
                "\x1b[{row};1H\x1b_Ga=T,i={id},p=1,f=32,s=8,v=8,c=4,r=2,q=2;{}\x1b\\",
                STANDARD.encode(color.repeat(64))
            ));
        }
        let frame = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        for n in 0..3 {
            target
                .draw(|ctx| {
                    renderer.draw_surface(ctx, 640.0, 360.0, false, &frame)?;
                    Ok(())
                })
                .unwrap();
            assert_eq!(renderer.images.len(), 1);
            assert_eq!(renderer.tile_bytes, 256);
            assert_eq!(renderer.image_uploads, 1);
            assert_eq!(renderer.image_attempts, 2 + n);
            assert!(
                renderer.rejected_images.is_empty(),
                "budget rejection must be retryable"
            );
        }
        terminal.advance_bytes(b"\x1b_Ga=d,d=I,i=1,q=2\x1b\\");
        let frame = Snapshot::capture(&terminal, None, 2, 0, String::new(), 0);
        target
            .draw(|ctx| {
                renderer.draw_surface(ctx, 640.0, 360.0, false, &frame)?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            renderer.image_uploads, 2,
            "deferred image admitted after capacity frees"
        );
        target
            .draw(|ctx| {
                renderer.draw_surface(ctx, 640.0, 360.0, true, &frame)?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            renderer.image_uploads, 3,
            "device change invalidates old GPU resources"
        );
    }
    #[test]
    fn repeated_image_id_replacement_resize_and_deletion_release_gpu_textures() {
        use base64::{Engine, engine::general_purpose::STANDARD};
        use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap();
        }
        struct Com;
        impl Drop for Com {
            fn drop(&mut self) {
                unsafe {
                    CoUninitialize();
                }
            }
        }
        let _com = Com;
        let mut renderer = Renderer::new().unwrap();
        let device = windows_canvas::GpuDevice::new_or_warp().unwrap();
        let target = device.create_render_target(640, 360).unwrap();
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let mut size = wezterm_term::TerminalSize {
            cols: 40,
            rows: 12,
            pixel_width: 400,
            pixel_height: 240,
            dpi: 96,
        };
        let mut terminal = w11term::terminal::create(size, tx);
        for frame in 0..100 {
            let pixels = [frame as u8, 255 - frame as u8, 42, 255].repeat(8 * 8);
            terminal.advance_bytes(format!(
                "\x1b[1;1H\x1b_Ga=T,i=7,p=1,f=32,s=8,v=8,c=2,r=2,q=2;{}\x1b\\",
                STANDARD.encode(pixels)
            ));
            if frame == 50 {
                size.cols = 60;
                size.pixel_width = 600;
                terminal.resize(size);
            }
            let snapshot = Snapshot::capture(&terminal, None, frame + 1, 0, String::new(), 0);
            target
                .draw(|ctx| {
                    renderer.draw_surface(ctx, 640.0, 360.0, frame == 0, &snapshot)?;
                    Ok(())
                })
                .unwrap();
            assert_eq!(
                renderer.images.len(),
                1,
                "old GPU image survived replacement {frame}"
            );
            assert_eq!(renderer.tile_bytes, 8 * 8 * 4);
            assert!(
                snapshot.rows[0]
                    .visible_cells()
                    .next()
                    .unwrap()
                    .attrs()
                    .images()
                    .is_some(),
                "placement lost on resize"
            );
        }
        terminal.advance_bytes(b"\x1b_Ga=d,d=I,i=7,q=2\x1b\\");
        let snapshot = Snapshot::capture(&terminal, None, 101, 0, String::new(), 0);
        target
            .draw(|ctx| {
                renderer.draw_surface(ctx, 640.0, 360.0, false, &snapshot)?;
                Ok(())
            })
            .unwrap();
        assert!(renderer.images.is_empty());
        assert_eq!(renderer.tile_bytes, 0);
    }
    #[test]
    fn preview_fits_the_entire_grid_and_renders_kitty_placements_in_the_pane_color_scheme() {
        let _com = Apartment::new();
        let mut renderer = Renderer::new().unwrap();
        let metrics = renderer.measure(800.0, 600.0, 1.0);
        let size = wezterm_term::TerminalSize {
            cols: 32,
            rows: 12,
            dpi: 96,
            pixel_width: 32 * metrics.cell_width as usize,
            pixel_height: 12 * metrics.cell_height as usize,
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut term = w11term::terminal::create_with_profile(
            size,
            tx,
            Default::default(),
            w11term::options::ColorScheme::OneHalfLight,
        );
        term.advance_bytes(include_bytes!("../tests/fixtures/kitty/placements.vt"));
        term.advance_bytes(b"\x1b[?25l\x1b[12;30H\x1b[0m \x1b[41m \x1b[48;2;255;0;255m ");
        let snapshot = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
        let device = windows_canvas::GpuDevice::new_or_warp().unwrap();
        for (width, height) in [(160, 220), (320, 80)] {
            let target = device.create_render_target(width, height).unwrap();
            target
                .draw(|ctx| {
                    NativeDrawing::new(ctx)?.set_dpi(96.0);
                    renderer.draw_preview(ctx, width as f32, height as f32, true, &snapshot)
                })
                .unwrap();
            let pixels = target.read_pixels().unwrap();
            let source_w = PADDING * 2.0 + size.pixel_width as f32;
            let source_h = PADDING * 2.0 + STATUS_HEIGHT + size.pixel_height as f32;
            let fit = (width as f32 / source_w).min(height as f32 / source_h);
            let sample = |col: f32, row: f32| {
                let x = (width as f32 - source_w * fit) / 2.0
                    + (PADDING + col * metrics.cell_width) * fit;
                let y = (height as f32 - source_h * fit) / 2.0
                    + (PADDING + row * metrics.cell_height) * fit;
                let i = (y as usize * width as usize + x as usize) * 4;
                [pixels[i + 2], pixels[i + 1], pixels[i]]
            };
            // At thumbnail scale Direct2D interpolates the adjacent colored tiles.
            let [red, green, blue] = sample(2.5, 2.5);
            assert!(
                red > 220 && green < 35 && blue < 35,
                "kitty placement {red}/{green}/{blue}"
            );
            assert_eq!(sample(29.5, 11.5), [250; 3], "profile background");
            assert_eq!(sample(30.5, 11.5), [0xe4, 0x56, 0x49], "profile ANSI red");
            assert_eq!(sample(31.5, 11.5), [255, 0, 255], "explicit RGB");
        }
    }

    #[test]
    fn offscreen_reverse_screen_recolors_existing_default_ink_and_paper() {
        let _com = Apartment::new();
        let mut renderer = Renderer::new().unwrap();
        let metrics = renderer.measure(320.0, 200.0, 1.0);
        let size = wezterm_term::TerminalSize {
            cols: 8,
            rows: 2,
            pixel_width: (8.0 * metrics.cell_width).round() as usize,
            pixel_height: (2.0 * metrics.cell_height).round() as usize,
            dpi: 96,
        };
        let width = size.pixel_width as u32 + 2 * PADDING as u32;
        let height = size.pixel_height as u32 + (2.0 * PADDING + STATUS_HEIGHT) as u32;
        let device = windows_canvas::GpuDevice::new_or_warp().unwrap();
        let target = device.create_render_target(width, height).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut terminal = w11term::terminal::create(size, tx);
        // Struck spaces provide a solid pixel of foreground ink without font rasterization.
        terminal.advance_bytes(concat!(
            "\x1b[?25l\x1b]10;#ff0000\x1b\\\x1b]11;#0000ff\x1b\\",
            "\x1b]4;17;#00ffff;18;#ff00ff\x1b\\",
            "\x1b[9m \x1b[7m ",
            "\x1b[27;38;5;17;48;5;18m ",
            "\x1b[38;2;0;255;0;48;2;255;255;0m \x1b[7m ",
            "\x1b[27;39m \x1b[38;5;17;49m \x1b[0m",
            "\x1b_Ga=T,f=32,s=1,v=1,c=1,r=1,C=1,q=2;/////w==\x1b\\",
        ));
        let red = [255, 0, 0];
        let blue = [0, 0, 255];
        let green = [0, 255, 0];
        let yellow = [255, 255, 0];
        let cyan = [0, 255, 255];
        let magenta = [255, 0, 255];
        let mut previous = None;
        // DECSCNM swaps the current default pair before SGR 7 resolves each cell.
        // The pen, palette indices, explicit RGB values and image remain intact.
        for (sequence, fg, bg) in [
            ("", red, blue),
            ("\x1b[?5h", blue, red),
            ("\x1b]10;#00ff00\x1b\\\x1b]11;#ffff00\x1b\\", yellow, green),
            ("\x1b[?5l", green, yellow),
        ] {
            terminal.advance_bytes(sequence);
            let frame = Snapshot::capture(&terminal, previous.as_ref(), 1, 0, String::new(), 0);
            target
                .draw(|ctx| {
                    NativeDrawing::new(ctx)?.set_dpi(96.0);
                    renderer.draw_surface(ctx, width as f32, height as f32, false, &frame)?;
                    Ok(())
                })
                .unwrap();
            let pixels = target.read_pixels().unwrap();
            let rgb = |x: usize, y: usize| {
                let offset = (y * width as usize + x) * 4;
                [pixels[offset + 2], pixels[offset + 1], pixels[offset]]
            };
            assert_eq!(rgb(1, 1), bg, "canvas clear after {sequence:?}");
            let paper_y = (PADDING + renderer.metrics.cell_height * 0.25) as usize;
            let ink_y = (PADDING + renderer.metrics.cell_height * 0.5).round() as usize;
            for (col, (ink, paper)) in [
                (fg, bg),
                (bg, fg),
                (cyan, magenta),
                (green, yellow),
                (yellow, green),
                (fg, yellow),
                (cyan, bg),
                ([255; 3], [255; 3]),
            ]
            .into_iter()
            .enumerate()
            {
                let x = (PADDING + (col as f32 + 0.5) * renderer.metrics.cell_width) as usize;
                assert_eq!(
                    rgb(x, paper_y),
                    paper,
                    "cell {col} paper after {sequence:?}"
                );
                assert_eq!(rgb(x, ink_y), ink, "cell {col} ink after {sequence:?}");
            }
            previous = Some(frame);
        }
    }

    #[test]
    fn offscreen_indexed_kitty_images_blend_and_follow_palette_changes() {
        let size = wezterm_term::TerminalSize {
            cols: 3,
            rows: 2,
            pixel_width: 30,
            pixel_height: 40,
            dpi: 96,
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut terminal = w11term::terminal::create(size, tx);
        terminal.advance_bytes(concat!(
            "\x1b[?25l\x1b]11;#000000\x07\x1b]4;1;#ff0000\x07",
            "\x1b_Ga=T,f=8,s=1,v=1,i=1,c=1,r=1,C=1,q=2;AQ==\x1b\\",
            "\x1b[1;2H\x1b_Ga=T,f=16,s=1,v=1,i=2,c=1,r=1,C=1,q=2;AYA=\x1b\\",
            "\x1b[1;3H\x1b_Ga=T,f=16,s=1,v=1,i=3,c=1,r=1,C=1,q=2;AQA=\x1b\\",
        ));
        let first = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        terminal.advance_bytes(b"\x1b]4;1;#00ff00\x07");
        let second = Snapshot::capture(&terminal, Some(&first), 2, 0, String::new(), 0);
        let directory = tempfile::tempdir().unwrap();
        for (name, frame, colors) in [
            ("initial", first, [[255, 0, 0], [128, 0, 0], [0, 0, 0]]),
            ("recolored", second, [[0, 255, 0], [0, 128, 0], [0, 0, 0]]),
        ] {
            let path = directory.path().join(format!("{name}.png"));
            screenshot(&frame, size, &path).unwrap();
            let actual = image::open(path).unwrap().into_rgb8();
            for (column, expected) in colors.into_iter().enumerate() {
                let x = (2 * column as u32 + 1) * actual.width() / 6;
                let pixel = actual.get_pixel(x, actual.height() / 4).0;
                assert_eq!(pixel, expected, "{name}, column {column}");
            }
        }
    }

    #[test]
    fn offscreen_explicit_default_rgb_covers_a_below_background_image() {
        let size = wezterm_term::TerminalSize {
            cols: 3,
            rows: 2,
            pixel_width: 30,
            pixel_height: 40,
            dpi: 96,
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut terminal = w11term::terminal::create(size, tx);
        terminal.advance_bytes(concat!(
            "\x1b[?25l\x1b]11;#000000\x1b\\",
            "\x1b[1;2H\x1b[48;2;0;0;0m \x1b[49m",
            "\x1b_Ga=t,f=32,s=1,v=1,i=1,q=2;/wAA/w==\x1b\\",
            "\x1b[1;1H\x1b_Ga=p,i=1,p=1,z=-2147483648,c=1,r=1,C=1,q=2\x1b\\",
            "\x1b[1;2H\x1b_Ga=p,i=1,p=2,z=-2147483648,c=1,r=1,C=1,q=2\x1b\\",
        ));
        let frame = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        assert_eq!(rgba(frame.palette.background), [0.0, 0.0, 0.0, 1.0]);
        let covered = frame.rows[0].get_cell(1).unwrap();
        assert!(
            covered
                .attrs()
                .images()
                .unwrap()
                .iter()
                .any(|tile| tile.image_id() == Some(1) && tile.z_index() == i32::MIN)
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("explicit-background.png");
        screenshot(&frame, size, &path).unwrap();
        let actual = image::open(path).unwrap().into_rgb8();
        let y = actual.height() / 4;
        let cell = |col: u32| actual.get_pixel((2 * col + 1) * actual.width() / 6, y).0;
        assert_eq!(cell(0), [255, 0, 0], "default background shows the image");
        assert_eq!(cell(1), [0, 0, 0], "explicit RGB paints over the image");
    }

    #[test]
    fn offscreen_kitty_fractional_text_scales_and_aligns_both_axes() {
        let size = wezterm_term::TerminalSize {
            cols: 8,
            rows: 4,
            dpi: 96,
            ..Default::default()
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut terminal = w11term::terminal::create(size, tx);
        terminal.advance_bytes(concat!(
            "\x1b[?25l\x1b]11;#000000\x07\x1b]10;#ffffff\x07",
            "\x1b]66;n=1:d=2:w=3;WW\x07\r\n",
            "\x1b]66;n=1:d=2:w=3:h=2:v=2;WW\x07\r\n",
            "\x1b]66;n=1:d=2:w=3:h=1:v=1;WW\x07\r\n",
            "\x1b]66;w=3;WW\x07"
        ));
        let frame = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("kitty-fractional.png");
        let (width, height) = screenshot(&frame, size, &path).unwrap();
        let pixels = image::open(path).unwrap().into_rgb8();
        let cw = width / 8;
        let ch = height / 4;
        let bounds = |row: u32| {
            let mut xs = Vec::new();
            let mut ys = Vec::new();
            for y in 0..ch {
                for x in 0..width {
                    if pixels.get_pixel(x, row * ch + y).0 != [0, 0, 0] {
                        assert!(x < 3 * cw, "ink escaped its three columns");
                        xs.push(x);
                        ys.push(y);
                    }
                }
            }
            (
                *xs.iter().min().unwrap(),
                *xs.iter().max().unwrap(),
                *ys.iter().min().unwrap(),
                *ys.iter().max().unwrap(),
            )
        };
        let (top, middle, bottom, full) = (bounds(0), bounds(1), bounds(2), bounds(3));
        assert!(
            top.0 < middle.0 && middle.0 < bottom.0,
            "{top:?} {middle:?} {bottom:?}"
        );
        assert!(
            top.2 < middle.2 && middle.2 < bottom.2,
            "{top:?} {middle:?} {bottom:?}"
        );
        assert!(top.1 - top.0 < full.1 - full.0);
        assert!(top.3 - top.2 < full.3 - full.2);
    }

    #[test]
    fn offscreen_kitty_multicell_text_renders_a_continuation_without_its_top_row() {
        let size = wezterm_term::TerminalSize {
            cols: 8,
            rows: 3,
            dpi: 96,
            ..Default::default()
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut terminal = w11term::terminal::create(size, tx);
        terminal.advance_bytes(concat!(
            "\x1b[?25l\x1b]11;#000000\x07\x1b]10;#ffffff\x07",
            "\x1b[1;3H\x1b]66;s=2:w=2;W\x07",
        ));
        let directory = tempfile::tempdir().unwrap();
        let capture = |terminal: &wezterm_term::Terminal, name: &str| {
            let frame = Snapshot::capture(terminal, None, 1, 0, String::new(), 0);
            let path = directory.path().join(name);
            screenshot(&frame, size, &path).unwrap();
            image::open(path).unwrap().into_rgb8()
        };
        let before = capture(&terminal, "kitty-multicell-before.png");
        let cw = before.width() / 8;
        let ch = before.height() / 3;
        for row in 0..3 {
            let mut ink = 0;
            for y in row * ch..(row + 1) * ch {
                for x in 0..before.width() {
                    if before.get_pixel(x, y).0 != [0, 0, 0] {
                        assert!((2 * cw..6 * cw).contains(&x));
                        ink += 1;
                    }
                }
            }
            assert_eq!(ink > 0, row < 2, "row {row}");
        }
        terminal.advance_bytes(b"\x1b[3;1H\r\n");
        let after = capture(&terminal, "kitty-multicell-after.png");
        for y in 0..ch {
            for x in 0..before.width() {
                assert_eq!(after.get_pixel(x, y), before.get_pixel(x, y + ch));
            }
        }
    }

    #[test]
    fn offscreen_bidi_reorders_ink_on_both_sides_of_scaled_kitty_text() {
        let size = wezterm_term::TerminalSize {
            cols: 9,
            rows: 2,
            dpi: 96,
            ..Default::default()
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut terminal = w11term::terminal::create(size, tx);
        terminal.advance_bytes(concat!(
            "\x1b[?25l\x1b]11;#000000\x07\x1b]10;#ffffff\x07",
            "\x1b[1;4H\x1b]66;s=2:w=1;W\x07",
            "\x1b[1;1H\x1b[31mא\x1b[32mב\x1b[1;7H\x1b[31mג\x1b[32mד",
            "\x1b[2;1H\x1b[31mה\x1b[32mו\x1b[2;7H\x1b[31mז\x1b[32mח",
        ));
        let frame = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("kitty-multicell-bidi.png");
        let (width, height) = screenshot(&frame, size, &path).unwrap();
        let pixels = image::open(path).unwrap().into_rgb8();
        let cw = width / 9;
        let ch = height / 2;
        for row in 0..2 {
            let mut columns = [0_u8; 9];
            for (col, mask) in columns.iter_mut().enumerate() {
                let x = col as u32 * cw;
                for y in row * ch..(row + 1) * ch {
                    for x in x..x + cw {
                        let [r, g, b] = pixels.get_pixel(x, y).0;
                        if r > g && r > b {
                            *mask |= 1;
                        } else if g > r && g > b {
                            *mask |= 2;
                        }
                    }
                }
            }
            assert_eq!(columns, [2, 1, 0, 0, 0, 0, 2, 1, 0], "row {row}");
        }
    }

    #[test]
    fn offscreen_kitty_multicell_bar_and_underline_follow_the_rectangle() {
        let size = wezterm_term::TerminalSize {
            cols: 6,
            rows: 3,
            dpi: 96,
            ..Default::default()
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut terminal = w11term::terminal::create(size, tx);
        terminal.advance_bytes(concat!(
            "\x1b]11;#000000\x07\x1b]12;#ff0000\x07",
            "\x1b[1;2H\x1b]66;s=2:w=2; \x07\x1b[2;4H",
        ));
        let directory = tempfile::tempdir().unwrap();
        for (shape, bar) in [("\x1b[6 q", true), ("\x1b[4 q", false)] {
            terminal.advance_bytes(shape);
            let frame = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
            let path = directory
                .path()
                .join(if bar { "bar.png" } else { "underline.png" });
            let (width, height) = screenshot(&frame, size, &path).unwrap();
            let pixels = image::open(path).unwrap().into_rgb8();
            let (cw, ch) = (width / 6, height / 3);
            let red = |x, y| pixels.get_pixel(x, y).0 == [255, 0, 0];
            assert_eq!(red(cw, 0), bar);
            assert!(red(cw, 2 * ch - 1));
            assert_eq!(red(5 * cw - 1, 2 * ch - 1), !bar);
            assert!(!red(cw, 2 * ch));
        }
    }

    #[test]
    fn offscreen_kitty_text_width_keeps_ink_and_background_in_the_requested_cells() {
        let size = wezterm_term::TerminalSize {
            cols: 8,
            rows: 2,
            dpi: 96,
            ..Default::default()
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut terminal = w11term::terminal::create(size, tx);
        terminal.advance_bytes(concat!(
            "\x1b[?25l\x1b]11;#000000\x07\x1b]10;#ffffff\x07",
            "\x1b[48;2;0;0;255m\x1b]66;w=5; \x07\x1b[49m",
            "\x1b]66;w=1;WWWWWWW\x07"
        ));
        let frame = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("kitty-width.png");
        let (width, _) = screenshot(&frame, size, &path).unwrap();
        let pixels = image::open(path).unwrap().into_rgb8();
        let cw = width / 8;
        let mut ink = false;
        for y in 2..15 {
            for x in 0..width {
                let color = pixels.get_pixel(x, y).0;
                match x / cw {
                    0..=4 => assert_eq!(color, [0, 0, 255]),
                    5 => ink |= color != [0, 0, 0],
                    _ => assert_eq!(color, [0, 0, 0], "ink overflow at {x}, {y}"),
                }
            }
        }
        assert!(ink);
    }

    #[test]
    fn offscreen_kitty_reference_cell_centers() {
        assert_kitty_reference_colors(
            &[
                include_bytes!("../tests/fixtures/kitty/placements.vt"),
                include_bytes!("../tests/fixtures/kitty/layers.vt"),
            ],
            include_bytes!("../tests/fixtures/kitty/samples.json"),
        );
    }

    #[test]
    fn clear_visible_kitty_images_matches_reference_pixels() {
        assert_kitty_reference_colors(
            &[KITTY_CLEAR_SETUP, KITTY_CLEAR_VISIBLE],
            include_bytes!("../tests/fixtures/kitty/clear-visible-samples.json"),
        );
    }

    #[test]
    fn clear_kitty_images_at_a_cell_matches_reference_pixels() {
        assert_kitty_reference_colors(
            &[include_bytes!("../tests/fixtures/kitty/clear-cell.vt")],
            include_bytes!("../tests/fixtures/kitty/clear-cell-samples.json"),
        );
    }

    #[test]
    fn clear_kitty_image_groups_matches_reference_pixels() {
        assert_kitty_reference_colors(
            &[include_bytes!("../tests/fixtures/kitty/clear-groups.vt")],
            include_bytes!("../tests/fixtures/kitty/clear-groups-samples.json"),
        );
    }

    #[test]
    fn clear_kitty_image_by_number_matches_reference_pixels() {
        assert_kitty_reference_colors(
            &[include_bytes!("../tests/fixtures/kitty/clear-number.vt")],
            include_bytes!("../tests/fixtures/kitty/clear-number-samples.json"),
        );
    }

    #[test]
    fn clear_kitty_animation_frames_matches_reference_pixels() {
        assert_kitty_reference_colors(
            &[include_bytes!("../tests/fixtures/kitty/clear-frame.vt")],
            include_bytes!("../tests/fixtures/kitty/clear-frame-samples.json"),
        );
    }

    #[test]
    fn independent_kitty_placements_match_reference_pixels() {
        assert_kitty_reference_colors(
            &[include_bytes!(
                "../tests/fixtures/kitty/independent-placements.vt"
            )],
            include_bytes!("../tests/fixtures/kitty/independent-placements-samples.json"),
        );
    }

    const KITTY_CLEAR_SETUP: &[u8] = include_bytes!("../tests/fixtures/kitty/clear-setup.vt");
    const KITTY_CLEAR_VISIBLE: &[u8] = include_bytes!("../tests/fixtures/kitty/clear-visible.vt");

    fn assert_kitty_reference_colors(streams: &[&[u8]], expected: &[u8]) {
        #[derive(serde::Deserialize)]
        struct Sample {
            column: usize,
            row: usize,
            rgb: [u8; 3],
        }
        let metrics = Renderer::new().unwrap().metrics;
        let size = wezterm_term::TerminalSize {
            cols: 16,
            rows: 8,
            dpi: 96,
            pixel_width: (16.0 * metrics.cell_width).round() as usize,
            pixel_height: (8.0 * metrics.cell_height).round() as usize,
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut terminal = w11term::terminal::create(size, tx);
        for stream in streams {
            terminal.advance_bytes(stream);
        }
        let frame = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("w11term.png");
        screenshot(&frame, size, &path).unwrap();
        let actual = image::open(path).unwrap().into_rgb8();
        let samples: Vec<Sample> = serde_json::from_slice(expected).unwrap();
        for sample in samples {
            let x =
                ((sample.column as f32 + 0.5) * actual.width() as f32 / size.cols as f32) as u32;
            let y = ((sample.row as f32 + 0.5) * actual.height() as f32 / size.rows as f32) as u32;
            let pixel = actual.get_pixel(x, y);
            for (a, b) in pixel.0.into_iter().zip(sample.rgb) {
                // The default terminal palettes and native texture interpolation
                // differ slightly. The samples are inside opaque color fields.
                assert!(
                    a.abs_diff(b) < 32,
                    "({},{}) was {:?}, expected {:?}",
                    sample.column,
                    sample.row,
                    pixel,
                    sample.rgb
                );
            }
        }
    }

    #[test]
    fn kitty_clear_commands_release_gpu_textures_on_the_next_draw() {
        let _com = Apartment::new();
        let mut renderer = Renderer::new().unwrap();
        let size = wezterm_term::TerminalSize {
            cols: 16,
            rows: 8,
            pixel_width: 160,
            pixel_height: 160,
            dpi: 96,
        };
        let device = windows_canvas::GpuDevice::new_or_warp().unwrap();
        let target = device.create_render_target(320, 240).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut terminal = w11term::terminal::create(size, tx);
        let clear_all = b"\x1b_Ga=d,d=R,x=0,y=4294967295,q=2\x1b\\";
        for (sequence, stream, textures) in [
            (1, KITTY_CLEAR_SETUP, 3),
            (2, KITTY_CLEAR_VISIBLE, 1),
            (3, clear_all.as_slice(), 0),
        ] {
            terminal.advance_bytes(stream);
            let snapshot = Snapshot::capture(&terminal, None, sequence, 0, String::new(), 0);
            target
                .draw(|ctx| {
                    renderer.draw_surface(ctx, 320.0, 240.0, sequence == 1, &snapshot)?;
                    Ok(())
                })
                .unwrap();
            assert_eq!(renderer.images.len(), textures, "after draw {sequence}");
            assert_eq!(renderer.tile_bytes, textures * 8 * 8 * 4);
        }
    }

    #[test]
    fn offscreen_png_contains_terminal_text_kitty_and_sixel_pixels() {
        use base64::Engine;
        use wezterm_term::TerminalSize;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pane.png");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let metrics = Renderer::new().unwrap().metrics;
        let size = TerminalSize {
            cols: 80,
            rows: 24,
            pixel_width: 80 * metrics.cell_width.ceil() as usize,
            pixel_height: 24 * metrics.cell_height.ceil() as usize,
            dpi: 96,
        };
        let mut terminal = w11term::terminal::create(size, tx);
        terminal.advance_bytes(b"HUD fps=144\r\n\x1b[?25l");
        let pixels = [255, 0, 0, 255].repeat(8 * 8);
        terminal.advance_bytes(format!(
            "\x1b_Ga=T,f=32,s=8,v=8,c=2,r=2,q=2;{}\x1b\\",
            base64::engine::general_purpose::STANDARD.encode(pixels)
        ));
        terminal
            .advance_bytes(b"\x1b[6;1H\x1bPq\"1;1;24;24#1;2;0;100;0#1!24~-!24~-!24~-!24~\x1b\\");
        let frame = Snapshot::capture(&terminal, None, 1, 0, "test".into(), 0);
        let (width, height) = screenshot(&frame, size, &path).unwrap();
        let image = image::open(&path).unwrap().into_rgba8();
        assert_eq!(image.dimensions(), (width, height));
        assert!(
            image
                .pixels()
                .filter(|p| p[0] > 240 && p[1] < 10 && p[2] < 10)
                .count()
                > 100,
            "kitty image missing from PNG"
        );
        assert!(
            image
                .pixels()
                .filter(|p| p[0] < 10 && p[1] > 240 && p[2] < 10)
                .count()
                > 20,
            "sixel image missing from PNG"
        );
        assert!(
            image
                .pixels()
                .filter(|p| p[0] > 100 && p[1] > 100 && p[2] > 100)
                .count()
                > 30,
            "HUD text missing from PNG"
        );
    }
}
