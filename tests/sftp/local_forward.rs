use super::{Command, Session, fixture};
use std::{fs, io::ErrorKind, path::Path, sync::atomic::Ordering, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::{Instant, timeout},
};

const LOOPBACK: &str = "127.0.0.1";
const TIMEOUT: Duration = Duration::from_secs(10);

async fn service(root: &Path) -> TcpListener {
    let listener = TcpListener::bind((LOOPBACK, 0)).await.unwrap();
    fs::write(
        root.join("forward-port"),
        listener.local_addr().unwrap().port().to_string(),
    )
    .unwrap();
    listener
}

async fn expect_closed(stream: &mut TcpStream) {
    match stream.read(&mut [0; 1]).await {
        Ok(0) => {}
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted
            ) => {}
        result => panic!("expected the connected socket to close, got {result:?}"),
    }
}

async fn wait_for_shell(session: &Session) -> u64 {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let frame = session.frames.load();
        if session
            .ssh_connection_at(frame.connection_generation)
            .is_some()
        {
            return frame.connection_generation;
        }
        assert!(
            session.frames.changed(frame.generation, deadline).await,
            "{}",
            frame.status
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_denied_client_can_retry_the_same_listener_and_half_close_its_request() {
    let (pane, _, root, _, server) = fixture(false).await;
    timeout(TIMEOUT, async {
        let generation = wait_for_shell(&pane).await;
        let listener = service(root.path()).await;
        let port = listener.local_addr().unwrap().port();
        assert!(
            pane.start_local_forward(generation, port, port).is_err(),
            "local bind conflict"
        );
        assert!(
            pane.start_local_forward(generation, 0, 0).is_err(),
            "missing remote port"
        );
        let forward = pane.start_local_forward(generation, 0, port).unwrap();
        let address = (LOOPBACK, forward.local_port());
        let mut events = forward.subscribe();
        let refusal = root.path().join("forward-refuse");
        fs::write(&refusal, []).unwrap();
        let mut denied = TcpStream::connect(address).await.unwrap();
        expect_closed(&mut denied).await;
        events
            .wait_for(|s| s.running && s.last_error.is_some())
            .await
            .unwrap();
        fs::remove_file(refusal).unwrap();

        let mut client = TcpStream::connect(address).await.unwrap();
        client
            .write_all(b"request terminated by EOF")
            .await
            .unwrap();
        client.shutdown().await.unwrap();
        let (mut remote, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        remote.read_to_end(&mut request).await.unwrap();
        assert_eq!(request, b"request terminated by EOF");
        remote.write_all(b"reply after EOF").await.unwrap();
        remote.shutdown().await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        assert_eq!(response, b"reply after EOF");

        // The reverse half-close also leaves the other direction usable.
        let mut client = TcpStream::connect(address).await.unwrap();
        let (mut remote, _) = listener.accept().await.unwrap();
        remote.write_all(b"banner").await.unwrap();
        remote.shutdown().await.unwrap();
        let mut banner = Vec::new();
        client.read_to_end(&mut banner).await.unwrap();
        assert_eq!(banner, b"banner");
        client.write_all(b"ack").await.unwrap();
        client.shutdown().await.unwrap();
        let mut ack = Vec::new();
        remote.read_to_end(&mut ack).await.unwrap();
        assert_eq!(ack, b"ack");
        forward.stop();
        events.wait_for(|s| !s.running).await.unwrap();
    })
    .await
    .expect("relay timed out while testing SSH and TCP EOFs");
    drop(pane);
    timeout(TIMEOUT, server).await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_the_owning_shell_revokes_its_forward_while_a_sibling_uses_the_same_login() {
    let (owner, source, root, shell_opens, server) = fixture(false).await;
    timeout(TIMEOUT, async {
        let generation = wait_for_shell(&owner).await;
        let sibling = Session::start_configured_with_connection(
            source,
            Default::default(),
            false,
            owner.ssh_connection(),
        );
        wait_for_shell(&sibling).await;
        assert_eq!(shell_opens.load(Ordering::Relaxed), 2);
        let listener = service(root.path()).await;
        let forward = owner
            .start_local_forward(generation, 0, listener.local_addr().unwrap().port())
            .unwrap();
        let address = (LOOPBACK, forward.local_port());
        let mut status = forward.subscribe();
        let mut client = TcpStream::connect(address).await.unwrap();
        let (mut remote, _) = listener.accept().await.unwrap();
        client.write_all(b"ready").await.unwrap();
        let mut buffer = [0u8; 5];
        remote.read_exact(&mut buffer).await.unwrap();
        assert_eq!(&buffer, b"ready");
        drop(owner);
        status.wait_for(|s| !s.running).await.unwrap();
        expect_closed(&mut client).await;
        expect_closed(&mut remote).await;
        assert!(TcpStream::connect(address).await.is_err());

        let (reply, ack) = tokio::sync::oneshot::channel();
        sibling
            .input
            .send(Command::Raw(b"sibling-still-connected".to_vec(), reply))
            .unwrap();
        ack.await.unwrap().unwrap();
        loop {
            let frame = sibling.frames.load();
            if frame
                .rows
                .iter()
                .any(|row| row.as_str().contains("sibling-still-connected"))
            {
                break;
            }
            assert!(
                sibling
                    .frames
                    .changed(frame.generation, Instant::now() + TIMEOUT)
                    .await
            );
        }
        assert_eq!(shell_opens.load(Ordering::Relaxed), 2);
        drop(sibling);
        server.await.unwrap();
    })
    .await
    .expect("forward outlived its pane or interrupted the shared login");
}
