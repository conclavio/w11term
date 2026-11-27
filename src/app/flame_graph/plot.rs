use crate::native_drawing::NativeDrawing;
use anyhow::Result;
#[cfg(test)]
use std::io::Write as _;
use std::{cell::RefCell, path::Path, rc::Rc, sync::Arc};
#[cfg(test)]
use w11term::cpu_profile::{Comparison, flame_graph::ProfileSide};
use w11term::cpu_profile::{
    Profile, Search,
    flame_graph::{
        BACKGROUND, Differential, MIN_WIDTH, ROW_HEIGHT, Range, color, extent, write_svg,
    },
};
use windows_canvas::{
    Canvas, ColorF, DrawingSession, Invalidator, ParagraphAlignment, Rect, TextFormat, WordWrapping,
};
use windows_reactor::View;

struct Scene {
    profile: Arc<Profile>,
    range: Range,
    search: Arc<Search>,
    differential: Option<Differential>,
    width: f32,
    last_hover: Option<usize>,
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
    w11term::transcript::save_with(path, false, |output| match differential {
        Some(differential) => differential.write_svg(zoom, output),
        None => write_svg(profile, zoom, output),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_testing_uses_zoom_weights_depth_and_visible_self_sample_gaps() {
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
        assert_eq!(rgb(220, 8), [237, 163, 247]);
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
        assert_eq!(
            rgb(65, 8),
            [108, 171, 255],
            "search keeps the differential fill"
        );
        assert_eq!(rgb(280, 42), [255, 111, 99]);
    }
}
