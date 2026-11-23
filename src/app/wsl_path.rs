use anyhow::{Context, Result, ensure};
use std::{
    io::{Read, Seek, SeekFrom},
    os::windows::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use windows::Win32::System::Threading::CREATE_NO_WINDOW;

#[derive(Clone, Copy)]
pub(super) enum Direction {
    ToLinux,
    ToWindows,
}

pub(super) fn translate(
    executable: &Path,
    distribution: &str,
    directory: Option<&str>,
    direction: Direction,
    path: &str,
    deadline: Instant,
    cancelled: impl Fn() -> bool,
) -> Result<String> {
    ensure!(
        !cancelled() && Instant::now() < deadline,
        "WSL path translation expired"
    );
    let mut output = tempfile::tempfile()?;
    let mut command = Command::new(executable);
    command.args(["--distribution", distribution]);
    if let Some(directory) = directory {
        command.args(["--cd", directory]);
    }
    command.args(["--exec", "/usr/bin/wslpath"]);
    match direction {
        Direction::ToLinux => command.arg("-u"),
        Direction::ToWindows => command.args(["-a", "-w"]),
    };
    // A file lets the child finish writing even while we wait for it to exit.
    let mut child = command
        .arg(path)
        .creation_flags(CREATE_NO_WINDOW.0)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::from(output.try_clone()?))
        .spawn()
        .context("Starting wslpath")?;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if !cancelled() && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => break Err(anyhow::anyhow!("WSL path translation expired")),
            Err(error) => break Err(error.into()),
        }
    };
    if status.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    ensure!(status?.success(), "wslpath couldn’t translate the path");
    output.seek(SeekFrom::Start(0))?;
    let mut text = String::new();
    output.take(MAX_OUTPUT + 1).read_to_string(&mut text)?;
    parse(&text, direction)
}

const MAX_OUTPUT: u64 = 128 * 1024;

pub(super) fn safe_text(text: &str) -> bool {
    !text
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
}

fn parse(text: &str, direction: Direction) -> Result<String> {
    ensure!(
        text.len() as u64 <= MAX_OUTPUT,
        "wslpath returned too much data"
    );
    let path = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text);
    ensure!(
        !path.is_empty()
            && safe_text(path)
            && (!matches!(direction, Direction::ToLinux) || path.starts_with('/')),
        "wslpath returned an unusable path"
    );
    Ok(path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translation_output_preserves_spaces_and_rejects_terminal_controls() {
        assert_eq!(
            parse("/mount/with trailing space \r\n", Direction::ToLinux).unwrap(),
            "/mount/with trailing space "
        );
        assert_eq!(
            parse("C:\\with trailing space \n", Direction::ToWindows).unwrap(),
            r"C:\with trailing space "
        );
        for value in [
            "relative\n",
            "/line\nline\n",
            "/x\u{1b}[200~\n",
            "/x\u{2028}\n",
            "\n",
        ] {
            assert!(parse(value, Direction::ToLinux).is_err());
        }
        assert!(
            parse(
                &format!("/{}", "a".repeat(MAX_OUTPUT as usize)),
                Direction::ToLinux
            )
            .is_err()
        );
        assert!(parse("C:\\a\r\nC:\\b\n", Direction::ToWindows).is_err());
    }
}
