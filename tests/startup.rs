//! A relocated GUI executable must explain the packaging error without hanging on a dialog.
#[test]
fn executable_without_runtime_reports_actionable_error_to_stderr_and_log() {
    use std::os::windows::process::CommandExt;
    let directory = tempfile::tempdir().unwrap();
    let binary = directory.path().join("w11term.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_w11term"), &binary).unwrap();
    let data = directory.path().join("data");
    let output = std::process::Command::new(&binary)
        .creation_flags(0x08000000) // CREATE_NO_WINDOW; redirected stderr remains usable.
        .args(["--demo", "--smoke-seconds", "1"])
        .env("W11TERM_DATA_DIR", &data)
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Copying w11term.exe alone is not sufficient"),
        "{stderr}"
    );
    assert!(stderr.contains("Startup log:"), "{stderr}");
    let log = std::fs::read_to_string(data.join("startup.log")).unwrap();
    assert!(log.contains("Windows App Runtime files are missing"));
    assert!(log.contains(&binary.display().to_string()));
}
