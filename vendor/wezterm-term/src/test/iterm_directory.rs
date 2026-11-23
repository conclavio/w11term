use super::TestTerm;

const OWN: &str = "\x1b]1337;RemoteHost=pilot@realm@LOCALHOST.\x07";
const PATH: &str = "\x1b]1337;CurrentDir=/a b/%2F;#雪\x1b\\";
const URI: &str = "file://localhost/a%20b/%252F%3B%23%E9%9B%AA";
const OSC7: &str = "\x1b]7;file://localhost/other\x07";

#[test]
fn iterm_directory_pair_and_osc7_take_turns_without_reusing_identity() {
    let mut t = TestTerm::new(4, 20, 0);
    t.term
        .enable_iterm_directory_reports("pilot@realm", "localhost");
    t.print(PATH);
    assert!(t.reported_current_dir().is_none());
    t.print(OWN);
    t.print(PATH);
    assert_eq!(t.reported_current_dir().map(|s| s.as_ref()), Some(URI));
    t.print("\x1b]1337;CurrentDir=/C|/project\x07");
    assert_eq!(t.get_current_dir().unwrap().path(), "/C%7C/project");
    assert_eq!(
        t.reported_current_dir().unwrap().as_ref(),
        "file://localhost/C%7C/project"
    );
    t.print(OSC7);
    t.print(PATH);
    assert_eq!(
        t.reported_current_dir().map(|s| s.as_ref()),
        Some("file://localhost/other")
    );
    for host in [
        "pilot@elsewhere",
        "pilot@realm@elsewhere",
        "other@localhost",
        "",
        "pilot@realm@localhost:22",
    ] {
        t.print(format!("\x1b]1337;RemoteHost={host}\x07"));
        t.print(PATH);
        assert!(t.reported_current_dir().is_none());
        t.print(OWN);
        assert!(t.reported_current_dir().is_none());
        t.print(PATH);
        assert_eq!(t.reported_current_dir().map(|s| s.as_ref()), Some(URI));
    }
    for path in [
        "relative",
        "",
        "//elsewhere/dir",
        "/../other",
        "/bad\nname",
        "/bad\\name",
    ] {
        t.print(format!("\x1b]1337;CurrentDir={path}\x07"));
        assert!(t.reported_current_dir().is_none());
        t.print(PATH);
        assert!(t.reported_current_dir().is_some());
    }
    t.print("\x1bc");
    t.print(PATH);
    assert!(t.reported_current_dir().is_none());
    t.print(OWN);
    t.print(PATH);
    t.term.reset_connection();
    t.print(PATH);
    assert!(t.reported_current_dir().is_none());
    t.print(OWN);
    t.print(PATH);
    assert_eq!(t.reported_current_dir().map(|s| s.as_ref()), Some(URI));
}

#[test]
fn ignore_disabled_reports_and_compare_ipv6_addresses() {
    let mut t = TestTerm::new(4, 20, 0);
    t.print(OSC7);
    t.print(OWN);
    t.print(PATH);
    assert_eq!(
        t.reported_current_dir().map(|s| s.as_ref()),
        Some("file://localhost/other")
    );
    t.term.enable_iterm_directory_reports("pilot", "[::1]");
    t.print("\x1b]1337;RemoteHost=pilot@0:0:0:0:0:0:0:1\x07\x1b]1337;CurrentDir=/\x07");
    assert_eq!(
        t.reported_current_dir().map(|s| s.as_ref()),
        Some("file://[::1]/")
    );
}
