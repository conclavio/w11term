use super::*;
use tokio::sync::mpsc;
use wezterm_term::TerminalSize;

pub(super) enum Wire {
    Shell(ChannelId, server::Handle),
    Window(ChannelId, [u32; 4]),
    Input(ChannelId, Vec<u8>),
}

struct WireReader(mpsc::UnboundedReceiver<Wire>);

impl WireReader {
    async fn next(&mut self) -> Wire {
        tokio::time::timeout(Duration::from_secs(4), self.0.recv())
            .await
            .expect("timed out waiting for the SSH peer")
            .expect("SSH peer closed")
    }

    async fn shell(&mut self) -> (ChannelId, server::Handle) {
        let Wire::Shell(id, handle) = self.next().await else {
            panic!("expected a new SSH shell")
        };
        (id, handle)
    }

    async fn window(&mut self, channel: ChannelId, expected: [u32; 4]) {
        match self.next().await {
            Wire::Window(id, dimensions) => {
                assert_eq!(id, channel);
                assert_eq!(dimensions, expected, "SSH window-change geometry");
            }
            Wire::Input(id, bytes) => panic!("input on {id} before window-change: {bytes:?}"),
            Wire::Shell(..) => panic!("unexpected shell"),
        }
    }

    async fn input(&mut self, channel: ChannelId, expected: &[u8]) {
        // Replies are byte streams; SSH may fragment a single reply.
        let mut actual = Vec::new();
        while actual.len() < expected.len() {
            match self.next().await {
                Wire::Input(id, bytes) => {
                    assert_eq!(id, channel);
                    actual.extend(bytes);
                    assert!(
                        expected.starts_with(&actual),
                        "expected {expected:?}, received {actual:?}"
                    );
                }
                Wire::Window(id, dimensions) => {
                    panic!("window-change on {id} overtook {expected:?}: {dimensions:?}")
                }
                Wire::Shell(..) => panic!("unexpected shell"),
            }
        }
    }
}

fn size(cols: usize, rows: usize, pixel_width: usize, pixel_height: usize) -> TerminalSize {
    TerminalSize {
        cols,
        rows,
        pixel_width,
        pixel_height,
        dpi: 96,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn negotiated_resize_reports_follow_each_ssh_window_change_on_the_correct_channel() {
    let accepted = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = mpsc::unbounded_channel();
    let (source, _temp, opens, server) =
        fixture_with_wire(false, 1, accepted.clone(), Some(tx)).await;
    let mut wire = WireReader(rx);
    let first = Session::start_sized(source.clone(), size(79, 21, 711, 357));
    let (channel, peer) = wire.shell().await;
    let mut ready = first.ssh_connection_changes();
    let shared = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if let Some(connection) = first.ssh_connection() {
                break connection;
            }
            ready
                .changed()
                .await
                .expect("SSH connection publication closed");
        }
    })
    .await
    .expect("first SSH shell never became shareable");
    let neighbor = Session::start_configured_with_connection(
        source,
        size(40, 12, 320, 192),
        true,
        Some(shared),
    );
    let (other, _) = wire.shell().await;
    assert_ne!(channel, other);
    assert_eq!(accepted.load(Ordering::Relaxed), 1);
    assert_eq!(opens.load(Ordering::Relaxed), 2);

    peer.data(channel, b"\x1b[?2048$p".as_slice())
        .await
        .unwrap();
    wire.input(channel, b"\x1b[?2048;2$y").await;
    for _ in 0..2 {
        peer.data(channel, b"\x1b[?2048h".as_slice()).await.unwrap();
        wire.input(channel, b"\x1b[48;21;79;357;711t").await;
    }
    peer.data(channel, b"\x1b[?2048$p".as_slice())
        .await
        .unwrap();
    wire.input(channel, b"\x1b[?2048;1$y").await;

    // Queue both changes before observing either. The second keeps all the cells.
    first
        .input
        .send(Command::Resize(size(101, 37, 1001, 613)))
        .unwrap();
    first
        .input
        .send(Command::Resize(size(101, 37, 1007, 619)))
        .unwrap();
    wire.window(channel, [101, 37, 1001, 613]).await;
    wire.input(channel, b"\x1b[48;37;101;613;1001t").await;
    wire.window(channel, [101, 37, 1007, 619]).await;
    wire.input(channel, b"\x1b[48;37;101;619;1007t").await;

    neighbor
        .input
        .send(Command::Resize(size(44, 16, 352, 256)))
        .unwrap();
    wire.window(other, [44, 16, 352, 256]).await;
    // A query answered after that resize proves the disabled neighbor emitted no report.
    peer.data(other, b"\x1b[?2048$p".as_slice()).await.unwrap();
    wire.input(other, b"\x1b[?2048;2$y").await;
    peer.data(other, b"\x1b[?2048h".as_slice()).await.unwrap();
    wire.input(other, b"\x1b[48;16;44;256;352t").await;

    peer.data(channel, b"\x1b[?2048l\x1b[?2048$p".as_slice())
        .await
        .unwrap();
    wire.input(channel, b"\x1b[?2048;2$y").await;
    first
        .input
        .send(Command::Resize(size(102, 38, 1020, 608)))
        .unwrap();
    wire.window(channel, [102, 38, 1020, 608]).await;
    peer.data(channel, b"\x1b[?2048$p".as_slice())
        .await
        .unwrap();
    wire.input(channel, b"\x1b[?2048;2$y").await;
    peer.data(channel, b"\x1b[?2048h".as_slice()).await.unwrap();
    wire.input(channel, b"\x1b[48;38;102;608;1020t").await;
    peer.data(channel, b"\x1bc\x1b[?2048$p".as_slice())
        .await
        .unwrap();
    wire.input(channel, b"\x1b[?2048;2$y").await;
    neighbor
        .input
        .send(Command::Resize(size(45, 16, 360, 256)))
        .unwrap();
    wire.window(other, [45, 16, 360, 256]).await;
    wire.input(other, b"\x1b[48;16;45;256;360t").await;
    drop(neighbor);
    drop(first);
    server.abort();
}
