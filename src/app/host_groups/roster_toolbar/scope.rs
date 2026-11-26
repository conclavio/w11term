use super::*;

const SHOW_WINDOW: &str = "Show all window SSH panes";
const SHOW_GROUP: &str = "Show group panes only";
const UPDATE: &str = "Update saved group…";
const SAVE_DEFAULTS: &str = "Save roster defaults";

pub(super) fn button(id: TabId, page: &HostGroup, ctx: &ViewContext<TerminalApp>) -> View {
    let Some(roster) = &page.roster else {
        return View::empty();
    };
    let defaults = page.roster_preferences;
    let saved = page.saved.origin().map(|(name, _)| SavedRoster {
        name: name.to_owned(),
        can_update: page.saved.can_update_roster(),
        unsaved_defaults: page
            .saved
            .has_unsaved_roster_defaults(defaults)
            .then_some(defaults),
    });
    View::component::<RosterScopeMenu>(Input {
        sender: ctx.sender(),
        state: State {
            id,
            show_window: roster.show_window,
            saved,
        },
    })
}

struct RosterScopeMenu(LocalSender<Message>);

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

#[derive(Clone, PartialEq)]
struct State {
    id: TabId,
    show_window: bool,
    saved: Option<SavedRoster>,
}

#[derive(Clone, PartialEq)]
struct SavedRoster {
    name: String,
    can_update: bool,
    unsaved_defaults: Option<SshRosterDefaults>,
}

impl State {
    fn switch_scope(&self) -> &str {
        if self.show_window {
            SHOW_GROUP
        } else {
            SHOW_WINDOW
        }
    }

    fn activate(&self, label: &str) -> Message {
        let id = self.id;
        let action = match label {
            label if label == self.switch_scope() => {
                Action::ShowWindowRoster(id, !self.show_window)
            }
            membership::LABEL => Action::Membership(id, membership::Action::Open),
            UPDATE if self.saved.as_ref().is_some_and(|saved| saved.can_update) => Action::Saved(
                id,
                saved::Action::UpdateRoster(saved::RosterUpdateAction::Open),
            ),
            SAVE_DEFAULTS => {
                let Some((group, defaults)) = self.saved.as_ref().and_then(|saved| {
                    saved
                        .unsaved_defaults
                        .map(|defaults| (saved.name.clone(), defaults))
                }) else {
                    return Message::ControlIdle;
                };
                Action::Saved(id, saved::Action::SaveRosterDefaults { group, defaults })
            }
            _ => return Message::ControlIdle,
        };
        Message::HostGroup(action)
    }
}

impl Component for RosterScopeMenu {
    type Input = Input;
    type Message = ();

    fn create(input: &Input, _: &ComponentContext<Self>) -> Self {
        Self(input.sender.clone())
    }

    fn view(&self, input: &Input, _: &mut ViewContext<Self>) -> View {
        let state = input.state.clone();
        let label = if state.show_window {
            "All window panes"
        } else {
            "Group panes"
        };
        let mut items = vec![
            MenuItem::item("scope", state.switch_scope()),
            MenuItem::separator("scope-end"),
            MenuItem::item("membership", membership::LABEL),
        ];
        if let Some(saved) = &state.saved {
            if saved.can_update {
                items.push(MenuItem::item("update", UPDATE));
            }
            items.push(command_menu_item(
                "defaults",
                SAVE_DEFAULTS,
                saved.unsaved_defaults.is_some(),
            ));
        }
        Button::new()
            .automation_name(label)
            .vertical_alignment(VerticalAlignment::Center)
            .content(
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        TextBlock::new()
                            .text(label)
                            .vertical_alignment(VerticalAlignment::Center),
                        FontIcon::new().glyph("\u{e70d}"),
                    ))
                    .tooltip_with(title_tools::below("SSH roster scope")),
            )
            .menu(Menu::new(
                items,
                self.0.callback(move |label: String| state.activate(&label)),
            ))
    }
}
