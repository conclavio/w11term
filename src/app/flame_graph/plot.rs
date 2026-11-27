use crate::native_drawing::NativeDrawing;
use anyhow::{Context, Result};
use std::{
    cell::RefCell,
    io::{BufWriter, Write},
    path::Path,
    rc::Rc,
    sync::Arc,
};
use w11term::cpu_profile::{Frame, Profile, Search, sample_share as share};
use windows_canvas::{
    Canvas, ColorF, DrawingSession, Invalidator, ParagraphAlignment, Rect, TextFormat, WordWrapping,
};
use windows_reactor::View;

pub(super) const ROW_HEIGHT: f32 = 23.0;
const MIN_WIDTH: f32 = 1.0;
const BACKGROUND: [u8; 3] = [26, 26, 28];
const HIGHLIGHT: [u8; 3] = [237, 163, 247];
const PALETTE: [[u8; 3]; 8] = [
    [251, 192, 106],
    [239, 136, 91],
    [218, 115, 105],
    [254, 214, 136],
    [229, 165, 94],
    [218, 129, 117],
    [247, 167, 114],
    [226, 147, 114],
];

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct Range {
    pub zoom: usize,
    pub first_depth: usize,
    pub rows: usize,
}

impl Range {
    pub fn height(self) -> f32 {
        ROW_HEIGHT * self.rows as f32
    }

    fn y(self, row: usize) -> f32 {
        ROW_HEIGHT * (self.rows - 1 - row) as f32
    }
}

struct Scene {
    profile: Arc<Profile>,
    range: Range,
    search: Arc<Search>,
    width: f32,
    last_hover: Option<usize>,
}

pub(super) struct Plot {
    scene: Rc<RefCell<Scene>>,
    invalidator: Invalidator,
    canvas: View,
}

impl Plot {
    pub fn new(profile: Arc<Profile>, search: Arc<Search>) -> Self {
        let scene = Rc::new(RefCell::new(Scene {
            profile,
            range: Range::default(),
            search,
            width: 0.0,
            last_hover: None,
        }));
        let painted = scene.clone();
        let invalidator = Invalidator::default();
        let format = RefCell::new(None);
        let canvas = Canvas::invalidated(&invalidator, move |ctx| {
            let mut scene = painted.borrow_mut();
            scene.width = ctx.width;
            let mut format = format.borrow_mut();
            let format = match &*format {
                Some(format) => format,
                None => format.insert(
                    TextFormat::new("Segoe UI", 12.0)?
                        .with_paragraph_alignment(ParagraphAlignment::Center)
                        .with_word_wrapping(WordWrapping::NoWrap),
                ),
            };
            draw(ctx, &scene, format)
        })
        .on_error(|error| log::warn!("Drawing CPU flame graph: {error:?}"))
        .into();
        Self {
            scene,
            invalidator,
            canvas,
        }
    }

    pub fn canvas(&self, range: Range, search: &Arc<Search>) -> View {
        let mut scene = self.scene.borrow_mut();
        if range != scene.range || !Arc::ptr_eq(search, &scene.search) {
            scene.range = range;
            scene.search = search.clone();
            self.invalidator.invalidate();
        }
        self.canvas.clone()
    }

    pub fn pointer(&self) -> impl Fn(f64, f64, bool) -> Option<Option<usize>> + use<> {
        let scene = self.scene.clone();
        move |x, y, hover| {
            let mut scene = scene.borrow_mut();
            let id = hit(&scene.profile, scene.range, scene.width, x as f32, y as f32);
            if hover && std::mem::replace(&mut scene.last_hover, id) == id {
                return None;
            }
            Some(id)
        }
    }
}

