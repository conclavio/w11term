use base64::{Engine, engine::general_purpose::STANDARD};
use w11term::terminal::{self, Snapshot, TransportInput};
use wezterm_term::{Terminal, TerminalSize, image::ImageDataType};

const PLACEMENTS: &[u8] = include_bytes!("fixtures/kitty/placements.vt");
const ANIMATION: &[u8] = include_bytes!("fixtures/kitty/animation.vt");

#[test]
fn range_deletion_round_trips_icat_and_rejects_malformed_bounds() {
    use termwiz::escape::{Action, KittyImage, apc::KittyImageDelete, parser::Parser};
    for (fields, canonical, first, last, delete) in [
        ("d=R,y=4294967295", "d=R,y=4294967295", 0, u32::MAX, true),
        ("d=r,x=7,y=9", "d=r,x=7,y=9", 7, 9, false),
        ("d=r,x=9,y=7", "d=r,x=9,y=7", 9, 7, false),
        ("d=R,x=7", "d=R,x=7", 7, 0, true),
        ("d=r", "d=r", 0, 0, false),
        ("d=R,x=0,y=0", "d=R", 0, 0, true),
    ] {
        let wire = format!("\x1b_Ga=d,{fields}\x1b\\");
        let actions = Parser::new().parse_as_vec(wire.as_bytes());
        let Action::KittyImage(cmd) = &actions[0] else {
            panic!("unparsed {fields}")
        };
        let KittyImage::Delete { what, .. } = cmd.as_ref() else {
            panic!("expected deletion")
        };
        assert_eq!(
            *what,
            KittyImageDelete::ByImageIdRange {
                first,
                last,
                delete
            }
        );
        let encoded: String = actions.iter().map(ToString::to_string).collect();
        assert_eq!(encoded, format!("\x1b_Ga=d,{canonical}\x1b\\"));
    }
    for bad in ["", "-1", "+1", "00000000000", "5x", "4294967296"] {
        for key in ["x", "y"] {
            assert!(KittyImage::parse_apc(format!("Ga=d,d=R,{key}={bad}").as_bytes()).is_none());
        }
    }
}

#[test]
fn animation_deletion_round_trips_image_and_frame_selectors() {
    use termwiz::escape::{Action, KittyImage, apc::KittyImageDelete, parser::Parser};
    for (fields, image_id, image_number, frame_number, delete) in [
        ("d=f,i=7,r=3", Some(7), None, Some(3), false),
        ("d=F,I=9", None, Some(9), None, true),
    ] {
        let wire = format!("\x1b_Ga=d,{fields}\x1b\\");
        let actions = Parser::new().parse_as_vec(wire.as_bytes());
        let reencoded: String = actions.iter().map(ToString::to_string).collect();
        for bytes in [wire.as_bytes(), reencoded.as_bytes()] {
            let Action::KittyImage(command) = &Parser::new().parse_as_vec(bytes)[0] else {
                panic!("expected a kitty command");
            };
            let KittyImage::Delete { what, .. } = command.as_ref() else {
                panic!("expected deletion");
            };
            assert_eq!(
                *what,
                KittyImageDelete::AnimationFrames {
                    image_id,
                    image_number,
                    frame_number,
                    delete
                }
            );
        }
    }
    for fields in ["i=7,r=", "i=7,r=-1", "i=7,r=4294967296", "i=oops", "I=oops"] {
        assert!(KittyImage::parse_apc(format!("Ga=d,d=F,{fields}").as_bytes()).is_none());
    }
}

#[test]
fn feature_query_round_trips_its_short_options_and_discards_the_payload() {
    use termwiz::escape::{Action, KittyImage, parser::Parser};
    let wire = b"\x1b_Ga=q,i=91,e=T,U=1,C=1,t=f;ignored-payload\xff\x1b\\";
    let actions = Parser::new().parse_as_vec(wire);
    let Some(Action::KittyImage(command)) = actions.first() else {
        panic!("expected kitty feature query");
    };
    let KittyImage::FeatureQuery {
        image_id,
        action,
        options,
        ..
    } = command.as_ref()
    else {
        panic!("expected a feature query");
    };
    assert_eq!((*image_id, *action), (91, 'T'));
    assert_eq!(
        options,
        &[("C", "1".into()), ("U", "1".into()), ("t", "f".into())]
    );
    let encoded: String = actions.iter().map(ToString::to_string).collect();
    assert_eq!(encoded, "\x1b_GC=1,U=1,a=q,e=T,i=91,t=f\x1b\\");
    assert_eq!(Parser::new().parse_as_vec(encoded.as_bytes()), actions);
    for malformed in [
        "Ga=q,e=T",
        "Ga=q,i=0,e=T",
        "Ga=q,i=1,I=2,e=T",
        "Ga=q,i=1,e=TT",
        "Ga=t,i=1,e=T",
    ] {
        assert!(
            KittyImage::parse_apc(malformed.as_bytes()).is_none(),
            "{malformed}"
        );
    }
}

#[tokio::test]
async fn graphics_feature_queries_only_report_options_supported_for_the_requested_action() {
    let (mut t, mut replies) = engine();
    t.advance_bytes(concat!(
        "\x1b_Ga=q,i=31,e=T,C=1,U=1,t=f,N=1,f=8,w=3,H=-2,z=0;ignored\x1b\\",
        "\x1b_Ga=q,i=32,e=f,r=1,X=1,f=24,U=1\x1b\\",
        "\x1b_Ga=q,i=33,e=d,d=F,r=2,m=1\x1b\\",
        "\x1b_Ga=q,i=34,e=c,C=1,r=3,w=1,o=z\x1b\\",
        "\x1b_Ga=q,i=35,e=p,U=2,c=4294967296,o=z\x1b\\",
        "\x1b_Ga=q,i=36,e=x,U=1\x1b\\",
        "\x1b_Ga=q,i=37,e=a,s=3,c=1,v=2,z=-1\x1b\\",
        "\x1b_Ga=q,i=38,e=t,t=d,o=z,f=32,s=1,v=1,S=4\x1b\\",
        "\x1b_Ga=q,i=39,e=p,q=1\x1b\\",
        "\x1b_Ga=q,i=40,e=p,q=2\x1b\\",
        "\x1b_Ga=q,i=41,s=1,v=1,f=24;AAAA\x1b\\",
        "\x1b_Ga=q,i=42,e=q,f=16\x1b\\",
        "\x1b_Ga=q,i=43,e=f,f=16\x1b\\",
        "\x1b[5n",
    ));
    let replies = read_through(&mut replies, b"\x1b[0n").await;
    assert_eq!(
        replies,
        concat!(
            "\x1b_Gi=31,e=T,C=1,H=-2,U=1,f=8,w=3;OK\x1b\\",
            "\x1b_Gi=32,e=f,X=1,f=24,r=1;OK\x1b\\",
            "\x1b_Gi=33,e=d,d=F,r=2;OK\x1b\\",
            "\x1b_Gi=34,e=c,C=1,r=3,w=1;OK\x1b\\",
            "\x1b_Gi=35,e=p;OK\x1b\\",
            "\x1b_Gi=36;OK\x1b\\",
            "\x1b_Gi=37,e=a,c=1,s=3,v=2,z=-1;OK\x1b\\",
            "\x1b_Gi=38,e=t,f=32,o=z,s=1,t=d,v=1;OK\x1b\\",
            "\x1b_Gi=41;OK\x1b\\",
            "\x1b_Gi=42,e=q,f=16;OK\x1b\\",
            "\x1b_Gi=43,e=f;OK\x1b\\",
            "\x1b[0n",
        )
    );
}

#[tokio::test]
async fn feature_query_has_no_effect_on_images_or_a_chunked_upload() {
    let (mut t, mut replies) = engine();
    transmit_red(&mut t, 11);
    place(&mut t, 11, 1, 2, 3, 1);
    t.advance_bytes(concat!(
        "\x1b_Ga=T,i=31,p=1,f=32,s=1,v=1,c=1,r=1,C=1,q=2,m=1;AP8A\x1b\\",
        "\x1b_Ga=q,i=11,e=d,d=I\x1b\\",
        "\x1b_Ga=q,i=31,e=T,m=1,t=s;invalid payload\x1b\\",
        "\x1b_Gm=0,q=2;/w==\x1b\\",
        "\x1b[5n",
    ));
    assert_eq!(
        read_through(&mut replies, b"\x1b[0n").await,
        "\x1b_Gi=11,e=d,d=I;OK\x1b\\\x1b_Gi=31,e=T;OK\x1b\\\x1b[0n"
    );
    let committed = frame(&t, None, 0);
    assert_eq!(pixel(&committed, 2, 1, 11), RED);
    assert_eq!(pixel(&committed, 2, 1, 31), Some([0, 255, 0, 255]));
}

