use super::*;

mod scope;

const UPLOAD: &str = "Upload files…";
const DOWNLOAD: &str = "Download files…";
const EXPORT_VARIABLES: &str = "Export group variables…";
const COPY: &str = "Copy shown roster";
const RECIPIENT_SETS: &str = "Recipient sets…";
const SELECT_READY: &str = "Select only ready shown";
const SAVE_SELECTION: &str = "Save selection as group…";

pub(super) fn view(
    id: TabId,
    page: &HostGroup,
    selected: &[Target],
    shown: &[&Target],
    ctx: &ViewContext<TerminalApp>,
) -> View {
    let shown_ids: BTreeSet<_> = shown.iter().map(|target| target.pane).collect();
    let bulk = |label, selection, enabled| {
        let panes = shown_ids.clone();
        Button::new()
            .is_enabled(enabled)
            .on_click(ctx.callback(move |_| {
                Message::HostGroup(Action::SelectShown(id, panes.clone(), selection))
            }))
            .content(label)
    };
    let selection = StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(6.0)
        .children((
            bulk(
                "Select shown",
                ShownSelection::All,
                selected.len() < shown.len(),
            ),
            bulk("Deselect shown", ShownSelection::None, !selected.is_empty()),
        ));
    let actions = View::component::<RosterMenu>(Input {
        sender: ctx.sender(),
        scope: Scope {
            id,
            select_ready: shown
                .iter()
                .any(|target| page.selected.contains(&target.pane) != target.state.is_ready()),
            shown: shown_ids,
            checked: selected.iter().map(|target| target.pane).collect(),
            upload: transfer::Direction::Upload.available(selected),
            download: transfer::Direction::Download.available(selected),
            export_variables: environment::available(page, selected),
            recipient_sets: page.saved.origin().is_some(),
        },
    });
    let display = View::component::<RosterDisplay>(DisplayInput {
        sender: ctx.sender(),
        id,
        last_output: page.roster_preferences.show_last_output,
        terminal_line: page.roster_preferences.show_terminal_line,
    });
    Grid::new()
        .columns([GridLength::STAR, GridLength::Auto])
        .column_spacing(8.0)
        .children((
            selection,
            StackPanel::new()
                .grid_column(1)
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .children((scope::button(id, page, ctx), display, actions)),
        ))
}

struct RosterMenu(LocalSender<Message>);

#[derive(Clone)]
struct Input {
    sender: LocalSender<Message>,
    scope: Scope,
}

impl PartialEq for Input {
    fn eq(&self, other: &Self) -> bool {
        self.scope == other.scope
    }
}

// The native menu can stay open across unrelated updates such as new shell output.
#[derive(Clone, PartialEq)]
struct Scope {
    id: TabId,
    shown: BTreeSet<u64>,
    checked: BTreeSet<u64>,
    select_ready: bool,
    upload: bool,
    download: bool,
    export_variables: bool,
    recipient_sets: bool,
}

impl Scope {
    fn activate(&self, label: &str) -> Message {
        let id = self.id;
        let action = match label {
            SELECT_READY if self.select_ready => {
                Action::SelectShown(id, self.shown.clone(), ShownSelection::Ready)
            }
            SAVE_SELECTION if !self.checked.is_empty() => {
                Action::Saved(id, saved::Action::NewFromSelection(self.checked.clone()))
            }
            UPLOAD if self.upload => {
                Action::Transfer(id, transfer::Action::Open(transfer::Direction::Upload))
            }
            DOWNLOAD if self.download => {
                Action::Transfer(id, transfer::Action::Open(transfer::Direction::Download))
            }
            EXPORT_VARIABLES if self.export_variables => {
                Action::Environment(id, environment::Action::Open)
            }
            host_list_export::LABEL if !self.checked.is_empty() => {
                Action::HostListExport(id, host_list_export::Action::Open(self.checked.clone()))
            }
            COPY if !self.shown.is_empty() => Action::CopyRoster(id, self.shown.clone()),
            RECIPIENT_SETS if self.recipient_sets => {
                Action::RecipientSets(id, recipient_sets::Action::Open)
            }
            _ => return Message::ControlIdle,
        };
        Message::HostGroup(action)
    }
}

impl Component for RosterMenu {
    type Input = Input;
    type Message = ();

    fn create(input: &Input, _: &ComponentContext<Self>) -> Self {
        Self(input.sender.clone())
    }

    fn view(&self, input: &Input, _: &mut ViewContext<Self>) -> View {
        let scope = input.scope.clone();
        let mut items = vec![
            command_menu_item("ready", SELECT_READY, scope.select_ready),
            command_menu_item("save", SAVE_SELECTION, !scope.checked.is_empty()),
            MenuItem::separator("selection-end"),
            command_menu_item("upload", UPLOAD, scope.upload),
            command_menu_item("download", DOWNLOAD, scope.download),
            MenuItem::separator("transfer-end"),
            command_menu_item("variables", EXPORT_VARIABLES, scope.export_variables),
            command_menu_item("hosts", host_list_export::LABEL, !scope.checked.is_empty()),
            command_menu_item("copy", COPY, !scope.shown.is_empty()),
        ];
        if scope.recipient_sets {
            items.extend([
                MenuItem::separator("roster-report-end"),
                MenuItem::item("recipients", RECIPIENT_SETS),
            ]);
        }
        title_tools::button("Roster actions")
            .content(
                FontIcon::new()
                    .glyph("\u{e712}")
                    .tooltip_with(title_tools::below("Roster actions")),
            )
            .menu(Menu::new(
                items,
                self.0.callback(move |label: String| scope.activate(&label)),
            ))
    }
}

struct RosterDisplay(LocalSender<Message>);

#[derive(Clone)]
struct DisplayInput {
    sender: LocalSender<Message>,
    id: TabId,
    last_output: bool,
    terminal_line: bool,
}

impl PartialEq for DisplayInput {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.last_output == other.last_output
            && self.terminal_line == other.terminal_line
    }
}

impl Component for RosterDisplay {
    type Input = DisplayInput;
    type Message = ();

    fn create(input: &DisplayInput, _: &ComponentContext<Self>) -> Self {
        Self(input.sender.clone())
    }

    fn view(&self, input: &DisplayInput, _: &mut ViewContext<Self>) -> View {
        let DisplayInput {
            id,
            last_output,
            terminal_line,
            ..
        } = *input;
        let output = if last_output {
            "Hide last command output"
        } else {
            "Show last command output"
        };
        let line = if terminal_line {
            "Hide current terminal line"
        } else {
            "Show current terminal line"
        };
        title_tools::button("Roster display")
            .content(
                FontIcon::new()
                    .glyph("\u{e890}")
                    .tooltip_with(title_tools::below("Roster display")),
            )
            .menu(Menu::new(
                [
                    MenuItem::item("output", output),
                    MenuItem::item("line", line),
                ],
                self.0.callback(move |label: String| {
                    let action = match label.as_str() {
                        label if label == output => Action::ShowLastOutput(id, !last_output),
                        label if label == line => Action::ShowTerminalLine(id, !terminal_line),
                        _ => return Message::ControlIdle,
                    };
                    Message::HostGroup(action)
                }),
            ))
    }
}
