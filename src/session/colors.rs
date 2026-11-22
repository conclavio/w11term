use crate::{
    options::{ColorScheme, ResolvedColorScheme},
    windows_appearance::WindowsAppearance,
};
use tokio::sync::watch;

pub(super) struct PaneColors {
    effective: ResolvedColorScheme,
    windows: Option<watch::Receiver<WindowsAppearance>>,
    subscribe: Box<dyn Fn() -> watch::Receiver<WindowsAppearance> + Send>,
}

impl PaneColors {
    pub(super) fn new(
        preference: ColorScheme,
        subscribe: impl Fn() -> watch::Receiver<WindowsAppearance> + Send + 'static,
    ) -> Self {
        let mut colors = Self {
            effective: ResolvedColorScheme::Default,
            windows: None,
            subscribe: Box::new(subscribe),
        };
        colors.choose(preference);
        colors
    }

    pub(super) fn effective(&self) -> ResolvedColorScheme {
        self.effective
    }

    pub(super) fn choose(&mut self, preference: ColorScheme) {
        if preference != ColorScheme::FollowWindows {
            self.windows = None;
        }
        self.effective = preference.resolve(|| {
            let windows = self.windows.get_or_insert_with(&self.subscribe);
            *windows.borrow_and_update()
        });
    }

    pub(super) fn sample(&mut self) {
        if let Some(windows) = &mut self.windows {
            self.effective = ColorScheme::FollowWindows.resolve(|| *windows.borrow_and_update());
        }
    }

    pub(super) async fn changed(&mut self) {
        let Some(windows) = &mut self.windows else {
            return std::future::pending().await;
        };
        if windows.changed().await.is_ok() {
            self.sample();
        } else {
            // Retain the last usable palette without waking repeatedly on a closed source.
            self.windows = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        session::{
            Command,
            test_worker::{capture, feed, text, with_demo_colors},
        },
        terminal::TransportInput,
    };
    use WindowsAppearance::{Dark, Light};
    use tokio::sync::mpsc;

    #[test]
    fn only_following_panes_subscribe_and_reenter_with_the_current_appearance() {
        let (source, _) = watch::channel(Light);
        let colors = || {
            let source = source.clone();
            PaneColors::new(ColorScheme::Default, move || source.subscribe())
        };
        let mut first = colors();
        let mut other = colors();
        assert_eq!(source.receiver_count(), 0);
        first.choose(ColorScheme::FollowWindows);
        first.choose(ColorScheme::FollowWindows);
        assert_eq!(source.receiver_count(), 1);
        assert_eq!(first.effective(), ResolvedColorScheme::OneHalfLight);
        other.choose(ColorScheme::FollowWindows);
        assert_eq!(source.receiver_count(), 2);
        first.choose(ColorScheme::OneHalfLight);
        assert_eq!(source.receiver_count(), 1);
        drop(other);
        assert_eq!(source.receiver_count(), 0);
        source.send_replace(Dark);
        first.choose(ColorScheme::FollowWindows);
        assert_eq!(first.effective(), ResolvedColorScheme::OneHalfDark);
    }

    async fn report(replies: &mut mpsc::UnboundedReceiver<TransportInput>, expected: &[u8]) {
        let Some(TransportInput::Automatic(reply)) = replies.recv().await else {
            panic!("missing automatic theme report");
        };
        assert_eq!(reply, expected);
    }

    #[tokio::test(start_paused = true)]
    async fn worker_follows_windows_without_activity_and_preserves_sync_and_remote_overrides() {
        let (source, _) = watch::channel(Light);
        let provider = source.clone();
        let colors = PaneColors::new(ColorScheme::FollowWindows, move || provider.subscribe());
        assert_eq!(colors.effective(), ResolvedColorScheme::OneHalfLight);
        source.send_replace(Dark); // Between the initial frame and worker startup.
        let (viewer, mut replies) = mpsc::unbounded_channel();
        with_demo_colors(
            Default::default(),
            colors,
            Some(viewer),
            async |input, frames| {
                let dark = feed(&input, "unchanged\x1b]4;1;#010203\x07\x1b[?2031h\x1b[?996n").await;
                report(&mut replies, b"\x1b[?997;1n").await;
                assert_eq!(dark.palette.background, (0x28, 0x2c, 0x34).into());
                source.send_replace(Light);
                report(&mut replies, b"\x1b[?997;2n").await; // Idle pane wakes on the watch alone.
                let light = capture(&input).await;
                assert_eq!(light.palette.background, (0xfa, 0xfa, 0xfa).into());
                assert_eq!(light.palette.colors.0[1], (1, 2, 3).into());
                assert_eq!(text(&light), text(&dark));
                assert_eq!(light.connection_generation, dark.connection_generation);
                input
                    .send(Command::SetColorScheme(ColorScheme::OneHalfLight))
                    .unwrap();
                assert_eq!(capture(&input).await.generation, light.generation);
                assert_eq!(source.receiver_count(), 0);
                source.send_replace(Dark);
                assert_eq!(capture(&input).await.generation, light.generation);

                let held = feed(&input, "\x1b[?2026h").await;
                input
                    .send(Command::SetColorScheme(ColorScheme::FollowWindows))
                    .unwrap();
                report(&mut replies, b"\x1b[?997;1n").await;
                source.send_replace(Light);
                report(&mut replies, b"\x1b[?997;2n").await;
                source.send_replace(Dark);
                report(&mut replies, b"\x1b[?997;1n").await;
                assert_eq!(frames.load().generation, held.generation);
                assert_eq!(capture(&input).await.palette, held.palette);
                let committed = feed(&input, "\x1b[?2026l").await;
                assert_eq!(committed.palette, dark.palette);
                source.send_replace(Dark);
                // A normal DSR is the next report even after a duplicate OS notification.
                feed(&input, "\x1b[?996n\x1bc\x1b[?2031$p\x1b[?996n").await;
                report(&mut replies, b"\x1b[?997;1n").await;
                report(&mut replies, b"\x1b[?2031;2$y").await;
                report(&mut replies, b"\x1b[?997;1n").await;
                let reset = capture(&input).await;
                assert_eq!(reset.palette.background, dark.palette.background);
                assert_eq!(reset.palette.colors.0[1], (0xe0, 0x6c, 0x75).into());
                assert!(replies.try_recv().is_err());
            },
        )
        .await;
    }
}