fn hit(profile: &Profile, range: Range, width: f32, x: f32, y: f32) -> Option<usize> {
    if !(0.0..width).contains(&x) || !(0.0..range.height()).contains(&y) {
        return None;
    }
    let row = range.rows - 1 - (y / ROW_HEIGHT) as usize;
    if y - range.y(row) >= ROW_HEIGHT - 1.0 {
        return None;
    }
    let root = profile.frame(range.zoom);
    let offset = ((f64::from(x) / f64::from(width)) * root.samples as f64) as u64;
    let index = root.start + offset.min(root.samples - 1);
    let frames = profile.row(range.zoom, range.first_depth + row);
    let id = *frames.get(
        frames
            .partition_point(|&id| profile.frame(id).start <= index)
            .checked_sub(1)?,
    )?;
    let frame = profile.frame(id);
    let (left, right) = extent(frame, root, width);
    (right - left >= MIN_WIDTH && (left..right).contains(&x)).then_some(id)
}

fn extent(frame: &Frame, root: &Frame, width: f32) -> (f32, f32) {
    let x = |sample| ((sample - root.start) as f64 * f64::from(width) / root.samples as f64) as f32;
    (x(frame.start), x(frame.end()))
}

fn color(frame: &Frame, matched: bool) -> [u8; 3] {
    if matched {
        return HIGHLIGHT;
    }
    if frame.name.starts_with("[unknown") {
        return [177, 173, 184];
    }
    let hash = frame.name.bytes().fold(0_usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(usize::from(byte))
    });
    PALETTE[(hash ^ (hash >> 8)) % PALETTE.len()]
}

fn colorf(channel: [u8; 3]) -> ColorF {
    let [r, g, b] = channel.map(|value| f32::from(value) / 255.0);
    ColorF::new(r, g, b, 1.0)
}

fn draw(
    ctx: &DrawingSession<'_>,
    scene: &Scene,
    format: &TextFormat,
) -> windows_canvas::Result<()> {
    ctx.clear(colorf(BACKGROUND));
    let native = NativeDrawing::new(ctx)?;
    let brush = ctx.create_solid_brush(ColorF::WHITE)?;
    let range = scene.range;
    let root = scene.profile.frame(range.zoom);
    for row in 0..range.rows {
        let y = range.y(row);
        for &id in scene.profile.row(range.zoom, range.first_depth + row) {
            let frame = scene.profile.frame(id);
            let (left, right) = extent(frame, root, scene.width);
            let width = right - left;
            if width < MIN_WIDTH {
                continue;
            }
            let bounds = Rect::from_xywh(left, y, (width - 0.6).max(0.5), ROW_HEIGHT - 1.0);
            brush.set_color(colorf(color(frame, scene.search.contains(id))));
            ctx.fill_rect(&bounds, &brush);
            if width >= 24.0 {
                let text = Rect::from_xywh(left + 4.0, y, width - 8.0, ROW_HEIGHT - 1.0);
                brush.set_color(colorf([26, 20, 16]));
                let end = frame
                    .name
                    .char_indices()
                    .nth((width as usize / 3).max(8))
                    .map_or(frame.name.len(), |(index, _)| index);
                native.with_clip(text, || {
                    ctx.draw_text(&frame.name[..end], format, &text, &brush)
                });
            }
        }
    }
    Ok(())
}

pub(super) fn export(profile: &Profile, zoom: usize, path: &Path) -> Result<()> {
    let directory = path.parent().context("Choose an SVG destination")?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    {
        let mut writer = BufWriter::new(file.as_file_mut());
        write_svg(profile, zoom, &mut writer)?;
        writer.flush()?;
    }
    file.as_file().sync_all()?;
    file.persist_noclobber(path)
        .map_err(|error| error.error)
        .context("Saving the flame graph SVG")?;
    Ok(())
}