#[tokio::test]
async fn upload_probes_validate_pixels_and_leave_the_image_table_untouched() {
    let (mut t, mut replies) = engine();
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(1, 1)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let encoded_png = STANDARD.encode(png.get_ref());
    let header_only = STANDARD.encode(&png.get_ref()[..33]);

    t.advance_bytes(format!(
        "\x1b_Ga=T,i=7,p=1,f=24,s=1,v=1,c=1,r=1,C=1,q=2;/wAA\x1b\\\
         \x1b_Ga=q,i=7,f=24,s=1,v=1;AP8A\x1b\\\
         \x1b_Ga=q,i=999,f=100;{encoded_png}\x1b\\\
         \x1b_Ga=q,i=11,f=100;{header_only}\x1b\\\
         \x1b_Ga=q,i=12,f=24,s=1,v=1;AAAAAA==\x1b\\\
         \x1b_Ga=q,i=13,f=32,s=1,v=1;AAAA\x1b\\\
         \x1b_Ga=q,i=14,f=24,s=1,v=1,o=z;AAAA\x1b\\\
         \x1b_Ga=q,i=15,f=24;AAAA\x1b\\\
         \x1b_Ga=q,i=16,f=24,s=8192,v=8192;AAAA\x1b\\\
         \x1b_Ga=q,i=17,f=24,s=4,v=4,o=z;eJxjYCANAAAAMAAB\x1b\\\
         \x1b_Ga=q,i=18,f=24,s=1,v=1,q=1;AAAA\x1b\\\
         \x1b_Ga=q,i=19,f=24,s=1,v=1,q=2;AA==\x1b\\\
         \x1b_Ga=q,f=24,s=1,v=1;AAAA\x1b\\\
         \x1b_Ga=q,i=20,I=30,f=24,s=1,v=1;AAAA\x1b\\\
         \x1b[1;2H\x1b_Ga=T,I=80,p=1,f=24,s=1,v=1,c=1,r=1,C=1;AAD/\x1b\\\
         \x1b[5n"
    ));
    let response = read_through(&mut replies, b"\x1b[0n").await;
    let received: Vec<_> = response
        .strip_suffix("\x1b[0n")
        .unwrap()
        .split_terminator("\x1b\\")
        .collect();
    let expected = [
        "\x1b_Gi=7;OK",
        "\x1b_Gi=999;OK",
        "\x1b_Gi=11;EINVAL:",
        "\x1b_Gi=12;EINVAL:",
        "\x1b_Gi=13;EINVAL:",
        "\x1b_Gi=14;EINVAL:",
        "\x1b_Gi=15;EINVAL:",
        "\x1b_Gi=16;EINVAL:",
        "\x1b_Gi=17;OK",
        "\x1b_GI=30,i=20;EINVAL:",
        "\x1b_GI=80,i=8,p=1;OK",
    ];
    assert_eq!(received.len(), expected.len(), "{response:?}");
    for (actual, expected) in received.iter().zip(expected) {
        assert!(actual.starts_with(expected), "{actual:?} vs {expected:?}");
    }
    let snapshot = frame(&t, None, 0);
    assert_eq!(pixel(&snapshot, 0, 0, 7), RED);
    assert_eq!(pixel(&snapshot, 1, 0, 8), Some([0, 0, 255, 255]));
    assert_eq!(pixel(&snapshot, 0, 0, 999), None);
}

#[tokio::test]
async fn upload_probe_chunks_reply_once_and_preserve_other_transfers() {
    let (mut t, mut replies) = engine();
    t.advance_bytes(concat!(
        "\x1b_Ga=q,i=7,f=32,s=1,v=1,m=1;AP8A\x1b\\",
        "\x1b_Ga=q,e=t,i=70,f=24\x1b\\",
        "\x1b[5n",
    ));
    assert_eq!(
        read_through(&mut replies, b"\x1b[0n").await,
        "\x1b_Gi=70,e=t,f=24;OK\x1b\\\x1b[0n",
    );
    t.advance_bytes(concat!(
        "\x1b_Gm=0;/w==\x1b\\",
        "\x1b_Ga=q,i=8,f=24,s=1,v=1,q=1,m=1;A\x1b\\",
        "\x1b_Gm=0;AA==\x1b\\",
        "\x1b_Ga=T,i=9,p=1,f=32,s=1,v=1,c=1,r=1,C=1,q=2,m=1;/wAA\x1b\\",
        "\x1b_Ga=q,i=71,f=24,s=1,v=1;AAAA\x1b\\",
        "\x1b_Ga=q,i=72,t=f,f=24,s=1,v=1;ZmFrZS1wYXRo\x1b\\",
        "\x1b_Ga=q,i=73,m=1,f=24,s=1,v=1;AAAA\x1b\\",
        "\x1b_Gm=0,q=2;/w==\x1b\\",
        "\x1b_Ga=q,i=74,f=32,s=1,v=1,m=1;AA==\x1b\\",
        "\x1b[1;2H\x1b_Ga=T,i=10,p=1,f=24,s=1,v=1,c=1,r=1,C=1;AAD/\x1b\\",
        "\x1b[5n",
    ));
    let response = read_through(&mut replies, b"\x1b[0n").await;
    let received: Vec<_> = response
        .strip_suffix("\x1b[0n")
        .unwrap()
        .split_terminator("\x1b\\")
        .collect();
    let expected = [
        "\x1b_Gi=7;OK",
        "\x1b_Gi=8;EINVAL:",
        "\x1b_Gi=71;OK",
        "\x1b_Gi=72;ENOTSUP:",
        "\x1b_Gi=73;EBUSY:",
        "\x1b_Gi=74;EINVAL:upload probe was interrupted",
        "\x1b_Gi=10,p=1;OK",
    ];
    assert_eq!(received.len(), expected.len(), "{response:?}");
    for (actual, expected) in received.iter().zip(expected) {
        assert!(actual.starts_with(expected), "{actual:?} vs {expected:?}");
    }
    let snapshot = frame(&t, None, 0);
    assert_eq!(pixel(&snapshot, 0, 0, 9), RED);
    assert_eq!(pixel(&snapshot, 1, 0, 10), Some([0, 0, 255, 255]));
}

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
async fn read_through(
    replies: &mut tokio::sync::mpsc::UnboundedReceiver<TransportInput>,
    suffix: &[u8],
) -> String {
    let mut answer = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !answer.ends_with(suffix) {
            let Some(TransportInput::Data(bytes)) = replies.recv().await else {
                panic!("expected a terminal reply");
            };
            answer.extend(bytes);
        }
    })
    .await
    .expect("terminal reply timed out");
    String::from_utf8(answer).unwrap()
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

#[tokio::test]
async fn kitty_replies_once_after_committing_each_addressed_upload_or_placement() {
    let (mut t, mut replies) = engine();
    t.advance_bytes(b"\x1b_Ga=t,i=7,f=32,s=1,v=1,m=1;/wAA\x1b\\\x1b[5n");
    assert_eq!(read_through(&mut replies, b"\x1b[0n").await, "\x1b[0n");
    t.advance_bytes(concat!(
        "\x1b_Gm=0;/w==\x1b\\",
        "\x1b_Ga=p,i=7,p=3,c=1,r=1,C=1\x1b\\",
        "\x1b[1;2H\x1b_Ga=T,I=81,p=4,f=24,s=1,v=1,c=1,r=1,C=1;AP8A\x1b\\",
        "\x1b[1;3H\x1b_Ga=p,I=81,p=5,c=1,r=1,C=1\x1b\\",
        "\x1b_Ga=t,i=9,f=24,s=1,v=1,q=1;AP8A\x1b\\",
        "\x1b_Ga=p,i=9,p=0,q=1\x1b\\",
        "\x1b_Ga=p,i=99,p=6,q=2\x1b\\",
        "\x1b_Ga=t,f=24,s=1,v=1;AP8A\x1b\\",
        "\x1b_Ga=p,p=333\x1b\\",
        "\x1b[5n",
    ));
    assert_eq!(
        read_through(&mut replies, b"\x1b[0n").await,
        concat!(
            "\x1b_Gi=7;OK\x1b\\",
            "\x1b_Gi=7,p=3;OK\x1b\\",
            "\x1b_GI=81,i=8,p=4;OK\x1b\\",
            "\x1b_GI=81,i=8,p=5;OK\x1b\\",
            "\x1b[0n",
        )
    );
    let committed = frame(&t, None, 0);
    assert_eq!(pixel(&committed, 0, 0, 7), RED);
    assert_eq!(pixel(&committed, 1, 0, 8), Some([0, 255, 0, 255]));
    assert_eq!(pixel(&committed, 2, 0, 8), Some([0, 255, 0, 255]));
}

#[tokio::test]
async fn kitty_upload_and_placement_errors_report_the_request_identifiers() {
    let (mut t, mut replies) = engine();
    t.advance_bytes(concat!(
        "\x1b_Ga=T,i=7,p=3,f=32,s=1,v=1,c=1,r=1,C=1,q=2;/wAA/w==\x1b\\",
        "\x1b_Ga=T,i=7,p=3,f=32,s=1,v=1,c=1,r=1,q=1;AP8A\x1b\\",
        "\x1b_Ga=p,i=7,p=4,x=9,q=1\x1b\\",
        "\x1b_Ga=p,i=99,p=5,q=1\x1b\\",
        "\x1b_Ga=p,I=99,p=6,q=1\x1b\\",
        "\x1b_Ga=T,I=99,p=7,t=s,f=24,s=1,v=1;ZmFrZS1vYmplY3Q=\x1b\\",
        "\x1b_Ga=p,i=7,I=99,p=8,q=1\x1b\\",
        "\x1b[5n",
    ));
    let response = read_through(&mut replies, b"\x1b[0n").await;
    let received: Vec<_> = response
        .strip_suffix("\x1b[0n")
        .unwrap()
        .split_terminator("\x1b\\")
        .collect();
    for (actual, expected) in received.iter().zip([
        "\x1b_Gi=7,p=3;EINVAL:",
        "\x1b_Gi=7,p=4;EINVAL:",
        "\x1b_Gi=99,p=5;ENOENT:",
        "\x1b_GI=99,p=6;ENOENT:",
        "\x1b_GI=99,p=7;ENOTSUP:",
        "\x1b_GI=99,i=7,p=8;EINVAL:",
    ]) {
        assert!(actual.starts_with(expected), "{actual:?} vs {expected:?}");
    }
    assert_eq!(received.len(), 6, "{response:?}");
    assert_eq!(pixel(&frame(&t, None, 0), 0, 0, 7), RED);
}

#[tokio::test]
async fn multipart_uploads_inherit_quiet_mode_and_allow_final_chunk_overrides() {
    let (mut t, mut replies) = engine();
    t.advance_bytes(concat!(
        "\x1b_Ga=T,i=7,p=3,f=32,s=1,v=1,c=1,r=1,C=1,q=2,m=1;/wAA\x1b\\",
        "\x1b_Gm=0;/w==\x1b\\",
        "\x1b[1;2H\x1b_Ga=T,I=80,p=4,f=32,s=1,v=1,c=1,r=1,C=1,q=1,m=1;AP8A\x1b\\",
        "\x1b_Gm=0;/w==\x1b\\",
        "\x1b[1;3H\x1b_Ga=T,i=9,p=5,f=32,s=1,v=1,c=1,r=1,C=1,m=1;AAD/\x1b\\",
        "\x1b_Gm=0,q=1;/w==\x1b\\",
        "\x1b[5n",
    ));
    assert_eq!(read_through(&mut replies, b"\x1b[0n").await, "\x1b[0n");
    let snapshot = frame(&t, None, 0);
    assert_eq!(pixel(&snapshot, 0, 0, 7), RED);
    assert_eq!(pixel(&snapshot, 1, 0, 8), Some([0, 255, 0, 255]));
    assert_eq!(pixel(&snapshot, 2, 0, 9), Some([0, 0, 255, 255]));
}

