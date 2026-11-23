use crate::renderer::{Metrics, PADDING, STATUS_HEIGHT};
use std::{
    cell::{Cell, RefCell},
    ops::Range,
    sync::Arc,
};
use w11term::search::SearchSnapshot;
use w11term::terminal::{ScrollbackView, Snapshot};
use wezterm_term::MouseEventKind;
use windows_canvas::{Brush, ColorF, DrawContext, DrawingSession, Rect};
use windows_reactor::PointerEventInfo;

const FAILURE_MARK_WIDTH: f32 = 3.0;

#[derive(Clone, Copy)]
struct Rail {
    view: ScrollbackView,
    width: f32,
    height: f32,
    scale: f32,
}

impl Rail {
    fn new(view: ScrollbackView, width: f32, height: f32, scale: f32) -> Option<Self> {
        let height = height - STATUS_HEIGHT - 2.0 * PADDING;
        (width > 2.0 * PADDING
            && height >= 4.0
            && width.is_finite()
            && height.is_finite()
            && scale.is_finite()
            && scale > 0.0)
            .then_some(Self {
                view,
                width,
                height,
                scale,
            })
    }

    fn compatible(self, other: Self) -> bool {
        self.view.same_screen(&other.view)
            && self.width == other.width
            && self.height == other.height
            && self.scale == other.scale
    }

    fn hit(self, x: f32, y: f32) -> bool {
        (self.width - PADDING..self.width).contains(&x)
            && (PADDING..PADDING + self.height).contains(&y)
    }

    fn failure_lane(self) -> Range<f32> {
        (self.width - PADDING)..(self.width - PADDING + FAILURE_MARK_WIDTH)
    }

    fn thumb(self) -> (f32, f32) {
        let history = (self.view.live - self.view.oldest) as f64;
        let rows = self.view.rows as f64;
        let height = (f64::from(self.height) * rows / (history + rows)) as f32;
        let height = height.max(20.0_f32.min(self.height * 0.5));
        let fraction = (self.view.top - self.view.oldest) as f64 / history;
        (PADDING + (self.height - height) * fraction as f32, height)
    }

    fn page(self, up: bool) -> Option<isize> {
        let count = self.view.rows as isize;
        let row = if up {
            self.view.top.saturating_sub(count).max(self.view.oldest)
        } else {
            self.view.top.saturating_add(count)
        };
        (row < self.view.live).then_some(row)
    }

    fn drag_target(self, y: f32, grab: f32) -> Option<isize> {
        let (_, height) = self.thumb();
        let fraction = ((y - PADDING - grab) / (self.height - height)).clamp(0.0, 1.0);
        let row = self.view.oldest
            + (f64::from(fraction) * (self.view.live - self.view.oldest) as f64).round() as isize;
        (row < self.view.live).then_some(row)
    }
}

/// Project a stable row onto the full retained buffer in physical pixels.
/// The viewport top moves the thumb independently of the result marks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MarkScale {
    oldest: isize,
    last: isize,
    pixels: u32,
    thickness: u32,
}

impl MarkScale {
    fn new(rail: Rail) -> Self {
        let top = (PADDING * rail.scale).round() as u32;
        let end = ((PADDING + rail.height) * rail.scale).round() as u32;
        let pixels = end.saturating_sub(top).max(1);
        Self {
            oldest: rail.view.oldest,
            last: rail
                .view
                .live
                .saturating_add_unsigned(rail.view.rows.saturating_sub(1)),
            pixels,
            thickness: ((2.0 * rail.scale).round() as u32).clamp(1, pixels),
        }
    }

    fn band(self, row: isize, thickness: u32) -> Option<Range<u32>> {
        if !(self.oldest..=self.last).contains(&row) {
            return None;
        }
        let fraction =
            self.oldest.abs_diff(row) as f64 / self.oldest.abs_diff(self.last).max(1) as f64;
        let center = (fraction * f64::from(self.pixels - 1)).round() as u32;
        let start = center
            .saturating_sub(thickness / 2)
            .min(self.pixels - thickness);
        Some(start..start + thickness)
    }
}

struct MarkBands {
    rows: Arc<[isize]>,
    scale: MarkScale,
    bands: Vec<Range<u32>>,
}

