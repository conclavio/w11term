//! Exercise the shell script inside a real, headless ConsoleHost and ConPTY.
use anyhow::{Context, Result, ensure};
use std::{path::Path, time::Duration};
use tokio::sync::mpsc;
use w11term::{conpty, options::LocalOptions, ssh::Event, terminal::TransportInput};
use wezterm_term::TerminalSize;

async fn through(output: &mut mpsc::Receiver<Event>, marker: &str) -> Result<String> {
    let mut text = String::new();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !text.contains(marker) {
            match output.recv().await.context("PowerShell closed")? {
                Event::Data(bytes) => text.push_str(&String::from_utf8_lossy(&bytes)),
                Event::Status(status) => text.push_str(&status),
                _ => {}
            }
        }
        anyhow::Ok(())
    })
    .await
    .with_context(|| format!("waiting for {marker:?}; output: {text:?}"))??;
    Ok(text)
}

struct Shell {
    input: mpsc::UnboundedSender<TransportInput>,
    output: mpsc::Receiver<Event>,
    task: tokio::task::JoinHandle<Result<()>>,
}

impl Shell {
    fn start(executable: &Path, startup: &Path) -> Result<Self> {
        let options = serde_json::from_value::<LocalOptions>(serde_json::json!({
            "executable":executable, "args":["-NoLogo","-NoProfile","-NoExit","-File",startup],
            "cwd":startup.parent()
        }))?;
        let (input, rx) = mpsc::unbounded_channel();
        let (tx, output) = mpsc::channel(32);
        let task = tokio::spawn(conpty::run(
            options,
            TerminalSize {
                cols: 120,
                rows: 30,
                ..Default::default()
            },
            rx,
            tx,
        ));
        Ok(Self {
            input,
            output,
            task,
        })
    }

    fn write(&self, line: &str) -> Result<()> {
        self.input
            .send(TransportInput::Data(line.as_bytes().to_vec()))
            .map_err(|_| anyhow::anyhow!("shell closed"))
    }

    async fn close(self) -> Result<()> {
        drop(self.input);
        drop(self.output);
        tokio::time::timeout(Duration::from_secs(8), self.task).await???;
        Ok(())
    }
}

async fn exercise(executable: &Path) -> Result<()> {
    eprintln!(
        "Checking ConsoleHost shell integration in {}",
        executable.display()
    );
    let temp = tempfile::tempdir()?;
    let startup = temp.path().join("startup.ps1");
    let hook = temp.path().join("hook.ps1");
    std::fs::write(&hook, include_bytes!("../src/conpty/shell-integration.ps1"))?;
    let ready = "\x1b]133;B";
    let mut stock = Shell::start(executable, &hook)?;
    through(&mut stock.output, ready)
        .await
        .context("pristine profile did not emit the input marker")?;
    stock.close().await?;
    // Install a representative user customization and never save fixture input
    // to the machine's real PSReadLine history.
    let custom_profile = r#"
Set-PSReadLineOption -HistorySaveStyle SaveNothing
$global:testReader = $function:PSConsoleHostReadLine
function global:PSConsoleHostReadLine { & $global:testReader }
$global:state = 'fixture'
function global:prompt { "$state-prompt-$?> " }
"#;
    std::fs::write(&startup, custom_profile)?;
    let mut baseline = Shell::start(executable, &startup)?;
    through(&mut baseline.output, "fixture-prompt-True>").await?;
    baseline.write("cmd.exe /d /c exit 23\r")?;
    through(&mut baseline.output, "fixture-prompt-False>").await?;
    let inspect = "'next-status-'+$?+'-'+$LASTEXITCODE\r";
    baseline.write(inspect)?;
    let plain = through(&mut baseline.output, "fixture-prompt-True>").await?;
    let observed = ["next-status-False-23", "next-status-True-23"]
        .into_iter()
        .find(|text| plain.contains(text))
        .with_context(|| format!("missing baseline status in {plain:?}"))?;
    baseline.close().await?;

    let load = ". (Join-Path $PSScriptRoot 'hook.ps1')";
    std::fs::write(&startup, format!("{custom_profile}\n{load}\n{load}\n"))?;
    let mut shell = Shell::start(executable, &startup)?;
    let initial = through(&mut shell.output, ready).await?;
    assert!(initial.contains("\x1b]133;A") && initial.contains("fixture-prompt-True>"));
    shell.write("discardme")?;
    through(&mut shell.output, "discardme").await?;
    shell.write("\x03")?;
    let canceled = through(&mut shell.output, ready)
        .await
        .context("Ctrl+C did not return to input")?;
    ensure!(
        !canceled.contains("\x1b]133;C") && !canceled.contains("\x1b]133;D"),
        "{canceled:?}"
    );
    for (command, expected) in [
        (" \r", None),
        ("Write-Output ('sent'+'inel')\r", Some("0")),
        ("prompt\r", Some("0")),
        ("if ($true) {\r'multiline'\r}\r", Some("0")),
        ("cmd.exe /d /c exit 23\r", Some("23")),
        (inspect, Some("0")),
        ("Write-Error fixture-error -ErrorAction Ignore\r", Some("1")),
        ("'saved-native-'+$LASTEXITCODE\r", Some("0")),
    ] {
        shell.write(command)?;
        let bytes = through(&mut shell.output, ready)
            .await
            .with_context(|| format!("{executable:?} after {command:?}"))?;
        ensure!(
            bytes.matches("\x1b]133;C").count() == usize::from(expected.is_some()),
            "{command:?}: {bytes:?}"
        );
        ensure!(
            bytes.matches("\x1b]133;D").count() == usize::from(expected.is_some()),
            "{command:?}: {bytes:?}"
        );
        if let Some(code) = expected {
            ensure!(
                bytes.contains(&format!("\x1b]133;D;{code}")),
                "{command:?}: {bytes:?}"
            );
            let succeeded = if code == "0" { "True" } else { "False" };
            ensure!(
                bytes.contains(&format!("fixture-prompt-{succeeded}>")),
                "{command:?}: {bytes:?}"
            );
        }
        if command.starts_with("'saved-native'") {
            ensure!(bytes.contains("saved-native-23"), "{bytes:?}");
        }
        if command == inspect {
            ensure!(
                bytes.contains(observed),
                "ordinary command observed {observed}, wrapped shell: {bytes:?}"
            );
        }
        if command == "prompt\r" {
            let (before, _) = bytes.split_once("\x1b]133;D").unwrap();
            ensure!(
                before.contains("fixture-prompt-True>"),
                "explicit prompt completed its parent command early: {bytes:?}"
            );
        }
    }
    shell.close().await?;
    Ok(())
}

#[tokio::test]
async fn consolehost_wraps_the_existing_reader_and_prompt() -> Result<()> {
    let installed = LocalOptions::installed();
    exercise(
        &installed
            .get("Windows PowerShell")
            .context("Windows PowerShell missing")?
            .executable,
    )
    .await?;
    if let Some(core) = installed.get("PowerShell 7") {
        exercise(&core.executable).await?;
    }
    Ok(())
}