#[tokio::test]
async fn malformed_upload_chunks_reply_with_the_original_image_and_placement() {
    let (mut t, mut replies) = engine();
    t.advance_bytes(concat!(
        "\x1b_Ga=T,i=7,p=3,f=24,s=1,v=1,c=1,r=1,C=1,q=2;/wAA\x1b\\",
        "\x1b_Ga=T,i=7,p=3,f=32,s=1,v=1,c=1,r=1,C=1,q=1,m=1;AAD/\x1b\\",
        "\x1b_Gm=0;A\x1b\\",
        "\x1b_Ga=T,I=81,p=4,f=32,s=1,v=1,c=1,r=1,C=1,q=2,m=1;AAD/\x1b\\",
        "\x1b_Gm=0,q=1;A\x1b\\",
        "\x1b_Ga=T,i=8,p=5,f=32,s=1,v=1,c=1,r=1,C=1,m=1;AAD/\x1b\\",
        "\x1b_Gm=0,q=2;A\x1b\\",
        "\x1b_Ga=T,i=9,p=6,f=32,s=1,v=1,c=1,r=1,C=1,m=1;AAD/\x1b\\",
        "\x1b_Gm=0,t=f;ZmFrZS1wYXRo\x1b\\",
        "\x1b_Ga=f,i=7,r=1,f=32,s=1,v=1,m=1;AAD/\x1b\\",
        "\x1b_Ga=f,m=0;A\x1b\\",
        "\x1b[5n",
    ));
    let response = read_through(&mut replies, b"\x1b[0n").await;
    let received: Vec<_> = response
        .strip_suffix("\x1b[0n")
        .unwrap()
        .split_terminator("\x1b\\")
        .collect();
    let expected = [
        "\x1b_Gi=7,p=3;EINVAL:",
        "\x1b_GI=81,p=4;EINVAL:",
        "\x1b_Gi=9,p=6;ENOTSUP:",
        "\x1b_Gi=7,r=1;EINVAL:",
    ];
    assert_eq!(received.len(), expected.len(), "{response:?}");
    for (actual, expected) in received.iter().zip(expected) {
        assert!(actual.starts_with(expected), "{actual:?} vs {expected:?}");
    }
    assert_eq!(pixel(&frame(&t, None, 0), 0, 0, 7), RED);
}

#[tokio::test]
async fn upload_chunk_limit_replies_to_the_original_id_and_allows_recovery() {
    let (mut t, mut replies) = engine();
    t.advance_bytes(b"\x1b_Ga=T,i=7,p=3,f=24,s=1,v=1,m=1;AAAA\x1b\\");
    for _ in 0..16384 {
        t.advance_bytes(b"\x1b_Gm=1;\x1b\\");
    }
    t.advance_bytes(b"\x1b_Ga=T,i=8,p=4,f=24,s=1,v=1,c=1,r=1,C=1;/wAA\x1b\\\x1b[5n");
    assert_eq!(
        read_through(&mut replies, b"\x1b[0n").await,
        "\x1b_Gi=7,p=3;ENOSPC:too many kitty transmission chunks\x1b\\\x1b_Gi=8,p=4;OK\x1b\\\x1b[0n",
    );
    let snapshot = frame(&t, None, 0);
    assert_eq!(pixel(&snapshot, 0, 0, 7), None);
    assert_eq!(pixel(&snapshot, 0, 0, 8), RED);
}

#[test]
fn indexed_images_follow_palette_changes_and_keep_old_snapshots_intact() {
    let (mut t, _) = engine();
    let configured = t.palette_mut();
    let color = |r, g, b| wezterm_term::color::RgbColor::new_8bpc(r, g, b).into();
    configured.colors.0[247] = color(5, 6, 7);
    configured.selection_bg = color(8, 9, 10);
    configured.selection_fg = color(11, 12, 13);
    configured.cursor_bg = color(14, 15, 16);
    configured.cursor_fg = color(17, 18, 19);
    let payload = STANDARD.encode([1, 247, 248, 249, 250, 251, 252, 253, 254, 255]);
    t.advance_bytes(format!(
        "\x1b]4;1;#010203\x07\x1b]11;#223344\x07\x1b]10;#556677\x07\
         \x1b_Ga=T,f=8,s=10,v=1,i=9,c=10,r=1,C=1,q=2;{payload}\x1b\\"
    ));
    let old = frame(&t, None, 0);
    let check = |frame: &Snapshot, ansi, bg, fg| {
        let opaque = |rgb: [u8; 3]| [rgb[0], rgb[1], rgb[2], 255];
        for (x, rgb) in [
            ansi,
            [5, 6, 7],
            bg,
            fg,
            [8, 9, 10],
            [11, 12, 13],
            [14, 15, 16],
            [17, 18, 19],
            fg,
            fg,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(pixel(frame, x, 0, 9), Some(opaque(rgb)), "at column {x}");
        }
    };
    check(&old, [1, 2, 3], [34, 51, 68], [85, 102, 119]);

    t.advance_bytes(b"\x1b]4;1;#040608\x07\x1b]11;#102030\x07\x1b]10;#abcdef\x07");
    let updated = frame(&t, Some(&old), 0);
    check(&updated, [4, 6, 8], [16, 32, 48], [171, 205, 239]);
    check(&old, [1, 2, 3], [34, 51, 68], [85, 102, 119]);

    t.advance_bytes(b"\x1b[?5h");
    check(
        &frame(&t, Some(&updated), 0),
        [4, 6, 8],
        [171, 205, 239],
        [16, 32, 48],
    );
}

#[tokio::test]
async fn kitty_animation_replies_identify_committed_frames_and_keep_playback_silent() {
    let (mut t, mut replies) = engine();
    t.advance_bytes(concat!(
        "\x1b_Ga=T,i=7,f=32,s=1,v=1,c=1,r=1,C=1,q=2;/wAA/w==\x1b\\",
        "\x1b_Ga=f,i=7,f=32,s=1,v=1,m=1;AP8A\x1b\\",
        "\x1b[5n",
    ));
    assert_eq!(read_through(&mut replies, b"\x1b[0n").await, "\x1b[0n");
    t.advance_bytes(concat!(
        "\x1b_Ga=f,m=0;/w==\x1b\\",
        "\x1b_Ga=f,i=7,r=90,f=24,s=1,v=1;AAD/\x1b\\",
        "\x1b_Ga=c,i=7,r=2,c=3\x1b\\",
        "\x1b_Ga=f,i=7,r=1,f=32,s=1,v=1,q=2,m=1;AAD/\x1b\\",
        "\x1b_Gm=0;/w==\x1b\\",
        "\x1b_Ga=a,i=7,c=3\x1b\\",
        "\x1b_Ga=t,I=91,f=24,s=1,v=1,q=2;/wAA\x1b\\",
        "\x1b_Ga=f,I=91,r=0,f=24,s=1,v=1;AP8A\x1b\\",
        "\x1b_Ga=c,I=91,r=1,c=2\x1b\\",
        "\x1b[2;1H\x1b_Ga=T,i=9,f=24,s=2,v=1,c=2,r=1,C=1,q=2;/wAAAAD/\x1b\\",
        "\x1b_Ga=c,i=9,r=1,c=1,x=1,w=1,h=1\x1b\\",
        "\x1b[5n",
    ));
    assert_eq!(
        read_through(&mut replies, b"\x1b[0n").await,
        concat!(
            "\x1b_Gi=7,r=2;OK\x1b\\",
            "\x1b_Gi=7,r=3;OK\x1b\\",
            "\x1b_Gi=7;OK\x1b\\",
            "\x1b_GI=91,r=2;OK\x1b\\",
            "\x1b_GI=91;OK\x1b\\",
            "\x1b_Gi=9;OK\x1b\\",
            "\x1b[0n",
        ),
    );
    let composed = frame(&t, None, 0);
    assert_eq!(pixel(&composed, 0, 0, 7), Some([0, 255, 0, 255]));
    assert_eq!(pixel(&composed, 1, 1, 9), RED);
    t.advance_bytes(b"\x1b_Ga=a,i=7,c=1,q=2\x1b\\");
    assert_eq!(
        pixel(&frame(&t, Some(&composed), 0), 0, 0, 7),
        Some([0, 0, 255, 255])
    );
}

#[tokio::test]
async fn kitty_animation_errors_identify_the_failed_request_and_keep_committed_pixels() {
    let (mut t, mut replies) = engine();
    t.advance_bytes(concat!(
        "\x1b_Ga=T,i=7,f=24,s=1,v=1,c=1,r=1,C=1,q=2;/wAA\x1b\\",
        "\x1b_Ga=f,i=7,f=24,s=1,v=1,q=2;AP8A\x1b\\",
        "\x1b_Ga=f,i=7,r=2,f=32,s=1,v=1,q=1,m=1;A\x1b\\",
        "\x1b_Gm=0;/w==\x1b\\",
        "\x1b_Ga=c,i=7,r=3,c=2,q=1\x1b\\",
        "\x1b_Ga=c,i=7,r=1,c=2,x=4294967295,q=1\x1b\\",
        "\x1b_Ga=c,i=7,r=2,c=2,q=1\x1b\\",
        "\x1b_Ga=f,I=99,r=4,f=24,s=1,v=1,q=1;AAD/\x1b\\",
        "\x1b_Ga=a,i=99,r=2,z=9,q=1\x1b\\",
        "\x1b_Ga=c,i=7,r=1,c=99,q=2\x1b\\",
        "\x1b_Ga=a,i=7,c=2,q=2\x1b\\",
        "\x1b[5n",
    ));
    let response = read_through(&mut replies, b"\x1b[0n").await;
    let received: Vec<_> = response
        .strip_suffix("\x1b[0n")
        .unwrap()
        .split_terminator("\x1b\\")
        .collect();
    for (actual, expected) in received.iter().zip([
        "\x1b_Gi=7,r=2;EINVAL:",
        "\x1b_Gi=7;ENOENT:",
        "\x1b_Gi=7;EINVAL:animation composition rectangle is out of bounds",
        "\x1b_Gi=7;EINVAL:animation composition rectangles overlap",
        "\x1b_GI=99,r=4;ENOENT:",
        "\x1b_Gi=99,r=2;ENOENT:",
    ]) {
        assert!(actual.starts_with(expected), "{actual:?} vs {expected:?}");
    }
    assert_eq!(received.len(), 6, "{response:?}");
    assert_eq!(pixel(&frame(&t, None, 0), 0, 0, 7), Some([0, 255, 0, 255]));
}

