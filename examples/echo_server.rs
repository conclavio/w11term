//! A loopback-only SSH typing benchmark. Never launches a process or exposes a filesystem.
use anyhow::{Context, Result, bail, ensure};
use russh::{
    Channel, ChannelId,
    keys::{Algorithm, HashAlg, PrivateKey},
    server,
};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::net::TcpListener;

const SCREEN: &[u8] = b"\x1b[0m\x1b[2J\x1b[H\x1b[6 q\x1b[?25h\x1b]0;Loopback SSH echo benchmark\x07Loopback SSH echo | type here | Ctrl+L reset | Ctrl+C disconnect\r\n\r\n";

#[derive(Default)]
struct Echo {
    channel: Option<ChannelId>,
    column: usize,
}

impl Echo {
    fn echo(&mut self, data: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(data.len());
        for &byte in data {
            match byte {
                8 | 127 if self.column > 0 => {
                    output.extend_from_slice(b"\x08 \x08");
                    self.column -= 1;
                }
                8 | 127 => {}
                12 => {
                    output.extend_from_slice(SCREEN);
                    self.column = 0;
                }
                b'\r' | b'\n' => {
                    output.extend_from_slice(b"\r\n");
                    self.column = 0;
                }
                32..=126 => {
                    output.push(byte);
                    self.column += 1;
                }
                _ => {}
            }
        }
        output
    }
}

impl server::Handler for Echo {
    type Error = anyhow::Error;

