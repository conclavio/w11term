use super::*;
use termwiz::escape::osc::{OperatingSystemCommand, VscodeShellIntegration};

fn terminal() -> Terminal {
    let (tx, _) = mpsc::unbounded_channel();
    create(
        TerminalSize {
            cols: 40,
            rows: 8,
            ..Default::default()
        },
        tx,
    )
}

#[test]
fn vscode_shell_markers_keep_the_prompt_position_and_capture_exact_command_and_output() {
    let marker = OperatingSystemCommand::parse(&[
        b"633",
        b"E",
        r"echo 雪\x3B\x0a  \\path\x09".as_bytes(),
        b"optional-nonce",
    ]);
    let line = "echo 雪;\n  \\path\t";
    assert_eq!(
        marker,
        OperatingSystemCommand::VscodeShellIntegration(VscodeShellIntegration::CommandLine(
            line.into()
        ))
    );
    assert_eq!(
        marker.to_string(),
        "\x1b]633;E;echo\\x20雪\\x3b\\x0a\\x20\\x20\\\\path\\x09\x1b\\"
    );

    let mut t = terminal();
    t.advance_bytes("prefix\x1b]633;A\x07$ ");
    assert_eq!((t.cursor_pos().x, t.cursor_pos().y), (8, 0));
    let wire = format!("\x1b]633;B\x07displayed\r\n{marker}\x1b]633;C\x07result\x1b]633;D;23\x07");
    for byte in wire.bytes() {
        t.advance_bytes([byte]);
    }
    let entry = t.last_command().unwrap();
    assert_eq!(entry.command.as_deref(), Some(line));
    assert_eq!(entry.exit_code, Some(23));
    assert_eq!(entry.output.as_deref(), Ok("result"));
    assert!(t.command_output_row(entry.id).is_some());
}

#[test]
fn vscode_shell_handles_late_text_empty_submissions_and_exit_only_shells() {
    let mut t = terminal();
    t.advance_bytes("\x1b]633;A\x07$ \x1b]633;B\x07\x1b]633;D\x07");
    t.advance_bytes("\x1b]633;A\x07$ \x1b]633;B\x07\x1b]633;E;\x07\x1b]633;C\x07\x1b]633;D\x07");
    assert!(t.command_history().is_empty());

    t.advance_bytes("\r\n\x1b]633;A\x07$ \x1b]633;B\x07displayed\r\n\x1b]633;C\x07live\x1b]633;E;echo\\x20real\x07\x1b]633;D\x07");
    let entry = t.last_command().unwrap();
    assert_eq!(entry.command.as_deref(), Some("echo real"));
    assert_eq!(entry.output.as_deref(), Ok("live"));
    assert_eq!(entry.exit_code, None);

    t.advance_bytes("\r\n\x1b]633;A\x07$ \x1b]633;B\x07plain prompt\r\n\x1b]633;D;1\x07");
    assert_eq!(t.command_history().len(), 2);
    let entry = t.last_command().unwrap();
    assert_eq!(entry.exit_code, Some(1));
    assert_eq!(entry.command, None);
    assert_eq!(entry.output.as_deref(), Err(&"No command output marker"));
}

#[test]
fn vscode_shell_limits_escaped_text_and_forgets_pending_commands_on_reset() {
    let mut t = terminal();
    let limit = "\\x21".repeat(4096);
    t.advance_bytes(format!(
        "\x1b]633;E;{limit};nonce\x07\x1b]633;C\x07\x1b]633;D;0\x07"
    ));
    assert_eq!(
        t.last_command().unwrap().command.as_deref(),
        Some("!".repeat(4096).as_str())
    );
    for bad in ["\\x07".to_owned(), "x".repeat(50000)] {
        t.advance_bytes(format!("\r\n\x1b]633;A\x07$ \x1b]633;B\x07safe\r\n\x1b]633;E;{bad};nonce\x07\x1b]633;C\x07\x1b]633;D;0\x07"));
        assert_eq!(t.last_command().unwrap().command.as_deref(), Some("safe"));
    }
    t.advance_bytes("\x1b]633;E;stale\x07\x1b[?1049h\x1b]633;E;alternate\x07\x1b[?1049l");
    t.reset_connection();
    t.advance_bytes("\x1b]633;C\x07\x1b]633;D;0\x07");
    assert_eq!(t.last_command().unwrap().command, None);
}

#[test]
fn vscode_directory_reports_use_the_configured_transport_and_literal_path() {
    let mut ssh = terminal();
    ssh.enable_iterm_directory_reports("pilot", "test.example");
    ssh.advance_bytes("\x1b]633;P;Cwd=/opt/100% \\x23雪\\x3b\x07");
    assert_eq!(
        ssh.reported_current_dir().map(AsRef::as_ref),
        Some("file://test.example/opt/100%25%20%23%E9%9B%AA%3B")
    );
    ssh.advance_bytes("\x1b]633;P;IsWindows=True\x07\x1b]633;P;Cwd=C:\\x5cUsers\x07");
    assert!(ssh.reported_current_dir().is_none());

    let mut local = terminal();
    local.enable_conemu_directory_reports();
    local.advance_bytes("\x1b]633;P;Cwd=C:\\x5cUsers\\x5c100% \\x23雪\x07");
    assert_eq!(
        local.reported_current_dir().map(AsRef::as_ref),
        Some("file:///C:/Users/100%25%20%23%E9%9B%AA")
    );

    let mut wsl = terminal();
    wsl.enable_wsl_directory_reports();
    wsl.advance_bytes("\x1b]633;P;Cwd=/home/pilot/first\\x20folder\x07");
    assert_eq!(
        wsl.reported_current_dir().map(AsRef::as_ref),
        Some("file:///home/pilot/first%20folder")
    );
}
