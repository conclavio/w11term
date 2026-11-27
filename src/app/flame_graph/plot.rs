use super::compare::ProfileSide;
use crate::native_drawing::NativeDrawing;
use anyhow::{Context, Result};
use std::{
    cell::RefCell,
    io::{BufWriter, Write},
    path::Path,
    rc::Rc,
    sync::Arc,
};
use w11term::cpu_profile::{Comparison, Frame, Profile, Search, sample_share as share};
use windows_canvas::{
    Canvas, ColorF, DrawingSession, Invalidator, ParagraphAlignment, Rect, TextFormat, WordWrapping,
};
use windows_reactor::View;

pub(super) const ROW_HEIGHT: f32 = 23.0;
const MIN_WIDTH: f32 = 1.0;
const BACKGROUND: [u8; 3] = [26, 26, 28];
const HIGHLIGHT: [u8; 3] = [237, 163, 247];
const NEUTRAL: [u8; 3] = [185, 181, 194];
const INCREASE: [u8; 3] = [255, 111, 99];
const DECREASE: [u8; 3] = [108, 171, 255];
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
    differential: Option<Differential>,
    width: f32,
    last_hover: Option<usize>,
}

#[derive(Clone)]
pub(super) struct Differential {
    pub comparison: Arc<Comparison>,
    pub side: ProfileSide,
}

impl Differential {
    fn color(&self, id: usize) -> [u8; 3] {
        let comparison = &self.comparison;
        let Some(change) = comparison.change_at_frame(self.side.index(), id) else {
            return NEUTRAL;
        };
        let delta = comparison.delta_points(change);
        let peak = comparison.delta_points(&comparison.changes()[0]).abs();
        let strength = 0.35 + 0.65 * (delta.abs() / peak).min(1.0);
        let endpoint = if delta > 0.0 { INCREASE } else { DECREASE };
        std::array::from_fn(|channel| {
            let base = f64::from(NEUTRAL[channel]);
            (base + (f64::from(endpoint[channel]) - base) * strength).round() as u8
        })
    }

    pub fn detail(&self, id: usize) -> String {
        let comparison = &self.comparison;
        let Some(change) = comparison.change_at_frame(self.side.index(), id) else {
            return "Exact-stack sample share unchanged".into();
        };
        let [before, after] = change.counts();
        format!(
            "Exact stack · Before {before} · After {after} · {}",
            comparison.delta(change)
        )
    }
}

pub(super) struct Plot {
    scene: Rc<RefCell<Scene>>,
    invalidator: Invalidator,
    canvas: View,
}

impl Plot {
    pub fn new(
        profile: Arc<Profile>,
        search: Arc<Search>,
        differential: Option<Differential>,
    ) -> Self {
        let scene = Rc::new(RefCell::new(Scene {
            profile,
            range: Range::default(),
            search,
            differential,
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
            let matched = scene.search.contains(id);
            let rgb = scene
                .differential
                .as_ref()
                .map_or_else(|| color(frame, matched), |diff| diff.color(id));
            brush.set_color(colorf(rgb));
            ctx.fill_rect(&bounds, &brush);
            if matched && scene.differential.is_some() && width >= 3.0 {
                brush.set_color(colorf([255, 255, 255]));
                ctx.draw_rect(
                    &Rect::from_xywh(left + 0.5, y + 0.5, width - 1.6, ROW_HEIGHT - 2.0),
                    &brush,
                    1.0,
                );
            }
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

pub(super) fn export(
    profile: &Profile,
    zoom: usize,
    path: &Path,
    differential: Option<&Differential>,
) -> Result<()> {
    let directory = path.parent().context("Choose an SVG destination")?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    {
        let mut writer = BufWriter::new(file.as_file_mut());
        write_svg(profile, zoom, differential, &mut writer)?;
        writer.flush()?;
    }
    file.as_file().sync_all()?;
    file.persist_noclobber(path)
        .map_err(|error| error.error)
        .context("Saving the flame graph SVG")?;
    Ok(())
}

fn write_svg(
    profile: &Profile,
    zoom: usize,
    differential: Option<&Differential>,
    mut writer: impl Write,
) -> Result<()> {
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
    let title = if differential.is_some() {
        "Differential CPU flame graph"
    } else {
        "CPU flame graph"
    };
    writeln!(writer, "<title>{title} — {}</title>", xml(&root.name))?;
    if let Some(diff) = differential {
        writeln!(
            writer,
            "<desc>{}. Red increased, blue decreased, gray unchanged. Colors show change in exact-stack sample share.</desc>",
            diff.side.widths()
        )?;
    }
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
            let [r, g, b] = differential.map_or_else(|| color(frame, false), |diff| diff.color(id));
            let name = xml(&frame.name);
            let detail = differential
                .map_or_else(String::new, |diff| format!(" · {}", xml(&diff.detail(id))));
            writeln!(
                writer,
                r#"<g><title>{name} — {} samples ({:.1}% of capture){detail}</title><rect x="{left:.2}" y="{y}" width="{:.2}" height="{}" fill="rgb({r},{g},{b})"/>"#,
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
        write_svg(&profile, main, None, &mut svg).unwrap();
        let svg = String::from_utf8(svg).unwrap();
        assert!(svg.contains("hot — 70 samples (70.0% of capture)"));
        assert!(!svg.contains("wait"));
        assert_eq!(xml("Func<A&B>"), "Func&lt;A&amp;B&gt;");
    }

    #[test]
    fn differential_colors_are_normalized_and_keep_the_same_direction_on_both_sides() {
        let before = Arc::new(Profile::parse("root;hot 4\nroot;removed 2\nroot;steady 2").unwrap());
        let after = Arc::new(Profile::parse("root;hot 4\nroot;added 8\nroot;steady 4").unwrap());
        let comparison = Arc::new(Comparison::new([before, after], Default::default()));
        let color = |side: ProfileSide, symbol: &str| {
            let profile = &comparison.profiles()[side.index()];
            let id = profile
                .row(0, 2)
                .iter()
                .copied()
                .find(|&id| profile.frame(id).name == symbol)
                .unwrap();
            Differential {
                comparison: comparison.clone(),
                side,
            }
            .color(id)
        };
        assert_eq!(color(ProfileSide::After, "added"), INCREASE);
        assert_eq!(
            color(ProfileSide::Before, "hot"),
            color(ProfileSide::After, "hot")
        );
        assert_ne!(color(ProfileSide::Before, "removed"), NEUTRAL);
        assert_eq!(color(ProfileSide::Before, "steady"), NEUTRAL);
        assert_eq!(color(ProfileSide::After, "steady"), NEUTRAL);
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
            differential: None,
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

        let after = Arc::new(Profile::parse("process;main;hot 25\nprocess;wait 75").unwrap());
        let search = Arc::new(after.search("hot"));
        let differential = Scene {
            profile: after.clone(),
            differential: Some(Differential {
                comparison: Arc::new(Comparison::new(
                    [scene.profile.clone(), after],
                    Default::default(),
                )),
                side: ProfileSide::After,
            }),
            search,
            ..scene
        };
        target
            .draw(|ctx| {
                NativeDrawing::new(ctx)?.set_dpi(144.0);
                draw(ctx, &differential, &format)
            })
            .unwrap();
        let pixels = target.read_pixels().unwrap();
        let rgb = |x, y| {
            let i = ((y * width + x) * 4) as usize;
            [pixels[i + 2], pixels[i + 1], pixels[i]]
        };
        assert_eq!(rgb(65, 8), DECREASE, "search keeps the differential fill");
        assert_eq!(rgb(280, 42), INCREASE);
    }
}
