use super::*;
use crate::app::command_palette::{CommandId, Row};

pub(in crate::app) const WINDOW_RUN_SEARCH_LABEL: &str = "Find group runs across tabs…";

impl HostGroup {
    pub(super) fn can_view_run(&self) -> bool {
        !self.saved.is_editing()
            && self.live_output.is_none()
            && !self.comparison.is_open()
            && !self.import.is_open()
            && !self.host_list_export.is_open()
            && !self.close_ended.is_reviewing()
            && !self.disconnect.is_reviewing()
            && !self.edit_membership_open()
            && !self.saved.reuse.is_open()
            && !self.prompt_setup.is_open()
            && !self.transfer.is_open()
            && !self.environment.is_open()
    }
}

fn matches(terms: &[&str], title: &str, label: &str, run: u64) -> bool {
    if terms.is_empty() {
        return true;
    }
    let label = label.to_lowercase();
    terms.iter().all(|term| {
        term.strip_prefix('#')
            .and_then(|number| number.parse::<u64>().ok())
            .map_or_else(
                || title.contains(term) || label.contains(term),
                |id| run == id,
            )
    })
}

impl TerminalApp {
    pub(in crate::app) fn has_window_group_runs(&self) -> bool {
        self.tabs.entries().iter().any(|entry| {
            matches!(&entry.value.page, Page::HostGroup(page) if !page.submissions.is_empty())
        })
    }

    pub(in crate::app) fn window_group_run_rows(&self, query: &str) -> Vec<Row> {
        let query = query.to_lowercase();
        let terms: Vec<_> = query.split_whitespace().collect();
        let mut rows = Vec::new();
        for entry in self.tabs.headers() {
            let Page::HostGroup(page) = &entry.value.page else {
                continue;
            };
            let group = entry.id;
            let title = self.tab_label(group, false);
            let terms_title = title.to_lowercase();
            let enabled = page.can_view_run();
            for run in page.submissions.iter().rev() {
                let label = run.content.label();
                if !matches(&terms, &terms_title, label, run.id) {
                    continue;
                }
                let progress = progress::Summary::capture(&run.results);
                let mut chord = progress.picker_caption();
                if !enabled {
                    chord.push_str(" · View open");
                }
                rows.push((
                    !progress.in_progress(),
                    Row {
                        id: CommandId::WindowGroupRun(group, run.id),
                        label: format!("{title} · Run {} · {label}", run.id),
                        chord,
                        enabled,
                    },
                ));
            }
        }
        // Stable ordering keeps results in tab order, newest first within each tab.
        rows.sort_by_key(|(settled, _)| *settled);
        rows.into_iter().map(|(_, row)| row).collect()
    }

    pub(in crate::app) fn open_window_group_run(
        &mut self,
        group: TabId,
        run: u64,
        ctx: &ComponentContext<Self>,
    ) {
        if !matches!(self.tabs.get(group).map(|tab| &tab.page), Some(Page::HostGroup(page))
            if page.can_view_run() && page.submissions.iter().any(|item| item.id == run))
        {
            return;
        }
        self.show_group_run(group, run);
        self.sync_selection(ctx);
        self.activate(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_group_and_command_terms_with_an_exact_run_number() {
        assert!(matches(&["west", "probe", "#3"], "fleet west", "PROBE", 3));
        assert!(!matches(
            &["west", "probe", "#3"],
            "fleet west",
            "PROBE",
            30
        ));
        assert!(!matches(&["east", "probe"], "fleet west", "PROBE", 3));
        assert!(matches(&["#pending"], "fleet west", "probe #pending", 3));
    }
}
