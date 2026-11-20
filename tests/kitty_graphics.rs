use w11term::terminal::{self, Snapshot, TransportInput};
use wezterm_term::{Terminal, TerminalSize, image::ImageDataType};

const PLACEMENTS: &[u8] = include_bytes!("fixtures/kitty/placements.vt");
const ANIMATION: &[u8] = include_bytes!("fixtures/kitty/animation.vt");

fn engine() -> (
    Terminal,
    tokio::sync::mpsc::UnboundedReceiver<TransportInput>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    (
        terminal::create(
            TerminalSize {
                rows: 8,
                cols: 16,
                pixel_width: 160,
                pixel_height: 80,
                dpi: 96,
            },
            tx,
        ),
        rx,
    )
}
fn frame(t: &Terminal, old: Option<&Snapshot>, scroll: usize) -> Snapshot {
    Snapshot::capture(t, old, 1, scroll, String::new(), 0)
}
fn pixel(f: &Snapshot, x: usize, y: usize, id: u32) -> Option<[u8; 4]> {
    let tiles = f.rows.get(y)?.get_cell(x)?.attrs().images()?;
    let tile = tiles.iter().find(|tile| tile.image_id() == Some(id))?;
    let ImageDataType::Rgba8 {
        data,
        width,
        height,
        ..
    } = f.image_data(tile.image_data())
    else {
        return None;
    };
    let left = tile.top_left();
    let right = tile.bottom_right();
    let u = (((left.x.into_inner() + right.x.into_inner()) * 0.5 * *width as f32) as usize)
        .min(*width as usize - 1);
    let v = (((left.y.into_inner() + right.y.into_inner()) * 0.5 * *height as f32) as usize)
        .min(*height as usize - 1);
    Some(
        data[(v * *width as usize + u) * 4..][..4]
            .try_into()
            .unwrap(),
    )
}

#[test]
fn placeholders_relative_placement_and_client_frame_survive_scroll_and_resize() {
    let (mut t, _) = engine();
    for chunk in PLACEMENTS.chunks(7) {
        t.advance_bytes(chunk);
    }
    let f = frame(&t, None, 0);
    assert_eq!(pixel(&f, 2, 2, 42), Some([255, 0, 0, 255]));
    assert_eq!(pixel(&f, 3, 2, 42), Some([0, 255, 0, 255]));
    assert_eq!(pixel(&f, 5, 3, 43), Some([0, 0, 255, 255]));
    assert_eq!(pixel(&f, 2, 4, 44), Some([255, 255, 0, 255]));
    assert_eq!(pixel(&f, 0, 0, 42), None);
    t.advance_bytes(b"\x1b[8;1H\r\n\r\n\r\n");
    let scrolled = frame(&t, Some(&f), 0);
    assert_eq!(
        pixel(&scrolled, 5, 0, 43),
        Some([0, 0, 255, 255]),
        "parent just above viewport"
    );
    let history = frame(&t, None, 3);
    assert_eq!(pixel(&history, 5, 3, 43), Some([0, 0, 255, 255]));
    t.resize(TerminalSize {
        rows: 10,
        cols: 10,
        pixel_width: 100,
        pixel_height: 100,
        dpi: 96,
    });
    let resized = frame(&t, Some(&history), usize::MAX);
    let parent = (0..10)
        .find(|&y| pixel(&resized, 2, y, 42).is_some())
        .unwrap();
    assert_eq!(pixel(&resized, 5, parent + 1, 43), Some([0, 0, 255, 255]));
    t.advance_bytes(b"\x1b_Ga=d,d=I,i=42,p=7\x1b\\");
    let deleted = frame(&t, Some(&resized), usize::MAX);
    assert_eq!(pixel(&deleted, 2, parent, 42), None);
    assert_eq!(pixel(&deleted, 5, parent + 1, 43), None);
}

#[test]
fn playback_uses_frame_gaps_and_finished_animation_has_no_timer() {
    let (mut t, _) = engine();
    t.advance_bytes(ANIMATION);
    let first = frame(&t, None, 0);
    let red = Some([255, 0, 0, 255]);
    let green = Some([0, 255, 0, 255]);
    let blue = Some([0, 0, 255, 255]);
    assert_eq!(pixel(&first, 0, 0, 8), red);
    let deadline = t.kitty_animation_deadline().unwrap();
    assert!(!t.advance_kitty_animations(deadline - std::time::Duration::from_millis(1)));
    assert!(t.advance_kitty_animations(deadline));
    let second = frame(&t, Some(&first), 0);
    assert_eq!(
        pixel(&first, 0, 0, 8),
        red,
        "committed frame remains immutable"
    );
    assert_eq!(pixel(&second, 0, 0, 8), green);
    let second_deadline = t.kitty_animation_deadline().unwrap();
    assert_eq!(
        second_deadline - deadline,
        std::time::Duration::from_millis(37)
    );
    assert!(t.advance_kitty_animations(second_deadline));
    assert_eq!(pixel(&frame(&t, Some(&second), 0), 0, 0, 8), blue);
    let last = t.kitty_animation_deadline().unwrap();
    assert_eq!(last - second_deadline, std::time::Duration::from_millis(51));
    t.advance_kitty_animations(last);
    assert!(t.kitty_animation_deadline().is_none());

    t.advance_bytes(b"\x1b_Ga=a,i=8,c=2,r=2,z=-1,s=2,q=2\x1b\\");
    let loading = t.kitty_animation_deadline().unwrap();
    t.advance_kitty_animations(loading);
    assert_eq!(t.kitty_animation_frame(Some(8)), 2);
    assert!(
        t.kitty_animation_deadline().is_none(),
        "loading waits for another frame without polling"
    );
    t.advance_bytes(b"\x1b_Ga=f,i=8,f=32,s=1,v=1,z=19,q=2,m=1;////\x1b\\\x1b_Gm=0,q=2;/w==\x1b\\");
    assert!(t.kitty_animation_deadline().is_some());
    t.advance_bytes(b"\x1b_Ga=a,i=8,s=1,q=2\x1b\\");
    assert!(t.kitty_animation_deadline().is_none());
}

