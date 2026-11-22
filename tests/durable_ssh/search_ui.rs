use super::Fixture;
use std::fmt::Write;

#[path = "../support/drive_gui.rs"]
mod drive;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "searches and refreshes real SSH discovery results from the native session browser"]
async fn native_search_remote_sessions_filters_cached_pages_and_attaches_the_refreshed_match() {
    let fixture = Fixture::new().await;
    {
        let mut peer = fixture.peer.backend.lock().unwrap();
        for index in 0..36 {
            let (dir, command) = if index == 35 {
                ("/projects/雪-orbit", "Survey-Recorder")
            } else {
                ("/batch", "worker")
            };
            writeln!(peer.zmx_extra_rows, "name=w11term-{index:032x}\tclients=0\tcwd={dir}\tcmd={command}\tw11term_generation={index:032x}").unwrap();
        }
        peer.tmux_name = Some("Night shift".into());
        peer.tmux_extra_listings = ["Night shift", "Moved elsewhere"]
            .map(|name| format!("$8\t11\t2346\t2\t1\t{name}\n"))
            .into();
    }
    let state = tempfile::tempdir().unwrap();
    drive::drive_mixed_gui(
        "remote-session-search-ui.ps1",
        &fixture.options,
        state.path(),
    );

    let peer = fixture.peer.backend.lock().unwrap();
    let calls = &peer.commands;
    let count = |needle: &str| calls.iter().filter(|s| s.contains(needle)).count();
    assert_eq!(
        count("zmx list"),
        2,
        "only initial discovery and explicit refresh: {calls:?}"
    );
    assert_eq!(count("tmux -uN -L default list-sessions"), 2);
    assert!(peer.tmux_extra_listings.is_empty());
    assert_eq!(
        count("if-shell"),
        1,
        "only one session was attached: {calls:?}"
    );
    assert_eq!(count("if-shell -F -t '$7:'"), 1);
    assert_eq!(count("list-clients -t '$7'"), 1);
    assert_eq!(
        peer.authenticated, 1,
        "refresh and attach should reuse the authenticated connection"
    );
    assert_eq!(
        peer.input.concat(),
        b"attached-search-pane",
        "search input reached the SSH peer"
    );
}