#[tokio::test]
async fn kitty_animation_reports_a_full_frame_table_and_can_still_edit_existing_frames() {
    let (mut t, mut replies) = engine();
    transmit_red(&mut t, 7);
    for _ in 1..256 {
        t.advance_bytes(b"\x1b_Ga=f,i=7,f=24,s=1,v=1,q=2;AP8A\x1b\\");
    }
    t.advance_bytes(concat!(
        "\x1b_Ga=f,i=7,f=24,s=1,v=1;AAD/\x1b\\",
        "\x1b_Ga=f,i=7,r=256,f=24,s=1,v=1;AAD/\x1b\\",
        "\x1b[5n",
    ));
    assert_eq!(
        read_through(&mut replies, b"\x1b[0n").await,
        "\x1b_Gi=7;ENOSPC:animation storage limit reached\x1b\\\x1b_Gi=7,r=256;OK\x1b\\\x1b[0n",
    );
}

#[test]
fn anonymous_and_named_indexed_images_follow_palette_across_screen_switches() {
    let (mut t, _) = engine();
    t.advance_bytes(concat!(
        "\x1b]4;1;#ff0000\x07",
        "\x1b_Ga=T,f=16,s=1,v=1,c=1,r=1,C=1,o=z,q=2,m=1;eJxjbAIA\x1b\\",
        "\x1b_Gm=0,q=2;AIYAhA==\x1b\\",
        "\x1b[?1049h",
        "\x1b_Ga=T,f=16,s=1,v=1,i=7,c=1,r=1,C=1,q=2;AQA=\x1b\\",
    ));
    let alternate = frame(&t, None, 0);
    assert_eq!(pixel(&alternate, 0, 0, 7), Some([255, 0, 0, 0]));
    t.advance_bytes(b"\x1b]4;1;#00ff00\x07\x1b[?1049l");
    let primary = frame(&t, Some(&alternate), 0);
    assert_eq!(pixel(&primary, 0, 0, 0), Some([0, 255, 0, 130]));
    assert_eq!(pixel(&alternate, 0, 0, 7), Some([255, 0, 0, 0]));

    t.advance_bytes(concat!(
        "\x1b_Ga=d,d=A,q=2\x1b\\",
        "\x1b_Ga=T,f=8,s=1,v=1,i=7,c=1,r=1,C=1,q=2;AQ==\x1b\\",
        "\x1b_Ga=T,f=24,s=1,v=1,i=7,c=1,r=1,C=1,q=2;AAD/\x1b\\",
        "\x1b]4;1;#ffffff\x07",
    ));
    let replaced = frame(&t, Some(&primary), 0);
    assert_eq!(pixel(&replaced, 0, 0, 0), None);
    assert_eq!(pixel(&replaced, 0, 0, 7), Some([0, 0, 255, 255]));
}

