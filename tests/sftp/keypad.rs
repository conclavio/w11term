//! Scan-code-bearing Win32 events through the native GUI and a real SSH channel.
use super::*;

enum Keys {
    Keypad,
    PhysicalLayout,
}

struct Captured {
    wire: Vec<u8>,
    logical: Option<char>,
}

async fn capture(mode: &[u8], keys: Keys) -> Captured {
    let (Source::Ssh(ssh), remote, opens, server) = fixture_server(false).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let trace = remote.path().join("keyboard-input.bin");
    fs::write(&trace, []).unwrap();
    let mut banner = mode.to_vec();
    banner.extend_from_slice(b"keypad-fixture-ready\r\n");
    fs::write(remote.path().join("copy-history.vt"), banner).unwrap();
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({"profiles": {"keypad-fixture": ssh}})).unwrap(),
    )
    .unwrap();
    let mut driver = std::process::Command::new("powershell.exe");
    driver
        .args(["-NoProfile", "-NonInteractive", "-File"])
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/keypad-ui.ps1"))
        .args([
            "-Gui",
            env!("CARGO_BIN_EXE_w11term"),
            "-Cli",
            env!("CARGO_BIN_EXE_w11term-cli"),
        ])
        .arg("-State")
        .arg(state.path())
        .arg("-Trace")
        .arg(&trace);
    if !mode.is_empty() {
        driver.arg("-Kitty");
    }
    if matches!(keys, Keys::PhysicalLayout) {
        driver.arg("-PhysicalLayout");
    }
    let result = driver.output().unwrap();
    let wire = fs::read(trace).unwrap();
    assert!(
        result.status.success(),
        "{}\n{}\nwire: {:?}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
        String::from_utf8_lossy(&wire),
        fs::read_to_string(state.path().join("gui.log")).unwrap_or_default()
    );
    assert_eq!(opens.load(Ordering::Relaxed), 1);
    server.abort();
    let logical = match keys {
        Keys::Keypad => None,
        Keys::PhysicalLayout => {
            let value = fs::read_to_string(state.path().join("logical-scalar.txt")).unwrap();
            Some(char::from_u32(value.trim().parse().unwrap()).unwrap())
        }
    };
    Captured { wire, logical }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "posts scan-code-bearing keypad messages to a real WinUI window over loopback SSH"]
async fn native_keypad_preserves_legacy_bytes() {
    let wire = capture(b"", Keys::Keypad).await.wire;
    assert_eq!(wire, b"\r\r\r\x1b[D\x1b[D4+\x1b[24~");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "checks kitty keypad identities, held keys and releases over loopback SSH"]
async fn native_keypad_kitty_event_bytes() {
    let wire = capture(b"\x1b[>11u", Keys::Keypad).await.wire;
    // The driver posts the VK and E0 bits for each physical key without
    // toggling the user's NumLock or CapsLock. Every keypad report must carry
    // the same lock mask throughout the captured sequence.
    let expected = [1, 65, 129, 193].map(|m| {
        let press = if m == 1 {
            String::new()
        } else {
            format!(";{m}")
        };
        format!(
            concat!(
                "\x1b[13;1u",         // main Enter down
                "\x1b[57414{press}u", // keypad Enter down while main is held
                "\x1b[13;1:3u",       // main Enter up
                "\x1b[57414;{m}:2u\x1b[57414;{m}:3u",
                "\x1b[57417{press}u\x1b[57417;{m}:3u", // NumLock-off keypad Left
                "\x1b[1;1D\x1b[1;1:3D",                // enhanced Left
                "\x1b[57403{press}u\x1b[57403;{m}:3u", // keypad digit 4
                "\x1b[57413{press}u\x1b[57413;{m}:3u", // keypad Add
                "\x1b[24;1~\x1b[24;1:3~",              // trailing F12 fence
            ),
            m = m,
            press = press
        )
    });
    assert!(
        expected
            .iter()
            .any(|candidate| candidate.as_bytes() == wire),
        "unexpected kitty keypad transcript: {:?}; expected one of {expected:?}",
        String::from_utf8_lossy(&wire)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "checks PC-101 alternate keys and held-key ownership over loopback SSH"]
async fn native_physical_layout_reports_distinct_scans_for_the_same_logical_key() {
    let capture = capture(b"\x1b[>15u", Keys::PhysicalLayout).await;
    let k = u32::from(capture.logical.unwrap());
    // Compare whole streams. Keep both lock keys as the fixture found them.
    // A press omits an unused modifiers field; repeats and releases require it.
    let expected = [1, 65, 129, 193].map(|modifier| {
        let press = if modifier == 1 {
            String::new()
        } else {
            format!(";{modifier}")
        };
        format!(
            concat!(
                "\x1b[{k}::121{press}u\x1b[{k}::122{press}u", // Y and Z down
                "\x1b[{k}::121;{m}:2u",                       // Y repeats
                "\x1b[{k}::122;{m}:3u\x1b[{k}::121;{m}:3u",   // Z then Y up
                "\x1b[24;1~\x1b[24;1:3~",                     // F12
            ),
            k = k,
            m = modifier,
            press = press,
        )
    });
    assert!(
        expected
            .iter()
            .any(|bytes| bytes.as_bytes() == capture.wire),
        "wire {:?}; expected one of {expected:?}",
        String::from_utf8_lossy(&capture.wire),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "keeps active-layout text for physically remapped keys in legacy mode"]
async fn native_physical_layout_preserves_legacy_text() {
    let capture = capture(b"", Keys::PhysicalLayout).await;
    let key = capture.logical.unwrap();
    assert_eq!(capture.wire, format!("{key}{key}{key}\x1b[24~").as_bytes());
}
