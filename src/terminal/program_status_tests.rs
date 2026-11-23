use super::*;
use wezterm_term::{ProgramBlockKind, ProgramState, Progress};

fn terminal() -> (Terminal, mpsc::UnboundedReceiver<TransportInput>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let term = create(
        TerminalSize {
            cols: 40,
            rows: 8,
            ..Default::default()
        },
        tx,
    );
    (term, rx)
}

fn report(term: &mut Terminal, body: &str) {
    term.advance_bytes(format!("\x1b]7501;{body}\x1b\\"));
}

#[test]
fn program_status_replaces_records_inherits_app_and_clears_subtrees() {
    let (mut term, _) = terminal();
    report(&mut term, "state=idle:app=parent");
    report(&mut term, "state=working:id=build:app=cargo:progress=26");
    report(&mut term, "state=done:id=builder");
    report(
        &mut term,
        "state=working:foreign=bad;pair:state=blocked:id=build/test:kind=permission:progress=42:title=VGVzdHM=:msg=Q29udGludWU/",
    );
    let status = term.program_status().unwrap();
    assert_eq!(status.state, ProgramState::Blocked);
    assert_eq!(status.kind, Some(ProgramBlockKind::Permission));
    assert_eq!(status.progress, Some(42));
    assert_eq!(status.app.as_deref(), Some("cargo"));
    assert_eq!(status.title.as_deref(), Some("Tests"));
    assert_eq!(status.message.as_deref(), Some("Continue?"));

    report(&mut term, "state=working:id=build/test:title=U2Vjb25k");
    let status = term.program_status().unwrap();
    assert_eq!(status.state, ProgramState::Working);
    assert_eq!(status.kind, None);
    assert_eq!(status.progress, None);
    assert_eq!(status.title.as_deref(), Some("Second"));
    assert_eq!(status.message, None);
    report(&mut term, "state=clear:id=build");
    let status = term.program_status().unwrap();
    assert_eq!(status.state, ProgramState::Done);
    assert_eq!(status.app.as_deref(), Some("parent"));
    report(&mut term, "state=clear");
    assert!(term.program_status().is_none());
}

#[test]
fn program_status_lifetime_covers_resets_prompts_and_process_exit() {
    let (mut term, _) = terminal();
    term.advance_bytes(b"\x1b]9;4;1;42\x07");
    assert_eq!(term.get_progress(), Progress::Percentage(42));
    report(&mut term, "state=done:id=result");
    report(&mut term, "state=blocked:id=active");
    term.advance_bytes(b"\x1b[?1049h\x1b[!p\x1b[?1049l");
    assert_eq!(term.program_status().unwrap().state, ProgramState::Blocked);
    term.advance_bytes(b"\x1b]133;A\x07\x1b]9;4;1;77\x07");
    assert_eq!(term.program_status().unwrap().state, ProgramState::Done);
    assert_eq!(term.get_progress(), Progress::None);

    report(&mut term, "state=working:id=active");
    term.retire_running_program_statuses();
    assert_eq!(term.program_status().unwrap().state, ProgramState::Done);
    term.advance_bytes(b"\x1bc\x1b]9;4;1;77\x07");
    assert!(term.program_status().is_none());
    assert_eq!(term.get_progress(), Progress::Percentage(77));
}

#[test]
fn program_status_rejects_invalid_reports_whole_and_evicts_oldest_records() {
    let (mut term, _) = terminal();
    report(&mut term, "state=error:app=cargo");
    for bad in [
        "state=idle:id=bad!",
        "state=idle:id=bad//path",
        "state=idle:title=AA==",
        "state=idle:msg=Lg===:msg=b2s=",
        "state=idle:toolongforakeyabc=ignore",
    ] {
        report(&mut term, bad);
        assert_eq!(term.program_status().unwrap().state, ProgramState::Error);
    }
    report(
        &mut term,
        &format!("state=idle:unrecognized={}", "x".repeat(4096)),
    );
    assert_eq!(term.program_status().unwrap().state, ProgramState::Error);
    for id in 0..63 {
        report(&mut term, &format!("state=working:id=job/{id}"));
    }
    assert_eq!(term.program_status().unwrap().state, ProgramState::Error);
    report(&mut term, "state=working:id=job/63");
    assert_eq!(term.program_status().unwrap().state, ProgramState::Working);
}

#[tokio::test]
async fn program_status_query_returns_only_the_fixed_capability_reply() {
    let (mut term, mut output) = terminal();
    report(&mut term, "state=error:id=private:msg=c2VjcmV0");
    report(&mut term, "?");
    let reply = tokio::time::timeout(std::time::Duration::from_secs(2), output.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(reply, TransportInput::Data(bytes) if bytes == b"\x1b]7501;?\x1b\\"));
    assert!(output.try_recv().is_err());
}
