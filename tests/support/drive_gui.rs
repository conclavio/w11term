use serde_json::json;
use std::fs;
use w11term::options::SshOptions;

pub(crate) fn drive_mixed_gui(script: &str, ssh: &SshOptions, state: &std::path::Path) {
    fs::write(
        state.join("profiles.json"),
        serde_json::to_vec(&json!({"profiles":{
        "palette-ssh":ssh, "Palette PowerShell":{
            "backend":"local", "executable":"powershell.exe", "args":["-NoProfile","-NoLogo"]}}}))
        .unwrap(),
    )
    .unwrap();
    drive_gui(script, state);
}

pub(crate) fn drive_gui(script: &str, state: &std::path::Path) {
    fs::write(
        state.join("appearance.json"),
        br#"{"shortcuts":{"command_palette":"F7","select_tab_1":"F8","copy_mode":"F9"}}"#,
    )
    .unwrap();
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-STA", "-File"])
        .arg(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests")
                .join(script),
        )
        .args([
            "-Gui",
            env!("CARGO_BIN_EXE_w11term"),
            "-Cli",
            env!("CARGO_BIN_EXE_w11term-cli"),
        ])
        .arg("-State")
        .arg(state)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        fs::read_to_string(state.join("gui.log")).unwrap_or_default()
    );
}
