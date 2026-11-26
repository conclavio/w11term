use super::*;

const NEW: &str = "New group";
const EDIT: &str = "Edit group";
const DUPLICATE: &str = "Duplicate group…";
const OPEN_TAB: &str = "Open in new tab";
const OPEN_WINDOW: &str = "Open group in new window";
const ADD_PROFILES: &str = "Add saved profiles…";
const EXPORT_HOSTS: &str = "Export saved group hosts…";
const IMPORT_DEFINITION: &str = "Import group definition…";
const EXPORT_DEFINITION: &str = "Export group definition…";

#[derive(Clone, PartialEq)]
struct Group {
    name: String,
    members: Vec<String>,
}

impl Group {
    fn open(&self, new_tab: bool) -> Action {
        Action::Open {
            name: self.name.clone(),
            members: self.members.clone(),
            new_tab,
        }
    }
}

impl TerminalApp {
    pub(in crate::app::host_groups) fn saved_host_group_toolbar(
        &self,
        id: TabId,
        page: &HostGroup,
        ctx: &ViewContext<Self>,
    ) -> View {
        let catalog = &self.catalog.ssh_host_groups;
        let group = page.saved.name(&self.catalog).and_then(|selected| {
            catalog.get(selected).map(|members| Group {
                name: selected.to_owned(),
                members: members.clone(),
            })
        });
        let action = move |action| Message::HostGroup(HostAction::Saved(id, action));
        let selector: View = group.as_ref().map_or_else(View::empty, |group| {
            let names: Vec<_> = catalog.keys().cloned().collect();
            let current = names.iter().position(|name| *name == group.name);
            ComboBox::new()
                .automation_name("Saved SSH groups")
                .min_width(0.0)
                .max_width(240.0)
                .horizontal_alignment(HorizontalAlignment::Left)
                .items_source(names.clone())
                .selected_index(current)
                .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                    index
                        .filter(|index| Some(*index) != current)
                        .and_then(|index| names.get(index))
                        .map_or(Message::ControlIdle, |name| {
                            action(Action::Select(name.clone()))
                        })
                }))
                .into()
        });
        let primary = if let Some(group) = &group {
            let open = group.clone();
            Button::new()
                .is_enabled(!group.members.is_empty())
                .on_click(ctx.callback(move |_| action(open.open(false))))
                .content("Open group")
        } else {
            Button::new()
                .on_click(ctx.callback(move |_| action(Action::New)))
                .content(NEW)
        };
        let members = group.as_ref().map(|group| {
            if group.members.is_empty() {
                "This group has no SSH profiles.".into()
            } else {
                group.members.join(", ")
            }
        });
        let menu = View::component::<SavedGroupMenu>(MenuInput {
            sender: ctx.sender(),
            state: MenuState {
                id,
                group,
                export_busy: self.export.pending.is_some(),
            },
        });
        let actions = Grid::new()
            .columns([GridLength::Auto, GridLength::Auto, GridLength::Auto])
            .column_spacing(6.0)
            .children((
                selector,
                Border::new().grid_column(1).content(primary),
                Border::new().grid_column(2).content(menu),
            ));

        let mut details = vec![KeyedView::new("actions", actions)];
        if let Some(members) = members {
            details.push(KeyedView::new(
                "members",
                TextBlock::new()
                    .text(members)
                    .text_trimming(TextTrimming::CharacterEllipsis),
            ));
        }
        for (key, text) in [
            ("feedback", &page.saved.feedback),
            ("import-feedback", &page.import.feedback),
        ] {
            if !text.is_empty() {
                details.push(KeyedView::new(
                    key,
                    TextBlock::new()
                        .text(text)
                        .text_wrapping(TextWrapping::Wrap),
                ));
            }
        }
        StackPanel::new()
            .grid_row(1)
            .spacing(4.0)
            .keyed_children(details)
    }
}

struct SavedGroupMenu(LocalSender<Message>);

#[derive(Clone)]
struct MenuInput {
    sender: LocalSender<Message>,
    state: MenuState,
}

