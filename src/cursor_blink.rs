//! Blink only the live terminal canvas. Previews and captures use their committed cursor.
use crate::ui_timer::RetryTimer;
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};
use termwiz::surface::CursorShape;
use windows::Win32::UI::WindowsAndMessaging::GetCaretBlinkTime;
use windows_canvas::Invalidator;

pub(crate) fn system_interval() -> Option<u32> {
    #[cfg(feature = "ui-smoke")]
    if let Some(ms) = std::env::var("W11TERM_TEST_CARET_BLINK_MS")
        .ok()
        .and_then(|value| value.parse().ok())
    {
        return interval(ms);
    }
    interval(unsafe { GetCaretBlinkTime() })
}

fn interval(ms: u32) -> Option<u32> {
    // INFINITE disables blinking. A failed query should also leave a visible caret.
    (ms != u32::MAX && ms != 0).then_some(ms)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CursorCell {
    pub col: usize,
    pub row: usize,
    pub stable_row: isize,
    pub shape: CursorShape,
    pub alternate: bool,
    pub connection: u64,
}

struct Cycle {
    cursor: CursorCell,
    clock: Rc<Cell<BlinkClock>>,
    _timer: RetryTimer,
}

#[derive(Clone, Copy)]
struct BlinkClock {
    shown: bool,
    next: Instant,
}

impl BlinkClock {
    fn restart(now: Instant, ms: u32) -> Self {
        Self {
            shown: true,
            next: now + Duration::from_millis(u64::from(ms)),
        }
    }

    /// Return whether the canvas needs painting and the delay to the next edge.
    /// Motion resets the deadline in memory; only this timer callback rearms Win32.
    fn tick(&mut self, now: Instant, ms: u32) -> (bool, u32) {
        let before = self.shown;
        if let Some(late) = now.checked_duration_since(self.next) {
            let period_ns = u64::from(ms) * 1_000_000;
            let missed = late.as_nanos() / u128::from(period_ns);
            if missed.is_multiple_of(2) {
                self.shown = !self.shown;
            }
            let excess = (late.as_nanos() % u128::from(period_ns)) as u64;
            self.next = now + Duration::from_nanos(period_ns - excess);
        }
        let delay_ms = (self.next - now).as_nanos().div_ceil(1_000_000) as u32;
        (before != self.shown, delay_ms)
    }
}

#[derive(Default)]
pub(crate) struct CursorBlink {
    focused: Option<(u32, Invalidator)>,
    cycle: Option<Cycle>,
}

impl CursorBlink {
    #[cfg(test)]
    pub(crate) fn is_running(&self) -> bool {
        self.cycle.is_some()
    }

    pub fn set_interval(&mut self, ms: Option<u32>, surface: &Invalidator) {
        if self.focused.as_ref().map(|(old, _)| *old) != ms {
            self.cycle = None;
            self.focused = ms.map(|ms| (ms, surface.clone()));
            surface.invalidate();
        }
    }

    pub fn visible(&mut self, cursor: Option<CursorCell>, now: Instant) -> bool {
        let Some((cursor, (ms, surface))) = cursor.zip(self.focused.as_ref()) else {
            self.cycle = None;
            return true;
        };
        let ms = *ms;
        if let Some(cycle) = &mut self.cycle {
            if cycle.cursor != cursor {
                cycle.cursor = cursor;
                cycle.clock.set(BlinkClock::restart(now, ms));
            }
            return cycle.clock.get().shown;
        }
        let clock = Rc::new(Cell::new(BlinkClock::restart(now, ms)));
        let on_tick = clock.clone();
        let surface = surface.clone();
        if let Ok(timer) = RetryTimer::with_next_delay(ms, move || {
            let mut state = on_tick.get();
            let (changed, delay) = state.tick(Instant::now(), ms);
            on_tick.set(state);
            if changed {
                surface.invalidate();
            }
            delay
        }) {
            self.cycle = Some(Cycle {
                cursor,
                clock,
                _timer: timer,
            });
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tick(blink: &CursorBlink, now: Instant) -> (bool, u32) {
        let clock = &blink.cycle.as_ref().unwrap().clock;
        let mut state = clock.get();
        let result = state.tick(now, blink.focused.as_ref().unwrap().0);
        clock.set(state);
        result
    }

    #[test]
    fn windows_disabled_and_failed_cadences_leave_a_steady_cursor() {
        assert_eq!(interval(0), None);
        assert_eq!(interval(u32::MAX), None);
        assert_eq!(interval(550), Some(550));
    }

    #[test]
    fn cursor_phases_restart_only_on_identity_focus_or_cadence_changes() {
        let surface = Invalidator::new();
        let start = Instant::now();
        let mut blink = CursorBlink::default();
        let mut cell = CursorCell {
            col: 3,
            row: 2,
            stable_row: 9,
            shape: CursorShape::BlinkingBar,
            alternate: false,
            connection: 1,
        };
        assert!(blink.visible(Some(cell), start));
        assert!(blink.cycle.is_none());
        blink.set_interval(Some(200), &surface);
        assert!(blink.visible(Some(cell), start));
        let at = |ms| start + Duration::from_millis(ms);
        assert_eq!(tick(&blink, at(199)), (false, 1));
        assert!(blink.visible(Some(cell), at(199)));
        assert_eq!(tick(&blink, at(200)), (true, 200));
        assert!(!blink.visible(Some(cell), at(200)));
        blink.set_interval(Some(200), &surface);
        assert!(!blink.visible(Some(cell), at(399)));
        assert_eq!(tick(&blink, at(400)), (true, 200));
        assert!(blink.visible(Some(cell), at(400)));
        cell.col += 1;
        assert!(blink.visible(Some(cell), at(650)));
        assert_eq!(tick(&blink, at(850)), (true, 200));
        assert!(!blink.visible(Some(cell), at(850)));
        cell.shape = CursorShape::BlinkingBlock;
        assert!(blink.visible(Some(cell), at(890)));
        cell.alternate = true;
        assert!(blink.visible(Some(cell), at(1090)));
        cell.connection += 1;
        assert!(blink.visible(Some(cell), at(1290)));
        blink.set_interval(Some(300), &surface);
        assert!(blink.visible(Some(cell), at(1490)));
        assert_eq!(tick(&blink, at(1790)), (true, 300));
        assert!(!blink.visible(Some(cell), at(1790)));
        assert!(blink.visible(None, at(1800)));
        assert!(blink.cycle.is_none());
        assert!(blink.visible(Some(cell), at(1820)));
        blink.set_interval(None, &surface);
        assert!(blink.cycle.is_none());
        assert!(blink.visible(Some(cell), at(2230)));
        blink.set_interval(Some(300), &surface);
        assert!(blink.visible(Some(cell), at(2290)));
    }

    #[test]
    fn output_defers_blinking_without_replacing_the_timer_or_requesting_extra_paints() {
        let mut blink = CursorBlink::default();
        blink.set_interval(Some(200), &Invalidator::new());
        let start = Instant::now();
        let mut cell = CursorCell {
            col: 0,
            row: 0,
            stable_row: 0,
            shape: CursorShape::BlinkingBar,
            alternate: false,
            connection: 1,
        };
        assert!(blink.visible(Some(cell), start));
        let first = blink.cycle.as_ref().unwrap().clock.clone();
        for idx in 1..=100 {
            cell.col = idx;
            let now = start + Duration::from_millis(idx as u64 * 10);
            assert!(blink.visible(Some(cell), now));
            assert!(Rc::ptr_eq(&blink.cycle.as_ref().unwrap().clock, &first));
            if idx % 20 == 0 {
                assert_eq!(tick(&blink, now), (false, 200));
            }
        }
        let at = |ms| start + Duration::from_millis(ms);
        assert_eq!(tick(&blink, at(1100)), (false, 100));
        assert_eq!(tick(&blink, at(1200)), (true, 200));
        assert!(!blink.visible(Some(cell), at(1200)));
        assert_eq!(tick(&blink, at(1660)), (false, 140));
        assert_eq!(tick(&blink, at(1800)), (true, 200));
        assert!(blink.visible(Some(cell), at(1800)));
    }
}
