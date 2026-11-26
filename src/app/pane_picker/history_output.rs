use super::*;

pub enum Action {
    Include(Arc<()>, bool),
    Ready(Arc<()>, Option<Report>),
}

pub struct Report(Vec<command_palette::Row>);

#[derive(Default)]
pub(super) struct Search {
    pub(super) included: bool,
    request: Option<Arc<()>>,
    task: Option<ComponentTask>,
    status: Option<&'static str>,
}

impl Search {
    fn cancel(&mut self) {
        self.request = None;
        self.status = None;
        if let Some(task) = self.task.take() {
            task.cancel();
        }
    }
}

impl Drop for Search {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl Picker {
    pub(super) fn uses_history_output(&self) -> bool {
        self.mode == Mode::Commands(command_palette::Search::CommandHistory)
            && self.history_output.included
            && !self.query.trim().is_empty()
    }

    pub(in crate::app) fn history_output_status(&self) -> Option<&'static str> {
        self.history_output.status
    }

    pub(in crate::app) fn history_output_option(&self, ctx: &ViewContext<TerminalApp>) -> View {
        let launch = self.launch.clone();
        CheckBox::new()
            .min_width(0.0)
            .automation_name("Include retained command output")
            .is_checked(self.history_output.included)
            .on_is_checked_changed(ctx.callback(move |included| {
                Message::HistoryOutput(Action::Include(launch.clone(), included))
            }))
            .content("Include command output")
    }
}

impl TerminalApp {
    pub(in crate::app) fn history_output_action(
        &mut self,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        match action {
            Action::Include(launch, included)
                if Arc::ptr_eq(&launch, &picker.launch)
                    && picker.mode == Mode::Commands(command_palette::Search::CommandHistory)
                    && picker.history_output.included != included =>
            {
                picker.history_output.included = included;
                self.search_history_output(ctx);
            }
            Action::Ready(request, rows)
                if picker
                    .history_output
                    .request
                    .as_ref()
                    .is_some_and(|active| Arc::ptr_eq(active, &request)) =>
            {
                picker.history_output.request = None;
                picker.history_output.task = None;
                picker.history_output.status = rows
                    .is_none()
                    .then_some("Could not search command output. Edit the query to try again.");
                picker.commands = rows.map(|report| report.0).unwrap_or_default();
                picker.selected = 0;
            }
            _ => {}
        }
    }

    pub(super) fn search_history_output(&mut self, ctx: &ComponentContext<Self>) {
        let Some(picker) = self.picker.as_ref() else {
            return;
        };
        let query = picker.query.trim().to_owned();
        let search_output = picker.uses_history_output();
        let candidates = self.window_command_captures(&query, search_output);
        let picker = self.picker.as_mut().unwrap();
        picker.history_output.cancel();
        picker.selected = 0;
        if !search_output {
            picker.commands = command_history::window_history_rows(candidates, None, || false);
            return;
        }
        picker.commands.clear();
        let request = Arc::new(());
        let rejected = Message::HistoryOutput(Action::Ready(request.clone(), None));
        picker.history_output.request = Some(request.clone());
        picker.history_output.status = Some("Searching retained command output…");
        picker.history_output.task = Some(ctx.spawn_background_with_rejection(
            move |cancel| {
                let rows = command_history::window_history_rows(candidates, Some(&query), || {
                    cancel.is_cancelled()
                });
                Message::HistoryOutput(Action::Ready(request, Some(Report(rows))))
            },
            rejected,
        ));
    }
}