    async fn auth_none(&mut self, user: &str) -> Result<server::Auth> {
        Ok(if user == "bench" {
            server::Auth::Accept
        } else {
            server::Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            }
        })
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut server::Session,
    ) -> Result<()> {
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _cols: u32,
        _rows: u32,
        _width: u32,
        _height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut server::Session,
    ) -> Result<()> {
        println!("PTY {_term}: {_cols}x{_rows}");
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut server::Session,
    ) -> Result<()> {
        ensure!(
            self.channel.is_none(),
            "only one echo surface per SSH connection"
        );
        self.channel = Some(channel);
        println!("Echo surface ready");
        session.channel_success(channel)?;
        session.data(channel, SCREEN)?;
        Ok(())
    }

    async fn env_request(
        &mut self,
        channel: ChannelId,
        _name: &str,
        _value: &str,
        session: &mut server::Session,
    ) -> Result<()> {
        // Nothing is executed, so environment values have no effect. Acknowledge the request:
        // some SSH clients wait for this reply before requesting a shell.
        session.channel_success(channel)?;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        _data: &[u8],
        session: &mut server::Session,
    ) -> Result<()> {
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        _name: &str,
        session: &mut server::Session,
    ) -> Result<()> {
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut server::Session,
    ) -> Result<()> {
        ensure!(
            self.channel == Some(channel),
            "echo requires a shell request"
        );
        if data.contains(&3) || data.contains(&4) {
            session.exit_status_request(channel, 0)?;
            session.eof(channel)?;
            session.close(channel)?;
        } else {
            // No timer, line buffering, logging, or artificial delay on the measured path.
            session.data(channel, self.echo(data))?;
        }
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        session: &mut server::Session,
    ) -> Result<()> {
        session.close(channel)?;
        Ok(())
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut port = 22222u16;
    let mut state = PathBuf::from(".local/benchmark");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => port = args.next().context("--port requires a port")?.parse()?,
            "--state-dir" => state = args.next().context("--state-dir requires a path")?.into(),
            _ => bail!("usage: echo_server [--port 22222] [--state-dir .local/benchmark]"),
        }
    }
    ensure!(port != 0, "specify a nonzero port");
    std::fs::create_dir_all(&state)?;
    let key_path = state.join("ssh_host_ed25519");
    let key = if key_path.exists() {
        russh::keys::load_secret_key(&key_path, None)?
    } else {
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
        std::fs::write(
            &key_path,
            key.to_openssh(russh::keys::ssh_key::LineEnding::LF)?
                .as_bytes(),
        )?;
        key
    };
    let fingerprint = key.public_key().fingerprint(HashAlg::Sha256).to_string();
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .context("binding loopback SSH benchmark")?;
    std::fs::write(
        state.join("known_hosts"),
        format!("[127.0.0.1]:{port} {}\n", key.public_key().to_openssh()?),
    )?;
    std::fs::write(
        state.join("profiles.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "profiles": { "loopback": { "host": "127.0.0.1", "user": "bench", "port": port,
                "auth": "none", "host_key": fingerprint } }
        }))?,
    )?;
    std::fs::write(
        state.join("server.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "pid": std::process::id(), "port": port, "host_key": fingerprint,
            "address": "127.0.0.1", "executable": std::env::current_exe()?
        }))?,
    )?;
    println!("Echo ready: bench@127.0.0.1:{port} | {fingerprint}");
    let config = Arc::new(server::Config {
        keys: vec![key],
        auth_rejection_time: Duration::ZERO,
        nodelay: true,
        ..Default::default()
    });
    loop {
        let (stream, address) = listener.accept().await?;
        ensure!(address.ip().is_loopback(), "non-loopback peer rejected");
        stream.set_nodelay(true)?;
        let config = config.clone();
        tokio::spawn(async move {
            let result = async {
                server::run_stream(config, stream, Echo::default())
                    .await?
                    .await
            }
            .await;
            if let Err(error) = result {
                eprintln!("Echo client ended: {error}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supports_typometer_pattern_deletion_and_reset_across_packets() {
        let mut echo = Echo::default();
        assert_eq!(echo.echo(b"....."), b".....");
        assert_eq!(echo.echo(&[127, 127]), b"\x08 \x08\x08 \x08");
        assert_eq!(echo.echo(&[8, 8, 8, 8]), b"\x08 \x08\x08 \x08\x08 \x08");
        assert_eq!(echo.column, 0);
        assert_eq!(echo.echo(b"x\x0c"), [b"x".as_slice(), SCREEN].concat());
        assert_eq!(echo.column, 0);
    }

    #[tokio::test]
    async fn loopback_authentication_pty_echo_erase_and_exec_rejection() {
        tokio::time::timeout(Duration::from_secs(5), async {
            struct Verify(String);
            impl russh::client::Handler for Verify {
                type Error = anyhow::Error;
                async fn check_server_key(
                    &mut self,
                    key: &russh::keys::PublicKeyOrCertificate,
                ) -> Result<bool> {
                    Ok(key.public_key().fingerprint(HashAlg::Sha256).to_string() == self.0)
                }
            }
            let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
            let fingerprint = key.public_key().fingerprint(HashAlg::Sha256).to_string();
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                stream.set_nodelay(true).unwrap();
                let config = Arc::new(server::Config {
                    keys: vec![key],
                    auth_rejection_time: Duration::ZERO,
                    ..Default::default()
                });
                let _ = server::run_stream(config, stream, Echo::default())
                    .await
                    .unwrap()
                    .await;
            });
            let mut client = russh::client::connect(
                Arc::new(russh::client::Config::default()),
                address,
                Verify(fingerprint),
            )
            .await
            .unwrap();
            assert!(!client.authenticate_none("root").await.unwrap().success());
            assert!(client.authenticate_none("bench").await.unwrap().success());
            let mut channel = client.channel_open_session().await.unwrap();
            channel
                .set_env(true, "TERM_PROGRAM", "benchmark-test")
                .await
                .unwrap();
            assert!(matches!(
                channel.wait().await,
                Some(russh::ChannelMsg::Success)
            ));
            channel
                .request_pty(true, "xterm-256color", 110, 36, 0, 0, &[])
                .await
                .unwrap();
            channel.request_shell(true).await.unwrap();
            assert_eq!(read_data(&mut channel).await, SCREEN);
            for (input, expected) in [
                (b".....".as_slice(), b".....".as_slice()),
                (
                    b"\x7f\x7f\x7f\x7f\x7f".as_slice(),
                    b"\x08 \x08\x08 \x08\x08 \x08\x08 \x08\x08 \x08".as_slice(),
                ),
                (b"\x0c".as_slice(), SCREEN),
            ] {
                channel.data(input).await.unwrap();
                assert_eq!(read_data(&mut channel).await, expected);
            }
            let mut exec = client.channel_open_session().await.unwrap();
            exec.exec(true, b"whoami".as_slice()).await.unwrap();
            assert!(matches!(
                exec.wait().await,
                Some(russh::ChannelMsg::Failure)
            ));
            client
                .disconnect(russh::Disconnect::ByApplication, "done", "en")
                .await
                .unwrap();
            drop(channel);
            drop(exec);
            drop(client);
            server.await.unwrap();
        })
        .await
        .expect("loopback benchmark server timed out");
    }

    async fn read_data(channel: &mut russh::Channel<russh::client::Msg>) -> Vec<u8> {
        loop {
            match channel.wait().await {
                Some(russh::ChannelMsg::Data { data }) => return data.to_vec(),
                Some(russh::ChannelMsg::Success) => {}
                message => panic!("unexpected channel message: {message:?}"),
            }
        }
    }
}
