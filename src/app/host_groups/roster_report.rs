use super::*;
use anyhow::{Result, ensure};
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
        let shown: Vec<_> = page.shown(&targets).collect();
        let current: BTreeSet<_> = shown.iter().map(|target| target.pane).collect();
        let feedback = if current != *offered {
            "The shown roster changed. Copy it again.".into()
        } else {
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
