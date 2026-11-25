//! Kitty OSC 99 presents a bounded, connection-scoped set of Attention inbox entries.
use super::{Events, State, display_field};
use base64::{
    Engine, alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use std::{collections::VecDeque, sync::Arc, time::Duration};
use tokio::{sync::Notify, time::Instant};

const CAPACITY: usize = 32;
const FIELD_BYTES: usize = 4096;
const CHUNK_BYTES: usize = 2048;
const MAX_BUTTONS: usize = 8;
const CAPABILITIES: &str =
    "a=focus,report:c=1:o=always,unfocused,invisible:p=title,body,buttons,close,alive,?:w=1";
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

#[derive(Clone, Copy, Debug, Default)]
enum Occasion {
    #[default]
    Always,
    Unfocused,
    Invisible,
}

#[derive(Clone, Copy, Debug, Default)]
enum Payload {
    #[default]
    Title,
    Body,
    Buttons,
    Close,
    Alive,
    Query,
}

#[derive(Debug)]
struct Request<'a> {
    id: Option<&'a str>,
    payload: Payload,
    done: bool,
    encoded: bool,
    focus: Option<bool>,
    report: Option<bool>,
    close: Option<bool>,
    occasion: Option<Occasion>,
    lifetime: Option<Duration>,
}

impl<'a> Request<'a> {
    fn parse(metadata: &'a [u8]) -> Option<Self> {
        let mut request = Self {
            id: None,
            payload: Payload::Title,
            done: true,
            encoded: false,
            focus: None,
            report: None,
            close: None,
            occasion: None,
            lifetime: None,
        };
        let metadata = std::str::from_utf8(metadata).ok()?;
        for (key, value) in metadata.split(':').filter_map(|part| part.split_once('=')) {
            match key {
                "i" => {
                    if value.is_empty()
                        || value.len() > 64
                        || !value
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"_+.-".contains(&b))
                    {
                        return None;
                    }
                    request.id = Some(value);
                }
                "d" => request.done = value != "0",
                "e" => request.encoded = value == "1",
                "c" => request.close = Some(value == "1"),
                "a" => {
                    for action in value.split(',') {
                        match action {
                            "focus" => request.focus = Some(true),
                            "-focus" => request.focus = Some(false),
                            "report" => request.report = Some(true),
                            "-report" => request.report = Some(false),
                            _ => {}
                        }
                    }
                }
                "o" => {
                    request.occasion = match value {
                        "always" => Some(Occasion::Always),
                        "unfocused" => Some(Occasion::Unfocused),
                        "invisible" => Some(Occasion::Invisible),
                        _ => None,
                    };
                }
                "w" => {
                    // Duration::ZERO represents the inbox's regular retained lifetime.
                    request.lifetime = value
                        .parse::<i32>()
                        .ok()
                        .filter(|&ms| ms >= -1)
                        .map(|ms| Duration::from_millis(ms.max(0) as u64));
                }
                "p" => {
                    request.payload = match value {
                        "title" => Payload::Title,
                        "body" => Payload::Body,
                        "buttons" => Payload::Buttons,
                        "close" => Payload::Close,
                        "alive" => Payload::Alive,
                        "?" => Payload::Query,
                        _ => return None,
                    };
                }
                _ => {}
            }
        }
        Some(request)
    }
}

#[derive(Debug, Default)]
struct Field {
    bytes: Vec<u8>,
    tail: Vec<u8>,
}

impl Field {
    fn push(&mut self, bytes: &[u8], encoded: bool) -> Option<()> {
        if encoded {
            let mut chunk = std::mem::take(&mut self.tail);
            chunk.extend_from_slice(bytes);
            let complete = chunk.len() / 4 * 4;
            self.tail.extend_from_slice(&chunk[complete..]);
            self.bytes.extend(BASE64.decode(&chunk[..complete]).ok()?);
        } else {
            if bytes.len() > CHUNK_BYTES
                || std::str::from_utf8(bytes)
                    .ok()?
                    .chars()
                    .any(char::is_control)
            {
                return None;
            }
            self.flush_tail()?;
            self.bytes.extend_from_slice(bytes);
        }
        (self.bytes.len() <= FIELD_BYTES).then_some(())
    }

    fn flush_tail(&mut self) -> Option<()> {
        self.bytes
            .extend(BASE64.decode(std::mem::take(&mut self.tail)).ok()?);
        Some(())
    }

    fn finish(mut self) -> Option<String> {
        self.flush_tail()?;
        if self.bytes.len() > FIELD_BYTES {
            return None;
        }
        String::from_utf8(self.bytes).ok()
    }
}

