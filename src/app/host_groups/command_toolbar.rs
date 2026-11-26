use super::*;

const RECENT: &str = "Recent commands…";
const SAVED: &str = "Saved commands…";
const SAVE: &str = "Save as command…";
const SEND_ALERT: &str = "Send with alert…";

pub(super) fn composer(
    id: TabId,
    page: &HostGroup,
    offer: CommandOffer,
    send_label: String,
    enabled: bool,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    View::component::<Toolbar>(Input {
        sender: ctx.sender(),
        controls: Controls {
            id,
            editor: page.command_editor.clone(),
            offer,
            send_label,
            enabled,
            can_save: page.command_save.can_save(&page.draft),
            restore_draft: page.previous_draft.is_some(),
        },
    })
}

struct Toolbar(LocalSender<Message>);

#[derive(Clone)]
struct Input {
    sender: LocalSender<Message>,
    controls: Controls,
}

impl PartialEq for Input {
    fn eq(&self, other: &Self) -> bool {
        // The toolbar belongs to the same TerminalApp for its entire lifetime.
        self.controls == other.controls
    }
}

#[derive(Clone, PartialEq)]
struct Controls {
    id: TabId,
    editor: ElementRef<TextBox>,
    offer: CommandOffer,
    send_label: String,
    enabled: bool,
    can_save: bool,
    restore_draft: bool,
}

// Reactor replaces owned flyouts when their view is published. Keep them
// mounted when live output updates without changing the command or recipients.
impl Component for Toolbar {
    type Input = Input;
    type Message = ();

    fn create(input: &Input, _: &ComponentContext<Self>) -> Self {
        Self(input.sender.clone())
    }

    fn view(&self, input: &Input, _: &mut ViewContext<Self>) -> View {
        self.composer(&input.controls)
    }
}

impl Toolbar {
    fn composer(&self, controls: &Controls) -> View {
        let id = controls.id;
        let sender = &self.0;
        let entry = TextBox::new()
            .element_ref(&controls.editor)
            .automation_name("Group command")
            .min_width(0.0)
            .placeholder_text("Enter one shell command")
            .text(controls.offer.text.as_ref())
            .on_text_changed(
                sender.callback(move |text| Message::HostGroup(Action::Edit(id, text))),
            );
        let command = controls.offer.clone();
        let send = Button::new()
            .style(ButtonStyle::Accent)
            .is_enabled(controls.enabled)
            .on_click(
                sender.callback(move |_| Message::HostGroup(Action::Send(id, command.clone()))),
            )
            .content(controls.send_label.as_str());
        let buttons = StackPanel::new()
            .grid_column(1)
            .orientation(Orientation::Horizontal)
            .spacing(4.0)
            .children((
                View::component::<CommandLibrary>(LibraryInput {
                    sender: sender.clone(),
                    id,
                    can_save: controls.can_save,
                }),
                send,
                send_options(id, controls.offer.clone(), controls.enabled, sender),
            ));
        let restore = if controls.restore_draft {
            Button::new()
                .on_click(sender.callback(move |_| Message::HostGroup(Action::RestoreDraft(id))))
                .content("Restore draft")
        } else {
            View::empty()
        };
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .rows([GridLength::Auto, GridLength::Auto])
            .column_spacing(6.0)
            .row_spacing(4.0)
            .children((entry, buttons, Border::new().grid_row(1).content(restore)))
    }
}

struct CommandLibrary(LocalSender<Message>);

#[derive(Clone)]
struct LibraryInput {
    sender: LocalSender<Message>,
    id: TabId,
    can_save: bool,
}

impl PartialEq for LibraryInput {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.can_save == other.can_save
    }
}

// History and saved commands depend only on the tab and whether Save is
// available. The naming form captures the current draft when it opens.
impl Component for CommandLibrary {
    type Input = LibraryInput;
    type Message = ();

    fn create(input: &LibraryInput, _: &ComponentContext<Self>) -> Self {
        Self(input.sender.clone())
    }

    fn view(&self, input: &LibraryInput, _: &mut ViewContext<Self>) -> View {
        let LibraryInput { id, can_save, .. } = *input;
        let sender = &self.0;
        let recall = sender.clone();
        title_tools::button("Command library")
            .key_accelerators(KeyAccelerators::new([KeyAccelerator::new(
                AcceleratorKey::R,
                AcceleratorModifiers::Control,
                move || {
                    recall.send(Message::HostGroup(Action::OpenRecentCommands(id)));
                },
            )]))
            .content(
                FontIcon::new()
                    .glyph("\u{e81c}")
                    .tooltip_with(title_tools::below(
                        "Command library · Ctrl+R for recent commands",
                    )),
            )
            .menu(Menu::new(
                [
                    MenuItem::item("recent", RECENT),
                    MenuItem::item("saved", SAVED),
                    MenuItem::separator("save-current"),
                    command_menu_item("save", SAVE, can_save),
                ],
                sender.callback(move |label: String| {
                    let action = match label.as_str() {
                        RECENT => Action::OpenRecentCommands(id),
                        SAVED => Action::OpenSavedCommands(id),
                        SAVE if can_save => Action::SaveCommand(id, save_command::Action::Begin),
                        _ => return Message::ControlIdle,
                    };
                    Message::HostGroup(action)
                }),
            ))
    }
}

fn send_options(
    id: TabId,
    offer: CommandOffer,
    enabled: bool,
    sender: &LocalSender<Message>,
) -> View {
    let record = if offer.canary.is_some() {
        "Run canary and record…"
    } else {
        "Send and record…"
    };
    title_tools::button("Send options")
        .width(32.0)
        .content(
            FontIcon::new()
                .glyph("\u{e70d}")
                .tooltip_with(title_tools::below("Send options")),
        )
        .menu(Menu::new(
            [
                command_menu_item("record", record, enabled),
                command_menu_item("alert", SEND_ALERT, enabled),
            ],
            sender.callback(move |label: String| {
                let action = match label.as_str() {
                    _ if !enabled => return Message::ControlIdle,
                    label if label == record => {
                        Action::RecordedSend(id, recorded_send::Action::Pick(offer.clone()))
                    }
                    SEND_ALERT => {
                        Action::AlertedSend(id, alerted_send::Action::Open(offer.clone()))
                    }
                    _ => return Message::ControlIdle,
                };
                Message::HostGroup(action)
            }),
        ))
}