#[tokio::test]
async fn indexed_queries_validate_pixels_and_animation_requests_preserve_the_image() {
    let (mut t, mut replies) = engine();
    t.advance_bytes(concat!(
        "\x1b]4;1;#ff0000\x07",
        "\x1b_Ga=T,f=8,s=1,v=1,i=7,c=1,r=1,C=1,q=2;AQ==\x1b\\",
        "\x1b_Ga=q,f=8,s=1,v=1,i=1;AQ==\x1b\\",
        "\x1b_Ga=q,f=16,s=1,v=1,i=2;AQ==\x1b\\",
        "\x1b_Ga=q,f=16,s=3000,v=2000,i=3;AA==\x1b\\",
        "\x1b_Ga=q,f=8,s=0,v=1,i=4;AQ==\x1b\\",
        "\x1b_Ga=T,f=16,s=1,v=1,i=7;AQ==\x1b\\",
        "\x1b_Ga=f,f=24,s=1,v=1,i=7;AAD/\x1b\\",
        "\x1b_Ga=c,i=7,r=1,c=1,w=1,h=1\x1b\\",
        "\x1b_Ga=a,i=7,s=3\x1b\\",
        "\x1b[5n",
    ));
    let output = read_through(&mut replies, b"\x1b[0n").await;
    assert!(output.contains("\x1b_Gi=1;OK\x1b\\"), "{output:?}");
    for id in [2, 3, 4] {
        assert!(
            output.contains(&format!("\x1b_Gi={id};EINVAL:")),
            "{output:?}"
        );
    }
    assert!(output.contains("exceeds 32 MiB"), "{output:?}");
    assert_eq!(output.matches("\x1b_Gi=7;EINVAL:").count(), 4, "{output:?}");
    assert_eq!(pixel(&frame(&t, None, 0), 0, 0, 7), RED);
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

#[test]
fn virtual_time_skips_whole_laps_and_preserves_live_pane_pacing() {
    use std::time::{Duration, Instant};
    let ms = Duration::from_millis;
    let start = Instant::now() + Duration::from_secs(3600);
    let (mut t, _) = engine();
    let (mut live, _) = engine();
    t.set_kitty_animation_time(Some(start));
    t.advance_bytes(ANIMATION);
    t.advance_bytes(b"\x1b_Ga=a,i=8,v=80000002,q=2\x1b\\");
    live.advance_bytes(ANIMATION);
    let live_due = live.kitty_animation_deadline().unwrap();
    assert!(live_due < start);
    assert_eq!(t.kitty_animation_deadline(), Some(start + ms(23)));

    // Root, green and blue occupy [0,23), [23,60) and [60,111) within a lap.
    let last_lap = start + ms(80_000_000 * 111);
    let step = last_lap + ms(35);
    t.set_kitty_animation_time(Some(step));
    assert!(t.advance_kitty_animations(step));
    assert_eq!(pixel(&frame(&t, None, 0), 0, 0, 8), Some([0, 255, 0, 255]));
    assert_eq!(t.kitty_animation_deadline(), Some(last_lap + ms(60)));
    assert!(!t.advance_kitty_animations(step));
    assert_eq!(live.kitty_animation_frame(Some(8)), 0);
    // A late wake on an ordinary pane still shows the next frame for its whole gap.
    assert!(live.advance_kitty_animations(live_due + ms(10_000)));
    assert_eq!(live.kitty_animation_frame(Some(8)), 1);
    assert_eq!(live.kitty_animation_deadline(), Some(live_due + ms(10_037)));
    let end = last_lap + ms(111);
    assert!(t.advance_kitty_animations(end - ms(1)));
    assert_eq!(t.kitty_animation_deadline(), Some(end));
    assert!(!t.advance_kitty_animations(end));
    assert_eq!(t.kitty_animation_frame(Some(8)), 2);
    assert_eq!(t.kitty_animation_deadline(), None);
}

#[test]
fn deleting_frames_rebases_running_playback_and_retires_the_last_frame() {
    use std::time::{Duration, Instant};
    let ms = Duration::from_millis;
    let start = Instant::now();
    let (mut t, _) = engine();
    t.set_kitty_animation_time(Some(start));
    t.advance_bytes(ANIMATION);
    t.advance_kitty_animations(start + ms(30));
    let old_green = frame(&t, None, 0);
    let green = Some([0, 255, 0, 255]);
    let blue = Some([0, 0, 255, 255]);
    assert_eq!(pixel(&old_green, 0, 0, 8), green);
    assert_eq!(t.kitty_animation_deadline(), Some(start + ms(60)));

    t.set_kitty_animation_time(Some(start + ms(40)));
    t.advance_bytes(b"\x1b_Ga=d,d=f,i=8\x1b\\");
    assert_eq!(t.kitty_animation_frame(Some(8)), 0);
    assert_eq!(pixel(&frame(&t, Some(&old_green), 0), 0, 0, 8), green);
    assert_eq!(t.kitty_animation_deadline(), Some(start + ms(60)));
    assert!(t.advance_kitty_animations(start + ms(60)));
    assert_eq!(pixel(&frame(&t, None, 0), 0, 0, 8), blue);

    t.set_kitty_animation_time(Some(start + ms(70)));
    t.advance_bytes(b"\x1b_Ga=d,d=F,i=8,r=4294967295\x1b\\");
    assert_eq!(pixel(&frame(&t, None, 0), 0, 0, 8), green);
    assert_eq!(t.kitty_animation_deadline(), None);
    t.advance_bytes(b"\x1b_Ga=d,d=f,i=8,r=0\x1b\\");
    assert_eq!(pixel(&frame(&t, None, 0), 0, 0, 8), green);
    t.advance_bytes(b"\x1b_Ga=f,i=8,f=32,s=1,v=1,z=51,q=2;AAD//w==\x1b\\");
    assert_eq!(t.kitty_animation_deadline(), Some(start + ms(107)));
    t.advance_bytes(b"\x1b_Ga=d,d=F,i=8,r=1\x1b\\\x1b_Ga=d,d=F,i=8\x1b\\");
    assert_eq!(t.kitty_animation_deadline(), None);
    assert_eq!(pixel(&frame(&t, None, 0), 0, 0, 8), None);
    assert_eq!(
        pixel(&old_green, 0, 0, 8),
        green,
        "published frames remain immutable"
    );
}

#[test]
fn deleting_the_current_middle_frame_restarts_the_successors_gap() {
    use std::time::{Duration, Instant};
    let ms = Duration::from_millis;
    let start = Instant::now();
    let (mut t, _) = engine();
    t.set_kitty_animation_time(Some(start));
    t.advance_bytes(ANIMATION);
    t.advance_kitty_animations(start + ms(30));
    t.set_kitty_animation_time(Some(start + ms(35)));
    t.advance_bytes(b"\x1b_Ga=d,d=f,i=8,r=2\x1b\\");
    assert_eq!(pixel(&frame(&t, None, 0), 0, 0, 8), Some([0, 0, 255, 255]));
    assert_eq!(t.kitty_animation_deadline(), Some(start + ms(86)));
}

#[test]
fn virtual_time_rechecks_active_frame_edits_and_resumes_loading_at_the_recorded_timestamp() {
    use std::time::{Duration, Instant};
    let ms = Duration::from_millis;
    let start = Instant::now() + Duration::from_secs(7200);
    let (mut t, _) = engine();
    t.set_kitty_animation_time(Some(start));
    t.advance_bytes(ANIMATION);
    t.advance_bytes(b"\x1b_Ga=a,i=8,v=1,q=2\x1b\\");
    let edit = start + ms(50);
    t.set_kitty_animation_time(Some(edit));
    assert!(t.advance_kitty_animations(edit));
    // Green has already been shown for 27 ms; shorten it to 10 ms and replace
    // its pixels with yellow. The change first takes effect at the edit time.
    t.advance_bytes(b"\x1b_Ga=f,i=8,f=32,s=1,v=1,r=2,z=10,q=2;//8A/w==\x1b\\");
    assert_eq!(
        pixel(&frame(&t, None, 0), 0, 0, 8),
        Some([255, 255, 0, 255])
    );
    assert_eq!(t.kitty_animation_deadline(), Some(edit));
    assert!(t.advance_kitty_animations(edit));
    assert_eq!(t.kitty_animation_frame(Some(8)), 2);
    assert_eq!(t.kitty_animation_deadline(), Some(edit + ms(51)));

    t.advance_bytes(b"\x1b_Ga=a,i=8,r=1,z=-1,q=2\x1b\\\x1b_Ga=a,i=8,r=2,z=-1,q=2\x1b\\\x1b_Ga=a,i=8,r=3,z=-1,s=2,c=1,q=2\x1b\\");
    assert_eq!(t.kitty_animation_deadline(), None);
    let arrival = start + Duration::from_secs(90_000);
    t.set_kitty_animation_time(Some(arrival));
    t.advance_bytes(b"\x1b_Ga=f,i=8,f=32,s=1,v=1,z=17,q=2;/wAA/w==\x1b\\");
    assert_eq!(t.kitty_animation_deadline(), Some(arrival));
    assert!(t.advance_kitty_animations(arrival));
    assert_eq!(t.kitty_animation_frame(Some(8)), 3);
    assert_eq!(
        t.kitty_animation_deadline(),
        None,
        "loading holds the last frame"
    );
    let next = arrival + ms(500);
    t.set_kitty_animation_time(Some(next));
    t.advance_bytes(b"\x1b_Ga=f,i=8,f=32,s=1,v=1,z=31,q=2;AP8A/w==\x1b\\");
    assert_eq!(t.kitty_animation_deadline(), Some(next + ms(17)));
    assert!(t.advance_kitty_animations(next + ms(17)));
    assert_eq!(t.kitty_animation_frame(Some(8)), 4);
    assert_eq!(t.kitty_animation_deadline(), None);
}

#[test]
fn restored_screen_uses_its_replay_timestamp_before_the_next_graphics_command() {
    use std::time::{Duration, Instant};
    let start = Instant::now();
    let (mut t, _) = engine();
    t.set_kitty_animation_time(Some(start));
    t.advance_bytes(ANIMATION);
    t.advance_bytes(b"\x1b[?1049h");
    let later = start + Duration::from_secs(86_400);
    t.set_kitty_animation_time(Some(later));
    t.advance_bytes(b"\x1b[?1049l\x1b_Ga=a,i=8,s=1,q=2\x1b\\");
    assert_eq!(t.kitty_animation_deadline(), None);
    assert_eq!(pixel(&frame(&t, None, 0), 0, 0, 8), Some([0, 0, 255, 255]));
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
        let response = read_through(&mut replies, b"\x1b\\").await;
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
fn visible_delete_retains_virtual_prototype_and_removes_its_displayed_child() {
    let (mut terminal, _) = engine();
    terminal.advance_bytes(PLACEMENTS);
    terminal.advance_bytes(b"\x1b_Ga=d,d=A\x1b\\");
    let committed = frame(&terminal, None, 0);
    assert!(pixel(&committed, 2, 2, 42).is_some());
    assert!(pixel(&committed, 5, 3, 43).is_none());
    assert!(pixel(&committed, 2, 4, 44).is_none());
}

const RED: Option<[u8; 4]> = Some([255, 0, 0, 255]);

fn transmit_red(t: &mut Terminal, id: u32) {
    t.advance_bytes(format!("\x1b_Ga=t,i={id},f=32,s=1,v=1,q=2;/wAA/w==\x1b\\"));
}

fn place(t: &mut Terminal, id: u32, placement: u32, row: usize, col: usize, height: usize) {
    t.advance_bytes(format!(
        "\x1b[{row};{col}H\x1b_Ga=p,i={id},p={placement},c=1,r={height},C=1,q=2\x1b\\"
    ));
}

#[test]
fn text_erasure_keeps_direct_kitty_layers_and_full_screen_erase_clears_them() {
    let (mut t, _) = engine();
    t.advance_bytes(concat!(
        "\x1b[2;3HTOP\x1b[3;3HLOW",
        "\x1b[2;3H\x1b_Ga=T,i=40,p=1,f=24,s=1,v=2,c=1,r=2,C=1,q=2;/wAAAP8A\x1b\\",
        "\x1b_Ga=T,i=41,p=2,f=24,s=1,v=1,c=1,r=1,C=1,q=2;AAD/\x1b\\",
        "\x1b_Ga=t,i=42,f=24,s=1,v=1,q=2;/wAA\x1b\\",
        "\x1b_Ga=p,i=42,p=3,P=40,Q=1,H=1,c=1,r=1,q=2\x1b\\",
    ));
    let original = frame(&t, None, 0);
    let check_layers = |snapshot: &Snapshot| {
        assert_eq!(pixel(snapshot, 2, 1, 40), RED);
        assert_eq!(pixel(snapshot, 2, 1, 41), Some([0, 0, 255, 255]));
        assert_eq!(pixel(snapshot, 2, 2, 40), Some([0, 255, 0, 255]));
        assert_eq!(pixel(snapshot, 3, 1, 42), RED);
    };
    check_layers(&original);

    t.advance_bytes(b"\x1b[2;3H\x1b[K");
    let line_erased = frame(&t, Some(&original), 0);
    assert!(!line_erased.rows[1].as_str().contains("TOP"));
    assert!(line_erased.rows[2].as_str().contains("LOW"));
    check_layers(&line_erased);

    t.advance_bytes(b"\x1b[2;1H\x1b[0J");
    let end_erased = frame(&t, Some(&line_erased), 0);
    assert!(!end_erased.rows[2].as_str().contains("LOW"));
    check_layers(&end_erased);

    t.advance_bytes(b"\x1b[2;3HTOP\x1b[3;3HLOW\x1b[3;3H\x1b[1J");
    let start_erased = frame(&t, Some(&end_erased), 0);
    assert!(!start_erased.rows[1].as_str().contains("TOP"));
    assert!(!start_erased.rows[2].as_str().contains("LOW"));
    assert!(start_erased.rows[2].as_str().contains("OW"));
    check_layers(&start_erased);

    t.advance_bytes(b"\x1b[2J");
    let cleared = frame(&t, Some(&start_erased), 0);
    assert_eq!(pixel(&cleared, 2, 1, 40), None);
    assert_eq!(pixel(&cleared, 2, 1, 41), None);
    assert_eq!(pixel(&cleared, 2, 2, 40), None);
    assert_eq!(pixel(&cleared, 3, 1, 42), None);
    place(&mut t, 40, 1, 2, 3, 2);
    place(&mut t, 42, 3, 2, 4, 1);
    let replaced = frame(&t, Some(&cleared), 0);
    assert_eq!(pixel(&replaced, 2, 1, 40), RED);
    assert_eq!(pixel(&replaced, 3, 1, 42), RED);
}

#[test]
fn line_erasure_preserves_tiles_on_a_wide_glyph_continuation() {
    let (mut t, _) = engine();
    t.advance_bytes("界");
    transmit_red(&mut t, 80);
    place(&mut t, 80, 1, 1, 2, 1);
    t.advance_bytes(b"\x1b[1;2H\x1b[K");
    let erased = frame(&t, None, 0);
    assert!(!erased.rows[0].as_str().contains('界'));
    assert_eq!(pixel(&erased, 1, 0, 80), RED);
}

#[test]
fn kitty_index_keeps_images_crossing_vertical_margins_and_clips_inside_images() {
    let (mut t, _) = engine();
    t.advance_bytes(concat!(
        "\x1b[2;2H\x1b_Ga=T,i=40,p=1,f=24,s=1,v=2,c=1,r=2,C=1,q=2;/wAAAP8A\x1b\\",
        "\x1b[3;5H\x1b_Ga=T,i=41,p=1,f=24,s=1,v=2,c=1,r=2,C=1,q=2;/wAAAAD/\x1b\\",
        "\x1b_Ga=t,i=42,f=24,s=1,v=1,q=2;/wAA\x1b\\",
        "\x1b_Ga=p,i=42,p=1,P=40,Q=1,H=1,c=1,r=1,q=2\x1b\\",
        "\x1b[4;7Hmove",
        "\x1b[3;6r\x1b[6;1H\x1bD",
    ));
    let scrolled = frame(&t, None, 0);
    assert_eq!(pixel(&scrolled, 1, 1, 40), RED);
    assert_eq!(pixel(&scrolled, 1, 2, 40), Some([0, 255, 0, 255]));
    assert_eq!(pixel(&scrolled, 2, 1, 42), RED);
    assert_eq!(pixel(&scrolled, 4, 2, 41), Some([0, 0, 255, 255]));
    assert_eq!(pixel(&scrolled, 4, 3, 41), None);
    assert!(scrolled.rows[2].as_str().contains("move"));

    t.advance_bytes(b"\x1bD");
    assert_eq!(pixel(&frame(&t, Some(&scrolled), 0), 4, 2, 41), None);
    place(&mut t, 41, 1, 4, 5, 2);
    let placed_again = frame(&t, Some(&scrolled), 0);
    assert_eq!(pixel(&placed_again, 4, 3, 41), RED);
    assert_eq!(pixel(&placed_again, 4, 4, 41), Some([0, 0, 255, 255]));
}

#[test]
fn kitty_reverse_index_honors_horizontal_margins_and_clips_at_the_bottom() {
    let (mut t, _) = engine();
    t.advance_bytes(concat!(
        "\x1b[?1049h",
        "\x1b[4;2H\x1b_Ga=T,i=40,p=1,f=24,s=2,v=1,c=2,r=1,C=1,q=2;/wAAAP8A\x1b\\",
        "\x1b[5;6H\x1b_Ga=T,i=41,p=1,f=24,s=1,v=2,c=1,r=2,C=1,q=2;/wAAAAD/\x1b\\",
        "\x1b[?69h\x1b[3;10s\x1b[2;6r\x1b[2;3H\x1bM",
    ));
    let scrolled = frame(&t, None, 0);
    assert_eq!(pixel(&scrolled, 1, 3, 40), RED);
    assert_eq!(pixel(&scrolled, 2, 3, 40), Some([0, 255, 0, 255]));
    assert_eq!(pixel(&scrolled, 2, 4, 40), None);
    assert_eq!(pixel(&scrolled, 5, 4, 41), None);
    assert_eq!(pixel(&scrolled, 5, 5, 41), RED);
}

#[test]
fn kitty_regional_scroll_clips_graphics_when_text_enters_primary_scrollback() {
    let (mut t, _) = engine();
    t.advance_bytes(concat!(
        "\x1b[1;1Htop",
        "\x1b[1;1H\x1b_Ga=T,i=40,p=1,f=24,s=1,v=2,c=1,r=2,C=1,q=2;/wAAAP8A\x1b\\",
        "\x1b[1;4r\x1b[4;1H\x1bD",
    ));
    let live = frame(&t, None, 0);
    assert_eq!(pixel(&live, 0, 0, 40), Some([0, 255, 0, 255]));
    let history = frame(&t, Some(&live), 1);
    assert_eq!(history.scroll_offset, 1);
    assert!(history.rows[0].as_str().contains("top"));
    assert_eq!(pixel(&history, 0, 0, 40), None);
    assert_eq!(pixel(&history, 0, 1, 40), Some([0, 255, 0, 255]));
}

#[test]
fn kitty_regional_scroll_keeps_a_placement_crossing_into_history_together() {
    let (mut t, _) = engine();
    t.advance_bytes(concat!(
        "\x1b_Ga=T,i=40,p=1,f=24,s=1,v=2,c=1,r=2,C=1,q=2;/wAAAP8A\x1b\\",
        "\x1b[8;1H\x1bD",
    ));
    let before = frame(&t, None, 1);
    assert_eq!(pixel(&before, 0, 0, 40), RED);
    assert_eq!(pixel(&before, 0, 1, 40), Some([0, 255, 0, 255]));

    t.advance_bytes(b"\x1b[1;4r\x1b[4;1H\x1bD");
    let after = frame(&t, Some(&before), 2);
    assert_eq!(after.scroll_offset, 2);
    assert_eq!(pixel(&after, 0, 0, 40), None);
    assert_eq!(pixel(&after, 0, 1, 40), RED);
    assert_eq!(pixel(&after, 0, 2, 40), Some([0, 255, 0, 255]));
}

#[test]
fn kitty_character_insert_and_delete_keep_direct_tiles_at_the_original_columns() {
    let (mut t, _) = engine();
    t.advance_bytes(concat!(
        "ABCD",
        "\x1b[1;2H\x1b_Ga=T,i=40,p=1,f=24,s=2,v=1,c=2,r=1,C=1,q=2;/wAAAP8A\x1b\\",
        "\x1b[1;3H\x1b_Ga=T,i=41,p=1,f=24,s=1,v=1,c=1,r=1,C=1,q=2;AAD/\x1b\\",
        "\x1b[1;2H\x1b[@",
    ));
    let inserted = frame(&t, None, 0);
    let check = |snapshot: &Snapshot| {
        assert_eq!(pixel(snapshot, 1, 0, 40), RED);
        assert_eq!(pixel(snapshot, 2, 0, 40), Some([0, 255, 0, 255]));
        assert_eq!(pixel(snapshot, 2, 0, 41), Some([0, 0, 255, 255]));
        assert_eq!(pixel(snapshot, 3, 0, 40), None);
        assert_eq!(pixel(snapshot, 3, 0, 41), None);
    };
    assert!(inserted.rows[0].as_str().starts_with("A BCD"));
    check(&inserted);

    t.advance_bytes(b"\x1b[P");
    let deleted = frame(&t, Some(&inserted), 0);
    assert!(deleted.rows[0].as_str().starts_with("ABCD"));
    check(&deleted);
}

#[test]
fn kitty_line_edits_keep_images_at_their_rows_when_text_moves_into_history() {
    let (mut t, _) = engine();
    t.advance_bytes(concat!(
        "top\x1b[2;1Hbottom",
        "\x1b[1;2H\x1b_Ga=T,i=40,p=1,f=24,s=1,v=2,c=1,r=2,C=1,q=2;/wAAAP8A\x1b\\",
        "\x1b[1;1H\x1b[M",
    ));
    let deleted = frame(&t, None, 0);
    assert!(deleted.rows[0].as_str().starts_with("bottom"));
    assert_eq!(pixel(&deleted, 1, 0, 40), RED);
    assert_eq!(pixel(&deleted, 1, 1, 40), Some([0, 255, 0, 255]));
    let history = frame(&t, Some(&deleted), 1);
    assert!(history.rows[0].as_str().starts_with("top"));
    assert_eq!(pixel(&history, 1, 0, 40), None);

    t.advance_bytes(b"\x1b[L");
    let inserted = frame(&t, Some(&deleted), 0);
    assert!(inserted.rows[1].as_str().starts_with("bottom"));
    assert_eq!(pixel(&inserted, 1, 0, 40), RED);
    assert_eq!(pixel(&inserted, 1, 1, 40), Some([0, 255, 0, 255]));
    assert_eq!(pixel(&inserted, 1, 2, 40), None);
}

#[test]
fn kitty_placeholder_images_move_with_inserted_characters_and_lines() {
    let (mut t, _) = engine();
    t.advance_bytes(b"\x1b_Ga=T,i=33554474,p=7,U=1,c=2,r=1,f=32,s=2,v=1,q=2;/wAA/wD/AP8=\x1b\\");
    t.advance_bytes("\x1b[38;2;0;0;42;58;2;0;0;7m\u{10eeee}\u{305}\u{305}\u{30e}\x1b[0m");
    let before = frame(&t, None, 0);
    assert_eq!(pixel(&before, 0, 0, 33554474), RED);

    t.advance_bytes(b"\x1b[H\x1b[@\x1b[L");
    let shifted = frame(&t, Some(&before), 0);
    assert_eq!(pixel(&shifted, 0, 0, 33554474), None);
    assert_eq!(pixel(&shifted, 1, 1, 33554474), RED);
}

#[test]
fn erasing_placeholder_text_hides_the_virtual_image() {
    let (mut t, _) = engine();
    t.advance_bytes(b"\x1b_Ga=T,i=33554474,p=7,U=1,c=2,r=1,f=32,s=2,v=1,q=2;/wAA/wD/AP8=\x1b\\");
    t.advance_bytes("\x1b[38;2;0;0;42;58;2;0;0;7m\u{10eeee}\u{305}\u{305}\u{30e}\u{10eeee}\x1b[0m");
    let before = frame(&t, None, 0);
    assert_eq!(pixel(&before, 0, 0, 33554474), RED);
    t.advance_bytes(b"\x1b[2K");
    let erased = frame(&t, Some(&before), 0);
    assert!(!erased.rows[0].as_str().contains('\u{10eeee}'));
    assert_eq!(pixel(&erased, 0, 0, 33554474), None);
}

#[test]
fn cell_delete_uses_the_live_grid_and_preserves_image_data_referenced_in_history() {
    for selector in ["d=C", "d=P,x=3,y=1"] {
        let (mut t, _) = engine();
        transmit_red(&mut t, 11);
        transmit_red(&mut t, 12);
        place(&mut t, 11, 1, 1, 1, 1);
        place(&mut t, 11, 2, 2, 3, 3);
        place(&mut t, 12, 1, 2, 3, 3);
        place(&mut t, 12, 2, 6, 10, 1);
        // Clip the second placement at the top edge of a bounded scroll.
        t.advance_bytes(b"\x1b[6;8r\x1b[8;1H\x1bD\x1b[r");
        t.advance_bytes(b"\x1b[8;1H\r\n\r\n\x1b[1;3H");
        let old = frame(&t, None, 2);
        assert_eq!(pixel(&old, 2, 1, 11), RED);
        assert_eq!(pixel(&old, 2, 2, 12), RED);
        assert_eq!(pixel(&old, 9, 5, 12), None);
        // Invalid and off-screen cells must not be clamped into the live grid.
        t.advance_bytes(b"\x1b_Ga=d,d=P,x=3,y=0\x1b\\\x1b_Ga=d,d=P,x=4294967295,y=1\x1b\\");
        assert_eq!(pixel(&frame(&t, None, 0), 2, 0, 11), RED);
        t.advance_bytes(format!("\x1b_Ga=d,{selector}\x1b\\"));
        let history = frame(&t, Some(&old), 2);
        assert_eq!(pixel(&history, 0, 0, 11), RED);
        for id in [11, 12] {
            for row in 1..=3 {
                assert_eq!(pixel(&history, 2, row, id), None, "{selector}");
            }
            place(&mut t, id, 3, 6, (id - 10) as usize, 1);
        }
        let live = frame(&t, None, 0);
        assert_eq!(pixel(&live, 0, 5, 11), RED, "history pins image data");
        assert_eq!(pixel(&live, 1, 5, 12), None, "last placement was deleted");
    }
}

#[test]
fn cell_and_z_deletion_preserve_other_layers_and_placeholder_text() {
    let (mut t, _) = engine();
    t.advance_bytes(PLACEMENTS);
    transmit_red(&mut t, 51);
    t.advance_bytes(b"\x1b[4;6H\x1b_Ga=p,i=51,p=1,z=7,c=1,r=1,C=1,q=2\x1b\\");
    let start = frame(&t, None, 0);
    assert!(pixel(&start, 5, 3, 43).is_some());
    assert_eq!(pixel(&start, 5, 3, 51), RED);
    t.advance_bytes(b"\x1b_Ga=d,d=Q,x=6,y=4,z=7\x1b\\");
    let filtered = frame(&t, Some(&start), 0);
    assert_eq!(pixel(&filtered, 5, 3, 51), None);
    assert!(pixel(&filtered, 5, 3, 43).is_some());
    t.advance_bytes(b"\x1b_Ga=d,d=p,x=6,y=4\x1b\\\x1b_Ga=d,d=P,x=3,y=3\x1b\\");
    let deleted = frame(&t, Some(&filtered), 0);
    assert_eq!(pixel(&deleted, 5, 3, 43), None);
    assert!(
        pixel(&deleted, 2, 2, 42).is_some(),
        "virtual prototype survives"
    );
    assert_eq!(
        deleted.rows[2].get_cell(2).unwrap().str(),
        start.rows[2].get_cell(2).unwrap().str()
    );
    place(&mut t, 43, 91, 7, 1, 1);
    place(&mut t, 51, 2, 7, 3, 1);
    let redisplayed = frame(&t, Some(&deleted), 0);
    assert!(
        pixel(&redisplayed, 0, 6, 43).is_some(),
        "lowercase retains bytes"
    );
    assert_eq!(
        pixel(&redisplayed, 2, 6, 51),
        None,
        "uppercase releases bytes"
    );
}

#[test]
fn row_and_column_deletion_select_whole_placements_at_the_correct_history_offsets() {
    let (mut t, _) = engine();
    for id in 11..=14 {
        transmit_red(&mut t, id);
    }
    place(&mut t, 11, 1, 1, 3, 1);
    place(&mut t, 12, 1, 2, 3, 3);
    place(&mut t, 13, 1, 3, 6, 2);
    place(&mut t, 14, 1, 6, 6, 1);
    t.advance_bytes(b"\x1b[8;1H\r\n\r\n");
    let start = frame(&t, None, 2);
    for (x, y, id) in [(2, 0, 11), (2, 1, 12), (5, 2, 13), (5, 5, 14)] {
        assert_eq!(pixel(&start, x, y, id), RED);
    }
    t.advance_bytes(b"\x1b_Ga=d,d=Y,y=0\x1b\\\x1b_Ga=d,d=X,x=4294967295\x1b\\");
    assert_eq!(pixel(&frame(&t, None, 0), 2, 0, 12), RED);
    t.advance_bytes(b"\x1b_Ga=d,d=y,y=1\x1b\\");
    let after_row = frame(&t, Some(&start), 2);
    assert_eq!(pixel(&after_row, 2, 0, 11), RED);
    assert_eq!(pixel(&after_row, 2, 1, 12), None);
    assert_eq!(pixel(&after_row, 5, 3, 13), None);
    place(&mut t, 12, 2, 5, 3, 1);
    place(&mut t, 13, 2, 5, 7, 1);
    t.advance_bytes(b"\x1b_Ga=d,d=X,x=3\x1b\\");
    let after_column = frame(&t, Some(&after_row), 2);
    assert_eq!(pixel(&after_column, 2, 0, 11), None);
    assert_eq!(pixel(&after_column, 5, 5, 14), RED);
    place(&mut t, 11, 3, 2, 1, 1);
    place(&mut t, 12, 3, 2, 2, 1);
    let live = frame(&t, None, 0);
    assert_eq!(pixel(&live, 0, 1, 11), None);
    assert_eq!(pixel(&live, 1, 1, 12), None);
    assert_eq!(pixel(&live, 6, 4, 13), RED);
}

#[test]
fn z_deletion_also_reaches_history_and_skips_virtual_placements() {
    let (mut t, _) = engine();
    t.advance_bytes(PLACEMENTS);
    transmit_red(&mut t, 51);
    t.advance_bytes(concat!(
        "\x1b[1;10H\x1b_Ga=p,i=51,p=1,z=-7,c=1,r=1,C=1,q=2\x1b\\",
        "\x1b[6;10H\x1b_Ga=p,i=51,p=2,z=-7,c=1,r=1,C=1,q=2\x1b\\",
        "\x1b[6;11H\x1b_Ga=p,i=51,p=3,z=6,c=1,r=1,C=1,q=2\x1b\\",
        "\x1b[8;1H\r\n\r\n",
    ));
    let start = frame(&t, None, 2);
    assert!(pixel(&start, 2, 2, 42).is_some());
    assert!(pixel(&start, 5, 3, 43).is_some());
    t.advance_bytes(b"\x1b_Ga=d,d=z,z=0\x1b\\\x1b_Ga=d,d=Z,z=-7\x1b\\");
    let deleted = frame(&t, Some(&start), 2);
    assert!(pixel(&deleted, 2, 2, 42).is_some());
    assert_eq!(pixel(&deleted, 5, 3, 43), None);
    assert_eq!(pixel(&deleted, 9, 0, 51), None);
    assert_eq!(pixel(&deleted, 9, 5, 51), None);
    assert_eq!(pixel(&deleted, 10, 5, 51), RED);
}

#[test]
fn image_number_deletion_selects_only_the_newest_surviving_image_and_matching_placement() {
    let (mut t, _) = engine();
    let upload = |t: &mut Terminal, col, pixels| {
        t.advance_bytes(format!(
            "\x1b[2;{col}H\x1b_Ga=T,I=77,p=1,f=32,s=1,v=1,c=1,r=1,C=1,q=2;{pixels}\x1b\\"
        ));
    };
    let put = |t: &mut Terminal, col, p| {
        t.advance_bytes(format!(
            "\x1b[3;{col}H\x1b_Ga=p,I=77,p={p},c=1,r=1,C=1,q=2\x1b\\"
        ));
    };
    upload(&mut t, 2, "/wAA/w==");
    upload(&mut t, 5, "AAD//w==");
    put(&mut t, 5, 2);
    let start = frame(&t, None, 0);
    let id_at = |col| {
        start.rows[1]
            .get_cell(col)
            .unwrap()
            .attrs()
            .images()
            .unwrap()[0]
            .image_id()
            .unwrap()
    };
    let red = id_at(1);
    let blue = id_at(4);
    let blue_pixel = Some([0, 0, 255, 255]);
    assert_ne!(red, blue);

    t.advance_bytes(b"\x1b_Ga=d,d=N,I=77,p=99\x1b\\\x1b_Ga=d,d=n,I=77,p=1\x1b\\");
    let partial = frame(&t, Some(&start), 0);
    assert_eq!(pixel(&partial, 1, 1, red), RED);
    assert_eq!(pixel(&partial, 4, 1, blue), None);
    assert_eq!(pixel(&partial, 4, 2, blue), blue_pixel);

    t.advance_bytes(b"\x1b_Ga=d,d=n,I=77\x1b\\");
    put(&mut t, 7, 3);
    let soft = frame(&t, Some(&partial), 0);
    assert_eq!(pixel(&soft, 4, 2, blue), None);
    assert_eq!(pixel(&soft, 6, 2, blue), blue_pixel);
    t.advance_bytes(b"\x1b_Ga=d,d=N,I=77,p=3\x1b\\");
    put(&mut t, 7, 3);
    let released = frame(&t, Some(&soft), 0);
    assert_eq!(pixel(&released, 6, 2, blue), None);
    assert_eq!(pixel(&released, 6, 2, red), RED);

    upload(&mut t, 5, "AAD//w==");
    let latest = frame(&t, Some(&released), 0);
    let latest = latest.rows[1]
        .get_cell(4)
        .unwrap()
        .attrs()
        .images()
        .unwrap()[0]
        .image_id()
        .unwrap();
    t.advance_bytes(format!("\x1b_Ga=d,d=I,i={latest}\x1b\\"));
    put(&mut t, 8, 4);
    assert_eq!(pixel(&frame(&t, None, 0), 7, 2, red), RED);
    t.advance_bytes(b"\x1b_Ga=d,d=N,I=77\x1b\\");
    put(&mut t, 9, 5);
    assert_eq!(pixel(&frame(&t, None, 0), 8, 2, red), None);
}

#[test]
fn targeted_delete_preserves_unnamed_placements_until_named_image_retransmission() {
    let (mut t, _) = engine();
    transmit_red(&mut t, 41);
    for (column, placement) in [(1, ""), (3, ",p=0"), (5, ",p=27")] {
        t.advance_bytes(format!(
            "\x1b[1;{column}H\x1b_Ga=p,i=41{placement},c=1,r=1,C=1,q=2\x1b\\"
        ));
    }
    let initial = frame(&t, None, 0);
    for x in [0, 2, 4] {
        assert_eq!(pixel(&initial, x, 0, 41), RED);
    }
    t.advance_bytes(b"\x1b_Ga=d,d=I,i=41,p=27\x1b\\");
    let deleted = frame(&t, Some(&initial), 0);
    assert_eq!(pixel(&deleted, 0, 0, 41), RED);
    assert_eq!(pixel(&deleted, 2, 0, 41), RED);
    assert_eq!(pixel(&deleted, 4, 0, 41), None);

    t.advance_bytes(b"\x1b_Ga=t,i=41,f=32,s=1,v=1,q=2;AAD//w==\x1b\\");
    let replaced = frame(&t, Some(&deleted), 0);
    for x in [0, 2, 4] {
        assert_eq!(pixel(&replaced, x, 0, 41), None);
    }
    place(&mut t, 41, 27, 2, 6, 1);
    assert_eq!(
        pixel(&frame(&t, Some(&replaced), 0), 5, 1, 41),
        Some([0, 0, 255, 255])
    );
}

#[test]
fn visible_delete_removes_intersecting_placements_and_preserves_historical_references() {
    for (delete, redisplay) in [("a=d", RED), ("a=d,d=a", RED), ("a=d,d=A", None)] {
        let (mut t, _) = engine();
        for id in 11..=14 {
            transmit_red(&mut t, id);
        }
        place(&mut t, 11, 1, 1, 1, 1);
        place(&mut t, 11, 2, 5, 7, 1); // A second placement sharing the historical image.
        place(&mut t, 12, 1, 2, 3, 2); // Straddles the new top edge.
        place(&mut t, 13, 1, 4, 5, 1);
        t.advance_bytes(b"\x1b[8;1H\r\n\r\n");
        let history = frame(&t, None, 2);
        let live = frame(&t, None, 0);
        assert_eq!(pixel(&history, 0, 0, 11), RED);
        assert_eq!(pixel(&history, 2, 1, 12), RED);
        assert_eq!(pixel(&live, 2, 0, 12), RED);
        assert_eq!(pixel(&live, 4, 1, 13), RED);
        assert_eq!(pixel(&live, 6, 2, 11), RED);

        t.advance_bytes(format!("\x1b_G{delete}\x1b\\"));
        let history = frame(&t, Some(&history), 2);
        let live = frame(&t, Some(&live), 0);
        assert_eq!(pixel(&history, 0, 0, 11), RED, "{delete}");
        assert_eq!(pixel(&history, 2, 1, 12), None, "{delete}");
        assert_eq!(pixel(&live, 2, 0, 12), None, "{delete}");
        assert_eq!(pixel(&live, 4, 1, 13), None, "{delete}");
        assert_eq!(pixel(&live, 6, 2, 11), None, "{delete}");

        for id in 11..=14 {
            place(&mut t, id, 3, 7, (id - 10) as usize, 1);
        }
        let placed = frame(&t, Some(&live), 0);
        assert_eq!(
            pixel(&placed, 0, 6, 11),
            RED,
            "historical reference keeps data"
        );
        assert_eq!(pixel(&placed, 1, 6, 12), redisplay, "{delete}");
        assert_eq!(pixel(&placed, 2, 6, 13), redisplay, "{delete}");
        assert_eq!(
            pixel(&placed, 3, 6, 14),
            RED,
            "unplaced image was not visible"
        );
    }
}

#[tokio::test]
async fn id_range_deletes_both_endpoints_and_releases_only_matching_images() {
    for (selector, redisplay) in [('r', RED), ('R', None)] {
        let (mut t, mut replies) = engine();
        for id in 19..=23 {
            transmit_red(&mut t, id);
            if id != 21 {
                // Keep one matching image in storage without a placement.
                place(
                    &mut t,
                    id,
                    1,
                    if id == 20 { 1 } else { 4 },
                    (id - 18) as usize,
                    1,
                );
            }
        }
        t.advance_bytes(b"\x1b[8;1H\r\n");
        let history = frame(&t, None, 1);
        let live = frame(&t, None, 0);
        assert_eq!(pixel(&history, 1, 0, 20), RED);
        assert_eq!(pixel(&live, 3, 2, 22), RED);
        t.advance_bytes(format!("\x1b_Ga=d,d={selector},x=20,y=22\x1b\\"));
        assert_eq!(pixel(&frame(&t, Some(&history), 1), 1, 0, 20), None);
        let deleted = frame(&t, Some(&live), 0);
        assert_eq!(pixel(&deleted, 0, 2, 19), RED);
        assert_eq!(pixel(&deleted, 3, 2, 22), None);
        assert_eq!(pixel(&deleted, 4, 2, 23), RED);

        for id in 19..=23 {
            place(&mut t, id, 2, 6, (id - 18) as usize, 1);
        }
        let placed = frame(&t, Some(&deleted), 0);
        for (x, expected) in [RED, redisplay, redisplay, redisplay, RED]
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                pixel(&placed, x, 5, 19 + x as u32),
                expected,
                "d={selector}, x={x}"
            );
        }

        // Delete itself is silent. A later identified animation command with q=1
        // reports a missing backing image; the status query fences both replies.
        t.advance_bytes(b"\x1b_Ga=a,i=21,s=1,q=1\x1b\\\x1b[5n");
        let reply = read_through(&mut replies, b"\x1b[0n").await;
        let answer = reply.strip_suffix("\x1b[0n").unwrap();
        if selector == 'R' {
            let error = answer
                .strip_prefix("\x1b_Gi=21;")
                .unwrap()
                .strip_suffix("\x1b\\")
                .unwrap();
            assert_eq!(error.split(':').next(), Some("ENOENT"));
        } else {
            assert_eq!(answer, "");
        }
    }
}

