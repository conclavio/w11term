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
        algorithm: String,
        fingerprint: String,
    },
    Authentication {
        name: String,
        instructions: String,
        challenges: Vec<Challenge>,
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
    pub async fn ask(&self, question: Question) -> Result<Answer> {
        if let Question::Authentication {
            name,
            instructions,
            challenges,
        } = &question
        {
            ensure!(
                name.len() <= 1024
                    && instructions.len() <= 8192
                    && challenges.len() <= 16
                    && challenges.iter().all(|c| c.label.len() <= 4096),
                "authentication challenge exceeds display limits"
            );
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
