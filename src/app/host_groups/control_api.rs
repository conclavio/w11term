use super::*;
use serde_json::{Value, json};

impl TerminalApp {
    pub(in crate::app) fn control_ssh_groups(&self) -> Value {
        let groups: Vec<_> = self
            .tabs
            .entries()
            .iter()
            .filter_map(|entry| {
                let Page::HostGroup(page) = &entry.value.page else {
                    return None;
                };
                let runs: Vec<_> = page.submissions.iter().map(report::summarize_run).collect();
                Some(json!({
                    "tab_id": entry.id,
                    "label": entry.value.label,
                    "runs": runs,
                }))
            })
            .collect();
        json!(groups)
    }

    pub(in crate::app) fn control_ssh_group_run(
        &self,
        tab: TabId,
        run: u64,
    ) -> Result<Value, String> {
        let Some(AppTab {
            page: Page::HostGroup(page),
            ..
        }) = self.tabs.get(tab)
        else {
            return Err(format!("unknown SSH group tab {tab} in this window"));
        };
        let submission = page
            .submissions
            .iter()
            .find(|submission| submission.id == run)
            .ok_or_else(|| format!("SSH group run {run} is not retained in tab {tab}"))?;
        Ok(report::describe_run(submission))
    }
}
