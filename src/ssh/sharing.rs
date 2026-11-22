//! A pane can explicitly lend an authenticated connection to a new shell
//! channel. No global host cache or saved authentication answers are involved.
use super::Handler;
use crate::{options::SshOptions, prompts::Prompts};
use anyhow::{Context, Result};
use russh::{Channel, client};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, Weak};
use tokio::{
    runtime,
    sync::{Semaphore, oneshot, watch},
};

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
    forward_channels: Arc<Semaphore>,
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
            forward_channels: Arc::new(Semaphore::new(super::local_forward::MAX_CHANNELS_PER_LINK)),
        }))
    }
    pub fn matches(&self, options: &SshOptions, prompts: Option<&Prompts>) -> bool {
        let connected = &self.0.options;
        !self.0.handle.is_closed()
            && !connected.player.rustmush
            && !options.player.rustmush
            && connected.host == options.host
            && connected.port == options.port
            && connected.user == options.user
            && connected.authentication == options.authentication
            && connected.host_key == options.host_key
            && connected.host_key_alias == options.host_key_alias
            && connected.proxy_jump == options.proxy_jump
            && self.0.known_hosts.as_deref() == prompts.and_then(Prompts::known_hosts)
    }
    pub(super) fn handle(&self) -> Arc<client::Handle<Handler>> {
        self.0.handle.clone()
    }
    pub(super) fn forward_channels(&self) -> &Arc<Semaphore> {
        &self.0.forward_channels
    }
    pub(super) fn fingerprint(&self) -> &str {
        &self.0.fingerprint
    }
    pub fn options(&self) -> &SshOptions {
        &self.0.options
    }
    pub(crate) fn verified_endpoint(&self) -> (&str, &str, u16, &str) {
        let options = &self.0.options;
        (
            &options.host,
            &options.user,
            options.port,
            &self.0.fingerprint,
        )
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
                .map(|channel| ShellChannel {
                    channel: Some(channel),
                    game_connection: None,
                });
            let _ = send.send(result);
        });
        Ok(receive.await.context("SSH channel opener stopped")??)
    }
}

pub(super) struct ShellChannel {
    channel: Option<Channel<client::Msg>>,
    game_connection: Option<ConnectionShare>,
}

impl ShellChannel {
    pub(super) fn into_stream(mut self) -> russh::ChannelStream<client::Msg> {
        self.channel.take().unwrap().into_stream()
    }
    pub(super) fn channel(&mut self) -> &mut Channel<client::Msg> {
        self.channel.as_mut().unwrap()
    }
    pub(super) fn close_game_session_on_drop(&mut self, link: ConnectionShare) {
        self.game_connection = Some(link);
    }
    /// Queue CLOSE in order with the caller's next channel open. If the send
    /// is cancelled, the usual drop guard still releases the channel.
    pub(super) async fn close(mut self) {
        if self.channel().close().await.is_ok() {
            self.channel.take();
        }
    }
}

impl Drop for ShellChannel {
    fn drop(&mut self) {
        if let Some(mut channel) = self.channel.take() {
            if let Some(link) = self.game_connection.take() {
                // An explicit pane close revokes its game lease. Hold the
                // transport until EOF/CLOSE have been queued before DISCONNECT.
                let runtime = link.0.runtime.clone();
                runtime.spawn(async move {
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), async {
                        let _ = channel.eof().await;
                        let _ = channel.close().await;
                        while let Some(message) = channel.wait().await {
                            if matches!(message, russh::ChannelMsg::Close) {
                                break;
                            }
                        }
                    })
                    .await;
                    drop(link);
                });
                return;
            }
            // ChannelStream supplies russh's asynchronous CLOSE guard. Pane
            // transports run on the shared runtime even during tab teardown.
            drop(channel.into_stream());
        }
    }
}

#[derive(Default)]
struct Publication {
    generation: u64,
    current: Weak<ConnectionShare>,
}

#[derive(Clone)]
pub(crate) struct ShareSlot {
    publication: Arc<Mutex<Publication>>,
    changed: watch::Sender<()>,
    pub(super) forward_listeners: Arc<Semaphore>,
}

impl Default for ShareSlot {
    fn default() -> Self {
        Self {
            publication: Default::default(),
            changed: Default::default(),
            forward_listeners: Arc::new(Semaphore::new(
                super::local_forward::MAX_LISTENERS_PER_PANE,
            )),
        }
    }
}

pub(super) struct PublishedShell {
    slot: ShareSlot,
    link: Weak<ConnectionShare>,
}

impl Drop for PublishedShell {
    fn drop(&mut self) {
        let mut state = self.slot.publication.lock().unwrap();
        if state.current.ptr_eq(&self.link) {
            state.current = Weak::new();
            self.slot.changed.send_replace(());
        }
    }
}

impl ShareSlot {
    /// Sample the published shell and subscribe under the publication lock: the
    /// next notification retires this authority, even if a sibling retains the transport.
    pub(super) fn forward_lease(
        &self,
        generation: u64,
    ) -> Option<(ConnectionShare, watch::Receiver<()>)> {
        let state = self.publication.lock().unwrap();
        let link = state.current.upgrade()?;
        (state.generation == generation && !link.0.handle.is_closed())
            .then(|| ((*link).clone(), self.changed.subscribe()))
    }
    pub(crate) fn current(&self) -> Option<ConnectionShare> {
        self.generation().map(|(_, link)| link)
    }
    pub(crate) fn generation(&self) -> Option<(u64, ConnectionShare)> {
        let state = self.publication.lock().unwrap();
        state
            .current
            .upgrade()
            .filter(|share| !share.0.handle.is_closed())
            .map(|share| (state.generation, (*share).clone()))
    }
    pub(crate) fn subscribe(&self) -> watch::Receiver<()> {
        self.changed.subscribe()
    }
    pub(super) fn shell_lifetime(&self, link: &Arc<ConnectionShare>) -> PublishedShell {
        PublishedShell {
            slot: self.clone(),
            link: Arc::downgrade(link),
        }
    }
    pub(super) fn publish(&self, local: &Arc<ConnectionShare>, generation: u64) {
        // Each pane owns its own outer Arc. Its offer expires on disconnect
        // even when other panes continue to use the underlying Link.
        let mut state = self.publication.lock().unwrap();
        state.generation = generation;
        state.current = Arc::downgrade(local);
        self.changed.send_replace(());
    }
}

#[derive(Default)]
pub(crate) struct Sharing {
    pub startup: Option<crate::workspace::SshStartup>,
    pub published: ShareSlot,
    pub offered: Option<ConnectionShare>,
    pub durable: Option<super::durability::Worker>,
    generations: Arc<std::sync::atomic::AtomicU64>,
}

impl Sharing {
    pub(crate) fn new(
        published: ShareSlot,
        offered: Option<ConnectionShare>,
        durable: Option<super::durability::Worker>,
        startup: Option<crate::workspace::SshStartup>,
    ) -> Self {
        Self {
            published,
            offered,
            durable,
            startup,
            ..Default::default()
        }
    }

    /// Only the original launch can consume a shared connection or attach worker.
    pub(crate) fn for_launch(&mut self) -> Self {
        Self {
            published: self.published.clone(),
            offered: self.offered.take(),
            durable: self.durable.take(),
            startup: self.startup.clone(),
            generations: self.generations.clone(),
        }
    }

    pub(crate) fn next_generation(&self) -> u64 {
        self.generations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1
    }
}
