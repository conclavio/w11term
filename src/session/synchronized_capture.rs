use super::test_worker::{capture, feed, text, with_demo};
use super::*;
use crate::control::{CapturePng, Pane, Registry, Request};
use std::{future::poll_fn, pin::Pin, task::Poll};

async fn assert_pending<F: Future>(mut future: Pin<&mut F>) {
    assert!(
        poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending())).await,
        "wait-for-text completed before the synchronized update was committed"
    );
}

#[tokio::test(start_paused = true)]
async fn inspect_reads_live_modes_during_a_held_frame_without_enabling_the_gui_inspector() {
    let size = TerminalSize {
        cols: 80,
        rows: 24,
        pixel_width: 800,
        pixel_height: 480,
        dpi: 144,
    };
    with_demo(size, async |input, frames| {
        let registry = Registry::default();
        registry.replace([Pane {
            id: 7,
            tab_id: 2,
            tab_title: None,
            pane_name: None,
            label: "background".into(),
            active: false,
            visible: false,
            read_only: true,
            capture: None,
            input: input.clone(),
        }]);
        let old = feed(&input, "shell").await;
        let get_text = || {
            registry.execute(
                Request::GetText { pane: 7 },
                Arc::new(|_, _, _| unreachable!()),
            )
        };
        let shell = get_text().await.unwrap();
        assert!(shell["text"].as_str().unwrap().contains("shell"));
        feed(
            &input,
            "\x1b[?2026h\x1b[?1049h\x1b[?2004h\x1b[?2048h\x1b[?1000h\x1b[?1016h\x1b[>3u",
        )
        .await;
        input.send(Command::SetReadOnly(true)).unwrap();
        let request: Request =
            serde_json::from_value(serde_json::json!({"verb":"inspect","pane":7})).unwrap();
        let modes = registry
            .execute(request, Arc::new(|_, _, _| unreachable!()))
            .await
            .unwrap();
        assert_eq!(
            modes,
            serde_json::json!({
                "pane":7, "connection_generation":0, "cols":80, "rows":24,
                "pixel_width":800, "pixel_height":480, "dpi":144, "alternate_screen":true,
                "bracketed_paste":true, "synchronized_output":true, "in_band_resize":true,
                "mouse":{"click":true, "drag":false, "motion":false, "encoding":"sgr-pixels"},
                "keyboard":{"encoding":"kitty", "flags":3}
            })
        );
        assert!(frames.inspection().is_none());
        assert_eq!(frames.load().generation, old.generation);
        assert!(!frames.load().alternate);
        assert_eq!(get_text().await.unwrap(), shell);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn changing_scheme_recolors_read_only_scrollback_and_commits_at_sync_boundaries() {
    let size = TerminalSize {
        rows: 3,
        cols: 40,
        ..Default::default()
    };
    with_demo(size, async |input, frames| {
        feed(&input, "first\nsecond\nthird\nfourth\nfifth").await;
        input.send(Command::ScrollEdge(true)).unwrap();
        input.send(Command::SetReadOnly(true)).unwrap();
        let old_live = capture(&input).await;
        let old_view = frames.load();
        assert!(old_view.scroll_offset > 0);
        input
            .send(Command::SetColorScheme(ColorScheme::OneHalfLight))
            .unwrap();
        let light = capture(&input).await;
        let light_view = frames.load();
        assert_eq!(light.palette.background, (0xfa, 0xfa, 0xfa).into());
        assert_eq!(light_view.palette, light.palette);
        assert_eq!(light_view.scroll_offset, old_view.scroll_offset);
        assert_eq!(text(&light_view), text(&old_view));
        assert_eq!(text(&light), text(&old_live));
        assert_eq!(light.connection_generation, old_live.connection_generation);
        input
            .send(Command::SetColorScheme(ColorScheme::OneHalfLight))
            .unwrap();
        assert_eq!(capture(&input).await.generation, light.generation);

        input.send(Command::SetReadOnly(false)).unwrap();
        let held = feed(&input, "\x1b[?2026h").await;
        let held_view = frames.load();
        input
            .send(Command::SetColorScheme(ColorScheme::OneHalfDark))
            .unwrap();
        let still_held = capture(&input).await;
        assert_eq!(still_held.palette, held.palette);
        assert_eq!(still_held.generation, held.generation);
        assert_eq!(frames.load().generation, held_view.generation);
        let committed = feed(&input, "\x1b[?2026l").await;
        assert_eq!(
            committed.generation,
            held.generation.max(held_view.generation) + 1
        );
        assert_eq!(committed.palette.background, (0x28, 0x2c, 0x34).into());
        assert_eq!(text(&committed), text(&held));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn fragmented_update_and_control_capture_share_the_committed_frame() {
    let size = TerminalSize {
        cols: 40,
        rows: 8,
        pixel_width: 400,
        pixel_height: 160,
        dpi: 96,
    };
    with_demo(size, async |input, frames| {
        let registry = Registry::default();
        registry.replace([Pane {
            id: 1,
            tab_id: 5,
            tab_title: None,
            pane_name: None,
            label: "test".into(),
            active: true,
            visible: true,
            read_only: false,
            capture: None,
            input: input.clone(),
        }]);

        let old = feed(&input, "old HUD").await;
        for piece in ["\x1b[?20", "26h", "\rnew HUD", "\x1b[2;1Hnew picture"] {
            let partial = feed(&input, piece).await;
            assert_eq!(partial.generation, old.generation);
            assert_eq!(text(&partial), text(&old));
            assert_eq!(text(&frames.load()), text(&old));
        }
        let expected = text(&old);
        let screenshot: CapturePng = Arc::new(move |frame, _, _| {
            assert_eq!(text(frame), expected);
            Ok((400, 160))
        });
        let screen = registry
            .execute(Request::GetText { pane: 1 }, screenshot.clone())
            .await
            .unwrap();
        assert!(screen["text"].as_str().unwrap().contains("old HUD"));
        assert_eq!(screen["generation"], old.generation);
        // The callback inspects the snapshot without writing a PNG. No
        // temporary directory or filesystem access is needed for this capture.
        let image = registry
            .execute(
                Request::Screenshot {
                    pane: 1,
                    path: std::env::temp_dir().join("unused.png"),
                },
                screenshot.clone(),
            )
            .await
            .unwrap();
        assert_eq!(image["generation"], old.generation);
        let waiting = registry.execute(
            Request::WaitForText {
                pane: 1,
                pattern: "new picture".into(),
                regex: false,
                timeout_seconds: 1,
            },
            screenshot,
        );
        tokio::pin!(waiting);
        assert_pending(waiting.as_mut()).await; // queues Command::Frames
        capture(&input).await; // FIFO barrier: the worker has replied to Frames
        assert_pending(waiting.as_mut()).await; // matcher has checked the held frame
        let done = feed(&input, "\x1b[?2026l").await;
        let found = waiting.await.unwrap();
        assert!(text(&done).contains("new HUD\nnew picture"));
        assert_eq!(done.generation, old.generation + 1);
        assert_eq!(found["generation"], done.generation);
    })
    .await;
}
