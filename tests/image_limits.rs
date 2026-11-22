use base64::{Engine, engine::general_purpose::STANDARD};
use w11term::terminal;
fn send(terminal: &mut wezterm_term::Terminal, params: &str, data: &[u8]) {
    terminal.advance_bytes(format!("\x1b_G{params};{}\x1b\\", STANDARD.encode(data)));
}
#[test]
fn cpu_cache_is_bounded_and_released_on_delete_and_reconnect() {
    let (writer, _) = tokio::sync::mpsc::unbounded_channel();
    let mut terminal = terminal::create(
        wezterm_term::TerminalSize {
            pixel_width: 800,
            pixel_height: 480,
            ..Default::default()
        },
        writer,
    );
    for id in 1..=8 {
        send(
            &mut terminal,
            &format!("a=t,f=32,s=512,v=512,i={id},q=2"),
            &vec![id as u8; 512 * 512 * 4],
        );
        assert!(terminal.image_cache_bytes() <= 4 * 1024 * 1024);
    }
    // Mutable kitty image ids have independent ownership outside the CPU dedup cache.
    assert_eq!(terminal.image_cache_bytes(), 0);
    terminal.advance_bytes(b"\x1b_Ga=d,d=A\x1b\\");
    assert_eq!(terminal.image_cache_bytes(), 0);
    send(&mut terminal, "a=t,f=32,s=1,v=1,i=1,q=2", &[1, 2, 3, 255]);
    assert_eq!(terminal.image_cache_bytes(), 0);
    terminal.reset_connection();
    assert_eq!(terminal.image_cache_bytes(), 0);
}
#[test]
fn animation_append_count_is_bounded_and_edits_still_work() {
    let (writer, _) = tokio::sync::mpsc::unbounded_channel();
    let mut terminal = terminal::create(
        wezterm_term::TerminalSize {
            pixel_width: 800,
            pixel_height: 480,
            ..Default::default()
        },
        writer,
    );
    send(
        &mut terminal,
        "a=T,f=32,s=1,v=1,i=1,q=2,C=1",
        &[1, 2, 3, 255],
    );
    for _ in 0..300 {
        send(&mut terminal, "a=f,f=32,s=1,v=1,i=1,q=2", &[4, 5, 6, 255]);
    }
    let snapshot = terminal::Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
    let image = snapshot
        .rows
        .iter()
        .flat_map(|r| r.visible_cells())
        .find_map(|c| c.attrs().images())
        .unwrap();
    let image = image[0].image_data();
    assert_eq!(image.len(), 256 * 4);
    send(
        &mut terminal,
        "a=f,f=32,s=1,v=1,i=1,r=1,q=2",
        &[9, 8, 7, 255],
    );
    assert_eq!(terminal.image_cache_bytes(), 0);
}
#[test]
fn oversized_unterminated_apc_is_discarded_and_parser_recovers() {
    let (writer, _) = tokio::sync::mpsc::unbounded_channel();
    let mut terminal = terminal::create(
        wezterm_term::TerminalSize {
            pixel_width: 800,
            pixel_height: 480,
            ..Default::default()
        },
        writer,
    );
    terminal.advance_bytes(b"\x1b_G");
    let block = vec![b'A'; 1024 * 1024];
    for _ in 0..49 {
        terminal.advance_bytes(&block);
    }
    terminal.advance_bytes(b"\x1b\\recovered");
    let snapshot = terminal::Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
    assert!(
        snapshot
            .rows
            .iter()
            .any(|r| r.as_str().contains("recovered"))
    );
}

#[test]
fn clearing_anonymous_thumbnails_releases_the_image_table_entries() {
    let (writer, _) = tokio::sync::mpsc::unbounded_channel();
    let mut term = terminal::create(
        wezterm_term::TerminalSize {
            pixel_width: 800,
            pixel_height: 480,
            ..Default::default()
        },
        writer,
    );
    let visible_tiles = |term: &wezterm_term::Terminal| {
        term.screen()
            .physical_lines()
            .flat_map(|row| row.visible_cells())
            .filter_map(|cell| cell.attrs().images())
            .map(|tiles| tiles.len())
            .sum::<usize>()
    };
    // More than 512 uploads over the session, with only eight live at a time.
    for batch in 0..70 {
        for column in 1..=8 {
            term.advance_bytes(format!("\x1b[1;{column}H"));
            send(
                &mut term,
                "a=T,f=32,s=1,v=1,c=1,r=1,C=1,q=2",
                &[batch, column, 180, 255],
            );
        }
        assert_eq!(visible_tiles(&term), 8, "batch {batch}");
        term.advance_bytes(b"\x1b_Ga=d,d=A\x1b\\");
        assert_eq!(visible_tiles(&term), 0);
    }
    send(
        &mut term,
        "a=T,i=1,p=1,f=32,s=1,v=1,c=1,r=1,C=1,q=2",
        &[255, 255, 0, 255],
    );
    assert_eq!(visible_tiles(&term), 1);
}

