use super::Command;
use crate::terminal::{RetainedRows, ScrollbackTarget, ScrollbackView};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

/// UIA calls are synchronous. A timeout cancels a queued or preparing request
/// before it can publish a viewport change.
pub struct RangeScroll {
    open: Arc<Mutex<bool>>,
    reply: mpsc::Receiver<bool>,
}

#[derive(Debug)]
pub struct RangeScrollRequest {
    destination: Destination,
    alive: Arc<AtomicBool>,
    open: Arc<Mutex<bool>>,
    reply: mpsc::Sender<bool>,
}

#[derive(Debug)]
enum Destination {
    TextRange(RetainedRows, bool),
    Viewport(ScrollbackTarget),
    Relative {
        view: ScrollbackView,
        rows_down: isize,
    },
}

impl RangeScroll {
    pub fn request(rows: RetainedRows, align_top: bool, alive: Arc<AtomicBool>) -> (Command, Self) {
        Self::new(Destination::TextRange(rows, align_top), alive)
    }

    pub fn viewport(target: ScrollbackTarget, alive: Arc<AtomicBool>) -> (Command, Self) {
        Self::new(Destination::Viewport(target), alive)
    }

    /// Apply the signed row count to the current viewport when this request reaches the worker.
    pub fn relative(
        view: ScrollbackView,
        rows_down: isize,
        alive: Arc<AtomicBool>,
    ) -> (Command, Self) {
        Self::new(Destination::Relative { view, rows_down }, alive)
    }

    fn new(destination: Destination, alive: Arc<AtomicBool>) -> (Command, Self) {
        let (tx, reply) = mpsc::channel();
        let open = Arc::new(Mutex::new(true));
        (
            Command::ScrollRange(RangeScrollRequest {
                destination,
                alive,
                open: open.clone(),
                reply: tx,
            }),
            Self { open, reply },
        )
    }

    pub fn wait(self, timeout: Duration) -> bool {
        self.reply.recv_timeout(timeout).unwrap_or_else(|_| {
            *self.open.lock().unwrap() = false;
            // A racing commit holding the lock has already published and replied.
            self.reply.try_recv().unwrap_or(false)
        })
    }
}

impl Drop for RangeScroll {
    fn drop(&mut self) {
        *self.open.lock().unwrap() = false;
    }
}

impl RangeScrollRequest {
    pub(super) fn offset(
        &self,
        term: &wezterm_term::Terminal,
        connection: u64,
        current: usize,
    ) -> Option<usize> {
        match &self.destination {
            Destination::TextRange(rows, align_top) => {
                rows.scroll_offset(term, connection, *align_top)
            }
            Destination::Viewport(target) => target.offset(term, connection),
            Destination::Relative { view, rows_down } => {
                let max = view.max_offset(term, connection)?;
                Some(current.min(max).saturating_sub_signed(*rows_down).min(max))
            }
        }
    }
    pub(super) fn available(&self) -> bool {
        self.alive.load(Ordering::Acquire) && *self.open.lock().unwrap()
    }

    pub(super) fn commit(self, publish: impl FnOnce()) {
        let open = self.open.lock().unwrap();
        let accepted = *open && self.alive.load(Ordering::Acquire);
        if accepted {
            publish();
        }
        let _ = self.reply.send(accepted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_worker::{capture, feed, with_demo};
    use wezterm_term::TerminalSize;

    #[tokio::test(start_paused = true)]
    async fn queued_relative_scrolls_accumulate_after_a_pointer_scroll_and_reject_stale_history() {
        with_demo(
            TerminalSize {
                rows: 3,
                cols: 16,
                ..Default::default()
            },
            async |input, frames| {
                let view = feed(&input, "a\nb\nc\nd\ne\nf\ng")
                    .await
                    .scrollback_view()
                    .unwrap();
                let submit = |rows_down| {
                    let (command, reply) =
                        RangeScroll::relative(view, rows_down, Arc::new(AtomicBool::new(true)));
                    input.send(command).unwrap();
                    reply
                };
                // All three requests originate from the same published frame at the live tail.
                input.send(view.scroll_to(Some(view.oldest))).unwrap();
                let first = submit(1);
                let second = submit(1);
                capture(&input).await;
                assert!(first.wait(Duration::ZERO));
                assert!(second.wait(Duration::ZERO));
                assert_eq!(frames.load().first_stable_row, view.oldest + 2);

                for (delta, expected) in [(isize::MIN, view.oldest), (isize::MAX, view.live)] {
                    let reply = submit(delta);
                    capture(&input).await;
                    assert!(reply.wait(Duration::ZERO));
                    assert_eq!(frames.load().first_stable_row, expected);
                }
                input.send(Command::ClearScrollback).unwrap();
                let new = feed(&input, "\nh\ni\nj").await;
                let stale = submit(-1);
                capture(&input).await;
                assert!(!stale.wait(Duration::ZERO));
                assert_eq!(frames.load().first_stable_row, new.first_stable_row);
            },
        )
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn retained_range_scroll_publishes_only_accepted_viewports() {
        with_demo(
            TerminalSize {
                rows: 4,
                cols: 16,
                ..Default::default()
            },
            async |input, frames| {
                let frame = feed(&input, "a\nb\nc\nd").await;
                let rows = frame.retained_rows(2, 3).unwrap();
                feed(&input, "\ne\nf\ng\nh\ni").await;
                let alive = Arc::new(AtomicBool::new(true));
                let send = |align_top| {
                    let (command, waiter) =
                        RangeScroll::request(rows.clone(), align_top, alive.clone());
                    input.send(command).unwrap();
                    waiter
                };

                let abandoned = send(true);
                assert!(!abandoned.wait(Duration::ZERO));
                capture(&input).await;
                assert_eq!(frames.load().scroll_offset, 0);
                alive.store(false, Ordering::Release);
                let retired = send(true);
                capture(&input).await;
                assert!(!retired.wait(Duration::ZERO));
                assert_eq!(frames.load().scroll_offset, 0);

                alive.store(true, Ordering::Release);
                input.send(Command::SetReadOnly(true)).unwrap();
                let top = send(true);
                let live = capture(&input).await;
                assert!(top.wait(Duration::ZERO));
                assert_eq!(live.first_stable_row, 5);
                assert_eq!(frames.load().first_stable_row, 2);
                let bottom = send(false);
                capture(&input).await;
                assert!(bottom.wait(Duration::ZERO));
                assert_eq!(frames.load().first_stable_row, 0);

                input.send(Command::SetReadOnly(false)).unwrap();
                feed(&input, "\x1b[?2026h").await;
                let hidden = send(true);
                capture(&input).await;
                assert!(!hidden.wait(Duration::ZERO));
                feed(&input, "\x1b[?2026l\x1b[3J").await;
                let stale = send(true);
                capture(&input).await;
                assert!(!stale.wait(Duration::ZERO));
                assert_eq!(frames.load().scroll_offset, 0);
            },
        )
        .await;
    }
}