impl PartialEq for MenuInput {
    fn eq(&self, other: &Self) -> bool {
        self.state == other.state
    }
}

#[derive(Clone, PartialEq)]
struct MenuState {
    id: TabId,
    group: Option<Group>,
    export_busy: bool,
}

impl MenuState {
    fn items(&self) -> Vec<MenuItem> {
        let mut items = Vec::new();
        if let Some(group) = &self.group {
            let has_members = !group.members.is_empty();
            items.extend([
                command_menu_item("open-tab", OPEN_TAB, has_members),
                command_menu_item("open-window", OPEN_WINDOW, has_members),
                command_menu_item("review", reuse::LABEL, has_members),
                MenuItem::separator("launch-end"),
                MenuItem::item("edit", EDIT),
                MenuItem::item("duplicate", DUPLICATE),
                MenuItem::item("new", NEW),
                MenuItem::separator("edit-end"),
            ]);
        }
        items.extend([
            MenuItem::item("add-profiles", ADD_PROFILES),
            MenuItem::item("import-hosts", import::LABEL),
            MenuItem::item("import-definition", IMPORT_DEFINITION),
        ]);
        if let Some(group) = &self.group {
            items.extend([
                MenuItem::separator("import-end"),
                command_menu_item("export-hosts", EXPORT_HOSTS, !group.members.is_empty()),
                command_menu_item("export-definition", EXPORT_DEFINITION, !self.export_busy),
            ]);
        }
        items
    }

    fn activate(&self, label: &str) -> Message {
        let id = self.id;
        let action = match label {
            ADD_PROFILES => HostAction::Import(id, import::Action::PickSaved),
            import::LABEL => HostAction::Import(id, import::Action::Begin),
            IMPORT_DEFINITION => {
                HostAction::Saved(id, Action::Definition(definition::Action::Open))
            }
            _ => {
                let Some(group) = &self.group else {
                    return Message::ControlIdle;
                };
                let has_members = !group.members.is_empty();
                match label {
                    NEW => HostAction::Saved(id, Action::New),
                    EDIT => HostAction::Saved(id, Action::Edit(group.name.clone())),
                    DUPLICATE => HostAction::Saved(
                        id,
                        Action::Duplicate(duplicate::Action::Open(group.name.clone())),
                    ),
                    OPEN_TAB if has_members => HostAction::Saved(id, group.open(true)),
                    OPEN_WINDOW if has_members => HostAction::Saved(
                        id,
                        Action::OpenWindow {
                            name: group.name.clone(),
                            members: group.members.clone(),
                        },
                    ),
                    reuse::LABEL if has_members => HostAction::Reuse(
                        id,
                        reuse::Action::Open {
                            name: group.name.clone(),
                            profiles: group.members.clone(),
                        },
                    ),
                    EXPORT_HOSTS if has_members => HostAction::HostListExport(
                        id,
                        host_list_export::Action::OpenSaved {
                            name: group.name.clone(),
                            members: group.members.clone(),
                        },
                    ),
                    EXPORT_DEFINITION if !self.export_busy => HostAction::Saved(
                        id,
                        Action::Definition(definition::Action::Export(group.name.clone())),
                    ),
                    _ => return Message::ControlIdle,
                }
            }
        };
        Message::HostGroup(action)
    }
}

impl Component for SavedGroupMenu {
    type Input = MenuInput;
    type Message = ();

    fn create(input: &MenuInput, _: &ComponentContext<Self>) -> Self {
        Self(input.sender.clone())
    }

    fn view(&self, input: &MenuInput, _: &mut ViewContext<Self>) -> View {
        let state = input.state.clone();
        title_tools::button("Saved group actions")
            .content(
                FontIcon::new()
                    .glyph("\u{e712}")
                    .tooltip_with(title_tools::below("Saved group actions")),
            )
            .menu(Menu::new(
                state.items(),
                self.0.callback(move |label: String| state.activate(&label)),
            ))
    }
}
