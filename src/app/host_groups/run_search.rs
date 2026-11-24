use super::*;
use crate::app::command_palette::{CommandId, Row};

pub(in crate::app) const WINDOW_RUN_SEARCH_LABEL: &str = "Find group runs across tabs…";
pub(in crate::app) const WINDOW_OUTPUT_ALERT_SEARCH_LABEL: &str = "Find armed SSH output alerts…";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowRunScope {
    All,
    ArmedOutputAlerts,
}

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

    fn can_open_window_run(&self, scope: WindowRunScope, run: u64) -> bool {
        self.can_view_run()
            || (scope == WindowRunScope::ArmedOutputAlerts
                && self
                    .live_output
                    .as_ref()
                    .is_some_and(|output| output.submission == run))
    }
}

fn matches(terms: &[&str], run: u64, fields: &[&str]) -> bool {
    if terms.is_empty() {
        return true;
    }
    let fields: Vec<_> = fields.iter().map(|field| field.to_lowercase()).collect();
    terms.iter().all(|term| {
        term.strip_prefix('#')
            .and_then(|number| number.parse::<u64>().ok())
            .map_or_else(
                || fields.iter().any(|field| field.contains(term)),
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

    pub(in crate::app) fn window_group_run_rows(
        &self,
        scope: WindowRunScope,
        query: &str,
    ) -> Vec<Row> {
        let query = query.to_lowercase();
        let terms: Vec<_> = query.split_whitespace().collect();
        let mut rows = Vec::new();
        for entry in self.tabs.headers() {
            let Page::HostGroup(page) = &entry.value.page else {
                continue;
            };
            let group = entry.id;
            let title = self.tab_label(group, false);
            for run in page.submissions.iter().rev() {
                let label = run.content.label();
                let label = match scope {
                    WindowRunScope::All if matches(&terms, run.id, &[&title, label]) => {
                        format!("{title} · Run {} · {label}", run.id)
                    }
                    WindowRunScope::All => continue,
                    WindowRunScope::ArmedOutputAlerts => {
                        let Some(rule) = run.transcript.output_alert_rule() else {
                            continue;
                        };
                        let host = rule.host.as_deref().unwrap_or("All output hosts");
                        if !matches(&terms, run.id, &[&title, label, &rule.query, host]) {
                            continue;
                        }
                        format!(
                            "{title} · Run {} · {host} · {} · {label}",
                            run.id, rule.query
                        )
                    }
                };
                let enabled = page.can_open_window_run(scope, run.id);
                let progress = progress::Summary::capture(&run.results);
                let mut chord = progress.picker_caption();
                if !enabled {
                    chord.push_str(" · View open");
                }
                rows.push((
                    !progress.in_progress(),
                    Row {
                        id: CommandId::WindowGroupRun(scope, group, run.id),
                        label,
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
        scope: WindowRunScope,
        group: TabId,
        run: u64,
        ctx: &ComponentContext<Self>,
    ) {
        let Some(Page::HostGroup(page)) = self.tabs.get(group).map(|tab| &tab.page) else {
            return;
        };
        if !page.can_open_window_run(scope, run)
            || !page.submissions.iter().any(|item| {
                item.id == run
                    && (scope == WindowRunScope::All
                        || item.transcript.output_alert_rule().is_some())
            })
        {
            return;
        }
        if page.can_view_run() {
            self.show_group_run(group, run);
        } else {
            self.tabs.select(group);
        }
        self.sync_selection(ctx);
        self.activate(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_group_and_command_terms_with_an_exact_run_number() {
        assert!(matches(
            &["west", "probe", "#3"],
            3,
            &["fleet west", "PROBE"]
        ));
        assert!(!matches(
            &["west", "probe", "#3"],
            30,
            &["fleet west", "PROBE"]
        ));
        assert!(!matches(&["east", "probe"], 3, &["fleet west", "PROBE"]));
        assert!(matches(&["#pending"], 3, &["fleet west", "probe #pending"]));
        assert!(matches(
            &["east", "deploy", "ready", "#4"],
            4,
            &["Fleet", "DEPLOY", "^READY$", "East"]
        ));
    }
}