fn write_svg(profile: &Profile, zoom: usize, mut writer: impl Write) -> Result<()> {
    const WIDTH: f32 = 1200.0;
    let root = profile.frame(zoom);
    let range = Range {
        zoom,
        first_depth: 0,
        rows: root.last_depth - root.depth + 1,
    };
    let height = range.height();
    writeln!(
        writer,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{height}" viewBox="0 0 {WIDTH} {height}" role="img">"#
    )?;
    writeln!(
        writer,
        "<title>CPU flame graph — {}</title>",
        xml(&root.name)
    )?;
    writeln!(
        writer,
        r##"<rect width="100%" height="100%" fill="#1a1a1c"/><g font-family="Segoe UI,sans-serif" font-size="12" fill="#1a1410">"##
    )?;
    for row in 0..range.rows {
        let y = range.y(row);
        for &id in profile.row(zoom, row) {
            let frame = profile.frame(id);
            let (left, right) = extent(frame, root, WIDTH);
            let width = right - left;
            if width < MIN_WIDTH {
                continue;
            }
            let [r, g, b] = color(frame, false);
            let name = xml(&frame.name);
            writeln!(
                writer,
                r#"<g><title>{name} — {} samples ({:.1}% of capture)</title><rect x="{left:.2}" y="{y}" width="{:.2}" height="{}" fill="rgb({r},{g},{b})"/>"#,
                frame.samples,
                share(frame.samples, profile.frame(0).samples),
                width - 0.6,
                ROW_HEIGHT - 1.0
            )?;
            if width >= 24.0 {
                writeln!(
                    writer,
                    r#"<svg x="{:.2}" y="{y}" width="{:.2}" height="{}" overflow="hidden"><text x="0" y="15">{name}</text></svg>"#,
                    left + 4.0,
                    width - 8.0,
                    ROW_HEIGHT - 1.0
                )?;
            }
            writeln!(writer, "</g>")?;
        }
    }
    writeln!(writer, "</g></svg>")?;
    Ok(())
}

fn xml(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '&' => escaped.push_str("&amp;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_testing_uses_zoom_weights_depth_and_visible_self_sample_gaps() {
        use std::io::Write as _;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            file,
            "process;main;hot 70\nprocess;main 20\nprocess;wait 10"
        )
        .unwrap();
        let profile = Profile::open(file.path()).unwrap();
        let main = profile.row(0, 2)[0];
        let hot = profile.row(main, 1)[0];
        let range = Range {
            zoom: main,
            first_depth: 0,
            rows: 2,
        };
        assert_eq!(hit(&profile, range, 900.0, 600.0, 4.0), Some(hot));
        assert_eq!(hit(&profile, range, 900.0, 850.0, 4.0), None);
        assert_eq!(hit(&profile, range, 900.0, 850.0, 24.0), Some(main));
        assert_eq!(hit(&profile, range, 900.0, 850.0, 22.5), None);
        let mut svg = Vec::new();
        write_svg(&profile, main, &mut svg).unwrap();
        let svg = String::from_utf8(svg).unwrap();
        assert!(svg.contains("hot — 70 samples (70.0% of capture)"));
        assert!(!svg.contains("wait"));
        assert_eq!(xml("Func<A&B>"), "Func&lt;A&amp;B&gt;");
    }

    #[test]
    fn paints_weighted_and_highlighted_frames_at_150_percent_dpi() {
        use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
        struct Apartment;
        impl Drop for Apartment {
            fn drop(&mut self) {
                unsafe { CoUninitialize() };
            }
        }
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() }.unwrap();
        let _apartment = Apartment;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "process;main;hot 75\nprocess;wait 25").unwrap();
        let profile = Arc::new(Profile::open(file.path()).unwrap());
        let search = Arc::new(profile.search("hot"));
        let scene = Scene {
            profile,
            search,
            range: Range {
                zoom: 0,
                first_depth: 0,
                rows: 4,
            },
            width: 200.0,
            last_hover: None,
        };
        let width = 300;
        let height = 138;
        let device = windows_canvas::GpuDevice::new_or_warp().unwrap();
        let target = device.create_render_target(width, height).unwrap();
        let format = TextFormat::new("Segoe UI", 12.0).unwrap();
        target
            .draw(|ctx| {
                NativeDrawing::new(ctx)?.set_dpi(144.0);
                draw(ctx, &scene, &format)
            })
            .unwrap();
        let pixels = target.read_pixels().unwrap();
        let rgb = |x, y| {
            let i = ((y * width + x) * 4) as usize;
            [pixels[i + 2], pixels[i + 1], pixels[i]]
        };
        assert_eq!(rgb(220, 8), HIGHLIGHT);
        assert_eq!(rgb(230, 8), BACKGROUND);
        assert_ne!(
            rgb(230, 42),
            BACKGROUND,
            "the other branch is on the caller row"
        );
    }
}