#[test]
fn deleting_on_the_alternate_screen_preserves_the_primary_image_with_the_same_id() {
    for delete in ["a=d,d=A", "a=d,d=R,x=41,y=41"] {
        let (mut t, _) = engine();
        transmit_red(&mut t, 41);
        place(&mut t, 41, 7, 3, 5, 1);
        let primary = frame(&t, None, 0);
        assert_eq!(pixel(&primary, 4, 2, 41), RED);
        t.advance_bytes(b"\x1b[?1049h");
        place(&mut t, 41, 7, 3, 5, 1);
        assert_eq!(pixel(&frame(&t, None, 0), 4, 2, 41), None);
        t.advance_bytes(b"\x1b_Ga=t,i=41,f=32,s=1,v=1,q=2;AAD//w==\x1b\\");
        place(&mut t, 41, 7, 3, 5, 1);
        let alternate = frame(&t, None, 0);
        assert_eq!(pixel(&alternate, 4, 2, 41), Some([0, 0, 255, 255]));
        t.advance_bytes(format!("\x1b_G{delete}\x1b\\"));
        place(&mut t, 41, 8, 2, 2, 1);
        let cleared = frame(&t, Some(&alternate), 0);
        assert_eq!(pixel(&cleared, 4, 2, 41), None);
        assert_eq!(pixel(&cleared, 1, 1, 41), None);
        t.advance_bytes(b"\x1b[?1049l");
        place(&mut t, 41, 8, 2, 2, 1);
        let restored = frame(&t, None, 0);
        assert_eq!(
            pixel(&restored, 4, 2, 41),
            RED,
            "{delete}: primary placement"
        );
        assert_eq!(pixel(&restored, 1, 1, 41), RED, "{delete}: primary data");
    }
}