#[test]
fn exhausted_automatic_image_ids_reject_without_panicking_or_breaking_text() {
    let (writer, _) = tokio::sync::mpsc::unbounded_channel();
    let mut terminal = terminal::create(Default::default(), writer);
    send(
        &mut terminal,
        "a=t,f=32,s=1,v=1,i=4294967295,q=2",
        &[1, 2, 3, 255],
    );
    send(&mut terminal, "a=t,f=32,s=1,v=1,I=1,q=2", &[1, 2, 3, 255]);
    terminal.advance_bytes(b"still usable");
    let snapshot = terminal::Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
    assert!(
        snapshot
            .rows
            .iter()
            .any(|line| line.as_str().contains("still usable"))
    );
}

#[test]
fn independent_kitty_animations_remain_inside_the_table_budget() {
    let (writer, _) = tokio::sync::mpsc::unbounded_channel();
    let mut terminal = terminal::create(
        wezterm_term::TerminalSize {
            pixel_width: 800,
            pixel_height: 480,
            ..Default::default()
        },
        writer,
    );
    let pixels = vec![128; 512 * 1024 * 4];
    send(
        &mut terminal,
        "a=T,f=32,s=512,v=1024,i=1,c=1,r=1,C=1,q=2",
        &pixels,
    );
    for id in 2..=16 {
        send(
            &mut terminal,
            &format!("a=t,f=32,s=512,v=1024,i={id},q=2"),
            &pixels,
        );
    }
    terminal.advance_bytes(b"\x1b[2;1H\x1b_Ga=p,i=2,p=1,c=1,r=1,C=1,q=2\x1b\\");
    // 32 MiB of base images plus two independently mutable animations leave
    // room for exactly sixteen further 2 MiB frames.
    for id in [1, 2] {
        for _ in 0..8 {
            send(
                &mut terminal,
                &format!("a=f,f=32,s=1,v=1,i={id},q=2"),
                &[1, 2, 3, 255],
            );
        }
    }
    send(&mut terminal, "a=f,f=32,s=1,v=1,i=2,q=2", &[1, 2, 3, 255]);
    let snapshot = terminal::Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
    for row in 0..2 {
        let tile = snapshot.rows[row]
            .get_cell(0)
            .unwrap()
            .attrs()
            .images()
            .unwrap();
        assert_eq!(tile[0].image_data().len(), 18 * 1024 * 1024);
    }
}

#[test]
fn anonymous_uploads_reclaim_scrolled_out_pixels_and_charge_only_reachable_images() {
    let (writer, _) = tokio::sync::mpsc::unbounded_channel();
    let mut term = terminal::create(
        wezterm_term::TerminalSize {
            rows: 2,
            cols: 4,
            pixel_width: 40,
            pixel_height: 20,
            dpi: 96,
        },
        writer,
    );
    for id in 1..=509 {
        send(
            &mut term,
            &format!("a=t,i={id},f=32,s=1,v=1,q=2"),
            &[0, 0, 0, 255],
        );
    }
    term.advance_bytes(b"\x1b[?1049h");
    // Once scrolled out, this named parent can still be redisplayed. Its
    // relative child loses its only placement and releases its own slot.
    send(
        &mut term,
        "a=T,i=510,p=1,f=32,s=1,v=1,c=1,r=1,C=1,q=2",
        &[1, 1, 1, 255],
    );
    send(
        &mut term,
        "a=T,i=511,p=1,P=510,Q=1,f=32,s=1,v=1,c=1,r=1,C=1,q=2",
        &[2, 2, 2, 255],
    );
    term.advance_bytes(b"\r\n\r\n");
    for params in ["a=t", "a=T,U=1"] {
        send(
            &mut term,
            &format!("{params},f=32,s=1,v=1,q=2"),
            &[9, 9, 9, 255],
        );
    }

    // With no alternate scrollback, each newline evicts the older of the two
    // thumbnails. Only the two newest may count against the table.
    for n in 0..520 {
        if n > 0 {
            term.advance_bytes(b"\r\n");
        }
        send(
            &mut term,
            "a=T,f=32,s=1,v=1,c=1,r=1,C=1,q=2",
            &[n as u8, 0, 0, 255],
        );
    }
    // Both remaining slots really are occupied.
    term.advance_bytes(b"\x1b[2;2H");
    send(
        &mut term,
        "a=T,i=600,f=32,s=1,v=1,c=1,r=1,C=1,q=2",
        &[0, 255, 0, 255],
    );
    let snapshot = terminal::Snapshot::capture(&term, None, 1, 0, String::new(), 0);
    for (row, expected) in [(0, 518u16 as u8), (1, 519u16 as u8)] {
        let tiles = snapshot.rows[row]
            .get_cell(0)
            .unwrap()
            .attrs()
            .images()
            .unwrap();
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].image_id(), Some(0));
        let wezterm_term::image::ImageDataType::Rgba8 { data, .. } =
            snapshot.image_data(tiles[0].image_data())
        else {
            panic!("expected decoded thumbnail");
        };
        assert_eq!(data.as_slice(), &[expected, 0, 0, 255]);
    }
    assert!(
        snapshot.rows[1]
            .get_cell(1)
            .and_then(|c| c.attrs().images())
            .unwrap_or_default()
            .is_empty()
    );
}
