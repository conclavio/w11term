use crate::renderer::{Metrics, PADDING, STATUS_HEIGHT};
use std::cell::Cell;
use w11term::terminal::{ScrollbackView, Snapshot};
use wezterm_term::MouseEventKind;
use windows_canvas::{Brush, ColorF, DrawContext, Rect};
use windows_reactor::PointerEventInfo;

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
        (width > 2.0 * PADDING && height >= 4.0 && width.is_finite() && height.is_finite())
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
    shown: Cell<Option<Rail>>,
    owns_press: Cell<bool>,
    drag: Cell<Option<Drag>>,
}

impl ScrollBar {
    pub(super) fn set_enabled(&self, enabled: bool) -> bool {
        let changed = self.enabled.replace(enabled) != enabled;
        if !enabled {
            self.present(None);
        }
        changed
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
        let geometry_matches =
            self.enabled.get() && frame.cols == metrics.cols && frame.rows.len() == metrics.rows;
        let rail = if geometry_matches {
            frame
                .scrollback_view()
                .and_then(|view| Rail::new(view, ctx.width, ctx.height, metrics.scale))
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
        }
        Ok(())
    }

    pub(super) fn pointer(
        &self,
        pointer: PointerEventInfo,
        kind: MouseEventKind,
        current: impl FnOnce() -> Option<ScrollbackView>,
        mut scroll: impl FnMut(ScrollbackView, Option<isize>),
    ) -> bool {
        let x = pointer.x as f32;
        let y = pointer.y as f32;
        let hit = self.shown.get().filter(|rail| rail.hit(x, y));
        if kind == MouseEventKind::Press {
            self.cancel();
            self.owns_press.set(hit.is_some());
            let Some(rail) = hit else { return false };
            if pointer.is_left_button_pressed
                && current().is_some_and(|view| rail.view.same_screen(&view))
            {
                let (top, height) = rail.thumb();
                if (top..top + height).contains(&y) {
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
                && current().is_some_and(|view| drag.rail.view.same_screen(&view))
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
                || Some(rail.view),
                |_, target| sent.push(target)
            ));
        }
        sent
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
            || Some(first.view),
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
                || Some(grown.view),
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
            || panic!(),
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
                || Some(oldest.view),
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
                    || current,
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
                || panic!(),
                |_, _| panic!()
            ));
        }
    }
}
