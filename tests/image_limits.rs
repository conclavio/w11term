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
    assert!(terminal.image_cache_bytes() > 0);
    terminal.advance_bytes(b"\x1b_Ga=d,d=A\x1b\\");
    assert_eq!(terminal.image_cache_bytes(), 0);
    send(&mut terminal, "a=t,f=32,s=1,v=1,i=1,q=2", &[1, 2, 3, 255]);
    assert_eq!(terminal.image_cache_bytes(), 4);
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
