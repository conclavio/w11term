//! Exercise the shell script inside a real, headless ConsoleHost and ConPTY.
use anyhow::{Context, Result, ensure};
use std::{path::Path, time::Duration};
use tokio::sync::mpsc;
use w11term::{conpty, options::LocalOptions, ssh::Event, terminal::TransportInput};
use wezterm_term::TerminalSize;

async fn through(output: &mut mpsc::Receiver<Event>, marker: &str) -> Result<String> {
    wait_for(output, marker, Duration::from_secs(15)).await
}

async fn first_prompt(output: &mut mpsc::Receiver<Event>, marker: &str) -> Result<String> {
    // A hosted runner can still be loading ConsoleHost and PSReadLine after
    // the pseudoconsole has painted its initial title. Bound cold startup
    // separately from an already-running shell executing a fixture command.
    wait_for(output, marker, Duration::from_secs(45)).await
}

async fn wait_for(
    output: &mut mpsc::Receiver<Event>,
    marker: &str,
    deadline: Duration,
) -> Result<String> {
    let mut text = String::new();
    tokio::time::timeout(deadline, async {
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
    .with_context(|| format!("waiting {deadline:?} for {marker:?}; output: {text:?}"))??;
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
        Ok(Self::launch(options))
    }

    fn launch(options: LocalOptions) -> Self {
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
        Self {
            input,
            output,
            task,
        }
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

#[tokio::test]
async fn legacy_powershell_opens_bracketed_directories_with_and_without_prompt_integration()
-> Result<()> {
    let installed = LocalOptions::installed();
    let legacy = installed
        .get("Windows PowerShell")
        .context("Windows PowerShell missing")?;
    let temp = tempfile::tempdir()?;
    let literal = temp.path().join("fixture [ab]");
    std::fs::create_dir(&literal)?;
    for matching_sibling in [false, true] {
        if matching_sibling {
            // The 5.1 fallback can select a different existing directory.
            std::fs::create_dir(temp.path().join("fixture a"))?;
        }
        for integration in [false, true] {
            let mut options = legacy.clone();
            options.args = vec!["-NoLogo".into(), "-NoProfile".into()];
            options.cwd = Some(literal.clone());
            options.powershell_integration = integration;
            let mut shell = Shell::launch(options);
            let ready = if integration { "\x1b]133;B" } else { ">" };
            let output = first_prompt(&mut shell.output, ready).await?;
            ensure!(
                output.contains("fixture [ab]>"),
                "integration={integration}, sibling={matching_sibling}: {output:?}"
            );
            shell.close().await?;
        }
    }
    Ok(())
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
    // These plain-text sentinels distinguish an interpreter still starting,
    // a hook that returned without installing, and a missing input marker in a
    // timeout transcript. They don't create a reader or generate OSC output.
    let stock_startup = temp.path().join("stock.ps1");
    std::fs::write(
        &stock_startup,
        "[Console]::WriteLine('[fixture: stock startup entered]')\n. (Join-Path $PSScriptRoot 'hook.ps1')\n[Console]::WriteLine('[fixture: stock hook installed=' + [bool]$global:__W11TermSemanticPrompt + ']')\n",
    )?;
    let mut stock = Shell::start(executable, &stock_startup)?;
    let initial = first_prompt(&mut stock.output, ready)
        .await
        .context("pristine profile did not emit the input marker")?;
    ensure!(
        initial.contains("[fixture: stock hook installed=True]"),
        "{initial:?}"
    );
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
    first_prompt(&mut baseline.output, "fixture-prompt-True>").await?;
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
    let initial = first_prompt(&mut shell.output, ready).await?;
    assert!(initial.contains("\x1b]133;A") && initial.contains("fixture-prompt-True>"));
    // Feed the real ConsoleHost output to the same parser that publishes pane
    // state. A provider drive has a display name unrelated to its native path.
    let (writer, _replies) = mpsc::unbounded_channel();
    let mut terminal = w11term::terminal::create(Default::default(), writer);
    terminal.advance_bytes(initial.as_bytes());
    let fixture_directory = temp.path().join("files 猫 #%2F;[]&");
    std::fs::create_dir(&fixture_directory)?;
    let start_drive = "New-PSDrive Fixture FileSystem (Join-Path $PWD ('files '+[char]0x732B+' #%2F;[]&')) -Scope Global | Out-Null; Set-Location Fixture:\\";
    for (command, expected) in [
        (start_drive, Some(fixture_directory.as_path())),
        ("Set-Location Env:\\", None),
        ("Set-Location Fixture:\\", Some(fixture_directory.as_path())),
    ] {
        shell.write(&format!("{command}\r"))?;
        let bytes = through(&mut shell.output, ready).await?;
        ensure!(
            bytes.matches("\x1b]7;").count() == 1,
            "{command}: {bytes:?}"
        );
        terminal.advance_bytes(bytes.as_bytes());
        ensure!(
            terminal
                .get_current_dir()
                .and_then(|url| url.to_file_path().ok())
                .as_deref()
                == expected,
            "{command}: expected {expected:?}, got {:?} in {bytes:?}",
            terminal.reported_current_dir(),
        );
        if expected.is_some() {
            let raw = terminal
                .reported_current_dir()
                .context("missing file URI")?;
            ensure!(
                raw.trim_end_matches('/')
                    .ends_with("/files%20%E7%8C%AB%20%23%252F%3B%5B%5D%26"),
                "{raw}"
            );
        } else {
            ensure!(terminal.reported_current_dir().is_none(), "{bytes:?}");
        }
    }
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
                before.contains("fixture-prompt-True>") && !before.contains("\x1b]7;"),
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