impl MarkBands {
    fn new(rows: &Arc<[isize]>, scale: MarkScale) -> Self {
        let mut bands: Vec<Range<u32>> = Vec::new();
        for band in rows
            .iter()
            .filter_map(|&row| scale.band(row, scale.thickness))
        {
            if let Some(last) = bands.last_mut()
                && band.start <= last.end
            {
                last.end = last.end.max(band.end);
            } else {
                bands.push(band);
            }
        }
        Self {
            rows: rows.clone(),
            scale,
            bands,
        }
    }

    fn project<'a>(
        cache: &'a mut Option<Self>,
        rows: &Arc<[isize]>,
        scale: MarkScale,
    ) -> &'a [Range<u32>] {
        if !cache
            .as_ref()
            .is_some_and(|old| old.scale == scale && Arc::ptr_eq(&old.rows, rows))
        {
            *cache = Some(Self::new(rows, scale));
        }
        &cache.as_ref().unwrap().bands
    }
}

#[derive(Clone, Copy)]
struct Drag {
    // Use the stable-row interval at press, even when new output arrives mid-drag.
    rail: Rail,
    grab: f32,
    last_target: Option<isize>,
}

#[derive(Default)]
pub(super) struct ScrollBar {
    enabled: Cell<bool>,
    find_open: Cell<bool>,
    show_failures: Cell<bool>,
    failure_rows: RefCell<Option<Arc<[isize]>>>,
    marks: RefCell<Option<MarkBands>>,
    shown: Cell<Option<Rail>>,
    owns_press: Cell<bool>,
    drag: Cell<Option<Drag>>,
}

impl ScrollBar {
    pub(super) fn configure(&self, enabled: bool, find_open: bool, show_failures: bool) -> bool {
        let was_enabled = self.enabled.replace(enabled);
        let had_find = self.find_open.replace(find_open);
        let had_failures = self.show_failures.replace(show_failures);
        if !enabled || !show_failures {
            self.failure_rows.borrow_mut().take();
        }
        if !enabled {
            self.present(None);
        }
        if !enabled || !find_open {
            self.marks.borrow_mut().take();
        }
        was_enabled != enabled || had_find != find_open || had_failures != show_failures
    }

    pub(super) fn cancel(&self) {
        // A captured release can still arrive after resize or another capture loss.
        // Keep ownership of that release so it cannot act on the terminal underneath.
        self.drag.set(None);
    }

    fn present(&self, rail: Option<Rail>) {
        if let Some(drag) = self.drag.get()
            && !rail.is_some_and(|new| drag.rail.compatible(new))
        {
            self.cancel();
        }
        if rail.is_none() {
            self.failure_rows.borrow_mut().take();
        }
        self.shown.set(rail);
    }

    pub(super) fn draw(
        &self,
        ctx: &DrawContext<'_>,
        frame: &Snapshot,
        metrics: Metrics,
        brush: &mut Option<Brush>,
    ) -> windows_canvas::Result<()> {
        if ctx.device_changed() {
            *brush = None;
        }
        self.paint(ctx, frame, ctx.width, ctx.height, metrics, brush)
    }

    pub(super) fn paint(
        &self,
        ctx: &DrawingSession<'_>,
        frame: &Snapshot,
        width: f32,
        height: f32,
        metrics: Metrics,
        brush: &mut Option<Brush>,
    ) -> windows_canvas::Result<()> {
        let geometry_matches =
            self.enabled.get() && frame.cols == metrics.cols && frame.rows.len() == metrics.rows;
        let rail = if geometry_matches {
            frame
                .scrollback_view()
                .and_then(|view| Rail::new(view, width, height, metrics.scale))
        } else {
            None
        };
        self.present(rail);
        if let Some(rail) = rail {
            let brush = match brush {
                Some(brush) => brush,
                None => brush.insert(ctx.create_solid_brush(ColorF::WHITE)?),
            };
            let x = rail.width - PADDING + 3.0;
            let w = PADDING - 6.0;
            let (top, height) = rail.thumb();
            brush.set_color(ColorF::new(0.16, 0.18, 0.21, 1.0));
            ctx.fill_rect(&Rect::from_xywh(x, PADDING, w, rail.height), brush);
            brush.set_color(ColorF::new(0.56, 0.60, 0.65, 1.0));
            ctx.fill_rect(&Rect::from_xywh(x, top, w, height), brush);
            if self.show_failures.get() && !frame.failed_command_rows.is_empty() {
                self.paint_failures(ctx, rail, brush, &frame.failed_command_rows);
                *self.failure_rows.borrow_mut() = Some(frame.failed_command_rows.clone());
            } else {
                self.failure_rows.borrow_mut().take();
            }
            if let Some(search) = frame
                .search
                .as_ref()
                .filter(|s| self.find_open.get() && !s.match_rows.is_empty())
            {
                self.paint_marks(ctx, rail, brush, search);
            } else {
                self.marks.borrow_mut().take();
            }
        } else {
            self.marks.borrow_mut().take();
        }
        Ok(())
    }

