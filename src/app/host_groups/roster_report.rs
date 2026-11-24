use super::*;
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::fmt::Write;

fn single_line(text: &str) -> String {
    text.replace(
        |c: char| {
            c.is_control()
                || matches!(
                    c,
                    '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
                )
        },
        " ",
    )
}

fn roster_text(
    label: &str,
    shown: &[&Target],
    selected: &BTreeSet<u64>,
    limit: usize,
) -> Result<String> {
    let checked = shown
        .iter()
        .filter(|target| selected.contains(&target.pane))
        .count();
    let label = single_line(label);
    let mut text = format!(
        "{label} roster\r\n{} shown · {checked} checked\r\n",
        shown.len()
    );
    for target in shown {
        let check = if selected.contains(&target.pane) {
            "Checked"
        } else {
            "Unchecked"
        };
        let name = single_line(&target.name);
        writeln!(text, "{check} · {name} · {}\r", target.status_caption())?;
        ensure!(
            text.len() <= limit,
            "Roster report exceeds the clipboard limit"
        );
    }
    Ok(text)
}

pub(super) fn roster_json(label: &str, page: &HostGroup, targets: &[Target]) -> Value {
    let mut scoped: Vec<_> = page.scoped_targets(targets).collect();
    page.sort_shown(&mut scoped);
    let mut shown_count = 0;
    let mut checked_count = 0;
    let mut checked_shown_count = 0;
    let mut ready_checked_shown_count = 0;
    let hosts: Vec<_> = scoped
        .into_iter()
        .map(|target| {
            let shown = page.filter.matches(target);
            let checked = page.selected.contains(&target.pane);
            shown_count += usize::from(shown);
            checked_count += usize::from(checked);
            checked_shown_count += usize::from(checked && shown);
            ready_checked_shown_count += usize::from(checked && shown && target.state.is_ready());
            let prompt_verification = match target.state {
                TargetState::Ready(_) => Some("shell_markers"),
                TargetState::Matched(_) => Some("pattern"),
                _ => None,
            };
            let reconnect_attempt = match target.state {
                TargetState::Reconnecting(attempt) => attempt,
                _ => None,
            };
            json!({
                "pane_id": target.pane,
                "name": target.names.label,
                "profile": target.profile,
                "address": target.names.address(),
                "state": target.state.status(),
                "connected": target.connected,
                "checked": checked,
                "shown": shown,
                "prompt_verification": prompt_verification,
                "reconnect_attempt": reconnect_attempt,
            })
        })
        .collect();
    let scope = if page
        .roster
        .as_ref()
        .is_some_and(|roster| !roster.show_window)
    {
        "group"
    } else {
        "window"
    };
    json!({
        "schema_version": 1,
        "label": label,
        "scope": scope,
        "sort": page.roster_preferences.order,
        "require_all_ready": page.roster_preferences.require_all_ready,
        "selection_ready": page.readiness_allows_send(ready_checked_shown_count, checked_shown_count),
        "filter": {
            "query": page.filter.query(),
            "wildcards": page.filter.wildcards(),
            "state": page.filter.status(),
            "error": page.filter.error(),
        },
        "counts": {
            "hosts": hosts.len(),
            "shown": shown_count,
            "checked": checked_count,
            "checked_shown": checked_shown_count,
            "ready_checked_shown": ready_checked_shown_count,
        },
        "hosts": hosts,
    })
}

impl TerminalApp {
    pub(super) fn copy_host_group_roster(&mut self, id: TabId, offered: &BTreeSet<u64>) {
        if offered.is_empty() || self.host_group_command_target() != Some(id) {
            return;
        }
        let Some(AppTab {
            label,
            page: Page::HostGroup(page),
        }) = self.tabs.get(id)
        else {
            return;
        };
        let targets = self.host_group_targets();
        let mut shown: Vec<_> = page.shown(&targets).collect();
        let current: BTreeSet<_> = shown.iter().map(|target| target.pane).collect();
        let feedback = if current != *offered {
            "The shown roster changed. Copy it again.".into()
        } else {
            page.sort_shown(&mut shown);
            match roster_text(label, &shown, &page.selected, w11term::clipboard::MAX_TEXT)
                .and_then(|text| w11term::clipboard::write(&text))
            {
                Ok(()) => format!(
                    "Copied roster with {} {}",
                    shown.len(),
                    if shown.len() == 1 { "host" } else { "hosts" }
                ),
                Err(error) => format!("Could not copy roster · {error}"),
            }
        };
        if let Some(page) = self.host_group_mut(id) {
            page.roster_copy_feedback = feedback;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roster_keeps_hidden_checks_and_excludes_nonmembers() {
        let target = |pane, name: &str, state| Target {
            pane,
            name: name.into(),
            names: filter::Names::new(name, "lab.example", "ops", 2222),
            profile: Some(name.to_owned()),
            connected: matches!(state, TargetState::Alternate),
            state,
        };
        let targets = [
            target(2, "West", TargetState::Reconnecting(Some(3))),
            target(1, "East", TargetState::Alternate),
            target(3, "Other group", TargetState::Offline),
        ];
        let mut page = HostGroup {
            roster: Some(Box::new(MemberRoster {
                panes: BTreeSet::from([1, 2]),
                ..Default::default()
            })),
            selected: BTreeSet::from([1, 2]),
            ..Default::default()
        };
        page.roster_preferences.order = RosterOrder::Label;
        page.filter.set_status(RosterStatus::Alternate);
        let report = roster_json("Fleet", &page, &targets);
        assert_eq!(report["scope"], "group");
        assert_eq!(
            report["counts"],
            json!({
                "hosts": 2, "shown": 1, "checked": 2, "checked_shown": 1, "ready_checked_shown": 0
            })
        );
        assert_eq!(report["selection_ready"], false);
        assert_eq!(report["hosts"][0]["pane_id"], 1);
        assert_eq!(report["hosts"][0]["shown"], true);
        assert_eq!(report["hosts"][1]["pane_id"], 2);
        assert_eq!(report["hosts"][1]["checked"], true);
        assert_eq!(report["hosts"][1]["shown"], false);
        assert_eq!(report["hosts"][1]["reconnect_attempt"], 3);
        assert_eq!(report["hosts"][1]["address"], "ops@lab.example:2222");
        page.show_window_roster(true);
        let report = roster_json("Fleet", &page, &targets);
        assert_eq!(report["scope"], "window");
        assert_eq!(report["counts"]["hosts"], 3);
    }

    #[test]
    fn copy_text_preserves_visible_order_check_state_and_single_line_fields() {
        let target = |pane, name: &str, state, connected| Target {
            pane,
            name: name.into(),
            names: filter::Names::default(),
            profile: None,
            connected,
            state,
        };
        let west = target(8, "West\t雪", TargetState::Alternate, true);
        let east = target(7, "East\r\nPane 7", TargetState::Offline, false);
        let shown = [&west, &east];
        let selected = BTreeSet::from([7, 9]);
        let expected = "Fleet roster\r\n2 shown · 1 checked\r\n\
                        Unchecked · West 雪 · Connected · Alternate screen\r\n\
                        Checked · East  Pane 7 · Disconnected\r\n";
        assert_eq!(
            roster_text("Fleet", &shown, &selected, expected.len()).unwrap(),
            expected
        );
        assert!(roster_text("Fleet", &shown, &selected, expected.len() - 1).is_err());
    }
}
