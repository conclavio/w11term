use super::*;
use crate::{options::SshOptions, ssh::transport};
use russh::{
    Channel, ChannelMsg,
    keys::{Algorithm, HashAlg, PrivateKey},
    server,
};
use std::sync::Arc;
use tokio::{io::AsyncReadExt, sync::mpsc};

type Offer = (Channel<server::Msg>, server::ChannelOpenHandle);

struct Peer(mpsc::UnboundedSender<Offer>);

impl server::Handler for Peer {
    type Error = anyhow::Error;

    async fn auth_none(&mut self, _: &str) -> Result<server::Auth> {
        Ok(server::Auth::Accept)
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<server::Msg>,
        host: &str,
        port: u32,
        origin: &str,
        source: u32,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> Result<()> {
        assert_eq!((host, port, origin), ("127.0.0.1", 10022, "127.0.0.1"));
        assert!(source > 0);
        self.0
            .send((channel, reply))
            .map_err(|_| anyhow::anyhow!("test stopped"))?;
        Ok(())
    }
}

async fn peer() -> Result<(ConnectionShare, mpsc::UnboundedReceiver<Offer>)> {
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
    let options: SshOptions = serde_json::from_value(serde_json::json!({
        "host": "fixture", "user": "fixture", "auth": "none",
        "host_key": key.public_key().fingerprint(HashAlg::Sha256).to_string(),
    }))?;
    let (client, server_io) = tokio::io::duplex(65_536);
    let (offers, pending) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let config = Arc::new(server::Config {
            keys: vec![key],
            ..Default::default()
        });
        server::run_stream(config, server_io, Peer(offers))
            .await?
            .await
    });
    let (handle, fingerprint) =
        transport::handshake(&options, None, &mut None, None, client, None).await?;
    Ok((
        ConnectionShare::new(handle, options, None, fingerprint, None),
        pending,
    ))
}

#[tokio::test]
async fn replacing_one_panes_publication_revokes_its_listeners_and_releases_its_limit() -> Result<()>
{
    tokio::time::timeout(Duration::from_secs(10), async {
        let (link, _offers) = peer().await?;
        let owner = ShareSlot::default();
        let first = Arc::new(link.clone());
        let first_lifetime = owner.shell_lifetime(&first);
        owner.publish(&first, 10);
        let listeners = (0..MAX_LISTENERS_PER_PANE)
            .map(|_| LocalForward::start(&owner, 10, 0, 10022))
            .collect::<Result<Vec<_>>>()?;
        assert!(LocalForward::start(&owner, 10, 0, 10022).is_err());
        assert!(LocalForward::start(&owner, 11, 0, 10022).is_err());
        let sibling = ShareSlot::default();
        sibling.publish(&first, 10);
        let independent = LocalForward::start(&sibling, 10, 0, 10022)?;

        let next = Arc::new(link);
        owner.publish(&next, 11);
        for forward in &listeners {
            forward.subscribe().wait_for(|s| !s.running).await?;
        }
        assert!(independent.status().running);
        assert!(LocalForward::start(&owner, 10, 0, 10022).is_err());
        let rebound = LocalForward::start(&owner, 11, listeners[0].local_port(), 10022)?;
        drop(first_lifetime);
        assert!(owner.forward_lease(11).is_some());
        assert!(rebound.status().running);
        anyhow::Ok(())
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn pending_confirms_are_bounded_across_stop_and_late_success_closes_every_ssh_channel()
-> Result<()> {
    tokio::time::timeout(Duration::from_secs(15), async {
        let (link, mut offers) = peer().await?;
        let link = Arc::new(link);
        let owner = ShareSlot::default();
        owner.publish(&link, 1);
        let forward = LocalForward::start(&owner, 1, 0, 10022)?;
        let address = (Ipv4Addr::LOCALHOST, forward.local_port());
        let mut clients = Vec::new();
        let mut pending = Vec::new();
        for _ in 0..MAX_CLIENTS_PER_LISTENER {
            clients.push(TcpStream::connect(address).await?);
            pending.push(offers.recv().await.unwrap());
        }
        let mut excess = TcpStream::connect(address).await?;
        let mut status = forward.subscribe();
        status
            .wait_for(|s| {
                s.last_error
                    .as_deref()
                    .is_some_and(|e| e.contains("client limit"))
            })
            .await?;
        assert_eq!(status.borrow().active_clients, MAX_CLIENTS_PER_LISTENER);
        assert!(matches!(excess.read(&mut [0]).await, Ok(0) | Err(_)));
        assert!(offers.try_recv().is_err());

        forward.stop();
        status.wait_for(|s| !s.running).await?;
        assert_eq!(status.borrow().active_clients, 0);
        assert!(TcpStream::connect(address).await.is_err());
        for mut socket in clients {
            assert!(matches!(socket.read(&mut [0]).await, Ok(0) | Err(_)));
        }
        assert_eq!(
            link.forward_channels().available_permits(),
            MAX_CHANNELS_PER_LINK - pending.len()
        );
        // Even a new listener must account for the old SSH opens awaiting confirmation.
        let other_clients = link
            .forward_channels()
            .clone()
            .try_acquire_many_owned((MAX_CHANNELS_PER_LINK - pending.len()) as u32)?;
        let new_forward = LocalForward::start(&owner, 1, 0, 10022)?;
        let mut blocked =
            TcpStream::connect((Ipv4Addr::LOCALHOST, new_forward.local_port())).await?;
        new_forward
            .subscribe()
            .wait_for(|s| {
                s.last_error
                    .as_deref()
                    .is_some_and(|m| m.contains("channel limit"))
            })
            .await?;
        assert!(matches!(blocked.read(&mut [0]).await, Ok(0) | Err(_)));
        assert!(offers.try_recv().is_err());
        drop(other_clients);
        for (mut channel, confirm) in pending {
            confirm.accept().await;
            assert!(matches!(channel.wait().await, Some(ChannelMsg::Close)));
        }
        assert_eq!(
            link.forward_channels().available_permits(),
            MAX_CHANNELS_PER_LINK
        );
        anyhow::Ok(())
    })
    .await??;
    Ok(())
}
