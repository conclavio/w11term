use super::*;
use text_export::GroupReportFormat;
use w11term::events::GroupCompletionState;

const LOAD: &str = "Load command";
const COMPARE_RESULTS: &str = "Compare results…";
const COMPARE_RUNS: &str = "Compare runs…";
const NOTIFY: &str = "Notify when run finishes";
const CANCEL_NOTICE: &str = "Cancel run notification";
const COPY_ALL: &str = "Copy results";
const COPY_SHOWN: &str = "Copy shown results";

pub(super) fn icon(label: &str, glyph: &str) -> View {
    FontIcon::new()
        .glyph(glyph)
        .tooltip_with(title_tools::below(label))
}

pub(super) fn menu(
    id: TabId,
    submission: &Submission,
    compare_runs: bool,
    shown: Option<Arc<[u64]>>,
    export_busy: bool,
    notice: GroupCompletionState,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    View::component::<RunActions>(Input {
        sender: ctx.sender(),
        state: State {
            id,
            run: submission.id,
            shared_command: submission.content.command().is_some(),
            compare_runs,
            shown,
            export_busy,
            notice,
        },
    })
}

struct RunActions(LocalSender<Message>);

#[derive(Clone)]
struct Input {
    sender: LocalSender<Message>,
    state: State,
}

impl PartialEq for Input {
    fn eq(&self, other: &Self) -> bool {
        self.state == other.state
    }
}

// Keep the menu open across output and progress updates that do not change its commands.
#[derive(Clone, PartialEq)]
struct State {
    id: TabId,
    run: u64,
    shared_command: bool,
    compare_runs: bool,
    shown: Option<Arc<[u64]>>,
    export_busy: bool,
    notice: GroupCompletionState,
}

impl State {
    fn items(&self) -> Vec<MenuItem> {
        let mut items = Vec::new();
        if self.shared_command {
            items.push(MenuItem::item("load", LOAD));
        }
        items.extend([
            MenuItem::item("compare-results", COMPARE_RESULTS),
            command_menu_item("compare-runs", COMPARE_RUNS, self.compare_runs),
        ]);
        match self.notice {
            GroupCompletionState::Available => items.push(MenuItem::item("notice", NOTIFY)),
            GroupCompletionState::Armed => items.push(MenuItem::item("notice", CANCEL_NOTICE)),
            GroupCompletionState::Settled => {}
        }
        items.extend([
            MenuItem::separator("reports"),
            MenuItem::item("copy", COPY_ALL),
            command_menu_item(
                "save-text",
                text_export::GROUP_REPORT_LABEL,
                !self.export_busy,
            ),
            command_menu_item(
                "save-json",
                text_export::GROUP_JSON_LABEL,
                !self.export_busy,
            ),
        ]);
        if self.shown.is_some() {
            items.extend([
                MenuItem::separator("shown"),
                MenuItem::item("copy-shown", COPY_SHOWN),
                command_menu_item(
                    "save-shown-text",
                    text_export::GROUP_SHOWN_REPORT_LABEL,
                    !self.export_busy,
                ),
                command_menu_item(
                    "save-shown-json",
                    text_export::GROUP_SHOWN_JSON_LABEL,
                    !self.export_busy,
                ),
            ]);
        }
        items
    }

    fn activate(&self, label: &str) -> Message {
        let (id, run) = (self.id, self.run);
        let save = |format, shown| Action::SaveResults(id, run, format, shown);
        let action = match label {
            LOAD if self.shared_command => Action::LoadCommand(id, run),
            COMPARE_RESULTS => Action::Compare(id, run, compare::Action::Open),
            COMPARE_RUNS if self.compare_runs => Action::Compare(
                id,
                run,
                compare::Action::RunOverview(compare::RunOverviewAction::Open),
            ),
            NOTIFY if self.notice == GroupCompletionState::Available => {
                Action::NotifyCompletion(id, run, true)
            }
            CANCEL_NOTICE if self.notice == GroupCompletionState::Armed => {
                Action::NotifyCompletion(id, run, false)
            }
            COPY_ALL => Action::CopyResults(id, run, None),
            COPY_SHOWN if self.shown.is_some() => Action::CopyResults(id, run, self.shown.clone()),
            text_export::GROUP_REPORT_LABEL if !self.export_busy => {
                save(GroupReportFormat::Text, None)
            }
            text_export::GROUP_JSON_LABEL if !self.export_busy => {
                save(GroupReportFormat::Json, None)
            }
            text_export::GROUP_SHOWN_REPORT_LABEL if !self.export_busy && self.shown.is_some() => {
                save(GroupReportFormat::Text, self.shown.clone())
            }
            text_export::GROUP_SHOWN_JSON_LABEL if !self.export_busy && self.shown.is_some() => {
                save(GroupReportFormat::Json, self.shown.clone())
            }
            _ => return Message::ControlIdle,
        };
        Message::HostGroup(action)
    }
}

impl Component for RunActions {
    type Input = Input;
    type Message = ();

    fn create(input: &Input, _: &ComponentContext<Self>) -> Self {
        Self(input.sender.clone())
    }

    fn view(&self, input: &Input, _: &mut ViewContext<Self>) -> View {
        let state = input.state.clone();
        title_tools::button("Run actions")
            .content(icon("Run actions", "\u{e712}"))
            .menu(Menu::new(
                state.items(),
                self.0.callback(move |label: String| state.activate(&label)),
            ))
    }
}
