use super::{in_band_resize::WireReader, *};
use tokio::sync::mpsc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kitty_text_width_probe_and_payload_limit_over_ssh() {
    let (tx, rx) = mpsc::unbounded_channel();
    let (source, _root, _, server) = fixture_with_wire(false, 1, Arc::default(), Some(tx)).await;
    let session = Session::start_sized(
        source,
        wezterm_term::TerminalSize {
            cols: 80,
            rows: 24,
            ..Default::default()
        },
    );
    let mut wire = WireReader(rx);
    let (channel, peer) = wire.shell().await;
    for part in [
        b"\x1b[2J\x1b[H\r\x1b[6n\x1b]66;w=2;".as_slice(),
        b" \x1b",
        b"\\\x1b[6n\x1b]66;s=2; \x07\x1b[6n",
    ] {
        peer.data(channel, part).await.unwrap();
    }
    wire.input(channel, b"\x1b[1;1R\x1b[1;3R\x1b[1;3R").await;
    let exact = format!("\x1b]66;w=7;{}\x07\x1b[6n", "é;".repeat(1365) + "!");
    let too_long = format!("\x1b]66;w=7;{}\x07Z\x1b[6n", "é".repeat(2049));
    peer.data(channel, exact.into_bytes()).await.unwrap();
    wire.input(channel, b"\x1b[1;10R").await;
    peer.data(channel, too_long.into_bytes()).await.unwrap();
    wire.input(channel, b"\x1b[1;11R").await;
    let frame = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let frame = session.frames.load();
            if frame.rows[0]
                .get_cell(9)
                .is_some_and(|cell| cell.str() == "Z")
            {
                break frame;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("published text width frame");
    let mut cells = frame.rows[0].visible_cells();
    assert_eq!(cells.next().unwrap().width(), 2);
    let text = cells.next().unwrap();
    assert_eq!(
        (text.cell_index(), text.width(), text.str().len()),
        (2, 7, 4096)
    );
    let tail = cells.next().unwrap();
    assert_eq!((tail.cell_index(), tail.str()), (9, "Z"));
    server.abort();
}
