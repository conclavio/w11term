use super::{ConnectionShare, NETWORK, ShareSlot};
use anyhow::{Context, Result, ensure};
use std::{net::Ipv4Addr, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, oneshot, watch},
    task::JoinSet,
};

pub(super) const MAX_LISTENERS_PER_PANE: usize = 8;
pub(super) const MAX_CHANNELS_PER_LINK: usize = 64;
const MAX_CLIENTS_PER_LISTENER: usize = 16;
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalForwardStatus {
    pub running: bool,
    /// Includes local clients waiting for SSH channel confirmation.
    pub active_clients: usize,
    pub last_error: Option<String>,
}

/// A manually opened Windows loopback listener owned by one authenticated pane.
/// Dropping the handle stops the listener and closes its clients.
pub struct LocalForward {
    local: u16,
    remote: u16,
    stop: watch::Sender<()>,
    state: watch::Receiver<LocalForwardStatus>,
}

impl LocalForward {
    pub(crate) fn start(
        owner: &ShareSlot,
        generation: u64,
        local: u16,
        remote: u16,
    ) -> Result<Self> {
        ensure!(remote != 0, "Enter a remote port from 1 to 65535");
        let (link, revoked) = owner
            .forward_lease(generation)
            .context("The original SSH connection is no longer available")?;
        let permit = owner
            .forward_listeners
            .clone()
            .try_acquire_owned()
            .with_context(|| {
                format!("At most {MAX_LISTENERS_PER_PANE} local forwards can run in one pane")
            })?;
        let socket = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, local))
            .with_context(|| format!("Could not listen on 127.0.0.1:{local}"))?;
        let local = socket.local_addr()?.port();
        socket.set_nonblocking(true)?;
        let _runtime = NETWORK.enter();
        let listener = TcpListener::from_std(socket)?;
        ensure!(
            matches!(revoked.has_changed(), Ok(false)),
            "The original SSH connection is no longer available"
        );
        let (stop, stopping) = watch::channel(());
        let (updates, state) = watch::channel(LocalForwardStatus {
            running: true,
            active_clients: 0,
            last_error: None,
        });
        NETWORK.spawn(serve(
            listener, remote, link, stopping, revoked, updates, permit,
        ));
        Ok(Self {
            local,
            remote,
            stop,
            state,
        })
    }

    pub fn local_port(&self) -> u16 {
        self.local
    }
    pub fn remote_port(&self) -> u16 {
        self.remote
    }
    pub fn subscribe(&self) -> watch::Receiver<LocalForwardStatus> {
        let mut changes = self.state.clone();
        changes.borrow_and_update();
        changes
    }
    pub fn status(&self) -> LocalForwardStatus {
        self.state.borrow().clone()
    }
    pub fn stop(&self) {
        self.stop.send_replace(());
    }
}

async fn serve(
    listener: TcpListener,
    remote: u16,
    link: ConnectionShare,
    mut stopping: watch::Receiver<()>,
    mut revoked: watch::Receiver<()>,
    state: watch::Sender<LocalForwardStatus>,
    listener_permit: OwnedSemaphorePermit,
) {
    let mut clients = JoinSet::new();
    let final_error = loop {
        tokio::select! {
            biased;
            _ = stopping.changed() => break None,
            _ = revoked.changed() => break Some("The owning SSH connection closed or changed".to_owned()),
            Some(done) = clients.join_next(), if !clients.is_empty() => {
                let result = done.context("Forwarded client stopped").and_then(|result| result);
                state.send_modify(|status| {
                    status.active_clients = clients.len();
                    if let Err(error) = result {
                        status.last_error = Some(format!("{error:#}"));
                    }
                });
            }
            result = listener.accept() => {
                let (socket, origin) = match result {
                    Ok(client) => client,
                    Err(error) => break Some(format!("Could not accept a local connection: {error}")),
                };
                if clients.len() >= MAX_CLIENTS_PER_LISTENER {
                    report_limit(&state, "This forward has reached its local client limit");
                    continue;
                }
                let Ok(channel_permit) = link.forward_channels().clone().try_acquire_owned() else {
                    report_limit(&state, "This SSH connection has reached its forwarding channel limit");
                    continue;
                };
                clients.spawn(relay(socket, origin.port(), link.clone(), remote, channel_permit));
                state.send_modify(|status| status.active_clients = clients.len());
            }
        }
    };
    drop(listener);
    clients.shutdown().await;
    drop(listener_permit);
    state.send_modify(|status| {
        status.running = false;
        status.active_clients = 0;
        if final_error.is_some() {
            status.last_error = final_error;
        }
    });
}

fn report_limit(state: &watch::Sender<LocalForwardStatus>, message: &str) {
    state.send_if_modified(|status| {
        if status.last_error.as_deref() == Some(message) {
            return false;
        }
        status.last_error = Some(message.into());
        true
    });
}

async fn relay(
    mut socket: TcpStream,
    origin: u16,
    link: ConnectionShare,
    remote: u16,
    permit: OwnedSemaphorePermit,
) -> Result<()> {
    let handle = link.handle();
    // Dropping russh's open future would abandon a late confirmation without
    // sending CLOSE. Keep just the bounded opener alive, without retaining the
    // pane, local socket or Link. A late ChannelStream closes on drop.
    let (send, receive) = oneshot::channel();
    NETWORK.spawn(async move {
        if send.is_closed() {
            return;
        }
        let opened = handle
            .channel_open_direct_tcpip(
                "127.0.0.1",
                u32::from(remote),
                "127.0.0.1",
                u32::from(origin),
            )
            .await
            .map(|channel| (channel.into_stream(), permit));
        let _ = send.send(opened);
    });
    let (mut stream, _permit) = tokio::time::timeout(OPEN_TIMEOUT, receive)
        .await
        .with_context(|| format!("Timed out opening remote 127.0.0.1:{remote}"))?
        .context("SSH forwarding channel opener stopped")?
        .with_context(|| format!("Could not open remote 127.0.0.1:{remote}"))?;
    tokio::io::copy_bidirectional(&mut socket, &mut stream)
        .await
        .with_context(|| format!("Forwarded connection to remote 127.0.0.1:{remote} failed"))?;
    Ok(())
}

#[cfg(test)]
mod tests;
