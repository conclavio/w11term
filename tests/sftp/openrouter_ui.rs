use super::{Source, drive_mixed_gui, fixture_connections};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    time::Duration,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "reviews drafts and a scrolled SSH pane against a local provider in the native UI"]
async fn native_openrouter_requires_an_explicit_review_and_never_submits_the_reply() {
    let (Source::Ssh(ssh), remote, _, ssh_server) = fixture_connections(false, 1).await else {
        unreachable!()
    };
    let mut history = String::from("ssh-offscreen-archive\r\n");
    for i in 0..180 {
        if i == 90 {
            history.push_str("ssh-visible-issue redact-this-fixture\r\n");
        } else {
            history.push_str(&format!("ssh-output-{i:03}\r\n"));
        }
    }
    history.push_str("ssh-live-tail\r\n");
    fs::write(remote.path().join("copy-history.vt"), history).unwrap();
    let wire = remote.path().join("keyboard-input.bin");
    fs::write(&wire, []).unwrap();
    let state = tempfile::tempdir().unwrap();
    write_packet_capture(&state.path().join("private before.pcap"), &[10]);
    write_packet_capture(&state.path().join("private after.pcap"), &[900, 902]);
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    fs::write(state.path().join("openrouter-port"), port.to_string()).unwrap();
    let directory = state.path().to_owned();
    let provider = std::thread::spawn(move || serve(listener, &directory));
    drive_mixed_gui("openrouter-ui.ps1", &ssh, state.path());
    provider.join().unwrap();
    assert!(fs::read(wire).unwrap().is_empty());
    ssh_server.abort();
}

fn write_packet_capture(path: &Path, timestamps: &[u32]) {
    let payload = b"private fixture packet payload";
    let mut frame = vec![
        0x45,
        0,
        0,
        (28 + payload.len()) as u8,
        0,
        0,
        0,
        0,
        64,
        17,
        0,
        0,
        192,
        0,
        2,
        41,
        198,
        51,
        100,
        72,
        0xc0,
        0,
        0x10,
        0xe1,
        0,
        (8 + payload.len()) as u8,
        0,
        0,
    ];
    frame.extend(payload);
    let mut capture = vec![
        0xd4, 0xc3, 0xb2, 0xa1, 2, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 128, 0, 0, 0, 101, 0, 0, 0,
    ];
    let length = frame.len() as u32;
    for &timestamp in timestamps {
        for field in [timestamp, 0, length, length] {
            capture.extend(field.to_le_bytes());
        }
        capture.extend(&frame);
    }
    fs::write(path, capture).unwrap();
}