    fn paint_failures(&self, ctx: &DrawingSession<'_>, rail: Rail, brush: &Brush, rows: &[isize]) {
        let scale = MarkScale::new(rail);
        let pixel_top = (PADDING * rail.scale).round();
        brush.set_color(ColorF::new(1.0, 104.0 / 255.0, 123.0 / 255.0, 1.0));
        for band in rows
            .iter()
            .filter_map(|&row| scale.band(row, scale.thickness))
        {
            ctx.fill_rect(
                &Rect::from_xywh(
                    rail.failure_lane().start,
                    (pixel_top + band.start as f32) / rail.scale,
                    FAILURE_MARK_WIDTH,
                    (band.end - band.start) as f32 / rail.scale,
                ),
                brush,
            );
        }
    }

    fn failed_command_at(&self, rail: Rail, x: f32, y: f32) -> Option<isize> {
        if !rail.failure_lane().contains(&x) {
            return None;
        }
        let rows = self.failure_rows.borrow();
        let scale = MarkScale::new(rail);
        let pixel = (y * rail.scale - (PADDING * rail.scale).round()).max(0.0);
        // The separate lane gives small marks a forgiving vertical target.
        // When distant rows share a pixel, pick the newer retained row.
        rows.as_ref()?
            .iter()
            .filter_map(|&row| {
                let band = scale.band(row, scale.thickness)?;
                let center = (band.start as f32 + band.end as f32) / 2.0;
                let distance = (pixel - center).abs();
                (distance <= 3.0_f32.max(scale.thickness as f32 / 2.0)).then_some((distance, row))
            })
            .min_by(|(a, first), (b, second)| a.total_cmp(b).then_with(|| second.cmp(first)))
            .map(|(_, row)| row)
    }

    fn paint_marks(
        &self,
        ctx: &DrawingSession<'_>,
        rail: Rail,
        brush: &Brush,
        search: &SearchSnapshot,
    ) {
        let scale = MarkScale::new(rail);
        let pixel_top = (PADDING * rail.scale).round();
        let fill = |band: &Range<u32>, width: f32| {
            let y = (pixel_top + band.start as f32) / rail.scale;
            ctx.fill_rect(
                &Rect::from_xywh(
                    rail.width - 2.0 - width,
                    y,
                    width,
                    (band.end - band.start) as f32 / rail.scale,
                ),
                brush,
            );
        };
        // Keep the left edge of the thumb visible even for dense results.
        brush.set_color(ColorF::new(1.0, 216.0 / 255.0, 76.0 / 255.0, 1.0));
        let mut cache = self.marks.borrow_mut();
        for band in MarkBands::project(&mut cache, &search.match_rows, scale) {
            fill(band, 3.0);
        }
        let selected_height = ((4.0 * rail.scale).round() as u32).clamp(1, scale.pixels);
        if let Some(band) = search
            .selected_row
            .and_then(|row| scale.band(row, selected_height))
        {
            brush.set_color(ColorF::new(1.0, 123.0 / 255.0, 41.0 / 255.0, 1.0));
            fill(&band, 7.0);
        }
    }