#[derive(Debug)]
struct Draft {
    id: Option<String>,
    title: Field,
    body: Field,
    buttons: Field,
    focus: bool,
    report: bool,
    close: bool,
    occasion: Occasion,
    lifetime: Duration,
}

impl Draft {
    fn new(id: Option<&str>) -> Self {
        Self {
            id: id.map(str::to_owned),
            title: Field::default(),
            body: Field::default(),
            buttons: Field::default(),
            focus: true,
            report: false,
            close: false,
            occasion: Occasion::Always,
            lifetime: Duration::ZERO,
        }
    }

    fn apply(&mut self, request: &Request<'_>, bytes: &[u8]) -> Option<()> {
        self.focus = request.focus.unwrap_or(self.focus);
        self.report = request.report.unwrap_or(self.report);
        self.close = request.close.unwrap_or(self.close);
        self.occasion = request.occasion.unwrap_or(self.occasion);
        self.lifetime = request.lifetime.unwrap_or(self.lifetime);
        match request.payload {
            Payload::Title => &mut self.title,
            Payload::Body => &mut self.body,
            Payload::Buttons => &mut self.buttons,
            _ => unreachable!("control payloads do not modify drafts"),
        }
        .push(bytes, request.encoded)
    }
}

#[derive(Debug)]
struct Live {
    sequence: u64,
    id: Option<String>,
    buttons: Arc<[String]>,
    focus: bool,
    report: bool,
    close: bool,
    deadline: Option<Instant>,
}

impl Live {
    fn close_reply(&self) -> Option<String> {
        self.close
            .then(|| response(self.id.as_deref(), ":p=close", ""))
    }
}

#[derive(Debug, Default)]
pub(super) struct Notifications {
    pub(super) eviction: Arc<Notify>,
    connection: Option<u64>,
    pending: VecDeque<Draft>,
    live: VecDeque<Live>,
    focused: bool,
    visible: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum NotificationChoice {
    Activate(Option<usize>),
    Dismiss,
}

#[derive(Debug)]
pub struct NotificationResponse {
    pub connection: u64,
    pub wire: Vec<String>,
    pub focus: bool,
}

fn response(id: Option<&str>, metadata: &str, payload: &str) -> String {
    format!("\x1b]99;i={}{metadata};{payload}\x1b\\", id.unwrap_or("0"))
}

impl State {
    fn notification_displayed(&self, sequence: u64) -> bool {
        self.inbox
            .as_ref()
            .is_none_or(|(pane, _, inbox)| inbox.has_notification(*pane, sequence))
    }

    fn clear_notifications(&mut self) {
        self.kitty_notifications.pending.clear();
        while let Some(dead) = self.kitty_notifications.live.pop_front() {
            self.erase_notification(dead.sequence);
        }
    }

    fn erase_notification(&self, sequence: u64) {
        if let Some((pane, _, inbox)) = &self.inbox {
            inbox.remove_notification(*pane, sequence);
        }
    }

    fn retire_notification(&mut self, index: usize) -> Live {
        let old = self.kitty_notifications.live.remove(index).unwrap();
        self.erase_notification(old.sequence);
        old
    }

    fn close_expired_notifications(&mut self, now: Instant) -> Vec<String> {
        let mut replies = Vec::new();
        let mut index = 0;
        while let Some(item) = self.kitty_notifications.live.get(index) {
            if !self.notification_displayed(item.sequence)
                || item.deadline.is_some_and(|deadline| deadline <= now)
            {
                replies.extend(self.retire_notification(index).close_reply());
            } else {
                index += 1;
            }
        }
        replies
    }

