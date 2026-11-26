use super::*;
use command_history::{HistoryOrder, ORDER_OPTIONS, STATUS_OPTIONS, WindowHistoryOptions};
use w11term::control::CommandStatus;
use w11term::events::CommandLink;

type Selection = (u64, CommandLink);

#[derive(Clone)]
pub(in crate::app) struct WindowHistoryReturn {
    origin: Option<TabId>,
    pub(super) query: String,
    pub(super) options: WindowHistoryOptions,
    pub(super) selection: Selection,
}

pub enum Action {
    Edit(Arc<()>, Edit),
    Ready(Arc<()>, Option<Report>),
}

pub enum Edit {
    Status(CommandStatus),
    Order(HistoryOrder),
    IncludeOutput(bool),
}

pub struct Report {
    rows: Vec<command_palette::Row>,
    selection: Option<Selection>,
}

#[derive(Default)]
pub(super) struct Search {
    pub(super) options: WindowHistoryOptions,
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
    fn set_window_history_rows(
        &mut self,
        rows: Vec<command_palette::Row>,
        selection: Option<Selection>,
    ) {
        self.selected = selection
            .and_then(|(pane, link)| {
                rows.iter().position(|row| {
                    matches!(
                        row.id,
                        command_palette::CommandId::OpenWindowCommand(candidate_pane, candidate, _)
                            if candidate_pane == pane && candidate == link
                    )
                })
            })
            .unwrap_or(0);
        self.commands = rows;
    }

    pub(super) fn uses_history_output(&self) -> bool {
        self.mode == Mode::Commands(command_palette::Search::CommandHistory)
            && self.window_history.options.include_output
            && !self.query.trim().is_empty()
    }

    pub(in crate::app) fn window_history_status(&self) -> Option<&'static str> {
        self.window_history.status
    }

    pub(in crate::app) fn window_history_controls(
        &self,
        narrow: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let launch = self.launch.clone();
        let options = self.window_history.options;
        let message = move |edit| Message::WindowHistory(Action::Edit(launch.clone(), edit));
        let order = message.clone();
        let status = message.clone();
        let include = CheckBox::new()
            .grid_column_span(if narrow { 2 } else { 1 })
            .min_width(0.0)
            .automation_name("Include retained command output")
            .is_checked(options.include_output)
            .on_is_checked_changed(
                ctx.callback(move |checked| message(Edit::IncludeOutput(checked))),
            )
            .content("Include command output")
            .tooltip_with(title_tools::below("Include command output · F9"));
        let status = ComboBox::new()
            .grid_row(i32::from(narrow))
            .grid_column(i32::from(!narrow))
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .automation_name("Window command status")
            .items_source(STATUS_OPTIONS.map(|(_, label)| label))
            .selected_index(
                STATUS_OPTIONS
                    .iter()
                    .position(|(value, _)| *value == options.status),
            )
            .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                index
                    .and_then(|index| STATUS_OPTIONS.get(index))
                    .map_or(Message::ControlIdle, |&(value, _)| {
                        status(Edit::Status(value))
                    })
            }))
            .tooltip_with(title_tools::below("Command status · F7"));
        let order = ComboBox::new()
            .grid_row(i32::from(narrow))
            .grid_column(if narrow { 1 } else { 2 })
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .automation_name("Window command order")
            .items_source(ORDER_OPTIONS.map(|(_, label)| label))
            .selected_index(
                ORDER_OPTIONS
                    .iter()
                    .position(|(value, _)| *value == options.order),
            )
            .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                index
                    .and_then(|index| ORDER_OPTIONS.get(index))
                    .map_or(Message::ControlIdle, |&(value, _)| {
                        order(Edit::Order(value))
                    })
            }))
            .tooltip_with(title_tools::below("Command order · F8"));
        Grid::new()
            .columns(if narrow {
                vec![GridLength::STAR; 2]
            } else {
                vec![GridLength::STAR, GridLength::Auto, GridLength::Auto]
            })
            .rows([GridLength::Auto, GridLength::Auto])
            .column_spacing(8.0)
            .row_spacing(if narrow { 4.0 } else { 0.0 })
            .children((include, status, order))
    }
}

