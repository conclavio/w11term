//! Opens a real PTY through the production terminal session, reports receipt of output, then closes.
//! No commands are sent to the remote shell. Uses the same profiles and host verification as the UI.
use anyhow::{Result, bail, ensure};
use std::time::{Duration, Instant};
use w11term::{
    options::{Options, Source},
    session::Session,
};

fn main() -> Result<()> {
    let Some(options) = Options::parse()? else {
        return Ok(());
    };
    let Source::Ssh(ref ssh) = options.source else {
        bail!("supply --profile NAME or ssh user@host")
    };
    let target = format!("{}@{}:{}", ssh.user, ssh.host, ssh.port);
    let session = Session::start(options.source);
    let started = Instant::now();
    loop {
        let frame = session.frames.load();
        ensure!(
            !frame.status.starts_with("SSH:") && frame.status != "Disconnected",
            "{target}: {}",
            frame.status
        );
        if frame.bytes_received > 0 {
            println!(
                "{target}: authenticated, PTY output processed ({} bytes, {}x{})",
                frame.bytes_received,
                frame.cols,
                frame.rows.len()
            );
            break;
        }
        ensure!(
            started.elapsed() < Duration::from_secs(25),
            "{target}: timed out waiting for PTY output: {}",
            frame.status
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(session);
    Ok(())
}
