use super::*;
use w11term::events::GroupCompletionState;

impl TerminalApp {
    pub(super) fn set_group_run_completion_notice(&self, id: TabId, run: u64, armed: bool) {
        if self.host_group_command_target() == Some(id)
            && let Some(AppTab {
                label,
                page: Page::HostGroup(page),
            }) = self.tabs.get(id)
            && let Some(submission) = page.viewed_submission(run)
        {
            submission
                .transcript
                .set_completion_notice(armed, &self.inbox, id, run, label);
        }
    }

    pub(in crate::app) fn group_run_retained(&self, id: TabId, run: u64) -> bool {
        matches!(self.tabs.get(id).map(|tab| &tab.page), Some(Page::HostGroup(page))
            if page.submissions.iter().any(|submission| submission.id == run))
    }

    pub(in crate::app) fn show_group_run(&mut self, id: TabId, run: u64) {
        if let Some(Page::HostGroup(page)) = self.tabs.get_mut(id).map(|tab| &mut tab.page) {
            page.viewing = run;
            page.preview_command_order = false;
            page.result_filter = progress::Filter::All;
            page.recipient_search.clear();
            page.copy_feedback.clear();
            page.live_output = None;
            page.comparison.close();
            self.tabs.select(id);
        }
    }
}

pub(super) fn button(id: TabId, submission: &Submission, ctx: &ViewContext<TerminalApp>) -> View {
    let (label, armed) = match submission.transcript.completion_state() {
        GroupCompletionState::Available => ("Notify when run finishes", true),
        GroupCompletionState::Armed => ("Cancel run notification", false),
        GroupCompletionState::Settled => return View::empty(),
    };
    let run = submission.id;
    Button::new()
        .on_click(
            ctx.callback(move |_| Message::HostGroup(Action::NotifyCompletion(id, run, armed))),
        )
        .content(label)
}