#[test]
fn replacing_a_moved_placement_then_deleting_it_leaves_no_tiles_in_the_retained_grid() {
    fn tile_positions(t: &Terminal) -> Vec<(usize, usize)> {
        let mut positions = Vec::new();
        for (row, line) in t.screen().physical_lines().enumerate() {
            for cell in line.visible_cells() {
                for tile in cell.attrs().images().into_iter().flatten() {
                    if tile.image_id() == Some(41) {
                        positions.push((row, cell.cell_index()));
                    }
                }
            }
        }
        positions
    }
    for resize in [true, false] {
        let (mut t, _) = engine();
        t.advance_bytes(b"first text line\r\nsecond text line");
        transmit_red(&mut t, 41);
        place(&mut t, 41, 7, 2, 14, 1);
        assert_eq!(tile_positions(&t), [(1, 13)]);
        if resize {
            let size = t.get_size();
            t.resize(TerminalSize {
                cols: 8,
                pixel_width: 80,
                ..size
            });
        } else {
            t.advance_bytes(b"\x1b[2T");
        }
        let moved = tile_positions(&t);
        assert_eq!(moved.len(), 1);
        assert!(
            moved[0].0 > 1,
            "old tile must move beyond row 1: {moved:?}, resize={resize}"
        );
        t.advance_bytes(b"\x1b[H\x1b_Ga=T,i=41,p=7,c=1,r=1,C=1,f=32,s=1,v=1,q=2;AAD//w==\x1b\\");
        assert_eq!(pixel(&frame(&t, None, 0), 0, 0, 41), Some([0, 0, 255, 255]));
        assert_eq!(
            tile_positions(&t),
            [(t.screen().phys_row(0), 0)],
            "stale tile after replacement"
        );
        t.advance_bytes(b"\x1b_Ga=d,d=R,x=41,y=41\x1b\\");
        assert!(tile_positions(&t).is_empty(), "stale tile after deletion");
    }
}

