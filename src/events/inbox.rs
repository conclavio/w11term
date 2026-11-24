use super::{CommandLink, Event, GroupTranscriptLine, PendingSshAttempt, SshAttemptOutcome};
use std::{
    collections::VecDeque,
    fmt,
    sync::{Arc, Mutex, MutexGuard},
    time::Instant,
};

const CAPACITY: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeTarget {
    Pane {
        pane: u64,
        sequence: u64,
    },
    GroupRun {
        tab: u64,
        submission: u64,
    },
    GroupCanary {
        tab: u64,
        submission: u64,
    },
    GroupOutput {
        tab: u64,
        submission: u64,
        pane: u64,
        line_id: u64,
    },
    GroupOutputMissing {
        tab: u64,
        submission: u64,
    },
    GroupLogins {
        tab: u64,
        sequence: u64,
    },
}

#[derive(Clone, Debug)]
pub struct SshLoginResult {
    pub pane: u64,
    pub attempt: PendingSshAttempt,
    pub host: String,
    pub outcome: SshAttemptOutcome,
}

#[derive(Clone, Debug)]
pub struct Notice {
    arrival: u64,
    effect: Effect,
    pub target: NoticeTarget,
    pub source: Arc<str>,
    pub message: String,
    pub unread: bool,
    pub command: Option<CommandLink>,
    pub login_results: Option<Arc<[SshLoginResult]>>,
}

#[derive(Clone, Copy, Debug, Default)]
enum Effect {
    #[default]
    None,
    FlashTaskbar,
    VisualBell(Instant),
}

#[derive(Default)]
pub struct InboxWake {
    pub flash_taskbar: bool,
    pub bells: Vec<(u64, Instant)>,
}

#[derive(Default)]
struct State {
    rows: VecDeque<Notice>,
    wake_pending: bool,
}

/// Window-wide, independently acknowledged view of the pane event journals.
#[derive(Clone)]
pub struct Inbox {
    state: Arc<Mutex<State>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl fmt::Debug for Inbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inbox").finish_non_exhaustive()
    }
}

