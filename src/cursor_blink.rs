//! Blink only the live terminal canvas. Previews and captures use their committed cursor.
use crate::ui_timer::RetryTimer;
use std::time::Instant;
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
    start: Instant,
    _timer: RetryTimer,
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
        if self
            .cycle
            .as_ref()
            .is_none_or(|cycle| cycle.cursor != cursor)
        {
            let surface = surface.clone();
            self.cycle = RetryTimer::with_interval(*ms, move || surface.invalidate())
                .ok()
                .map(|timer| Cycle {
                    cursor,
                    start: now,
                    _timer: timer,
                });
        }
        self.cycle.as_ref().is_none_or(|cycle| {
            let phase = now.duration_since(cycle.start).as_millis() / u128::from(*ms);
            phase.is_multiple_of(2)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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
        assert!(blink.visible(Some(cell), at(199)));
        assert!(!blink.visible(Some(cell), at(200)));
        blink.set_interval(Some(200), &surface);
        assert!(!blink.visible(Some(cell), at(399)));
        assert!(blink.visible(Some(cell), at(400)));
        cell.col += 1;
        assert!(blink.visible(Some(cell), at(650)));
        assert!(!blink.visible(Some(cell), at(850)));
        cell.shape = CursorShape::BlinkingBlock;
        assert!(blink.visible(Some(cell), at(890)));
        cell.alternate = true;
        assert!(blink.visible(Some(cell), at(1090)));
        cell.connection += 1;
        assert!(blink.visible(Some(cell), at(1290)));
        blink.set_interval(Some(300), &surface);
        assert!(blink.visible(Some(cell), at(1490)));
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
}
