use crate::native_drawing::{GpuImage, NativeDrawing, Typography};
use std::collections::{HashMap, HashSet};
use w11term::terminal::{Snapshot, colors, rgba};
use wezterm_term::{Intensity, Underline, image::ImageDataType};
use windows_canvas::{
    ColorF, DrawContext, DrawingSession, Rect, TextFormat, TextLayout, WordWrapping,
};

pub const PADDING: f32 = 12.0;
pub const STATUS_HEIGHT: f32 = 26.0;
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

/// First renderer: cached DirectWrite layouts + GPU-resident Direct2D images.
/// The engine/transport boundary allows an atlas renderer to replace this independently.
pub struct Renderer {
    normal: TextFormat,
    bold: TextFormat,
    small: TextFormat,
    layouts: HashMap<(String, bool), CachedLayout>,
    typography: Typography,
    ligatures: bool,
    images: HashMap<[u8; 32], CachedImage>,
    tile_bytes: usize,
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
struct TextRun<'a> {
    text: &'a str,
    bold: bool,
    x: f32,
    y: f32,
    cols: usize,
    fg: [f32; 4],
    shape: bool,
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
            normal,
            bold,
            small: TextFormat::new("Segoe UI", 11.0)?.with_word_wrapping(WordWrapping::NoWrap),
            layouts: HashMap::new(),
            typography: Typography::new(family, ligatures)?,
            ligatures,
            images: HashMap::new(),
            tile_bytes: 0,
            metrics,
            base_cell: (probe.width_including_trailing_whitespace, probe.height),
        })
    }

    fn text(&mut self, native: &NativeDrawing, run: TextRun<'_>) -> windows_canvas::Result<()> {
        let TextRun {
            text,
            bold,
            x,
            y,
            cols,
            fg,
            shape,
        } = run;
        if text.trim_end_matches(' ').is_empty() {
            return Ok(());
        }
        let key = (text.to_owned(), bold);
        if !self.layouts.contains_key(&key) {
            if self.layouts.len() >= 2048 {
                self.layouts.clear();
            }
            let layout = TextLayout::new(
                text,
                if bold { &self.bold } else { &self.normal },
                self.metrics.cell_width * cols as f32 + 4.0,
                self.metrics.cell_height,
            )?;
            self.typography.apply(&layout)?;
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
        // Monospaced ASCII shaping keeps its total advance equal to the occupied cells.
        // Scale the run once so pixel-snapped cells cannot accumulate fractional drift.
        let scale = if shape && cached.natural_width > 0.0 {
            cols as f32 * self.metrics.cell_width / cached.natural_width
        } else {
            1.0
        };
        native.text(&cached.layout, x, y, fg, scale)
    }

    pub fn draw(
        &mut self,
        ctx: &DrawContext<'_>,
        frame: &Snapshot,
    ) -> windows_canvas::Result<Metrics> {
        self.draw_surface(ctx, ctx.width, ctx.height, ctx.device_changed(), frame)
    }

    pub fn draw_interaction(
        &self,
        ctx: &DrawContext<'_>,
        frame: &Snapshot,
        selection: Option<&w11term::interaction::Selection>,
        link: Option<&w11term::interaction::Link>,
    ) -> windows_canvas::Result<()> {
        let cw = self.metrics.cell_width;
        let ch = self.metrics.cell_height;
        let brush = ctx.create_solid_brush(ColorF::new(0.25, 0.53, 0.90, 0.4))?;
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
                        ctx.fill_rect(
                            &Rect::from_xywh(
                                PADDING + point.col as f32 * cw,
                                PADDING + y as f32 * ch,
                                end.saturating_sub(point.col) as f32 * cw,
                                ch,
                            ),
                            &brush,
                        );
                    }
                }
            }
        }
        brush.set_color(ColorF::new(0.25, 0.53, 0.90, 0.4));
        if let Some(selection) =
            selection.filter(|s| s.alternate == frame.alternate && s.cols == frame.cols)
        {
            for y in 0..frame.rows.len().min(self.metrics.rows) {
                if let Some(mut span) =
                    selection.span(frame.first_stable_row + y as isize, self.metrics.cols)
                {
                    for cell in frame.rows[y].visible_cells() {
                        if cell.cell_index() < span.start
                            && cell.cell_index() + cell.width() > span.start
                        {
                            span.start = cell.cell_index();
                        }
                        if cell.cell_index() < span.end
                            && cell.cell_index() + cell.width() > span.end
                        {
                            span.end = cell.cell_index() + cell.width();
                        }
                    }
                    ctx.fill_rect(
                        &Rect::from_xywh(
                            PADDING + span.start as f32 * cw,
                            PADDING + y as f32 * ch,
                            span.len() as f32 * cw,
                            ch,
                        ),
                        &brush,
                    );
                }
            }
        }
        brush.set_color(ColorF::new(0.48, 0.79, 0.98, 1.0));
        if let Some(link) = link {
            for (row, span) in &link.spans {
                let y = *row - frame.first_stable_row;
                if y >= 0 && y < self.metrics.rows as isize {
                    ctx.fill_rect(
                        &Rect::from_xywh(
                            PADDING + span.start as f32 * cw,
                            PADDING + (y + 1) as f32 * ch - 2.0,
                            span.len() as f32 * cw,
                            1.0,
                        ),
                        &brush,
                    );
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

    fn draw_surface(
        &mut self,
        ctx: &DrawingSession<'_>,
        width: f32,
        height: f32,
        device_changed: bool,
        frame: &Snapshot,
    ) -> windows_canvas::Result<Metrics> {
        let native = NativeDrawing::new(ctx)?;
        if device_changed {
            self.layouts.clear();
            self.images.clear();
            self.tile_bytes = 0;
        }
        let scale = (native.dpi() / 96.0).max(0.5);
        self.measure(width, height, scale);
        // Keep GPU ownership tied to the current visible model. Replacing an id,
        // deleting a placement, changing tabs, or scrolling releases obsolete textures.
        let mut live = HashSet::new();
        for line in frame.rows.iter().take(self.metrics.rows) {
            for cell in line
                .visible_cells()
                .take_while(|c| c.cell_index() < self.metrics.cols)
            {
                if let Some(images) = cell.attrs().images() {
                    for tile in images.iter() {
                        let data = tile.image_data();
                        let guard = data.data();
                        live.insert(match &*guard {
                            ImageDataType::Rgba8 { hash, .. } => *hash,
                            ImageDataType::AnimRgba8 { hashes, .. } if !hashes.is_empty() => {
                                hashes[0]
                            }
                            _ => data.hash(),
                        });
                    }
                }
            }
        }
        self.images.retain(|key, _| live.contains(key));
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

        // Cell backgrounds, then image/text layers in protocol z order.
        for (y, line) in frame.rows.iter().take(self.metrics.rows).enumerate() {
            for cell in line
                .visible_cells()
                .take_while(|cell| cell.cell_index() < self.metrics.cols)
            {
                let (_, bg) = colors(cell.attrs(), &frame.palette);
                if bg != background {
                    brush.set_color(color(bg));
                    ctx.fill_rect(
                        &Rect::from_xywh(
                            PADDING + cell.cell_index() as f32 * cw,
                            PADDING + y as f32 * ch,
                            cw * cell.width() as f32,
                            ch,
                        ),
                        &brush,
                    );
                }
            }
        }
        self.draw_images(&native, frame, true)?;
        for (y, line) in frame.rows.iter().take(self.metrics.rows).enumerate() {
            let cells: Vec<_> = line
                .visible_cells()
                .take_while(|cell| cell.cell_index() < self.metrics.cols)
                .collect();
            let mut shaped_until = 0;
            for (index, cell) in cells.iter().enumerate() {
                let attrs = cell.attrs();
                let text = cell.str();
                let x = PADDING + cell.cell_index() as f32 * cw;
                let y = PADDING + y as f32 * ch;
                let (fg, _) = colors(attrs, &frame.palette);
                brush.set_color(color(fg));
                if cell.cell_index() >= shaped_until && !text.is_empty() && !attrs.invisible() {
                    if self.ligatures
                        && cell.width() == 1
                        && text.len() == 1
                        && matches!(text.as_bytes()[0], b' '..=b'~')
                    {
                        let mut run = String::new();
                        for next in &cells[index..] {
                            if next.cell_index() != cell.cell_index() + run.len()
                                || next.width() != 1
                                || next.attrs() != attrs
                                || next.str().len() != 1
                                || !matches!(next.str().as_bytes()[0], b' '..=b'~')
                            {
                                break;
                            }
                            run.push_str(next.str());
                        }
                        shaped_until = cell.cell_index() + run.len();
                        let run = run.trim_end_matches(' ');
                        self.text(
                            &native,
                            TextRun {
                                text: run,
                                bold: attrs.intensity() == Intensity::Bold,
                                x,
                                y,
                                cols: run.len(),
                                fg,
                                shape: true,
                            },
                        )?;
                    } else {
                        self.text(
                            &native,
                            TextRun {
                                text,
                                bold: attrs.intensity() == Intensity::Bold,
                                x,
                                y,
                                cols: cell.width().max(1),
                                fg,
                                shape: false,
                            },
                        )?;
                    }
                }
                if attrs.underline() != Underline::None {
                    let underline = if attrs.underline_color()
                        == wezterm_term::color::ColorAttribute::Default
                    {
                        fg
                    } else {
                        rgba(frame.palette.resolve_fg(attrs.underline_color()))
                    };
                    brush.set_color(color(underline));
                    ctx.fill_rect(
                        &Rect::from_xywh(x, y + ch - 3.0, cw * cell.width() as f32, 1.0),
                        &brush,
                    );
                    if attrs.underline() == Underline::Double {
                        ctx.fill_rect(
                            &Rect::from_xywh(x, y + ch - 5.0, cw * cell.width() as f32, 1.0),
                            &brush,
                        );
                    }
                }
                if attrs.strikethrough() {
                    ctx.fill_rect(
                        &Rect::from_xywh(x, y + ch * 0.5, cw * cell.width() as f32, 1.0),
                        &brush,
                    );
                }
            }
        }
        self.draw_images(&native, frame, false)?;
        if frame.cursor_visible
            && frame.cursor.x < self.metrics.cols
            && frame.cursor.y >= 0
            && (frame.cursor.y as usize) < self.metrics.rows
        {
            let x = PADDING + frame.cursor.x as f32 * cw;
            let y = PADDING + frame.cursor.y as f32 * ch;
            brush.set_color(color(rgba(frame.palette.cursor_bg)));
            use termwiz::surface::CursorShape;
            match frame.cursor.shape {
                CursorShape::SteadyBar | CursorShape::BlinkingBar => {
                    ctx.fill_rect(&Rect::from_xywh(x, y, 2.0, ch), &brush)
                }
                CursorShape::SteadyUnderline | CursorShape::BlinkingUnderline => {
                    ctx.fill_rect(&Rect::from_xywh(x, y + ch - 2.0, cw, 2.0), &brush)
                }
                _ => ctx.draw_rect(&Rect::from_xywh(x, y, cw, ch), &brush, 1.0),
            }
        }
        brush.set_color(ColorF::new(0.09, 0.10, 0.12, 1.0));
        ctx.fill_rect(
            &Rect::from_xywh(0.0, height - STATUS_HEIGHT, width, STATUS_HEIGHT),
            &brush,
        );
        brush.set_color(ColorF::new(0.59, 0.64, 0.71, 1.0));
        let label = format!(
            "{}   ·   {} × {}   ·   {} KiB{}",
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
        below_text: bool,
    ) -> windows_canvas::Result<()> {
        let mut tiles = Vec::new();
        for (row, line) in frame.rows.iter().take(self.metrics.rows).enumerate() {
            for cell in line
                .visible_cells()
                .take_while(|c| c.cell_index() < self.metrics.cols)
            {
                let Some(images) = cell.attrs().images() else {
                    continue;
                };
                for tile in images
                    .iter()
                    .filter(|tile| (tile.z_index() < 0) == below_text)
                {
                    tiles.push((row, cell.cell_index(), tile.clone()));
                }
            }
        }
        // Sort all placements in each layer, including overlapping placements
        // anchored in different cells. Stable ties preserve model order.
        tiles.sort_by_key(|(_, _, tile)| tile.z_index());
        for (row, column, tile) in tiles {
            let data = tile.image_data();
            let guard = data.data();
            let key = match &*guard {
                ImageDataType::Rgba8 { hash, .. } => *hash,
                ImageDataType::AnimRgba8 { hashes, .. } if !hashes.is_empty() => hashes[0],
                _ => data.hash(),
            };
            if !self.images.contains_key(&key) {
                let decoded;
                let (pixels, width, height) = match &*guard {
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
                        let Ok(mut reader) = image::ImageReader::new(std::io::Cursor::new(bytes))
                            .with_guessed_format()
                        else {
                            continue;
                        };
                        let mut limits = image::Limits::default();
                        limits.max_image_width = Some(4096);
                        limits.max_image_height = Some(4096);
                        limits.max_alloc = Some(32 * 1024 * 1024);
                        reader.limits(limits);
                        let Ok(image) = reader.decode() else {
                            continue;
                        };
                        decoded = image.to_rgba8();
                        (
                            decoded.as_raw().as_slice(),
                            decoded.width(),
                            decoded.height(),
                        )
                    }
                    _ => continue,
                };
                let Some(bytes) = (width as usize)
                    .checked_mul(height as usize)
                    .and_then(|n| n.checked_mul(4))
                else {
                    continue;
                };
                // Bound this renderer's GPU cache. The engine owns its own image storage.
                if bytes > 32 * 1024 * 1024 {
                    continue;
                }
                if self.tile_bytes + bytes > 32 * 1024 * 1024 || self.images.len() >= 512 {
                    self.images.clear();
                    self.tile_bytes = 0;
                }
                self.images.insert(
                    key,
                    CachedImage {
                        bitmap: native.image(pixels, width, height)?,
                        width,
                        height,
                    },
                );
                self.tile_bytes += bytes;
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
            let cw = self.metrics.cell_width;
            let ch = self.metrics.cell_height;
            let dest = Rect::from_xywh(
                PADDING + column as f32 * cw + pl as f32 / self.metrics.scale,
                PADDING + row as f32 * ch + pt as f32 / self.metrics.scale,
                (cw - (pl as f32 + pr as f32) / self.metrics.scale).max(0.0),
                (ch - (pt as f32 + pb as f32) / self.metrics.scale).max(0.0),
            );
            native.draw_image(&cached.bitmap, Rect::new(left, top, right, bottom), dest);
        }
        Ok(())
    }
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
        appearance.font_size,
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
    let grid = image::imageops::crop_imm(&image, pad, pad, grid_width, grid_height).to_image();
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("screenshot path has no parent"))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    image::DynamicImage::ImageRgba8(grid).write_to(file.as_file_mut(), image::ImageFormat::Png)?;
    file.persist(path)?;
    Ok((grid_width, grid_height))
}

#[cfg(test)]
mod tests {
    use super::*;
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
