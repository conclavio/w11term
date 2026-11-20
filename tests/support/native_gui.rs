use serde_json::json;
use std::{fs, path::Path};
use w11term::options::Source;

/// Run the native UI against the loopback SSH peer used by the
/// protocol tests. Every file the GUI can update belongs to this test.
pub(crate) async fn run_gui(source: &Source, state: &Path, flags: &[(&str, &str)], marker: &str) {
    let Source::Ssh(options) = source else {
        panic!("expected SSH")
    };
    let profiles = state.join("profiles.json");
    fs::write(
        &profiles,
        serde_json::to_vec(&json!({"profiles":{"fixture":options}})).unwrap(),
    )
    .unwrap();
    let log = state.join("gui.log");
    let mut command = std::process::Command::new("powershell.exe");
    command.args(["-NoProfile", "-NonInteractive", "-Command", r#"
        $ErrorActionPreference = 'Stop'
        $process = Start-Process -FilePath $env:W11TERM_TEST_GUI -ArgumentList @('--profiles', ('"' + $env:W11TERM_TEST_PROFILES + '"'), '--profile', 'fixture') -WindowStyle Hidden -PassThru -RedirectStandardError $env:W11TERM_TEST_LOG
        if (-not $process.WaitForExit(60000)) {
            $process.Kill()
            throw 'Native SSH smoke timed out'
        }
        exit $process.ExitCode
    "#])
        .env("W11TERM_TEST_GUI", env!("CARGO_BIN_EXE_w11term"))
        .env("W11TERM_TEST_PROFILES", profiles)
        .env("W11TERM_TEST_LOG", &log)
        .env("W11TERM_DATA_DIR", state.join("data"))
        .env("W11TERM_SETTINGS_PATH", state.join("settings.json"))
        .env("W11TERM_UI_SMOKE", "1")
        .envs(flags.iter().copied());
    let outcome = tokio::task::spawn_blocking(move || command.output())
        .await
        .unwrap()
        .unwrap();
    let report = fs::read_to_string(log).unwrap_or_default();
    assert!(
        outcome.status.success() && report.contains(marker),
        "{}\n{report}",
        String::from_utf8_lossy(&outcome.stderr)
    );
}
