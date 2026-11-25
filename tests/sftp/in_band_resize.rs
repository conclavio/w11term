use super::*;
use tokio::sync::mpsc;
use wezterm_term::TerminalSize;

pub(super) enum Wire {
    Shell(ChannelId, server::Handle),
    Window(ChannelId, [u32; 4]),
    Input(ChannelId, Vec<u8>),
}

pub(super) struct WireReader(pub(super) mpsc::UnboundedReceiver<Wire>);

impl WireReader {
    async fn next(&mut self) -> Wire {
        tokio::time::timeout(Duration::from_secs(4), self.0.recv())
            .await
            .expect("timed out waiting for the SSH peer")
            .expect("SSH peer closed")
    }

    pub(super) async fn shell(&mut self) -> (ChannelId, server::Handle) {
        let Wire::Shell(id, handle) = self.next().await else {
            panic!("expected a new SSH shell")
        };
        (id, handle)
    }

    pub(super) async fn window(&mut self, channel: ChannelId, expected: [u32; 4]) {
        match self.next().await {
            Wire::Window(id, dimensions) => {
                assert_eq!(id, channel);
                assert_eq!(dimensions, expected, "SSH window-change geometry");
            }
            Wire::Input(id, bytes) => panic!("input on {id} before window-change: {bytes:?}"),
            Wire::Shell(..) => panic!("unexpected shell"),
        }
    }

    pub(super) async fn input(&mut self, channel: ChannelId, expected: &[u8]) {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cell_size_queries_report_logical_and_physical_geometry_at_fractional_dpi() {
    let (tx, rx) = mpsc::unbounded_channel();
    let (source, _temp, _, server) = fixture_with_wire(false, 1, Arc::default(), Some(tx)).await;
    let session = Session::start_sized(
        source,
        TerminalSize {
            dpi: 120,
            ..size(80, 24, 880, 552)
        },
    );
    let mut wire = WireReader(rx);
    let (channel, peer) = wire.shell().await;
    for (request, reply) in [
        (
            b"\x1b]1337;ReportCellSize\x07\x1b[16t\x1b[14t".as_slice(),
            b"\x1b]1337;ReportCellSize=18.4;8.8;1.25\x1b\\\x1b[6;23;11t\x1b[4;552;880t".as_slice(),
        ),
        (
            b"\x1b[H\x1b]1337;ReportCellSize=18.4;8.8;1.25\x1b\\\x1b[6n".as_slice(),
            b"\x1b[1;1R".as_slice(),
        ),
    ] {
        peer.data(channel, request).await.unwrap();
        wire.input(channel, reply).await;
    }

    session
        .input
        .send(Command::Resize(TerminalSize {
            dpi: 144,
            ..size(80, 24, 960, 576)
        }))
        .unwrap();
    wire.window(channel, [80, 24, 960, 576]).await;
    peer.data(
        channel,
        b"\x1b]1337;ReportCellSize\x1b\\\x1b[16t".as_slice(),
    )
    .await
    .unwrap();
    wire.input(
        channel,
        b"\x1b]1337;ReportCellSize=16.0;8.0;1.5\x1b\\\x1b[6;24;12t",
    )
    .await;
    session
        .input
        .send(Command::Resize(size(80, 24, 800, 480)))
        .unwrap();
    wire.window(channel, [80, 24, 800, 480]).await;
    peer.data(channel, b"\x1b]1337;ReportCellSize\x07".as_slice())
        .await
        .unwrap();
    wire.input(channel, b"\x1b]1337;ReportCellSize=20.0;10.0;1.0\x1b\\")
        .await;
    server.abort();
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
