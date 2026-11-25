use super::{in_band_resize::WireReader, *};
use tokio::sync::mpsc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn primary_screen_reflows_scaled_text_and_surrounding_output_on_ssh_resize() {
    let (tx, rx) = mpsc::unbounded_channel();
    let (source, _root, _, server) = fixture_with_wire(false, 1, Arc::default(), Some(tx)).await;
    let size = wezterm_term::TerminalSize {
        cols: 8,
        rows: 5,
        pixel_width: 64,
        pixel_height: 75,
        dpi: 96,
    };
    let session = Session::start_sized(source, size);
    let mut wire = WireReader(rx);
    let (channel, peer) = wire.shell().await;
    peer.data(
        channel,
        b"\x1b[2J\x1b[H12345\x1b]66;s=2;W\x07q\x1b[1;8H\x1b[6n".as_slice(),
    )
    .await
    .unwrap();
    wire.input(channel, b"\x1b[1;8R").await;
    for (cols, new_cursor) in [(6, b"\x1b[2;3R".as_slice()), (8, b"\x1b[1;8R".as_slice())] {
        session
            .input
            .send(w11term::session::Command::Resize(
                wezterm_term::TerminalSize {
                    cols,
                    pixel_width: 8 * cols,
                    ..size
                },
            ))
            .unwrap();
        wire.window(channel, [cols as u32, 5, (8 * cols) as u32, 75])
            .await;
        peer.data(channel, b"\x1b[6n".as_slice()).await.unwrap();
        wire.input(channel, new_cursor).await;
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kitty_text_sizing_probe_multicell_fraction_and_payload_limit_over_ssh() {
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
    wire.input(channel, b"\x1b[1;1R\x1b[1;3R\x1b[1;5R").await;
    peer.data(channel, b"\x1b[2J\x1b[H\x1b]66;w=2; \x07".as_slice())
        .await
        .unwrap();
    let exact = format!("\x1b]66;w=7;{}\x07\x1b[6n", "é;".repeat(1365) + "!");
    let too_long = format!("\x1b]66;w=7;{}\x07Z\x1b[6n", "é".repeat(2049));
    peer.data(channel, exact.into_bytes()).await.unwrap();
    wire.input(channel, b"\x1b[1;10R").await;
    peer.data(channel, too_long.into_bytes()).await.unwrap();
    wire.input(channel, b"\x1b[1;11R").await;
    peer.data(
        channel,
        b"\x1b[2;1H\x1b]66;s=2:w=3:n=1:d=2:h=1:v=1;SUP\x07!\x1b[6n".as_slice(),
    )
    .await
    .unwrap();
    wire.input(channel, b"\x1b[2;8R").await;
    let frame = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let frame = session.frames.load();
            if frame.rows[1]
                .get_cell(6)
                .is_some_and(|cell| cell.str() == "!")
            {
                break frame;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("published fractional text");
    let mut cells = frame.rows[0].visible_cells();
    assert_eq!(cells.next().unwrap().width(), 2);
    let text = cells.next().unwrap();
    assert_eq!(
        (text.cell_index(), text.width(), text.str().len()),
        (2, 7, 4096)
    );
    let tail = cells.next().unwrap();
    assert_eq!((tail.cell_index(), tail.str()), (9, "Z"));
    let cell = frame.rows[1].get_cell(0).unwrap();
    assert_eq!((cell.str(), cell.width()), ("SUP", 6));
    assert_eq!(
        cell.attrs().fractional_text_scale(),
        wezterm_term::FractionalTextScale::new(1, 2, 1, 1)
    );
    let continuation = frame.rows[2].get_cell(0).unwrap();
    assert_eq!(continuation.str(), " ");
    let slice = continuation.attrs().multicell_row().unwrap();
    assert_eq!((slice.offset(), slice.run().text()), (1, "SUP"));
    server.abort();
}
