//! A pane can explicitly lend an authenticated connection to a new shell
//! channel. No global host cache or saved authentication answers are involved.
use super::Handler;
use crate::{options::SshOptions, prompts::Prompts};
use anyhow::{Context, Result};
use russh::{Channel, client};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, Weak};
use tokio::{runtime, sync::oneshot};

// A socket and its russh driver must outlive whichever terminal opened it.
// Terminal parsing and presentation continue on each pane's own worker.
pub(crate) static NETWORK: LazyLock<runtime::Runtime> = LazyLock::new(|| {
    runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("w11term-ssh")
        .enable_all()
        .build()
        .expect("creating SSH network runtime")
});

struct Link {
    // A forwarded connection owns its authenticated upstream transport.
    _upstream: Option<ConnectionShare>,
    handle: Arc<client::Handle<Handler>>,
    fingerprint: String,
    options: SshOptions,
    known_hosts: Option<PathBuf>,
    runtime: runtime::Handle,
}

impl Drop for Link {
    fn drop(&mut self) {
        let handle = self.handle.clone();
        self.runtime.spawn(async move {
            let _ = handle
                .disconnect(russh::Disconnect::ByApplication, "Last pane closed", "en")
                .await;
        });
    }
}

#[derive(Clone)]
pub struct ConnectionShare(Arc<Link>);

impl ConnectionShare {
    pub(super) fn new(
        handle: client::Handle<Handler>,
        options: SshOptions,
        prompts: Option<&Prompts>,
        fingerprint: String,
        upstream: Option<ConnectionShare>,
    ) -> Self {
        Self(Arc::new(Link {
            handle: Arc::new(handle),
            fingerprint,
            _upstream: upstream,
            options,
            known_hosts: prompts.and_then(Prompts::known_hosts).map(PathBuf::from),
            runtime: runtime::Handle::current(),
        }))
    }
    pub(super) fn matches(&self, options: &SshOptions, prompts: Option<&Prompts>) -> bool {
        let connected = &self.0.options;
        !self.0.handle.is_closed()
            && connected.host == options.host
            && connected.port == options.port
            && connected.user == options.user
            && connected.authentication == options.authentication
            && connected.host_key == options.host_key
            && connected.proxy_jump == options.proxy_jump
            && self.0.known_hosts.as_deref() == prompts.and_then(Prompts::known_hosts)
    }
    pub(super) fn handle(&self) -> Arc<client::Handle<Handler>> {
        self.0.handle.clone()
    }
    pub(super) fn fingerprint(&self) -> &str {
        &self.0.fingerprint
    }
    pub(super) async fn open_shell(&self) -> Result<ShellChannel> {
        // russh doesn't close a bare Channel on drop. An opener cancelled
        // during channel confirmation still has to close a later acceptance.
        let handle = self.0.handle.clone();
        let (send, receive) = oneshot::channel();
        self.0.runtime.spawn(async move {
            let result = handle
                .channel_open_session()
                .await
                .map(|channel| ShellChannel(Some(channel)));
            let _ = send.send(result);
        });
        Ok(receive.await.context("SSH channel opener stopped")??)
    }
}

pub(super) struct ShellChannel(Option<Channel<client::Msg>>);

impl ShellChannel {
    pub(super) fn channel(&mut self) -> &mut Channel<client::Msg> {
        self.0.as_mut().unwrap()
    }
}

impl Drop for ShellChannel {
    fn drop(&mut self) {
        if let Some(channel) = self.0.take() {
            // ChannelStream supplies russh's asynchronous CLOSE guard. Pane
            // transports run on the shared runtime even during tab teardown.
            drop(channel.into_stream());
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct ShareSlot(Arc<Mutex<Weak<ConnectionShare>>>);

impl ShareSlot {
    pub(crate) fn current(&self) -> Option<ConnectionShare> {
        self.0
            .lock()
            .unwrap()
            .upgrade()
            .filter(|share| !share.0.handle.is_closed())
            .map(|share| (*share).clone())
    }
    pub(super) fn publish(&self, local: &Arc<ConnectionShare>) {
        // Each pane owns its own outer Arc. Its offer expires on disconnect
        // even when other panes continue to use the underlying Link.
        *self.0.lock().unwrap() = Arc::downgrade(local);
    }
}

#[derive(Default)]
pub(crate) struct Sharing {
    pub published: ShareSlot,
    pub offered: Option<ConnectionShare>,
    pub durable: Option<super::durability::Worker>,
}
