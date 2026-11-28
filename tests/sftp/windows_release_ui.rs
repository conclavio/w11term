use super::drive_gui;
use serde_json::json;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    time::{Duration, Instant},
};
use w11term::windows_release::{VERSION, build_commit};

#[test]
#[ignore = "opens Settings in WinUI and checks GitHub release metadata using loopback HTTP"]
fn native_windows_release_checks_only_when_requested_and_can_retry() {
    let state = tempfile::tempdir().unwrap();
    let root = state.path();
    fs::write(root.join("profiles.json"), br#"{"profiles":{}}"#).unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    fs::write(root.join("release-port"), port.to_string()).unwrap();
    let sample = "09ad0eb3a2c55c52e84667fdf5add693b8d19fab";
    let first = build_commit().unwrap_or(sample);
    let other = if first == sample {
        "91d8aab6712f19b6e21e06f2a860d1f3e82a9cf0"
    } else {
        sample
    };
    let (running, expected) = match build_commit() {
        Some(revision) => (
            format!("Running w11term {VERSION} · build-{}", &revision[..12]),
            "This is the latest published Windows build".into(),
        ),
        None => (
            format!("Running w11term {VERSION} · local build"),
            format!("Latest Windows build · build-{}", &first[..12]),
        ),
    };
    fs::write(
        root.join("release-expected.json"),
        serde_json::to_vec(&json!({
            "running": running,
            "first": expected,
            "retry": format!("Latest Windows build · build-{}", &other[..12])
        }))
        .unwrap(),
    )
    .unwrap();
    let checks = root.to_owned();
    let server = std::thread::spawn(move || {
        for (index, response) in [Some(first), None, Some(other)].into_iter().enumerate() {
            let stream = accept(&listener);
            assert!(
                checks.join(format!("release-check-{index}")).exists(),
                "unexpected automatic release request"
            );
            respond(stream, response);
        }
    });
    drive_gui("windows-release-ui.ps1", root);
    server.join().unwrap();
}

fn accept(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match listener.accept() {
            Ok((stream, _)) => return stream,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("waiting for the Windows release check: {error}"),
        }
    }
}

fn respond(mut stream: TcpStream, commit: Option<&str>) {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(&mut stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(
        line.trim_end(),
        "GET /repos/conclavio/w11term/releases/latest HTTP/1.1"
    );
    let mut git_api = false;
    loop {
        line.clear();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if line.trim_end().is_empty() {
            break;
        }
        let header = line.to_ascii_lowercase();
        assert!(!header.starts_with("authorization:") && !header.starts_with("cookie:"));
        git_api |= header.starts_with("accept: application/vnd.github+json");
    }
    assert!(git_api);
    let (status, body) = match commit {
        Some(commit) => (
            "200 OK",
            serde_json::to_string(&json!({
                "tag_name": format!("build-{}", &commit[..12]),
                "target_commitish": commit,
                "published_at": "2026-11-27T22:00:00Z",
                "draft": false,
                "prerelease": false,
                "assets": [
                    {"name":"w11term-windows-x64.zip","state":"uploaded","size":45038313},
                    {"name":"w11term-windows-x64.zip.sha256","state":"uploaded","size":90}
                ]
            }))
            .unwrap(),
        ),
        None => ("429 Too Many Requests", String::new()),
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ).unwrap();
}
