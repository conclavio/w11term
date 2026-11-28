use super::drive_gui;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufRead, BufReader, Cursor, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    time::{Duration, Instant},
};
use w11term::windows_release::{VERSION, build_commit};

#[test]
#[ignore = "checks release metadata and saves a verified Windows ZIP through WinUI and loopback HTTP"]
fn native_windows_release_checks_and_saves_verified_zip() {
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
    let archive = fixture_zip();
    let expected_archive = archive.clone();
    let digest: String = Sha256::digest(&archive)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let checksum = format!("{digest}  w11term-windows-x64.zip\n");
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
            "retry": format!("Latest Windows build · build-{}", &other[..12]),
            "published": format!("Published 2026-11-27 · {:.1} MiB ZIP", archive.len() as f64 / 1048576.0)
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
            respond_metadata(stream, response, archive.len(), &digest, checksum.len());
        }
        let prefix = format!(
            "/conclavio/w11term/releases/download/build-{}/w11term-windows-x64.zip",
            &other[..12]
        );
        for stage in ["bad", "good", "cancel"] {
            let stream = accept(&listener);
            assert!(
                checks.join(format!("release-download-{stage}")).exists(),
                "unexpected release download"
            );
            let (mut stream, path, _) = request(stream);
            assert_eq!(path, format!("{prefix}.sha256"));
            response_head(&mut stream, "200 OK", checksum.len());
            stream.write_all(checksum.as_bytes()).unwrap();
            drop(stream);
            let (mut stream, path, _) = request(accept(&listener));
            assert_eq!(path, prefix);
            response_head(&mut stream, "200 OK", archive.len());
            match stage {
                "bad" => {
                    let mut corrupted = archive.clone();
                    corrupted[0] ^= 1;
                    stream.write_all(&corrupted).unwrap();
                }
                "good" => stream.write_all(&archive).unwrap(),
                "cancel" => {
                    let split = archive.len() / 2;
                    stream.write_all(&archive[..split]).unwrap();
                    fs::write(checks.join("release-download-cancel-ready"), []).unwrap();
                    wait_for_file(&checks.join("release-download-cancelled"));
                    let _ = stream.write_all(&archive[split..]);
                }
                _ => unreachable!(),
            }
        }
    });
    drive_gui("windows-release-ui.ps1", root);
    server.join().unwrap();
    let saved = root.join("verified releases").join("release 雪.zip");
    assert_eq!(fs::read(&saved).unwrap(), expected_archive);
    let mut zone = saved.into_os_string();
    zone.push(":Zone.Identifier");
    assert_eq!(
        fs::read_to_string(&zone).unwrap(),
        "[ZoneTransfer]\r\nZoneId=3\r\n"
    );
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

fn fixture_zip() -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file(
            "build.txt",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
    writer.write_all(&vec![b'x'; 1100 * 1024]).unwrap();
    writer.finish().unwrap().into_inner()
}

fn wait_for_file(file: &Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !file.exists() {
        assert!(
            Instant::now() < deadline,
            "missing fixture signal {}",
            file.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn request(mut stream: TcpStream) -> (TcpStream, String, bool) {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(&mut stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let path = line
        .trim_end()
        .strip_prefix("GET ")
        .unwrap()
        .strip_suffix(" HTTP/1.1")
        .unwrap()
        .to_owned();
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
    (stream, path, git_api)
}

fn response_head(stream: &mut TcpStream, status: &str, size: usize) {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
}

fn respond_metadata(
    stream: TcpStream,
    commit: Option<&str>,
    bytes: usize,
    digest: &str,
    checksum_bytes: usize,
) {
    let (mut stream, path, git_api) = request(stream);
    assert_eq!(path, "/repos/conclavio/w11term/releases/latest");
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
                    {"name":"w11term-windows-x64.zip","state":"uploaded","size":bytes,"digest":format!("sha256:{digest}")},
                    {"name":"w11term-windows-x64.zip.sha256","state":"uploaded","size":checksum_bytes}
                ]
            }))
            .unwrap(),
        ),
        None => ("429 Too Many Requests", String::new()),
    };
    response_head(&mut stream, status, body.len());
    stream.write_all(body.as_bytes()).unwrap();
}