#[test]
fn same_image_and_placement_ids_on_reconnect_leave_the_previous_sessions_pixels_in_history() {
    let (mut t, _) = engine();
    transmit_red(&mut t, 41);
    place(&mut t, 41, 7, 1, 1, 1);
    t.reset_connection();
    assert_eq!(pixel(&frame(&t, None, usize::MAX), 0, 0, 41), RED);
    // A new connection is free to reuse both identifiers at the exact same cell.
    t.advance_bytes(b"\x1b_Ga=t,i=41,f=32,s=1,v=1,q=2;AAD//w==\x1b\\");
    place(&mut t, 41, 7, 1, 1, 1);
    let current = frame(&t, None, 0);
    assert_eq!(pixel(&current, 0, 0, 41), Some([0, 0, 255, 255]));
    assert_eq!(pixel(&frame(&t, None, usize::MAX), 0, 0, 41), RED);
    t.advance_bytes(b"\x1b_Ga=d,d=R,x=41,y=41\x1b\\");
    assert_eq!(pixel(&frame(&t, Some(&current), 0), 0, 0, 41), None);
    assert_eq!(pixel(&frame(&t, None, usize::MAX), 0, 0, 41), RED);
}

#[test]
fn both_screens_share_the_image_id_budget() {
    let (mut t, _) = engine();
    for id in 1..=512 {
        transmit_red(&mut t, id);
    }
    place(&mut t, 512, 1, 1, 1, 1);
    assert_eq!(pixel(&frame(&t, None, 0), 0, 0, 512), RED);
    t.advance_bytes(b"\x1b[?1049h");
    transmit_red(&mut t, 513);
    place(&mut t, 513, 1, 1, 1, 1);
    assert_eq!(pixel(&frame(&t, None, 0), 0, 0, 513), None);
    t.advance_bytes(b"\x1b[?1049l\x1b_Ga=d,d=R,x=1,y=1\x1b\\\x1b[?1049h");
    transmit_red(&mut t, 513);
    place(&mut t, 513, 1, 1, 1, 1);
    assert_eq!(pixel(&frame(&t, None, 0), 0, 0, 513), RED);
    t.advance_bytes(b"\x1b[?1049l");
    assert_eq!(pixel(&frame(&t, None, 0), 0, 0, 512), RED);
}

#[test]
fn delete_aborts_an_unfinished_chunk_even_when_the_selector_matches_nothing() {
    for delete in ["a=d", "a=d,d=i,i=99", "a=d,d=R,x=80,y=81"] {
        let (mut t, _) = engine();
        // Three bytes of a four-byte RGBA pixel precede deletion. The orphaned
        // continuation would make it a valid green pixel if the upload survived.
        t.advance_bytes(b"\x1b_Ga=T,i=31,p=1,f=32,s=1,v=1,c=1,r=1,C=1,q=2,m=1;AP8A\x1b\\");
        t.advance_bytes(format!("\x1b_G{delete}\x1b\\"));
        t.advance_bytes(b"\x1b_Gm=0,q=2;/w==\x1b\\");
        place(&mut t, 31, 2, 1, 2, 1);
        let cancelled = frame(&t, None, 0);
        assert_eq!(pixel(&cancelled, 0, 0, 31), None, "{delete}");
        assert_eq!(pixel(&cancelled, 1, 0, 31), None, "{delete}");
        transmit_red(&mut t, 31);
        place(&mut t, 31, 3, 1, 3, 1);
        assert_eq!(pixel(&frame(&t, Some(&cancelled), 0), 2, 0, 31), RED);
    }
}
