use std::{
    future::{Future, pending},
    io,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::{oneshot, watch},
    time::{Instant, sleep_until},
};

pub(super) async fn expired(clock: &mut watch::Receiver<Option<Instant>>) {
    loop {
        let deadline = *clock.borrow_and_update();
        tokio::select! {
            biased;
            change = clock.changed() => {
                if change.is_err() {
                    // The connection future supplies the actual failure.
                    return pending().await;
                }
            }
            () = async {
                match deadline {
                    Some(deadline) => sleep_until(deadline).await,
                    None => pending().await,
                }
            } => return,
        }
    }
}

// russh owns the stream in a spawned task while key exchange is pending.
// Cancelling connect_stream alone can leave that task waiting on the peer.
pub(super) struct HandshakeStream<S> {
    inner: S,
    handshake: Handshake,
}

enum Handshake {
    Pending(oneshot::Receiver<()>),
    Complete,
    Cancelled,
}

impl<S> HandshakeStream<S> {
    pub(super) fn new(inner: S) -> (Self, oneshot::Sender<()>) {
        let (complete, signal) = oneshot::channel();
        (
            Self {
                inner,
                handshake: Handshake::Pending(signal),
            },
            complete,
        )
    }

    fn check(&mut self, context: &mut Context<'_>) -> io::Result<()> {
        if let Handshake::Pending(signal) = &mut self.handshake {
            match Pin::new(signal).poll(context) {
                Poll::Pending => {}
                Poll::Ready(Ok(())) => self.handshake = Handshake::Complete,
                Poll::Ready(Err(_)) => self.handshake = Handshake::Cancelled,
            }
        }
        match self.handshake {
            Handshake::Cancelled => Err(io::ErrorKind::ConnectionAborted.into()),
            _ => Ok(()),
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for HandshakeStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.check(context)?;
        Pin::new(&mut self.inner).poll_read(context, buffer)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for HandshakeStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.check(context)?;
        Pin::new(&mut self.inner).poll_write(context, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.check(context)?;
        Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}
