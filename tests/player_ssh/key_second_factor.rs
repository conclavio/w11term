use super::{Client, connected, key, options, prompt, start};
use anyhow::{Context, Result, ensure};
use russh::keys::ssh_key::{LineEnding, certificate};
use std::{
    fs,
    io::ErrorKind,
    path::Path,
    process::{Child, Command},
    time::Duration,
};
use w11term::{
    options::{Authentication, SshOptions},
    prompts::{Answer, Prompt, Prompts, Question},
    terminal::TransportInput,
};
use zeroize::Zeroizing;

#[path = "../support/drive_gui.rs"]
mod drive_gui;

struct FixtureProcess(Child);

impl Drop for FixtureProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn fixture(
    directory: &Path,
    mode: &str,
    use_certificate: bool,
) -> Result<(FixtureProcess, SshOptions)> {
    let identity = key();
    let identity_path = directory.join("fixture-key");
    fs::write(
        &identity_path,
        identity.to_openssh(LineEnding::LF)?.as_bytes(),
    )?;
    let public = directory.join("fixture-key.pub");
    let authentication = if use_certificate {
        let mut cert = certificate::Builder::new_with_random_nonce(
            &mut rand::rng(),
            identity.public_key().key_data().clone(),
            0,
            u64::MAX,
        )?;
        cert.cert_type(certificate::CertType::User)?;
        cert.valid_principal("pilot+character")?;
        fs::write(&public, cert.sign(&key())?.to_openssh()?)?;
        Authentication::identity(identity_path, Some(public.clone()))
    } else {
        fs::write(&public, identity.public_key().to_openssh()?)?;
        Authentication::identity(identity_path, None)
    };
    let ready = directory.join("peer.ready");
    let mut peer = FixtureProcess(
        Command::new("python")
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/player_ssh/key_second_factor.py"),
            )
            .args([mode.as_ref(), public.as_os_str(), ready.as_os_str()])
            .spawn()
            .context("start the Python Paramiko SSH fixture")?,
    );
    let (port, pin) = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match fs::read_to_string(&ready) {
                Ok(line) => {
                    let (port, pin) = line.split_once(' ').context("missing fixture host pin")?;
                    return Ok::<_, anyhow::Error>((port.parse::<u16>()?, pin.to_owned()));
                }
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    ensure!(
                        peer.0.try_wait()?.is_none(),
                        "SSH fixture exited before listening"
                    );
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
    })
    .await
    .context("SSH fixture did not start")??;
    Ok((peer, options(port, authentication, Some(pin))))
}

async fn challenge(prompts: &Prompts, label: &str, echo: bool) -> Prompt {
    let question = prompt(prompts).await;
    let Question::Authentication {
        name, challenges, ..
    } = &question.question
    else {
        panic!("expected an additional SSH authentication prompt");
    };
    assert!(name.starts_with("Verify sign-in as pilot+character@127.0.0.1:"));
    assert_eq!(challenges.len(), 1);
    assert_eq!(challenges[0].label, label);
    assert_eq!(challenges[0].echo, echo);
    question
}

fn answer(prompts: &Prompts, question: Prompt, value: &str) {
    prompts
        .answer(
            question.id,
            Answer::Authentication(vec![Zeroizing::new(value.into())]),
        )
        .unwrap();
}

async fn stop_client(client: Client) -> String {
    let error = tokio::time::timeout(Duration::from_secs(8), client.task)
        .await
        .expect("SSH session did not stop")
        .unwrap()
        .unwrap_err();
    format!("{error:#}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Python and Paramiko 4.0.0 for the loopback SSH peer"]
async fn key_then_password_retries_and_prompts_again_after_a_dropped_link() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (_peer, options) = fixture(directory.path(), "password", false).await?;
    let prompts = Prompts::with_known_hosts(directory.path().join("known_hosts"));
    let mut client = start(options, prompts.clone());
    let first = challenge(&prompts, "Password", false).await;
    let first_id = first.id;
    answer(&prompts, first, "incorrect");
    let retry = challenge(&prompts, "Password", false).await;
    assert_ne!(first_id, retry.id);
    answer(&prompts, retry, "fixture-password");
    connected(&mut client, 1).await;
    client.input.send(TransportInput::Data(b"drop".to_vec()))?;
    let again = challenge(&prompts, "Password", false).await;
    assert_ne!(first_id, again.id);
    answer(&prompts, again, "fixture-password");
    connected(&mut client, 2).await;
    assert!(prompts.current().is_none());
    client.task.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Python and Paramiko 4.0.0 for the loopback SSH peer"]
async fn certificate_then_keyboard_interactive_supports_multiple_rounds() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (_peer, options) = fixture(directory.path(), "interactive", true).await?;
    let prompts = Prompts::with_known_hosts(directory.path().join("known_hosts"));
    let mut client = start(options, prompts.clone());
    let code = challenge(&prompts, "One-time code", false).await;
    answer(&prompts, code, "123456");
    let device = challenge(&prompts, "Device name", true).await;
    answer(&prompts, device, "test-desktop");
    connected(&mut client, 1).await;
    client.task.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Python and Paramiko 4.0.0 for the loopback SSH peer"]
async fn server_must_accept_the_key_and_offer_a_supported_factor() -> Result<()> {
    for (mode, reason) in [
        ("rejected", "public-key authentication rejected"),
        ("two-keys", "unsupported additional authentication method"),
    ] {
        let directory = tempfile::tempdir()?;
        let (_peer, options) = fixture(directory.path(), mode, false).await?;
        let prompts = Prompts::with_known_hosts(directory.path().join("known_hosts"));
        let client = start(options, prompts.clone());
        let message = stop_client(client).await;
        assert!(message.contains(reason), "{message}");
        assert!(prompts.current().is_none());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Python and Paramiko 4.0.0 for the loopback SSH peer"]
async fn falls_back_to_a_server_offered_password_and_honors_cancel() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (_peer, options) = fixture(directory.path(), "alternatives", false).await?;
    let prompts = Prompts::with_known_hosts(directory.path().join("known_hosts"));
    let client = start(options, prompts.clone());
    let code = challenge(&prompts, "One-time code", false).await;
    answer(&prompts, code, "unavailable");
    let password = challenge(&prompts, "Password", false).await;
    prompts.answer(password.id, Answer::Cancel)?;
    assert!(
        stop_client(client)
            .await
            .contains("authentication cancelled")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a Windows desktop and Python with Paramiko 4.0.0"]
async fn native_key_second_factor_uses_a_masked_dialog() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (_peer, options) = fixture(directory.path(), "password", false).await?;
    drive_gui::drive_mixed_gui("ssh-key-second-factor-ui.ps1", &options, directory.path());
    Ok(())
}
