//! Authentication/trust questions cross to the UI; answers never enter terminal input.
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

static NEXT: AtomicU64 = AtomicU64::new(1);

fn is_bidi_or_separator(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
    )
}

/// Sanitize untrusted single-line text (e.g. remote prompt names/labels) by
/// stripping control characters and Unicode BiDi override/isolate characters.
pub fn sanitize_single_line(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut last_space = true;
    let mut count = 0;
    for ch in text.chars() {
        if count >= max_chars {
            break;
        }
        if is_bidi_or_separator(ch) {
            continue;
        }
        if matches!(ch, '\r' | '\n' | '\t') || ch.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
                count += 1;
            }
            continue;
        }
        if ch.is_control() {
            continue;
        }
        out.push(ch);
        last_space = false;
        count += 1;
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Sanitize untrusted multi-line instructions by normalizing line endings,
/// stripping BiDi overrides and control characters, and bounding line/char counts.
pub fn sanitize_multiline(text: &str, max_chars: usize, max_lines: usize) -> String {
    let mut out = String::new();
    let mut chars = 0;
    let mut lines = 1;
    let mut iter = text.chars().peekable();
    while let Some(ch) = iter.next() {
        if chars >= max_chars {
            break;
        }
        if is_bidi_or_separator(ch) {
            continue;
        }
        match ch {
            '\r' => {
                if iter.peek() == Some(&'\n') {
                    let _ = iter.next();
                }
                if lines < max_lines {
                    out.push('\n');
                    lines += 1;
                    chars += 1;
                }
            }
            '\n' => {
                if lines < max_lines {
                    out.push('\n');
                    lines += 1;
                    chars += 1;
                }
            }
            '\t' => {
                out.push(' ');
                chars += 1;
            }
            c if c.is_control() => {}
            c => {
                out.push(c);
                chars += 1;
            }
        }
    }
    out.trim().to_string()
}
#[derive(Clone, Debug, Serialize)]
pub struct Challenge {
    pub label: String,
    pub echo: bool,
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Question {
    HostKey {
        host: String,
        port: u16,
        #[serde(skip_serializing_if = "Option::is_none")]
        host_key_alias: Option<String>,
        algorithm: String,
        fingerprint: String,
    },
    Authentication {
        name: String,
        instructions: String,
        challenges: Vec<Challenge>,
    },
    IdentityPassphrase {
        identity: String,
        retry: bool,
    },
}
#[derive(Clone, Debug, Serialize)]
pub struct Prompt {
    pub id: u64,
    pub question: Question,
}
pub enum Answer {
    Trust(bool),
    Authentication(Vec<Zeroizing<String>>),
    IdentityPassphrase(Zeroizing<String>),
    Cancel,
}
impl std::fmt::Debug for Answer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<private prompt answer>")
    }
}
#[derive(Default)]
struct State {
    prompt: Option<Prompt>,
    reply: Option<oneshot::Sender<Answer>>,
}
#[derive(Clone, Default)]
pub struct Prompts(
    Arc<Mutex<State>>,
    Arc<tokio::sync::Notify>,
    Arc<Option<std::path::PathBuf>>,
);
impl Prompts {
    pub fn current(&self) -> Option<Prompt> {
        self.0.lock().unwrap().prompt.clone()
    }
    pub fn with_known_hosts(path: std::path::PathBuf) -> Self {
        Self(Default::default(), Default::default(), Arc::new(Some(path)))
    }
    pub fn known_hosts(&self) -> Option<&std::path::Path> {
        self.2.as_deref()
    }
    pub async fn changed(&self) {
        self.1.notified().await;
    }
    pub fn answer(&self, id: u64, answer: Answer) -> Result<()> {
        let mut state = self.0.lock().unwrap();
        ensure!(
            state.prompt.as_ref().is_some_and(|p| p.id == id),
            "authentication prompt has expired"
        );
        state.prompt = None;
        self.1.notify_one();
        state
            .reply
            .take()
            .context("authentication prompt is closed")?
            .send(answer)
            .map_err(|_| anyhow::anyhow!("authentication prompt is closed"))
    }
    pub fn cancel(&self) {
        let mut state = self.0.lock().unwrap();
        state.prompt = None;
        state.reply.take();
        self.1.notify_one();
    }
    pub async fn ask(&self, mut question: Question) -> Result<Answer> {
        if let Question::Authentication {
            name,
            instructions,
            challenges,
        } = &mut question
        {
            ensure!(
                name.len() <= 1024
                    && instructions.len() <= 8192
                    && challenges.len() <= 16
                    && challenges.iter().all(|c| c.label.len() <= 4096),
                "authentication challenge exceeds display limits"
            );
            *name = sanitize_single_line(name, 256);
            *instructions = sanitize_multiline(instructions, 2048, 24);
            for challenge in challenges.iter_mut() {
                challenge.label = sanitize_single_line(&challenge.label, 256);
            }
        }
        if let Question::IdentityPassphrase { identity, .. } = &mut question {
            *identity = sanitize_single_line(identity, 1024);
        }
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let (reply, response) = oneshot::channel();
        {
            let mut state = self.0.lock().unwrap();
            ensure!(
                state.reply.is_none(),
                "another authentication prompt is pending"
            );
            state.prompt = Some(Prompt { id, question });
            state.reply = Some(reply);
        }
        self.1.notify_one();
        struct Clear(Prompts, u64);
        impl Drop for Clear {
            fn drop(&mut self) {
                let mut state = self.0.0.lock().unwrap();
                if state.prompt.as_ref().is_some_and(|p| p.id == self.1) {
                    state.prompt = None;
                    state.reply.take();
                    self.0.1.notify_one();
                }
            }
        }
        let _clear = Clear(self.clone(), id);
        tokio::time::timeout(std::time::Duration::from_secs(300), response)
            .await
            .context("authentication question timed out")?
            .context("authentication cancelled")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_sanitizers_strip_bidi_overrides_controls_and_bound_lines() {
        assert_eq!(
            sanitize_single_line("MFA\u{202E}gnifoops\r\n\x1b[31mPrompt\x07", 256),
            "MFAgnifoops [31mPrompt"
        );
        let many_lines = (0..40)
            .map(|i| format!("line-{i}\u{202E}"))
            .collect::<Vec<_>>()
            .join("\r\n");
        let sanitized = sanitize_multiline(&many_lines, 2048, 24);
        assert!(!sanitized.contains('\u{202E}'));
        assert!(!sanitized.contains('\r'));
        assert_eq!(sanitized.lines().count(), 24);
    }

    #[tokio::test]
    async fn ask_sanitizes_authentication_prompt_fields() {
        let prompts = Prompts::default();
        let waiter = prompts.clone();
        let handle = tokio::spawn(async move {
            waiter
                .ask(Question::Authentication {
                    name: "Sign in\u{202E}\nspoof".into(),
                    instructions: "Remote\x1b[2J\u{2066}body".into(),
                    challenges: vec![Challenge {
                        label: "Code\r\n\u{202E}:".into(),
                        echo: false,
                    }],
                })
                .await
        });
        prompts.changed().await;
        let current = prompts.current().expect("pending prompt");
        let Question::Authentication {
            name,
            instructions,
            challenges,
        } = current.question
        else {
            panic!("expected authentication question");
        };
        assert_eq!(name, "Sign in spoof");
        assert_eq!(instructions, "Remote[2Jbody");
        assert_eq!(challenges[0].label, "Code :");
        prompts.cancel();
        assert!(handle.await.unwrap().is_err());
    }
}
