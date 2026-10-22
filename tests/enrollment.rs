use tokio::sync::{broadcast, mpsc};
use w11term::{enroll, keys, session::Command};
#[tokio::test]
async fn enrollment_and_revocation_verify_server_listing_and_never_send_private_material() {
    let dir = tempfile::tempdir().unwrap();
    let key = keys::generate_in(dir.path(), "test-device").unwrap();
    for remove in [false, true] {
        let (input, mut commands) = mpsc::unbounded_channel();
        let (output, _) = broadcast::channel(32);
        let device = key.clone();
        let peer = tokio::spawn(async move {
            let mut operations = 0;
            while let Some(command) = commands.recv().await {
                match command {
                    Command::Observe(reply) => {
                        reply.send(output.subscribe()).unwrap();
                    }
                    Command::Raw(bytes, reply) => {
                        let text = String::from_utf8(bytes).unwrap();
                        assert!(!text.contains("PRIVATE"));
                        operations += 1;
                        reply.send(Ok(())).unwrap();
                        let response = if operations == 1 {
                            if remove {
                                assert_eq!(text, format!("@sshkey/del {}\r", device.fingerprint));
                                format!("@sshkey: removed key {}.\r\n", device.fingerprint)
                            } else {
                                assert_eq!(text, format!("@sshkey/add {}\r", device.public_line));
                                format!(
                                    "@sshkey: key {} added. You can now log in.\r\n",
                                    device.fingerprint
                                )
                            }
                        } else {
                            assert_eq!(text, "@sshkey/list\r@sshkey/list\r");
                            if remove {
                                "@sshkey: you have no registered SSH keys.\r\n@sshkey: you have no registered SSH keys.\r\n".into()
                            } else {
                                format!(
                                    "SSH keys on account pilot:\r\n  1) {}  desktop  (added today)\r\nSSH keys on account pilot:\r\n",
                                    device.fingerprint
                                )
                            }
                        };
                        let cut = response.len() / 3;
                        for chunk in response.as_bytes().chunks(cut) {
                            let _ = output.send(chunk.to_vec());
                            tokio::task::yield_now().await;
                        }
                    }
                    _ => panic!("unexpected enrollment command"),
                }
            }
            assert_eq!(operations, 2);
        });
        enroll::run(input, key.clone(), "pilot+character".into(), remove)
            .await
            .unwrap();
        peer.await.unwrap();
    }
}