    fn show_notification(&mut self, draft: Draft, now: Instant, replies: &mut Vec<String>) -> bool {
        let eligible = match draft.occasion {
            Occasion::Always => true,
            Occasion::Unfocused => !self.kitty_notifications.focused,
            Occasion::Invisible => {
                !self.kitty_notifications.visible && !self.kitty_notifications.focused
            }
        };
        if !eligible {
            return false;
        }
        let (Some(mut title), Some(mut body), Some(buttons)) = (
            draft.title.finish(),
            draft.body.finish(),
            draft.buttons.finish(),
        ) else {
            return false;
        };
        title = display_field(&title);
        body = display_field(&body);
        if title.is_empty() {
            title = std::mem::take(&mut body);
        }
        if title.is_empty() {
            return false;
        }
        let buttons: Arc<[String]> = if buttons.is_empty() {
            Arc::from([])
        } else {
            buttons
                .split('\u{2028}')
                .take(MAX_BUTTONS)
                .map(display_field)
                .collect()
        };
        if let Some(id) = draft.id.as_deref()
            && let Some(index) = self
                .kitty_notifications
                .live
                .iter()
                .position(|item| item.id.as_deref() == Some(id))
        {
            self.retire_notification(index);
        }
        if self.kitty_notifications.live.len() == CAPACITY {
            replies.extend(self.retire_notification(0).close_reply());
        }
        let sequence = self.next + 1;
        self.kitty_notifications.live.push_back(Live {
            sequence,
            id: draft.id.clone(),
            buttons: buttons.clone(),
            focus: draft.focus,
            report: draft.report,
            close: draft.close,
            deadline: (!draft.lifetime.is_zero()).then(|| now + draft.lifetime),
        });
        let data = Events::bounded_data(
            "notification",
            serde_json::json!({
                "protocol": "kitty", "id": draft.id, "title": title, "body": body, "buttons": buttons.as_ref(),
            }),
        );
        Events::append_locked(self, "notification", data, super::EventLinks::default());
        true
    }

    pub(super) fn is_kitty_notification_live(&self, sequence: u64) -> bool {
        self.kitty_notifications
            .live
            .iter()
            .any(|item| item.sequence == sequence)
    }
}

impl Events {
    pub(crate) fn kitty_notification(&self, metadata: &[u8], payload: &[u8]) -> Vec<String> {
        let Some(request) = Request::parse(metadata) else {
            return Vec::new();
        };
        let mut state = self.state.lock().unwrap();
        if state.kitty_notifications.connection.is_none() {
            return Vec::new();
        }
        if matches!(
            request.payload,
            Payload::Query | Payload::Alive | Payload::Close
        ) && !payload.is_empty()
        {
            return Vec::new();
        }
        let now = Instant::now();
        let mut replies = state.close_expired_notifications(now);
        match request.payload {
            Payload::Query => replies.push(response(request.id, ":p=?", CAPABILITIES)),
            Payload::Alive => {
                let ids = state
                    .kitty_notifications
                    .live
                    .iter()
                    .filter_map(|item| item.id.as_deref())
                    .collect::<Vec<_>>()
                    .join(",");
                replies.push(response(request.id, ":p=alive", &ids));
            }
            Payload::Close => {
                if let Some(id) = request.id {
                    state
                        .kitty_notifications
                        .pending
                        .retain(|item| item.id.as_deref() != Some(id));
                    if let Some(index) = state
                        .kitty_notifications
                        .live
                        .iter()
                        .position(|item| item.id.as_deref() == Some(id))
                    {
                        replies.extend(state.retire_notification(index).close_reply());
                    }
                }
            }
            _ => {
                let pending = &mut state.kitty_notifications.pending;
                let draft = pending
                    .iter()
                    .position(|item| item.id.as_deref() == request.id)
                    .and_then(|index| pending.remove(index));
                let mut draft = draft.unwrap_or_else(|| Draft::new(request.id));
                if draft.apply(&request, payload).is_some() {
                    if request.done {
                        if state.show_notification(draft, now, &mut replies) {
                            drop(state);
                            self.changed.notify_waiters();
                        }
                    } else {
                        if pending.len() == CAPACITY {
                            pending.pop_front();
                        }
                        pending.push_back(draft);
                    }
                }
            }
        }
        replies
    }

    pub fn set_notification_visibility(&self, focused: bool, visible: bool) {
        let mut state = self.state.lock().unwrap();
        state.kitty_notifications.focused = focused;
        state.kitty_notifications.visible = visible;
    }

    pub(crate) fn notification_stream(&self, connection: Option<u64>) {
        let mut state = self.state.lock().unwrap();
        state.kitty_notifications.connection = connection;
        state.clear_notifications();
    }

    pub(crate) fn reset_notifications(&self) {
        self.state.lock().unwrap().clear_notifications();
    }

    pub(crate) fn notification_evictions(&self) -> Arc<Notify> {
        self.state
            .lock()
            .unwrap()
            .kitty_notifications
            .eviction
            .clone()
    }

    pub(crate) fn next_notification_expiry(&self) -> Option<Instant> {
        let state = self.state.lock().unwrap();
        state
            .kitty_notifications
            .live
            .iter()
            .filter_map(|item| item.deadline)
            .min()
    }

    pub(crate) fn expire_notifications(&self) -> Option<NotificationResponse> {
        let mut state = self.state.lock().unwrap();
        let connection = state.kitty_notifications.connection?;
        let wire = state.close_expired_notifications(Instant::now());
        Some(NotificationResponse {
            connection,
            wire,
            focus: false,
        })
    }

