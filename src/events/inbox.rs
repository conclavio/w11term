use super::Event;
use std::{
    collections::VecDeque,
    fmt,
    sync::{Arc, Mutex, MutexGuard},
};

const CAPACITY: usize = 128;

#[derive(Clone, Debug)]
pub struct Notice {
    arrival: u64,
    flash_pending: bool,
    pub pane: u64,
    pub source: Arc<str>,
    pub sequence: u64,
    pub message: String,
    pub unread: bool,
}

#[derive(Default)]
struct State {
    rows: VecDeque<Notice>,
    wake_pending: bool,
}

/// Window-wide, independently acknowledged view of the pane event journals.
#[derive(Clone)]
pub struct Inbox {
    state: Arc<Mutex<State>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl fmt::Debug for Inbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inbox").finish_non_exhaustive()
    }
}

fn display_field(field: &str) -> String {
    let prefix: String = field.chars().take(200).collect();
    prefix
        .split(|c: char| {
            c.is_whitespace()
                || c.is_control()
                || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn notification_text(data: &serde_json::Value) -> String {
    let message = ["title", "body"]
        .into_iter()
        .filter_map(|field| data[field].as_str())
        .map(display_field)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    if message.is_empty() {
        return "Notification".into();
    }
    message.chars().take(200).collect()
}

impl Inbox {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            state: Default::default(),
            wake: Arc::new(wake),
        }
    }

    pub(super) fn receive(&self, pane: u64, source: &Arc<str>, event: &Event, live: bool) {
        let message = match event.kind.as_str() {
            "bell" => "Bell".to_owned(),
            "recording-error" => {
                let Some(error) = event.data["error"].as_str() else {
                    return;
                };
                let reason = display_field(error);
                if reason.is_empty() {
                    "Recording failed".into()
                } else {
                    format!("Recording failed · {reason}")
                        .chars()
                        .take(200)
                        .collect()
                }
            }
            "command-finished" => event.data["exit_code"].as_i64().map_or_else(
                || "Command finished · exit status unavailable".into(),
                |code| format!("Command finished · exit code {code}"),
            ),
            "notification" => event.data["summary"]
                .as_str()
                .map_or_else(|| notification_text(&event.data), str::to_owned),
            _ => return,
        };
        let mut state = self.state.lock().unwrap();
        if state.rows.len() == CAPACITY {
            state.rows.pop_front();
        }
        state.rows.push_back(Notice {
            arrival: {
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            },
            pane,
            flash_pending: live && event.kind == "command-finished",
            source: source.clone(),
            sequence: event.sequence,
            message,
            unread: true,
        });
        self.wake_ui(state);
    }

    pub(super) fn refresh(&self) {
        self.wake_ui(self.state.lock().unwrap());
    }

    fn wake_ui(&self, mut state: MutexGuard<'_, State>) {
        if !state.wake_pending {
            state.wake_pending = true;
            drop(state);
            (self.wake)();
        }
    }

    /// Acknowledge this batch and consume its taskbar flash request, if any.
    /// A notice can request a flash only once, even when moved between windows.
    pub fn acknowledge_wake(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        if !std::mem::take(&mut state.wake_pending) {
            return false;
        }
        let mut flash = false;
        for item in &mut state.rows {
            flash |= std::mem::take(&mut item.flash_pending) && item.unread;
        }
        flash
    }

    pub(super) fn same_window(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }

    pub(super) fn move_pane_to(&self, pane: u64, target: &Self) {
        let mut moved = Vec::new();
        self.state.lock().unwrap().rows.retain(|notice| {
            if notice.pane == pane {
                moved.push(notice.clone());
                false
            } else {
                true
            }
        });
        if moved.is_empty() {
            return;
        }
        let mut dest = target.state.lock().unwrap();
        dest.rows.extend(moved);
        dest.wake_pending = true;
        dest.rows
            .make_contiguous()
            .sort_by_key(|notice| notice.arrival);
        let excess = dest.rows.len().saturating_sub(CAPACITY);
        dest.rows.drain(..excess);
        drop(dest);
        (self.wake)();
        (target.wake)();
    }

    pub fn unread_count(&self) -> usize {
        self.state
            .lock()
            .unwrap()
            .rows
            .iter()
            .filter(|row| row.unread)
            .count()
    }

    pub fn rows(&self) -> Vec<Notice> {
        self.state
            .lock()
            .unwrap()
            .rows
            .iter()
            .rev()
            .cloned()
            .collect()
    }

    pub fn mark_read(&self, pane: u64, sequence: u64) -> bool {
        let mut state = self.state.lock().unwrap();
        if let Some(row) = state
            .rows
            .iter_mut()
            .find(|row| row.pane == pane && row.sequence == sequence)
        {
            row.unread = false;
            return true;
        }
        false
    }

    pub fn mark_all_read(&self) {
        for row in &mut self.state.lock().unwrap().rows {
            row.unread = false;
        }
    }

    pub fn clear(&self) {
        self.state.lock().unwrap().rows.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Events;
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn only_new_unread_command_completions_flash_the_current_owner_once() {
        let source = Inbox::new(|| {});
        let target = Inbox::new(|| {});
        let journal = Events::default();
        source.refresh();
        assert!(source.rows().is_empty());
        assert!(!source.acknowledge_wake());
        journal.push("command-finished", json!({"exit_code": null}));
        journal.connect_inbox(7, "SSH", &source);
        assert_eq!(
            source.rows()[0].message,
            "Command finished · exit status unavailable"
        );
        assert!(!source.acknowledge_wake());

        journal.push("bell", Value::Null);
        journal.push("notification", json!({"body":"command-finished"}));
        assert!(!source.acknowledge_wake());
        journal.push(
            "command-finished",
            json!({"exit_code": 17, "body":"private"}),
        );
        journal.push("command-finished", json!({"exit_code": 17}));
        journal.connect_inbox(7, "SSH", &target);
        assert!(!source.acknowledge_wake());
        assert_eq!(target.rows()[0].message, "Command finished · exit code 17");
        assert!(target.acknowledge_wake());
        assert!(!target.acknowledge_wake());

        journal.connect_inbox(7, "SSH", &source);
        assert!(!source.acknowledge_wake());
        journal.push("command-finished", json!({"exit_code": 0}));
        source.mark_all_read();
        assert!(!source.acknowledge_wake());
    }

    #[test]
    fn moving_panes_keeps_inbox_order_and_read_state_and_routes_new_alerts() {
        let source = Inbox::new(|| {});
        let target = Inbox::new(|| {});
        let a = Events::default();
        let b = Events::default();
        a.connect_inbox(1, "ssh", &source);
        b.connect_inbox(2, "local", &source);
        a.push("bell", Value::Null);
        b.push("bell", Value::Null);
        a.push("bell", Value::Null);
        assert!(source.mark_read(1, 1));
        b.connect_inbox(2, "local", &target);
        a.connect_inbox(1, "ssh", &target);
        b.push("bell", Value::Null);
        assert!(source.rows().is_empty());
        assert_eq!(target.unread_count(), 3);
        let rows: Vec<_> = target
            .rows()
            .into_iter()
            .map(|n| (n.pane, n.sequence))
            .collect();
        assert_eq!(rows, [(2, 2), (1, 2), (2, 1), (1, 1)]);
    }

    #[test]
    fn window_aggregates_panes_and_replays_startup_once_without_consuming_the_journal() {
        let left = Events::default();
        let right = Events::default();
        let wakes = Arc::new(AtomicUsize::new(0));
        let signal = wakes.clone();
        let inbox = Inbox::new(move || {
            signal.fetch_add(1, Ordering::SeqCst);
        });
        left.push(
            "notification",
            json!({"title":"Task\nupdate","body":"  finished "}),
        );
        left.connect_inbox(1, "left", &inbox);
        left.connect_inbox(1, "left", &inbox);
        right.connect_inbox(2, "right", &inbox);
        right.push("bell", Value::Null);
        left.push("title", json!({"title":"other"}));
        assert_eq!(inbox.unread_count(), 2);
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        let rows = inbox.rows();
        assert_eq!(rows[0].message, "Bell");
        assert_eq!(&*rows[0].source, "right");
        assert_eq!(rows[1].message, "Task update · finished");
        assert!(inbox.mark_read(1, 1));
        assert!(inbox.rows()[0].unread);
        assert!(!inbox.rows()[1].unread);
        inbox.acknowledge_wake();
        right.push("bell", Value::Null);
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
        inbox.mark_all_read();
        assert_eq!(inbox.unread_count(), 0);
        inbox.clear();
        assert!(inbox.rows().is_empty());
        assert_eq!(left.since(0).events.len(), 2);
    }

    #[test]
    fn evicted_and_cleared_notices_cannot_select_a_pane() {
        let events = Events::default();
        let inbox = Inbox::new(|| {});
        events.connect_inbox(7, "test", &inbox);
        for _ in 0..=CAPACITY {
            events.push("bell", Value::Null);
        }
        assert_eq!(inbox.rows().len(), CAPACITY);
        assert!(!inbox.mark_read(7, 1));
        assert!(inbox.mark_read(7, 2));
        inbox.clear();
        assert!(!inbox.mark_read(7, 2));
    }

    #[test]
    fn oversized_notice_keeps_a_bounded_single_line_summary_even_at_startup() {
        let events = Events::default();
        let inbox = Inbox::new(|| {});
        events.push(
            "notification",
            json!({"title":"Done\u{202e}\tbuilding","body":"é".repeat(5000)}),
        );
        events.connect_inbox(1, "dev", &inbox);
        let row = inbox.rows().pop().unwrap();
        assert!(row.message.starts_with("Done building · é"));
        assert_eq!(row.message.chars().count(), 200);
    }
}