#[tokio::test]
async fn ancestry_errors_preserve_good_placements_and_normal_parent_reanchors() {
    let (mut t, mut replies) = engine();
    t.advance_bytes(b"\x1b_Ga=T,f=32,s=1,v=1,i=1,p=1,c=1,r=1,C=1,q=2;/wAA/w==\x1b\\\x1b_Ga=t,f=32,s=1,v=1,i=2,q=2;AAD//w==\x1b\\");
    t.advance_bytes(b"\x1b_Ga=p,i=2,p=2,P=1,Q=1,H=1,c=1,r=1,q=2\x1b\\");
    let f = frame(&t, None, 0);
    assert_eq!(pixel(&f, 1, 0, 2), Some([0, 0, 255, 255]));
    for (command, reason) in [
        ("i=1,p=1,P=2,Q=2", "ECYCLE"),
        ("i=2,p=2,U=1,P=1,Q=1", "EINVAL"),
        ("i=2,p=2,P=88,Q=3", "ENOPARENT"),
    ] {
        t.advance_bytes(format!("\x1b_Ga=p,{command}\x1b\\"));
        let mut response = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !response.ends_with(b"\x1b\\") {
                if let Some(TransportInput::Data(data)) = replies.recv().await {
                    response.extend(data);
                } else {
                    break;
                }
            }
        })
        .await
        .unwrap();
        let response = String::from_utf8(response).unwrap();
        assert!(
            response.contains(reason),
            "expected {reason}, received {response:?}"
        );
    }
    t.advance_bytes(b"\x1b[4;6H\x1b_Ga=p,i=1,p=1,c=1,r=1,C=1,q=2\x1b\\");
    let moved = frame(&t, Some(&f), 0);
    assert!(pixel(&moved, 1, 0, 2).is_none());
    assert_eq!(pixel(&moved, 6, 3, 2), Some([0, 0, 255, 255]));
}

#[test]
fn high_image_byte_and_underline_placement_follow_inherited_cells() {
    let (mut t, _) = engine();
    t.advance_bytes(b"\x1b_Ga=T,i=33554474,p=7,U=1,c=2,r=1,f=32,s=2,v=1,q=2;/wAA/wD/AP8=\x1b\\");
    t.advance_bytes("\x1b[38;2;0;0;42;58;2;0;0;7m\u{10eeee}\u{305}\u{305}\u{30e}\u{10eeee}\x1b[0m");
    let f = frame(&t, None, 0);
    assert_eq!(pixel(&f, 0, 0, 33554474), Some([255, 0, 0, 255]));
    assert_eq!(pixel(&f, 1, 0, 33554474), Some([0, 255, 0, 255]));
    assert!(
        f.rows[0].as_str().starts_with('\u{10eeee}'),
        "copy and search retain text"
    );
    t.advance_bytes(b"\x1b_Ga=d,d=A\x1b\\");
    assert!(
        pixel(&frame(&t, Some(&f), 0), 1, 0, 33554474).is_some(),
        "global delete preserves virtual prototypes"
    );
}

#[test]
fn hidden_animation_does_not_schedule_presentations() {
    let (mut t, _) = engine();
    t.advance_bytes(ANIMATION);
    let visible = frame(&t, None, 0);
    assert!(visible.animation_deadline(&t).is_some());
    t.advance_bytes(b"\x1b_Ga=d,d=i,i=8,p=1\x1b\\");
    let hidden = frame(&t, Some(&visible), 0);
    assert!(hidden.animation_deadline(&t).is_none());
}

#[tokio::test]
async fn running_session_publishes_frames_without_incoming_data() {
    use w11term::{
        options::Source,
        session::{Command, Session},
    };
    let session = Session::start_sized(
        Source::Demo,
        TerminalSize {
            rows: 8,
            cols: 16,
            pixel_width: 160,
            pixel_height: 80,
            dpi: 96,
        },
    );
    // Put the fixture at the top left regardless of the demo's initial cursor.
    session
        .input
        .send(Command::Paste(format!(
            "\x1b[2J\x1b[H{}",
            std::str::from_utf8(ANIMATION).unwrap()
        )))
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let next = session.frames.load();
        if pixel(&next, 0, 0, 8) == Some([0, 0, 255, 255]) {
            break;
        }
        assert!(
            session.frames.changed(next.generation, deadline).await,
            "animation never reached its final blue frame"
        );
    }
    let steady = session.frames.load().generation;
    let idle = tokio::time::Instant::now() + std::time::Duration::from_millis(150);
    assert!(
        !session.frames.changed(steady, idle).await,
        "completed animation repainted"
    );
}

#[test]
fn global_delete_retains_only_virtual_prototypes() {
    let (mut terminal, _) = engine();
    terminal.advance_bytes(PLACEMENTS);
    terminal.advance_bytes(b"\x1b_Ga=d,d=A\x1b\\");
    let committed = frame(&terminal, None, 0);
    assert!(pixel(&committed, 2, 2, 42).is_some());
    assert!(pixel(&committed, 5, 3, 43).is_none());
    assert!(pixel(&committed, 2, 4, 44).is_none());
}