    pub fn notification_buttons(&self, sequence: u64) -> Option<Arc<[String]>> {
        self.state
            .lock()
            .unwrap()
            .kitty_notifications
            .live
            .iter()
            .find(|item| item.sequence == sequence)
            .map(|item| item.buttons.clone())
    }

    pub fn act_on_notification(
        &self,
        sequence: u64,
        choice: NotificationChoice,
    ) -> Option<NotificationResponse> {
        let mut state = self.state.lock().unwrap();
        let connection = state.kitty_notifications.connection?;
        let index = state
            .kitty_notifications
            .live
            .iter()
            .position(|item| item.sequence == sequence)?;
        let item = &state.kitty_notifications.live[index];
        if !state.notification_displayed(sequence)
            || item
                .deadline
                .is_some_and(|deadline| deadline <= Instant::now())
        {
            return None;
        }
        let focus = matches!(choice, NotificationChoice::Activate(_)) && item.focus;
        let mut wire = Vec::new();
        if let NotificationChoice::Activate(button) = choice {
            let payload = if let Some(index) = button {
                let label = item.buttons.get(index)?;
                if label.is_empty() {
                    return None;
                }
                (index + 1).to_string()
            } else {
                String::new()
            };
            if item.report {
                wire.push(response(item.id.as_deref(), "", &payload));
            }
        }
        wire.extend(state.retire_notification(index).close_reply());
        Some(NotificationResponse {
            connection,
            wire,
            focus,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Inbox, NoticeTarget};

    fn make() -> (Events, Inbox) {
        let events = Events::default();
        let inbox = Inbox::new(|| {});
        events.connect_inbox(42, "test", &inbox);
        events.notification_stream(Some(3));
        (events, inbox)
    }

    fn seq(inbox: &Inbox) -> u64 {
        let NoticeTarget::Pane { sequence, .. } = inbox.rows().last().unwrap().target else {
            panic!("expected notification pane");
        };
        sequence
    }

    #[test]
    fn chunked_content_and_actions_use_connection_identity() {
        let (events, inbox) = make();
        let notify = |meta: &str, payload: &str| {
            events.kitty_notification(meta.as_bytes(), payload.as_bytes())
        };
        assert!(notify("i=build:p=title:d=0:a=-focus,report:c=1", "Build; ready").is_empty());
        assert!(notify("i=other:p=title:d=0", "Another job").is_empty());
        let raw = "Done 雪";
        let encoded = BASE64.encode(raw);
        notify("i=build:p=body:d=0:e=1", &encoded[..5]);
        notify("i=build:p=body:d=0:e=1", encoded[5..].trim_end_matches('='));
        assert!(inbox.rows().is_empty());
        notify("i=build:p=buttons", "Open\u{2028}Archive");
        let current = seq(&inbox);
        assert_eq!(inbox.rows()[0].message, "Build; ready · Done 雪");
        assert_eq!(
            &*events.notification_buttons(current).unwrap(),
            &["Open", "Archive"]
        );
        assert!(
            events
                .act_on_notification(current, NotificationChoice::Activate(Some(2)))
                .is_none()
        );
        let action = events
            .act_on_notification(current, NotificationChoice::Activate(Some(1)))
            .unwrap();
        assert!(!action.focus);
        assert_eq!(action.connection, 3);
        assert_eq!(
            action.wire,
            ["\x1b]99;i=build;2\x1b\\", "\x1b]99;i=build:p=close;\x1b\\"]
        );
        assert!(inbox.rows().is_empty());
        assert!(
            events
                .act_on_notification(current, NotificationChoice::Activate(None))
                .is_none()
        );
        assert_eq!(
            notify("i=probe:p=alive", ""),
            ["\x1b]99;i=probe:p=alive;\x1b\\"]
        );
        notify("i=other:p=body", "completed");
        assert_eq!(inbox.rows()[0].message, "Another job · completed");
    }

    #[test]
    fn replacement_and_reconnect_reject_old_buttons_and_retain_journal_entries() {
        let (events, inbox) = make();
        events.kitty_notification(b"i=same:c=1:a=report", b"Old");
        let old = seq(&inbox);
        assert!(events.kitty_notification(b"i=same:c=1", b"New").is_empty());
        let new = seq(&inbox);
        assert_ne!(old, new);
        assert_eq!(inbox.rows().len(), 1);
        assert!(
            events
                .act_on_notification(old, NotificationChoice::Activate(None))
                .is_none()
        );
        assert_eq!(
            events.kitty_notification(b"i=q:p=alive", b""),
            ["\x1b]99;i=q:p=alive;same\x1b\\"]
        );
        let new_inbox = Inbox::new(|| {});
        events.connect_inbox(42, "new window", &new_inbox);
        assert!(inbox.rows().is_empty());
        assert_eq!(seq(&new_inbox), new);
        events.notification_stream(None);
        assert!(new_inbox.rows().is_empty());
        assert!(events.kitty_notification(b"i=q:p=?", b"").is_empty());
        events.notification_stream(Some(4));
        assert!(
            events
                .act_on_notification(new, NotificationChoice::Activate(None))
                .is_none()
        );
        assert_eq!(events.since(0).events.len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn occasions_and_expiry_use_the_pane_and_monotonic_clock() {
        let (events, inbox) = make();
        events.set_notification_visibility(true, true);
        events.kitty_notification(b"i=focused:o=unfocused", b"Hidden");
        events.set_notification_visibility(false, true);
        events.kitty_notification(b"i=split:o=invisible", b"Also hidden");
        events.kitty_notification(b"i=split:o=unfocused", b"Visible split");
        events.set_notification_visibility(false, false);
        events.kitty_notification(b"i=hidden:o=invisible:c=1:w=20", b"Hidden tab");
        assert_eq!(inbox.rows().len(), 2);
        assert_eq!(
            events.next_notification_expiry(),
            Some(Instant::now() + Duration::from_millis(20))
        );
        tokio::time::advance(Duration::from_millis(20)).await;
        assert_eq!(
            events.expire_notifications().unwrap().wire,
            ["\x1b]99;i=hidden:p=close;\x1b\\"]
        );
        assert!(events.next_notification_expiry().is_none());
        assert_eq!(inbox.rows()[0].message, "Visible split");
    }

    #[test]
    fn invalid_ids_controls_and_reflected_replies_cannot_create_notifications() {
        let (events, inbox) = make();
        assert!(events.kitty_notification(b"i=A/B:p=?", b"").is_empty());
        assert!(
            events
                .kitty_notification(b"i=probe:p=?", CAPABILITIES.as_bytes())
                .is_empty()
        );
        events.kitty_notification(b"i=bad", b"control\x1b");
        events.kitty_notification(b"i=bad:e=1", b"abc!");
        events.kitty_notification(b"i=bad", &vec![b'a'; CHUNK_BYTES + 1]);
        events.kitty_notification(b"i=bad:p=unknown", b"Unknown payload");
        assert!(inbox.rows().is_empty());
        assert_eq!(
            events.kitty_notification(b"i=probe:p=?", b""),
            [response(Some("probe"), ":p=?", CAPABILITIES)]
        );
    }

    #[test]
    fn capacity_eviction_and_dismissal_close_only_retained_notifications() {
        let (events, inbox) = make();
        for i in 0..CAPACITY {
            assert!(
                events
                    .kitty_notification(format!("i={i}:c=1").as_bytes(), b"Queued")
                    .is_empty()
            );
        }
        assert_eq!(
            events.kitty_notification(b"i=last:c=1", b"Latest"),
            ["\x1b]99;i=0:p=close;\x1b\\"]
        );
        assert_eq!(inbox.rows().len(), CAPACITY);
        inbox.clear();
        let replies = events.expire_notifications().unwrap().wire;
        assert_eq!(replies.len(), CAPACITY);
        assert_eq!(replies.last().unwrap(), "\x1b]99;i=last:p=close;\x1b\\");
        assert!(inbox.rows().is_empty());
    }

    #[tokio::test]
    async fn an_unrelated_inbox_eviction_wakes_the_originating_worker() {
        let (events, inbox) = make();
        let retirement = events.notification_evictions();
        events.kitty_notification(b"i=evicted:c=1", b"Oldest alert");
        let old = seq(&inbox);
        let neighbor = Events::default();
        neighbor.connect_inbox(43, "other pane", &inbox);
        for _ in 0..128 {
            neighbor.push("bell", serde_json::json!({}));
        }
        tokio::time::timeout(Duration::from_secs(1), retirement.notified())
            .await
            .expect("eviction wakes the worker even without a notification timer");
        assert!(
            events
                .act_on_notification(old, NotificationChoice::Activate(None))
                .is_none()
        );
        assert_eq!(
            events.expire_notifications().unwrap().wire,
            ["\x1b]99;i=evicted:p=close;\x1b\\"]
        );
        assert_eq!(inbox.rows().len(), 128);
    }
}