pub(crate) fn display_field(field: &str) -> String {
    let prefix: String = field.chars().take(200).collect();
    prefix
        .split(|c: char| {
            c.is_whitespace()
                || c.is_control()
                || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn notification_text(data: &serde_json::Value) -> String {
    let message = ["title", "body"]
        .into_iter()
        .filter_map(|field| data[field].as_str())
        .map(display_field)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    if message.is_empty() {
        return "Notification".into();
    }
    message.chars().take(200).collect()
}

impl Inbox {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            state: Default::default(),
            wake: Arc::new(wake),
        }
    }

    pub(super) fn receive(&self, pane: u64, source: &Arc<str>, event: &Event, live: bool) {
        let message = match event.kind.as_str() {
            "bell" => "Bell".to_owned(),
            "recording-error" => {
                let Some(error) = event.data["error"].as_str() else {
                    return;
                };
                let reason = display_field(error);
                if reason.is_empty() {
                    "Recording failed".into()
                } else {
                    format!("Recording failed · {reason}")
                        .chars()
                        .take(200)
                        .collect()
                }
            }
            "command-finished" => event.data["exit_code"].as_i64().map_or_else(
                || "Command finished · exit status unavailable".into(),
                |code| format!("Command finished · exit code {code}"),
            ),
            "notification" => event.data["summary"]
                .as_str()
                .map_or_else(|| notification_text(&event.data), str::to_owned),
            _ => return,
        };
        self.push(
            NoticeTarget::Pane {
                pane,
                sequence: event.sequence,
            },
            source.clone(),
            message,
            event.command,
            None,
            match (live, event.kind.as_str()) {
                (true, "command-finished") => Effect::FlashTaskbar,
                (true, "bell") => Effect::VisualBell(Instant::now()),
                _ => Effect::None,
            },
        );
    }

    pub(super) fn group_finished(
        &self,
        tab: u64,
        submission: u64,
        source: &str,
        succeeded: usize,
        attention: usize,
        skipped: usize,
    ) {
        let attention_verb = if attention == 1 { "needs" } else { "need" };
        let mut message = format!(
            "Group run finished · {succeeded} succeeded · {attention} {attention_verb} attention"
        );
        if skipped > 0 {
            message.push_str(&format!(" · {skipped} skipped"));
        }
        self.push(
            NoticeTarget::GroupRun { tab, submission },
            display_field(source).into(),
            message,
            None,
            None,
            Effect::FlashTaskbar,
        );
    }

    pub fn group_canary_finished(
        &self,
        tab: u64,
        submission: u64,
        source: &str,
        host: &str,
        succeeded: bool,
    ) {
        let result = if succeeded {
            "Canary ready for approval"
        } else {
            "Canary did not report a successful exit"
        };
        self.push(
            NoticeTarget::GroupCanary { tab, submission },
            display_field(source).into(),
            format!("{result} · {}", display_field(host)),
            None,
            None,
            Effect::FlashTaskbar,
        );
    }

    pub fn group_connections_finished(
        &self,
        tab: u64,
        sequence: u64,
        source: &str,
        results: Vec<SshLoginResult>,
    ) {
        let (mut connected, mut failed, mut stopped, mut closed) = (0, 0, 0, 0);
        for result in &results {
            match result.outcome {
                SshAttemptOutcome::Connected => connected += 1,
                SshAttemptOutcome::Failed(_) => failed += 1,
                SshAttemptOutcome::Stopped => stopped += 1,
                SshAttemptOutcome::Closed => closed += 1,
            }
        }
        self.push(
            NoticeTarget::GroupLogins { tab, sequence },
            display_field(source).into(),
            format!("SSH logins finished · {connected} connected · {failed} failed · {stopped} stopped · {closed} closed"),
            None,
            Some(results.into()),
            Effect::FlashTaskbar,
        );
    }

    pub(super) fn group_output_matched(
        &self,
        tab: u64,
        submission: u64,
        source: &str,
        host: &str,
        line: &GroupTranscriptLine,
    ) {
        let host = display_field(host);
        let excerpt = display_field(&line.text);
        let mut message = format!("Matched output · {host}");
        if !excerpt.is_empty() {
            message.push_str(&format!(" · {excerpt}"));
        }
        self.push(
            NoticeTarget::GroupOutput {
                tab,
                submission,
                pane: line.pane,
                line_id: line.id,
            },
            display_field(source).into(),
            message,
            None,
            None,
            Effect::FlashTaskbar,
        );
    }

    pub(super) fn group_output_unmatched(
        &self,
        tab: u64,
        submission: u64,
        source: &str,
        query: &str,
        confirmed: bool,
    ) {
        let status = if confirmed {
            "Expected output missing"
        } else {
            "Could not verify expected output"
        };
        self.push_output_expectation(tab, submission, source, query, status);
    }

    pub(super) fn group_output_missing_by_host(
        &self,
        tab: u64,
        submission: u64,
        source: &str,
        query: &str,
        missing: usize,
        unverified: usize,
    ) {
        let count_hosts = |count| format!("{count} {}", if count == 1 { "host" } else { "hosts" });
        let mut parts = Vec::new();
        if missing > 0 {
            parts.push(format!(
                "Expected output missing on {}",
                count_hosts(missing)
            ));
        }
        if unverified > 0 {
            parts.push(format!(
                "Could not verify expected output on {}",
                count_hosts(unverified)
            ));
        }
        self.push_output_expectation(tab, submission, source, query, &parts.join(" · "));
    }

    fn push_output_expectation(
        &self,
        tab: u64,
        submission: u64,
        source: &str,
        query: &str,
        status: &str,
    ) {
        let query = display_field(query);
        let message = if query.is_empty() {
            status.into()
        } else {
            format!("{status} · {query}")
        };
        self.push(
            NoticeTarget::GroupOutputMissing { tab, submission },
            display_field(source).into(),
            message,
            None,
            None,
            Effect::FlashTaskbar,
        );
    }

    fn push(
        &self,
        target: NoticeTarget,
        source: Arc<str>,
        message: String,
        command: Option<CommandLink>,
        login_results: Option<Arc<[SshLoginResult]>>,
        effect: Effect,
    ) {
        let mut state = self.state.lock().unwrap();
        if state.rows.len() == CAPACITY {
            state.rows.pop_front();
        }
        state.rows.push_back(Notice {
            arrival: {
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            },
            target,
            effect,
            source,
            message,
            unread: true,
            command,
            login_results,
        });
        self.wake_ui(state);
    }

    pub(crate) fn command_links_changed(
        &self,
        pane: u64,
        changed: impl Fn(CommandLink) -> bool,
    ) -> bool {
        self.state.lock().unwrap().rows.iter().any(|row| {
            matches!(row.target, NoticeTarget::Pane { pane: from, .. } if from == pane)
                && row.command.is_some_and(&changed)
        })
    }

    pub(crate) fn refresh(&self) {
        self.wake_ui(self.state.lock().unwrap());
    }

    fn wake_ui(&self, mut state: MutexGuard<'_, State>) {
        if !state.wake_pending {
            state.wake_pending = true;
            drop(state);
            (self.wake)();
        }
    }

    /// Consume transient UI effects once, keeping the notices for the inbox.
    pub fn acknowledge_wake(&self) -> InboxWake {
        let mut state = self.state.lock().unwrap();
        let mut wake = InboxWake::default();
        if !std::mem::take(&mut state.wake_pending) {
            return wake;
        }
        for item in &mut state.rows {
            match (std::mem::take(&mut item.effect), item.target) {
                (Effect::FlashTaskbar, _) => wake.flash_taskbar |= item.unread,
                (Effect::VisualBell(at), NoticeTarget::Pane { pane, .. }) => {
                    wake.bells.push((pane, at));
                }
                _ => {}
            }
        }
        wake
    }

    pub(super) fn same_window(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }

    pub(super) fn move_pane_to(&self, pane: u64, target: &Self) {
        let mut moved = Vec::new();
        self.state.lock().unwrap().rows.retain(|notice| {
            if matches!(notice.target, NoticeTarget::Pane { pane: from, .. } if from == pane) {
                let mut notice = notice.clone();
                if matches!(notice.effect, Effect::VisualBell(_)) {
                    notice.effect = Effect::None;
                }
                moved.push(notice);
                false
            } else {
                true
            }
        });
        if moved.is_empty() {
            return;
        }
        let mut dest = target.state.lock().unwrap();
        dest.rows.extend(moved);
        dest.wake_pending = true;
        dest.rows
            .make_contiguous()
            .sort_by_key(|notice| notice.arrival);
        let excess = dest.rows.len().saturating_sub(CAPACITY);
        dest.rows.drain(..excess);
        drop(dest);
        (self.wake)();
        (target.wake)();
    }

    pub fn unread_count(&self) -> usize {
        self.state
            .lock()
            .unwrap()
            .rows
            .iter()
            .filter(|row| row.unread)
            .count()
    }

    pub fn rows(&self) -> Vec<Notice> {
        self.state
            .lock()
            .unwrap()
            .rows
            .iter()
            .rev()
            .cloned()
            .collect()
    }

    pub fn command_link(&self, pane: u64, sequence: u64) -> Option<CommandLink> {
        self.state
            .lock()
            .unwrap()
            .rows
            .iter()
            .find(|row| row.target == NoticeTarget::Pane { pane, sequence })?
            .command
    }

    pub fn group_login_notice(&self, target: NoticeTarget) -> Option<Notice> {
        self.state
            .lock()
            .unwrap()
            .rows
            .iter()
            .find(|row| row.target == target && row.login_results.is_some())
            .cloned()
    }

    pub fn mark_read(&self, pane: u64, sequence: u64) -> bool {
        self.mark_target_read(NoticeTarget::Pane { pane, sequence })
    }

    pub fn read_group_output_notice(&self, target: NoticeTarget) -> Option<String> {
        let NoticeTarget::GroupOutput { .. } = target else {
            return None;
        };
        let mut state = self.state.lock().unwrap();
        let row = state.rows.iter_mut().find(|row| row.target == target)?;
        row.unread = false;
        Some(row.message.clone())
    }

    pub fn mark_target_read(&self, target: NoticeTarget) -> bool {
        let mut state = self.state.lock().unwrap();
        if let Some(row) = state.rows.iter_mut().find(|row| row.target == target) {
            row.unread = false;
            return true;
        }
        false
    }

    pub fn mark_all_read(&self) {
        for row in &mut self.state.lock().unwrap().rows {
            row.unread = false;
        }
    }

    pub fn clear(&self) {
        self.state.lock().unwrap().rows.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_canary_and_full_completion_for_the_same_run_are_read_independently() {
        let inbox = Inbox::new(|| {});
        let canary = NoticeTarget::GroupCanary {
            tab: 4,
            submission: 2,
        };
        let run = NoticeTarget::GroupRun {
            tab: 4,
            submission: 2,
        };

        inbox.group_canary_finished(4, 2, "Fleet", "East", true);
        inbox.group_finished(4, 2, "Fleet", 2, 0, 0);

        assert_eq!(inbox.unread_count(), 2);
        assert!(inbox.mark_target_read(canary));
        let rows = inbox.rows();
        assert_eq!(rows[0].target, run);
        assert!(rows[0].unread);
        assert_eq!(rows[1].target, canary);
        assert_eq!(rows[1].message, "Canary ready for approval · East");
        assert!(!rows[1].unread);
        assert!(inbox.mark_target_read(run));
        assert_eq!(inbox.unread_count(), 0);
    }
    use crate::events::Events;
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn only_new_unread_command_completions_flash_the_current_owner_once() {
        let source = Inbox::new(|| {});
        let target = Inbox::new(|| {});
        let journal = Events::default();
        source.refresh();
        assert!(source.rows().is_empty());
        assert!(!source.acknowledge_wake().flash_taskbar);
        journal.push("command-finished", json!({"exit_code": null}));
        journal.connect_inbox(7, "SSH", &source);
        assert_eq!(
            source.rows()[0].message,
            "Command finished · exit status unavailable"
        );
        assert!(!source.acknowledge_wake().flash_taskbar);

        journal.push("bell", Value::Null);
        journal.push("notification", json!({"body":"command-finished"}));
        assert!(!source.acknowledge_wake().flash_taskbar);
        journal.push(
            "command-finished",
            json!({"exit_code": 17, "body":"private"}),
        );
        journal.push("command-finished", json!({"exit_code": 17}));
        journal.connect_inbox(7, "SSH", &target);
        assert!(!source.acknowledge_wake().flash_taskbar);
        assert_eq!(target.rows()[0].message, "Command finished · exit code 17");
        assert!(target.acknowledge_wake().flash_taskbar);
        assert!(!target.acknowledge_wake().flash_taskbar);

        journal.connect_inbox(7, "SSH", &source);
        assert!(!source.acknowledge_wake().flash_taskbar);
        journal.push("command-finished", json!({"exit_code": 0}));
        source.mark_all_read();
        assert!(!source.acknowledge_wake().flash_taskbar);
    }

    #[test]
    fn moving_panes_keeps_inbox_order_and_read_state_and_routes_new_alerts() {
        let source = Inbox::new(|| {});
        let target = Inbox::new(|| {});
        let a = Events::default();
        let b = Events::default();
        a.connect_inbox(1, "ssh", &source);
        b.connect_inbox(2, "local", &source);
        a.push("bell", Value::Null);
        b.push("bell", Value::Null);
        a.push("bell", Value::Null);
        assert!(source.mark_read(1, 1));
        b.connect_inbox(2, "local", &target);
        a.connect_inbox(1, "ssh", &target);
        b.push("bell", Value::Null);
        assert!(source.rows().is_empty());
        assert_eq!(target.unread_count(), 3);
        let wake = target.acknowledge_wake();
        assert!(!wake.flash_taskbar);
        assert_eq!(
            wake.bells.len(),
            1,
            "only the new bell travels to the new window"
        );
        assert_eq!(wake.bells[0].0, 2);
        assert!(target.acknowledge_wake().bells.is_empty());
        let rows: Vec<_> = target.rows().into_iter().map(|n| n.target).collect();
        assert_eq!(
            rows,
            [(2, 2), (1, 2), (2, 1), (1, 1)]
                .map(|(pane, sequence)| NoticeTarget::Pane { pane, sequence })
        );
    }

    #[test]
    fn window_aggregates_panes_and_replays_startup_once_without_consuming_the_journal() {
        let left = Events::default();
        let right = Events::default();
        let wakes = Arc::new(AtomicUsize::new(0));
        let signal = wakes.clone();
        let inbox = Inbox::new(move || {
            signal.fetch_add(1, Ordering::SeqCst);
        });
        left.push(
            "notification",
            json!({"title":"Task\nupdate","body":"  finished "}),
        );
        left.connect_inbox(1, "left", &inbox);
        left.connect_inbox(1, "left", &inbox);
        right.connect_inbox(2, "right", &inbox);
        right.push("bell", Value::Null);
        left.push("title", json!({"title":"other"}));
        assert_eq!(inbox.unread_count(), 2);
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        let rows = inbox.rows();
        assert_eq!(rows[0].message, "Bell");
        assert_eq!(&*rows[0].source, "right");
        assert_eq!(rows[1].message, "Task update · finished");
        assert!(inbox.mark_read(1, 1));
        assert!(inbox.rows()[0].unread);
        assert!(!inbox.rows()[1].unread);
        inbox.acknowledge_wake();
        right.push("bell", Value::Null);
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
        inbox.mark_all_read();
        assert_eq!(inbox.unread_count(), 0);
        inbox.clear();
        assert!(inbox.rows().is_empty());
        assert_eq!(left.since(0).events.len(), 2);
    }

    #[test]
    fn evicted_and_cleared_notices_cannot_select_a_pane() {
        let events = Events::default();
        let inbox = Inbox::new(|| {});
        events.connect_inbox(7, "test", &inbox);
        for _ in 0..=CAPACITY {
            events.push("bell", Value::Null);
        }
        assert_eq!(inbox.rows().len(), CAPACITY);
        assert!(!inbox.mark_read(7, 1));
        assert!(inbox.mark_read(7, 2));
        inbox.clear();
        assert!(!inbox.mark_read(7, 2));
    }

    #[test]
    fn oversized_notice_keeps_a_bounded_single_line_summary_even_at_startup() {
        let events = Events::default();
        let inbox = Inbox::new(|| {});
        events.push(
            "notification",
            json!({"title":"Done\u{202e}\tbuilding","body":"é".repeat(5000)}),
        );
        events.connect_inbox(1, "dev", &inbox);
        let row = inbox.rows().pop().unwrap();
        assert!(row.message.starts_with("Done building · é"));
        assert_eq!(row.message.chars().count(), 200);
    }
}
