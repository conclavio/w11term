use super::drive_gui;
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    time::Duration,
};

#[test]
#[ignore = "reviews, sends and cancels requests against a local provider in the native UI"]
fn native_openrouter_requires_an_explicit_review_and_never_submits_the_reply() {
    let state = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    fs::write(state.path().join("openrouter-port"), port.to_string()).unwrap();
    let directory = state.path().to_owned();
    let provider = std::thread::spawn(move || serve(listener, &directory));
    drive_gui("openrouter-ui.ps1", state.path());
    provider.join().unwrap();
}

fn serve(listener: TcpListener, directory: &Path) {
    listener.set_nonblocking(true).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    for index in 0..4 {
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
            assert_eq!(
                request["messages"][1]["content"],
                [
                    "approved edit\nwith a second line",
                    "try provider rejection",
                    "cancel after receipt"
                ][index - 1]
            );
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
            _ => (
                "200 OK",
                json!({"choices": [{"message": {"content": "late fixture reply"}}]}),
            ),
        };
        let body = serde_json::to_string(&reply).unwrap();
        let response = write!(
            client,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        if index != 3 {
            response.unwrap();
        }
    }
    fs::write(directory.join("openrouter-fixture-done"), []).unwrap();
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
