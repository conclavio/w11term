use super::*;

const UPLOAD: &str = "Upload files…";
const DOWNLOAD: &str = "Download files…";
const EXPORT_VARIABLES: &str = "Export group variables…";
const COPY: &str = "Copy shown roster";
const RECIPIENT_SETS: &str = "Recipient sets…";

pub(super) fn button(
    id: TabId,
    page: &HostGroup,
    selected: &[Target],
    shown: BTreeSet<u64>,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    View::component::<RosterMenu>(Input {
        sender: ctx.sender(),
        scope: Scope {
            id,
            shown,
            checked: selected.iter().map(|target| target.pane).collect(),
            upload: transfer::Direction::Upload.available(selected),
            download: transfer::Direction::Download.available(selected),
            export_variables: environment::available(page, selected),
            recipient_sets: page.saved.origin().is_some(),
        },
    })
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
    upload: bool,
    download: bool,
    export_variables: bool,
    recipient_sets: bool,
}

impl Scope {
    fn activate(&self, label: &str) -> Message {
        let id = self.id;
        let action = match label {
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
