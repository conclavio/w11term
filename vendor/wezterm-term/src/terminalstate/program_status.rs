use base64::{engine::general_purpose, Engine};
use std::sync::Arc;

const MAX_RECORDS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProgramState {
    Idle,
    Done,
    Working,
    Blocked,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramBlockKind {
    Permission,
    Question,
    Auth,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramStatus {
    pub state: ProgramState,
    pub kind: Option<ProgramBlockKind>,
    pub progress: Option<u8>,
    pub app: Option<String>,
    pub title: Option<String>,
    pub message: Option<String>,
}

enum Update {
    Set(String, ProgramStatus),
    Clear(String),
}

fn valid_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'+' | b'-'))
}

fn decode_text(value: &str, encoded_limit: usize, decoded_limit: usize) -> Option<String> {
    if value.len() > encoded_limit {
        return None;
    }
    let bytes = if value.ends_with('=') {
        general_purpose::STANDARD.decode(value)
    } else {
        general_purpose::STANDARD_NO_PAD.decode(value)
    }
    .ok()?;
    let text = String::from_utf8(bytes).ok()?;
    (text.len() <= decoded_limit && !text.chars().any(char::is_control)).then_some(text)
}

fn parse(body: &[u8]) -> Option<Update> {
    let mut id = "";
    let mut state = None;
    let mut kind = None;
    let mut progress = None;
    let mut app = None;
    let mut title = None;
    let mut message = None;
    for pair in body.split(|&b| b == b':') {
        let Ok(pair) = std::str::from_utf8(pair) else {
            continue;
        };
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let key = key.trim_ascii();
        let value = value.trim_ascii();
        if key.len() > 16 {
            return None;
        }
        if key == "id"
            && (value.len() > 128
                || value.split('/').count() > 8
                || !value.split('/').all(valid_segment))
        {
            return None;
        }
        if !value.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b',' | b'+' | b'/' | b'=' | b'-')
        }) {
            continue;
        }
        match key {
            "id" => id = value,
            "state" => state = Some(value),
            "kind" => {
                kind = match value {
                    "permission" => Some(ProgramBlockKind::Permission),
                    "question" => Some(ProgramBlockKind::Question),
                    "auth" => Some(ProgramBlockKind::Auth),
                    _ => None,
                }
            }
            "progress" => {
                progress = value
                    .parse::<u8>()
                    .ok()
                    .filter(|&p| p <= 100 && value.bytes().all(|b| b.is_ascii_digit()))
            }
            "app" => {
                if value.len() > 32 {
                    return None;
                }
                app = valid_segment(value).then(|| value.to_owned());
            }
            "title" => title = Some(decode_text(value, 256, 192)?),
            "msg" => message = Some(decode_text(value, 2732, 2048)?),
            _ => {}
        }
    }
    let state = match state? {
        "clear" => return Some(Update::Clear(id.into())),
        "idle" => ProgramState::Idle,
        "done" => ProgramState::Done,
        "working" => ProgramState::Working,
        "blocked" => ProgramState::Blocked,
        "error" => ProgramState::Error,
        _ => return None,
    };
    let status = ProgramStatus {
        state,
        kind: kind.filter(|_| state == ProgramState::Blocked),
        progress: progress
            .filter(|_| matches!(state, ProgramState::Working | ProgramState::Blocked)),
        app,
        title,
        message,
    };
    Some(Update::Set(id.into(), status))
}

#[derive(Default)]
pub(super) struct ProgramStatuses {
    records: Vec<(String, ProgramStatus)>,
    pub(super) seen_report: bool,
    pub(super) summary: Option<Arc<ProgramStatus>>,
}

impl ProgramStatuses {
    pub(super) fn apply(&mut self, body: &[u8]) {
        let Some(update) = parse(body) else { return };
        self.seen_report = true;
        match update {
            Update::Set(id, status) => {
                if let Some(old) = self.records.iter().position(|(key, _)| *key == id) {
                    self.records.remove(old);
                } else if self.records.len() == MAX_RECORDS {
                    self.records.remove(0);
                }
                self.records.push((id, status));
            }
            Update::Clear(id) => self.records.retain(|(key, _)| {
                !id.is_empty()
                    && key != &id
                    && !key.strip_prefix(&id).is_some_and(|s| s.starts_with('/'))
            }),
        }
        self.refresh_summary();
    }

    pub(super) fn process_ended(&mut self) {
        let before = self.records.len();
        self.records
            .retain(|(_, status)| matches!(status.state, ProgramState::Done | ProgramState::Error));
        if self.records.len() != before {
            self.refresh_summary();
        }
    }

    fn refresh_summary(&mut self) {
        let Some((id, status)) = self
            .records
            .iter()
            .max_by_key(|(_, status)| status.state)
            .filter(|(_, status)| status.state != ProgramState::Idle)
        else {
            self.summary = None;
            return;
        };
        let mut status = status.clone();
        let mut parent = id.as_str();
        while status.app.is_none() && !parent.is_empty() {
            parent = parent.rsplit_once('/').map_or("", |(base, _)| base);
            status.app = self
                .records
                .iter()
                .find(|(key, _)| key == parent)
                .and_then(|(_, ancestor)| ancestor.app.clone());
        }
        if self.summary.as_deref() != Some(&status) {
            self.summary = Some(Arc::new(status));
        }
    }
}
