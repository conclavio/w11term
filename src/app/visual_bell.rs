use super::*;
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
use windows_canvas::{Brush, ColorF, DrawContext, Rect};

const DURATION: Duration = Duration::from_millis(450);
const MIN_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Default)]
pub(super) struct Pulse {
    last: Option<Instant>,
    pub(super) visible: bool,
}

impl Pulse {
    pub(super) fn ring(&mut self, received: Instant, now: Instant) -> bool {
        if now.saturating_duration_since(received) >= DURATION
            || self.last.is_some_and(|last| now - last < MIN_INTERVAL)
        {
            return false;
        }
        self.last = Some(now);
        self.visible = true;
        true
    }

    pub(super) fn elapsed(self, now: Instant) -> bool {
        self.last.is_some_and(|last| now - last >= DURATION)
    }
}

impl TerminalApp {
    pub(super) fn ring_visual_bells(
        &mut self,
        bells: Vec<(u64, Instant)>,
        ctx: &ComponentContext<Self>,
    ) {
        if bells.is_empty()
            || !self.appearance.read().unwrap().interaction.visual_bell
            || self.keyboard.hwnd() != Some(unsafe { GetForegroundWindow() })
        {
            return;
        }
        let now = Instant::now();
        let mut started = false;
        for (pane, received) in bells {
            if let Some(id) = self.tab_for_pane(pane).filter(|&id| self.tabs.visible(id))
                && let Some(Page::Terminal(live)) = self.tabs.get(id).map(|tab| &tab.page)
                && live.error.is_none()
                && !live.browsing_sessions()
            {
                started |= live.surface.ring_bell(received, now);
            }
        }
        if started && self.visual_bell_timer.is_none() {
            let sender = ctx.sender();
            match crate::ui_timer::RetryTimer::with_next_delay(50, move || {
                sender.send(Message::VisualBellTick);
                50
            }) {
                Ok(timer) => self.visual_bell_timer = Some(timer),
                Err(error) => {
                    log::warn!("Starting the visual bell timer: {error}");
                    self.update_visual_bells(false);
                }
            }
        }
    }

    pub(super) fn expire_visual_bells(&mut self) {
        let enabled = self.appearance.read().unwrap().interaction.visual_bell;
        self.update_visual_bells(enabled);
    }

    fn update_visual_bells(&mut self, enabled: bool) {
        let now = Instant::now();
        let mut pending = false;
        for entry in self.tabs.entries() {
            if let Page::Terminal(live) = &entry.value.page {
                pending |= live
                    .surface
                    .update_bell(now, enabled && self.tabs.visible(entry.id));
            }
        }
        if !pending {
            self.visual_bell_timer = None;
        }
    }
}

pub(super) fn draw(ctx: &DrawContext<'_>, visible: bool, brush: &mut Option<Brush>) {
    if ctx.device_changed() {
        *brush = None;
    }
    if !visible || ctx.width <= 6.0 || ctx.height <= 6.0 {
        return;
    }
    if brush.is_none() {
        *brush = ctx.create_solid_brush(ColorF::WHITE).ok();
    }
    if let Some(brush) = brush {
        let bounds = Rect::from_xywh(2.0, 2.0, ctx.width - 4.0, ctx.height - 4.0);
        // The dark edge keeps the signal visible on light and dark backgrounds.
        brush.set_color(ColorF::new(0.16, 0.10, 0.02, 1.0));
        ctx.draw_rect(&bounds, brush, 4.0);
        brush.set_color(ColorF::new(1.0, 0.73, 0.23, 1.0));
        ctx.draw_rect(&bounds, brush, 2.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_ignores_stale_events_and_limits_bursts_without_extending_the_outline() {
        let mut pulse = Pulse::default();
        let now = Instant::now();
        assert!(!pulse.ring(now, now + DURATION));
        assert!(!pulse.visible);
        assert!(pulse.ring(now, now));
        assert!(pulse.visible);
        let during = now + DURATION / 2;
        assert!(!pulse.ring(during, during));
        assert!(!pulse.elapsed(now + DURATION - Duration::from_millis(1)));
        assert!(pulse.elapsed(now + DURATION));
        let during_gap = now + MIN_INTERVAL - Duration::from_millis(1);
        assert!(!pulse.ring(during_gap, during_gap));
        let next = now + MIN_INTERVAL;
        assert!(pulse.ring(next, next));
        assert!(!pulse.elapsed(next));
    }
}