    pub(super) fn pointer(
        &self,
        pointer: PointerEventInfo,
        kind: MouseEventKind,
        current: impl FnOnce(Option<isize>) -> Option<ScrollbackView>,
        mut scroll: impl FnMut(ScrollbackView, Option<isize>),
    ) -> bool {
        let x = pointer.x as f32;
        let y = pointer.y as f32;
        let hit = self.shown.get().filter(|rail| rail.hit(x, y));
        if kind == MouseEventKind::Press {
            self.cancel();
            self.owns_press.set(hit.is_some());
            let Some(rail) = hit else { return false };
            let failed = self.failed_command_at(rail, x, y);
            if pointer.is_left_button_pressed
                && current(failed).is_some_and(|view| rail.view.same_screen(&view))
            {
                let (top, height) = rail.thumb();
                if let Some(row) = failed {
                    scroll(rail.view, Some(row));
                } else if (top..top + height).contains(&y) {
                    if pointer.capture_succeeded {
                        self.drag.set(Some(Drag {
                            rail,
                            grab: y - top,
                            last_target: (rail.view.top < rail.view.live).then_some(rail.view.top),
                        }));
                    }
                } else {
                    scroll(rail.view, rail.page(y < top));
                }
            }
            return true;
        }
        let any_button = pointer.is_left_button_pressed
            || pointer.is_middle_button_pressed
            || pointer.is_right_button_pressed;
        if self.owns_press.get() && (any_button || kind == MouseEventKind::Release) {
            if let Some(mut drag) = self.drag.take()
                && y.is_finite()
                && current(None).is_some_and(|view| drag.rail.view.same_screen(&view))
            {
                let target = drag.rail.drag_target(y, drag.grab);
                if target != drag.last_target {
                    scroll(drag.rail.view, target);
                    drag.last_target = target;
                }
                if kind != MouseEventKind::Release {
                    self.drag.set(Some(drag));
                }
            }
            if kind == MouseEventKind::Release {
                self.owns_press.set(false);
            }
            return true;
        }
        self.owns_press.set(false);
        self.cancel();
        kind == MouseEventKind::Move && !any_button && hit.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use w11term::terminal::create;
    use wezterm_term::{Terminal, TerminalSize};

    const WIDTH: f32 = 400.0;
    const HEIGHT: f32 = 250.0;

    fn terminal() -> Terminal {
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        create(
            TerminalSize {
                cols: 20,
                rows: 4,
                ..Default::default()
            },
            tx,
        )
    }

    fn frame(term: &Terminal, offset: usize) -> Snapshot {
        Snapshot::capture(term, None, 1, offset, String::new(), 0)
    }

    fn rail(term: &Terminal, offset: usize) -> Rail {
        Rail::new(
            frame(term, offset).scrollback_view().unwrap(),
            WIDTH,
            HEIGHT,
            1.0,
        )
        .unwrap()
    }

    fn point(x: f32, y: f32, left: bool) -> PointerEventInfo {
        PointerEventInfo {
            x: x.into(),
            y: y.into(),
            is_left_button_pressed: left,
            capture_succeeded: true,
            ..Default::default()
        }
    }

    fn click(bar: &ScrollBar, rail: Rail, y: f32) -> Vec<Option<isize>> {
        let mut sent = Vec::new();
        for kind in [MouseEventKind::Press, MouseEventKind::Release] {
            assert!(bar.pointer(
                point(WIDTH - 6.0, y, kind == MouseEventKind::Press),
                kind,
                |_| Some(rail.view),
                |_, target| sent.push(target)
            ));
        }
        sent
    }

    #[test]
    fn failure_hit_uses_its_own_lane_and_rechecks_the_current_frame() {
        let mut term = terminal();
        term.advance_bytes(b"x\r\n".repeat(200));
        let rail = rail(&term, 0);
        let row = rail.view.oldest + 40;
        let scale = MarkScale::new(rail);
        let band = scale.band(row, scale.thickness).unwrap();
        let y = PADDING + (band.start as f32 + band.end as f32) / 2.0 / rail.scale;
        let bar = ScrollBar::default();
        bar.configure(true, false, true);
        bar.present(Some(rail));
        *bar.failure_rows.borrow_mut() = Some(Arc::from([row]));
        let lane = rail.failure_lane();
        let x = (lane.start + lane.end) / 2.0;
        assert_eq!(bar.failed_command_at(rail, x, y), Some(row));
        assert_eq!(bar.failed_command_at(rail, WIDTH - 6.0, y), None);
        assert!(bar.pointer(
            point(x, y, true),
            MouseEventKind::Press,
            |target| {
                assert_eq!(target, Some(row));
                None // The output was erased after this mark was drawn.
            },
            |_, _| panic!("would scroll to a stale command")
        ));
        assert!(bar.configure(true, false, false));
        assert_eq!(bar.failed_command_at(rail, x, y), None);
    }

    #[test]
    fn marks_cover_every_retained_row_including_the_bottom_of_the_live_screen() {
        let mut term = terminal();
        term.advance_bytes(b"x\r\n".repeat(200));
        let mut rail = rail(&term, 0);
        rail.view.oldest = 100;
        rail.view.live = 297;
        rail.view.rows = 4;
        rail.height = 96.0;
        rail.scale = 1.25;
        let scale = MarkScale::new(rail);
        assert_eq!((scale.pixels, scale.thickness, scale.last), (120, 3, 300));
        assert_eq!(scale.band(100, 3), Some(0..3));
        assert_eq!(scale.band(200, 3), Some(59..62));
        assert_eq!(scale.band(200, 5), Some(58..63));
        assert_eq!(scale.band(300, 3), Some(117..120));
        assert_eq!(scale.band(301, 3), None);
        assert_eq!(scale.band(99, 3), None);

        rail.view.top = 175;
        rail.width += 20.0;
        assert_eq!(MarkScale::new(rail), scale);
        rail.scale = 1.5;
        assert_eq!(MarkScale::new(rail).pixels, 144);
        assert!(Rail::new(rail.view, WIDTH, HEIGHT, f32::NAN).is_none());
    }

    #[test]
    fn dense_marks_share_bands_and_invalidate_on_a_new_result_set_or_geometry() {
        let all: Arc<[isize]> = (0..10_000).collect();
        let sparse: Arc<[isize]> = Arc::from([-1, 0, 5_000, 9_999, 10_000]);
        let scale = MarkScale {
            oldest: 0,
            last: 9_999,
            pixels: 1_000,
            thickness: 2,
        };
        let mut cache = None;
        let [dense] = MarkBands::project(&mut cache, &all, scale) else {
            panic!("dense results should coalesce into a single band");
        };
        assert_eq!(*dense, 0..1_000);
        let first = cache.as_ref().unwrap().bands.as_ptr();
        // A repaint or selecting a different match in this result set shares the projection.
        assert_eq!(
            MarkBands::project(&mut cache, &all.clone(), scale).as_ptr(),
            first
        );
        assert_eq!(
            MarkBands::project(&mut cache, &sparse, scale),
            [0..2, 499..501, 998..1_000]
        );
        let shorter = MarkScale {
            pixels: 500,
            ..scale
        };
        assert_eq!(
            MarkBands::project(&mut cache, &sparse, shorter),
            [0..2, 249..251, 498..500]
        );
        let bar = ScrollBar::default();
        bar.configure(true, true, true);
        *bar.marks.borrow_mut() = cache;
        assert!(!bar.configure(true, true, true));
        assert!(bar.marks.borrow().is_some());
        assert!(bar.configure(true, false, true));
        assert!(bar.marks.borrow().is_none());
    }

    #[test]
    fn only_primary_history_has_a_rail_and_hit_testing_preserves_grid_and_status() {
        let mut term = terminal();
        assert!(frame(&term, 0).scrollback_view().is_none());
        term.advance_bytes(b"x\r\n".repeat(100));
        let rail = rail(&term, 20);
        assert!(rail.hit(WIDTH - PADDING, PADDING));
        assert!(!rail.hit(WIDTH - PADDING - 0.1, PADDING));
        assert!(!rail.hit(WIDTH, PADDING));
        assert!(!rail.hit(WIDTH - 6.0, PADDING - 0.1));
        assert!(!rail.hit(WIDTH - 6.0, HEIGHT - STATUS_HEIGHT - PADDING));
        assert_eq!(rail.thumb().1, 20.0);
        let tiny = Rail::new(rail.view, WIDTH, 2.0 * PADDING + STATUS_HEIGHT + 6.0, 1.5).unwrap();
        assert_eq!(tiny.thumb().1, 3.0);
        assert!(Rail::new(rail.view, WIDTH, 2.0 * PADDING + STATUS_HEIGHT, 1.5).is_none());
        term.advance_bytes(b"\x1b[?1049h");
        assert!(frame(&term, 0).scrollback_view().is_none());
    }

    #[test]
    fn track_pages_once_by_visible_rows_and_clamps_at_both_ends() {
        let bar = ScrollBar::default();
        let mut term = terminal();
        term.advance_bytes(b"x\r\n".repeat(80));
        let middle = rail(&term, 30);
        bar.present(Some(middle));
        assert_eq!(click(&bar, middle, PADDING), [Some(middle.view.top - 4)]);
        assert_eq!(
            click(&bar, middle, PADDING + middle.height - 0.1),
            [Some(middle.view.top + 4)]
        );
        let mut near_oldest = middle;
        near_oldest.view.top = near_oldest.view.oldest + 1;
        bar.present(Some(near_oldest));
        assert_eq!(
            click(&bar, near_oldest, PADDING),
            [Some(near_oldest.view.oldest)]
        );
        let mut near_live = middle;
        near_live.view.top = near_live.view.live - 1;
        bar.present(Some(near_live));
        assert_eq!(
            click(&bar, near_live, PADDING + middle.height - 0.1),
            [None]
        );
    }

    #[test]
    fn captured_drag_keeps_original_row_interval_across_output_and_rejoins_live() {
        let bar = ScrollBar::default();
        let mut term = terminal();
        term.advance_bytes(b"x\r\n".repeat(100));
        let first = rail(&term, usize::MAX);
        bar.present(Some(first));
        let grab = 8.0;
        assert!(bar.pointer(
            point(WIDTH - 6.0, PADDING + grab, true),
            MouseEventKind::Press,
            |_| Some(first.view),
            |_, _| panic!("thumb press scrolled")
        ));
        term.advance_bytes(b"new\r\n".repeat(100));
        let grown = rail(&term, usize::MAX);
        assert!(grown.view.live > first.view.live);
        bar.present(Some(grown));
        let halfway = PADDING + (first.height - first.thumb().1) * 0.5 + grab;
        let mut sent = Vec::new();
        for (kind, y) in [
            (MouseEventKind::Move, halfway),
            (MouseEventKind::Move, halfway),
            (MouseEventKind::Release, HEIGHT + 200.0),
        ] {
            assert!(bar.pointer(
                point(-80.0, y, kind == MouseEventKind::Move),
                kind,
                |_| Some(grown.view),
                |_, target| sent.push(target)
            ));
        }
        assert_eq!(
            sent,
            [
                Some(first.view.oldest + (first.view.live - first.view.oldest + 1) / 2),
                None
            ]
        );
        assert!(!bar.pointer(
            point(-80.0, halfway, true),
            MouseEventKind::Move,
            |_| panic!(),
            |_, _| panic!()
        ));
    }

    #[test]
    fn interrupted_gestures_eat_their_release_without_moving_the_view() {
        let mut term = terminal();
        term.advance_bytes(b"x\r\n".repeat(100));
        let oldest = rail(&term, usize::MAX);
        let bar = ScrollBar::default();
        for cause in [
            "capture failed",
            "capture lost",
            "resize",
            "reconnect",
            "hidden",
        ] {
            bar.present(Some(oldest));
            let mut press = point(WIDTH - 6.0, PADDING + 5.0, true);
            press.capture_succeeded = cause != "capture failed";
            assert!(bar.pointer(
                press,
                MouseEventKind::Press,
                |_| Some(oldest.view),
                |_, _| panic!()
            ));
            let mut current = Some(oldest.view);
            match cause {
                "capture lost" => bar.cancel(),
                "resize" => bar.present(Some(Rail {
                    width: WIDTH + 1.0,
                    ..oldest
                })),
                "reconnect" => {
                    // A new frame can be published before XAML repaints the old rail.
                    let mut reconnected = frame(&term, usize::MAX);
                    reconnected.connection_generation += 1;
                    current = reconnected.scrollback_view();
                }
                "hidden" => bar.present(None),
                _ => {}
            }
            for kind in [MouseEventKind::Move, MouseEventKind::Release] {
                assert!(bar.pointer(
                    point(50.0, 155.0, kind == MouseEventKind::Move),
                    kind,
                    |_| current,
                    |_, _| panic!("{cause} left the drag active")
                ));
            }
        }
        bar.present(Some(oldest));
        // Captured text/remote drags starting on the grid keep their move and release.
        for kind in [MouseEventKind::Move, MouseEventKind::Release] {
            assert!(!bar.pointer(
                point(WIDTH - 6.0, 50.0, kind == MouseEventKind::Move),
                kind,
                |_| panic!(),
                |_, _| panic!()
            ));
        }
    }
}
