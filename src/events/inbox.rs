use super::Event;
use std::{
    collections::VecDeque,
    fmt,
    sync::{Arc, Mutex},
};

const CAPACITY: usize = 128;

#[derive(Clone, Debug)]
pub struct Notice {
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

pub(super) fn notification_text(data: &serde_json::Value) -> String {
    let fields = ["title", "body"]
        .into_iter()
        .filter_map(|field| data[field].as_str());
    let parts = fields.map(|field| {
        let prefix: String = field.chars().take(200).collect();
        prefix
            .split(|c: char| {
                c.is_whitespace()
                    || c.is_control()
                    || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            })
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    });
    let message = parts
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

    pub(super) fn receive(&self, pane: u64, source: &Arc<str>, event: &Event) {
        let message = match event.kind.as_str() {
            "bell" => "Bell".to_owned(),
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
            pane,
            source: source.clone(),
            sequence: event.sequence,
            message,
            unread: true,
        });
        if !state.wake_pending {
            state.wake_pending = true;
            drop(state);
            (self.wake)();
        }
    }

    pub fn acknowledge_wake(&self) {
        self.state.lock().unwrap().wake_pending = false;
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
