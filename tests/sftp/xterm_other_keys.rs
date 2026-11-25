use super::{in_band_resize::WireReader, *};
use tokio::sync::mpsc;
use wezterm_term::{KeyCode as K, KeyModifiers as M, TerminalSize};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn xterm_modified_keys_negotiate_and_report_on_the_originating_ssh_pane() {
    let (tx, rx) = mpsc::unbounded_channel();
    let (source, _temp, opens, server) =
        fixture_with_wire(false, 1, Arc::default(), Some(tx)).await;
    let size = TerminalSize {
        rows: 24,
        cols: 80,
        ..Default::default()
    };
    let first = Session::start_sized(source.clone(), size);
    let mut wire = WireReader(rx);
    let (id, peer) = wire.shell().await;
    peer.data(id, b"\x1b[>4;2m\x1b[?4m\x1b[?4g".as_slice())
        .await
        .unwrap();
    wire.input(id, b"\x1b[>4;2m\x1b[>4;0f").await;
    let neighbor = Session::start_configured_with_connection(
        source,
        size,
        true,
        Some(first.ssh_connection().expect("negotiated SSH transport")),
    );
    let (other, neighbor_peer) = wire.shell().await;
    assert_ne!(id, other);
    for (key, modifiers) in [(K::Char('i'), M::CTRL), (K::Tab, M::NONE)] {
        first
            .input
            .send(Command::Key {
                key,
                modifiers,
                down: true,
                repeat: false,
            })
            .unwrap();
        first
            .input
            .send(Command::Key {
                key,
                modifiers,
                down: false,
                repeat: false,
            })
            .unwrap();
    }
    wire.input(id, b"\x1b[27;5;105~\t").await;

    neighbor_peer
        .data(other, b"\x1b[?4m".as_slice())
        .await
        .unwrap();
    wire.input(other, b"\x1b[>4;0m").await;
    neighbor
        .input
        .send(Command::Key {
            key: K::Char('i'),
            modifiers: M::CTRL,
            down: true,
            repeat: false,
        })
        .unwrap();
    wire.input(other, b"\t").await;

    peer.data(id, b"\x1b[>4;1f\x1b[?4g\x1bP$q>4m\x1b\\".as_slice())
        .await
        .unwrap();
    wire.input(id, b"\x1b[>4;1f\x1bP1$r>4;2m\x1b\\").await;
    first
        .input
        .send(Command::DetailedKey {
            key: K::Char('I'),
            details: Default::default(),
            modifiers: M::CTRL | M::SHIFT,
            down: true,
            repeat: false,
        })
        .unwrap();
    wire.input(id, b"\x1b[73;6u").await;

    assert_eq!(opens.load(Ordering::Relaxed), 2);
    drop(neighbor);
    drop(first);
    server.abort();
}
