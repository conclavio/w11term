//! Bounded per-pane event journal with monotonic cursors and loss detection.
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Notify;

const CAPACITY: usize = 4096;
#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub kind: String,
    pub data: Value,
}
#[derive(Clone, Debug, Serialize)]
pub struct Batch {
    pub events: Vec<Event>,
    pub next: u64,
    pub oldest: u64,
    pub lost: bool,
}
#[derive(Debug, Default)]
struct State {
    next: u64,
    events: VecDeque<Event>,
}
#[derive(Clone, Debug, Default)]
pub struct Events {
    state: Arc<Mutex<State>>,
    changed: Arc<Notify>,
}
impl Events {
    pub fn push(&self, kind: &str, data: Value) {
        let serialized = serde_json::to_string(&data).unwrap_or_default();
        let data = if serialized.len() > 4096 {
            serde_json::json!({"truncated":true,"preview":serialized.chars().take(1024).collect::<String>()})
        } else {
            data
        };
        let mut state = self.state.lock().unwrap();
        state.next += 1;
        let sequence = state.next;
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        state.events.push_back(Event {
            sequence,
            timestamp_ms,
            kind: kind.into(),
            data,
        });
        if state.events.len() > CAPACITY {
            state.events.pop_front();
        }
        drop(state);
        self.changed.notify_waiters();
    }
    pub fn since(&self, cursor: u64) -> Batch {
        let state = self.state.lock().unwrap();
        let oldest = state.events.front().map_or(state.next + 1, |e| e.sequence);
        Batch {
            events: state
                .events
                .iter()
                .filter(|e| e.sequence > cursor)
                .cloned()
                .collect(),
            next: state.next,
            oldest,
            lost: cursor < oldest.saturating_sub(1),
        }
    }
    pub async fn wait(&self, cursor: u64, timeout: std::time::Duration) -> Batch {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let batch = self.since(cursor);
            if !batch.events.is_empty() || batch.lost {
                return batch;
            }
            if tokio::time::timeout_at(deadline, changed).await.is_err() {
                return self.since(cursor);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cursors_detect_loss_and_wait_without_polling() {
        let e = Events::default();
        let waiter = tokio::spawn({
            let e = e.clone();
            async move { e.wait(0, std::time::Duration::from_secs(1)).await }
        });
        e.push("bell", serde_json::json!({}));
        assert_eq!(waiter.await.unwrap().events.len(), 1);
        for _ in 0..CAPACITY {
            e.push("title", Value::Null);
        }
        let b = e.since(0);
        assert!(b.lost);
        assert_eq!(b.events.len(), CAPACITY);
        assert_eq!(b.oldest, 2);
        assert!(e.since(b.next).events.is_empty());
    }
}