fn serve(listener: TcpListener, directory: &Path) {
    listener.set_nonblocking(true).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    for index in 0..8 {
        let mut client = loop {
            match listener.accept() {
                Ok((client, _)) => break client,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "missing request {index}"
                    );
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("{error}"),
            }
        };
        // Winsock can inherit the listener's nonblocking mode on accept.
        client.set_nonblocking(false).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let (method, headers, body) = receive(&mut client);
        if index == 0 {
            assert_eq!(method, "GET /api/v1/models HTTP/1.1");
            assert!(!headers.contains("authorization:"));
            assert!(body.is_empty());
        } else {
            assert_eq!(method, "POST /api/v1/chat/completions HTTP/1.1");
            assert!(headers.contains("authorization: bearer w11term-fixture-key\r\n"));
            let request: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(request["model"], "fixture/model");
            assert_eq!(
                request["provider"],
                json!({"data_collection": "deny", "zdr": true})
            );
            assert_eq!(request["messages"][1]["role"], "user");
            let text = request["messages"][1]["content"].as_str().unwrap();
            if index == 4 {
                let expected = fs::read_to_string(directory.join("approved-viewport")).unwrap();
                assert_eq!(text, expected);
                assert!(text.contains("ssh-visible-issue [redacted]"));
                for excluded in [
                    "redact-this-fixture",
                    "ssh-offscreen-archive",
                    "ssh-live-tail",
                    "neighbor-private-42",
                ] {
                    assert!(!text.contains(excluded));
                }
                assert!(
                    request["messages"][0]["content"]
                        .as_str()
                        .unwrap()
                        .contains("quoted terminal evidence")
                );
            } else if index == 5 {
                let expected = fs::read_to_string(directory.join("approved-syscall")).unwrap();
                assert_eq!(text, expected);
                assert!(text.contains("Resource [resource hidden]"));
                assert!(!text.contains("/tmp/review-data"));
                let prompt = request["messages"][0]["content"].as_str().unwrap();
                assert!(prompt.contains("quoted diagnostic evidence"));
                assert!(prompt.contains("run side, source number, and line number"));
            } else if index == 6 {
                let expected = fs::read_to_string(directory.join("approved-cpu")).unwrap();
                assert_eq!(text, expected);
                assert!(text.contains("scan → [redacted leaf]"));
                assert!(!text.contains("scan → hot"));
                let prompt = request["messages"][0]["content"].as_str().unwrap();
                assert!(prompt.contains("quoted diagnostic evidence"));
                assert!(prompt.contains("percentage-point deltas"));
                assert!(prompt.contains("latency or absolute CPU time"));
            } else if index == 7 {
                let expected = fs::read_to_string(directory.join("approved-packets")).unwrap();
                assert_eq!(text, expected);
                assert!(text.contains("Scope · Whole captures"));
                assert!(text.contains("Selected UDP pair · Endpoint A port [redacted]"));
                assert!(text.contains("Before · A to B 1 packets"));
                assert!(text.contains("After · A to B 2 packets"));
                for excluded in [
                    "192.0.2.41",
                    "198.51.100.72",
                    "49152",
                    "private before.pcap",
                    "private after.pcap",
                    "private fixture packet payload",
                ] {
                    assert!(!text.contains(excluded), "{excluded}");
                }
                let prompt = request["messages"][0]["content"].as_str().unwrap();
                assert!(prompt.contains("quoted diagnostic evidence"));
                assert!(prompt.contains("unequal durations, truncation, and incomplete coverage"));
            } else {
                assert_eq!(
                    text,
                    [
                        "approved edit\nwith a second line",
                        "try provider rejection",
                        "cancel after receipt"
                    ][index - 1]
                );
            }
            assert!(request.get("tools").is_none());
        }
        fs::write(directory.join(format!("openrouter-request-{index}")), &body).unwrap();
        if index == 3 {
            let cancel = directory.join("openrouter-cancel-seen");
            while !cancel.is_file() {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let (status, reply) = match index {
            0 => (
                "200 OK",
                json!({"data": [{"id": "fixture/model", "name": "Fixture"}]}),
            ),
            1 => {
                let marker = directory.join("executed-provider-reply");
                let command = format!("Set-Content -LiteralPath '{}' -Value ran", marker.display());
                (
                    "200 OK",
                    json!({"model": "fixture/model", "choices": [{"message": {"content": command}}],
                    "usage": {"prompt_tokens": 17, "completion_tokens": 7, "total_tokens": 24}}),
                )
            }
            2 => (
                "404 Not Found",
                json!({"error": {"message": "private provider diagnostics"}}),
            ),
            3 => (
                "200 OK",
                json!({"choices": [{"message": {"content": "late fixture reply"}}]}),
            ),
            4 => (
                "200 OK",
                json!({"choices": [{"message": {"content": "explained the reviewed viewport"}}]}),
            ),
            5 => (
                "200 OK",
                json!({"choices": [{"message": {"content": "reviewed the syscall evidence"}}]}),
            ),
            6 => (
                "200 OK",
                json!({"choices": [{"message": {"content": "reviewed the CPU profile changes"}}]}),
            ),
            _ => (
                "200 OK",
                json!({"choices": [{"message": {"content": "reviewed the packet capture changes"}}]}),
            ),
        };
        let body = serde_json::to_string(&reply).unwrap();
        let response = write!(
            client,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        if index == 3 {
            fs::write(directory.join("openrouter-late-sent"), []).unwrap();
        } else {
            response.unwrap();
        }
    }
}

fn receive(client: &mut TcpStream) -> (String, String, Vec<u8>) {
    let mut request = BufReader::new(client);
    let mut method = String::new();
    request.read_line(&mut method).unwrap();
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        assert!(request.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        headers.push_str(&line.to_ascii_lowercase());
        assert!(headers.len() <= 8192);
    }
    let len = headers
        .lines()
        .find_map(|header| header.strip_prefix("content-length: "))
        .map_or(0, |len| len.parse().unwrap());
    assert!(len <= 32 * 1024);
    let mut body = vec![0; len];
    request.read_exact(&mut body).unwrap();
    (method.trim_end().into(), headers, body)
}
