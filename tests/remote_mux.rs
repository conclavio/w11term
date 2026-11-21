use std::{env, fs, path::PathBuf, time::Duration};
use w11term::terminal::{self, Snapshot, TransportInput};
use wezterm_term::{TerminalSize, image::ImageDataType};

fn grid(rows: usize, cols: usize) -> TerminalSize {
    TerminalSize {
        rows,
        cols,
        pixel_width: cols * 9,
        pixel_height: rows * 18,
        dpi: 96,
    }
}

#[tokio::test]
#[ignore = "requires the VT artifacts from tests/durable/run-mux-linux.sh"]
async fn real_remote_mux_frames_survive_input_and_resize() {
    let captures = PathBuf::from(env::var_os("W11_MUX_CAPTURES").expect("set W11_MUX_CAPTURES"));
    for case in ["zellij", "venti-default", "venti-sixel"] {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut term = terminal::create(grid(30, 100), tx);
        let mut previous = None;
        for (stage, label) in [("first", "W11FIRST "), ("moved", "W11MOVED ")] {
            if stage == "moved" {
                term.resize(grid(38, 126));
            }
            let bytes = fs::read(captures.join(format!("{case}-{stage}.vt"))).unwrap();
            for chunk in bytes.chunks(4093) {
                term.advance_bytes(chunk);
            }
            // The reply writer is asynchronous. XTVERSION is a barrier on that
            // same writer and leaves screen contents and negotiated modes intact.
            term.advance_bytes(b"\x1b[>0q");
            let barrier = format!("\x1bP>|w11term {}\x1b\\", env!("CARGO_PKG_VERSION"));
            let mut replies = Vec::new();
            tokio::time::timeout(Duration::from_secs(5), async {
                while !replies.ends_with(barrier.as_bytes()) {
                    let Some(TransportInput::Data(bytes)) = rx.recv().await else {
                        panic!("terminal reply writer closed");
                    };
                    replies.extend(bytes);
                }
            })
            .await
            .expect("terminal reply barrier timed out");
            replies.truncate(replies.len() - barrier.len());
            let expected = fs::read(captures.join(format!("{case}-{stage}.replies"))).unwrap();
            assert_eq!(
                String::from_utf8(replies).unwrap(),
                String::from_utf8(expected).unwrap(),
                "{case}/{stage}: SSH peer must receive the native terminal's query replies"
            );
            let frame = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
            assert!(frame.alternate, "{case}/{stage}: expected alternate screen");
            assert!(
                frame.rows.iter().any(|line| line.as_str().contains(label)),
                "{case}/{stage}: missing input acknowledgement {label}"
            );
            let mut anchor = None;
            let mut images = Vec::new();
            for (y, row) in frame.rows.iter().enumerate() {
                for cell in row.visible_cells() {
                    let x = cell.cell_index();
                    if cell.str() == "W"
                        && b"W11ANCHOR".iter().enumerate().all(|(i, byte)| {
                            row.get_cell(x + i)
                                .is_some_and(|c| c.str().as_bytes() == [*byte])
                        })
                    {
                        anchor = Some((x + 9, y));
                    }
                    for tile in cell.attrs().images().into_iter().flatten() {
                        let ImageDataType::Rgba8 { data, .. } = frame.image_data(tile.image_data())
                        else {
                            panic!("{case}/{stage}: expected decoded Sixel pixels");
                        };
                        assert!(
                            data.as_chunks::<4>().0.contains(&[255, 0, 0, 255]),
                            "{case}/{stage}: red test image lost"
                        );
                        images.push((x, y));
                    }
                }
            }
            let anchor = anchor.unwrap_or_else(|| panic!("{case}/{stage}: missing pane anchor"));
            if case == "venti-sixel" {
                assert_eq!(images, [anchor], "{case}/{stage}: Sixel placement");
            } else {
                assert!(images.is_empty(), "{case}/{stage}: unexpected graphics");
            }
            if let Some((x, y)) = previous {
                assert_eq!(anchor, (x + 13, y + 4), "{case}: resized pane origin");
            }
            previous = Some(anchor);
        }
    }
}