impl TerminalApp {
    pub(in crate::app) fn window_history_return(
        &self,
        selection: Selection,
    ) -> Option<WindowHistoryReturn> {
        let picker = self.picker.as_ref().filter(|picker| {
            picker.mode == Mode::Commands(command_palette::Search::CommandHistory)
        })?;
        Some(WindowHistoryReturn {
            origin: self.tabs.active_id(),
            query: picker.query.clone(),
            options: picker.window_history.options,
            selection,
        })
    }

    pub(in crate::app) fn restore_window_history(
        &mut self,
        saved: WindowHistoryReturn,
        ctx: &ComponentContext<Self>,
    ) {
        if self.picker.is_some() {
            return;
        }
        if let Some(origin) = saved.origin
            && self.tabs.select(origin)
        {
            self.sync_selection(ctx);
            self.activate(ctx);
        }
        self.start_picker_from(
            Mode::Commands(command_palette::Search::CommandHistory),
            Some(saved),
            ctx,
        );
    }

    pub(in crate::app) fn window_history_action(
        &mut self,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        match action {
            Action::Edit(launch, edit)
                if Arc::ptr_eq(&launch, &picker.launch)
                    && picker.mode == Mode::Commands(command_palette::Search::CommandHistory) =>
            {
                let previous = picker.window_history.options;
                let options = &mut picker.window_history.options;
                match edit {
                    Edit::Status(status) => options.status = status,
                    Edit::Order(order) => options.order = order,
                    Edit::IncludeOutput(include) => options.include_output = include,
                }
                if previous != *options {
                    self.search_window_history(None, ctx);
                }
            }
            Action::Ready(request, rows)
                if picker
                    .window_history
                    .request
                    .as_ref()
                    .is_some_and(|active| Arc::ptr_eq(active, &request)) =>
            {
                picker.window_history.request = None;
                picker.window_history.task = None;
                picker.window_history.status = rows
                    .is_none()
                    .then_some("Could not search command output. Edit the query to try again.");
                if let Some(report) = rows {
                    picker.set_window_history_rows(report.rows, report.selection);
                }
            }
            _ => {}
        }
    }

    pub(in crate::app) fn cycle_window_history(
        &mut self,
        choice: crate::input::HistoryPickerOption,
        ctx: &ComponentContext<Self>,
    ) {
        use crate::input::HistoryPickerOption;
        let Some(picker) = self.picker.as_ref() else {
            return;
        };
        let options = picker.window_history.options;
        let edit = match choice {
            HistoryPickerOption::Status => {
                let index = STATUS_OPTIONS
                    .iter()
                    .position(|(value, _)| *value == options.status)
                    .unwrap_or(0);
                Edit::Status(STATUS_OPTIONS[(index + 1) % STATUS_OPTIONS.len()].0)
            }
            HistoryPickerOption::Order => Edit::Order(match options.order {
                HistoryOrder::Newest => HistoryOrder::Slowest,
                HistoryOrder::Slowest => HistoryOrder::Newest,
            }),
            HistoryPickerOption::Output => Edit::IncludeOutput(!options.include_output),
        };
        self.window_history_action(Action::Edit(picker.launch.clone(), edit), ctx);
    }

    pub(super) fn search_window_history(
        &mut self,
        selection: Option<Selection>,
        ctx: &ComponentContext<Self>,
    ) {
        let Some(picker) = self.picker.as_ref() else {
            return;
        };
        let query = picker.query.trim().to_owned();
        let search_output = picker.uses_history_output();
        let options = picker.window_history.options;
        let candidates = self.window_command_captures(&query, options);
        let picker = self.picker.as_mut().unwrap();
        picker.window_history.cancel();
        picker.selected = 0;
        if !search_output {
            let rows =
                command_history::window_history_rows(candidates, None, options.order, || false);
            picker.set_window_history_rows(rows, selection);
            return;
        }
        picker.commands.clear();
        let request = Arc::new(());
        let rejected = Message::WindowHistory(Action::Ready(request.clone(), None));
        picker.window_history.request = Some(request.clone());
        picker.window_history.status = Some("Searching retained command output…");
        picker.window_history.task = Some(ctx.spawn_background_with_rejection(
            move |cancel| {
                let rows = command_history::window_history_rows(
                    candidates,
                    Some(&query),
                    options.order,
                    || cancel.is_cancelled(),
                );
                Message::WindowHistory(Action::Ready(request, Some(Report { rows, selection })))
            },
            rejected,
        ));
    }
}
