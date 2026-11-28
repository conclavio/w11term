use super::*;
use clipboard_paste::Origin;
use w11term::saved_commands::{Entry, valid_command_text};

pub enum Action {
    Edit(Key, usize, Option<Origin>),
    Text(String),
    Insert(Origin),
    Cancel,
    StartSave,
    Name(String),
    Save,
    CancelSave,
}

pub(super) struct Draft {
    pub(super) page: Key,
    origin: Option<Origin>,
    text: String,
    name: Option<String>,
    status: Option<String>,
}

fn message(action: Action) -> Message {
    Message::CommandReference(super::Action::Example(action))
}

impl Draft {
    fn insertion_target(&self, origin: Option<Origin>) -> Option<Origin> {
        self.origin
            .filter(|&target| Some(target) == origin && catalog::insertable(&self.text))
    }

    pub(super) fn view(
        &self,
        origin: Option<Origin>,
        field: &ElementRef<TextBox>,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let target = self.insertion_target(origin);
        let mut rows = vec![
            KeyedView::new(
                "text",
                TextBox::new()
                    .element_ref(field)
                    .automation_name("Edit command example")
                    .text(&self.text)
                    .on_text_changed(ctx.callback(|text| message(Action::Text(text)))),
            ),
            KeyedView::new(
                "actions",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .is_enabled(target.is_some())
                            .on_click(ctx.callback(move |_| {
                                target.map_or(Message::ControlIdle, |target| {
                                    message(Action::Insert(target))
                                })
                            }))
                            .content("Insert into terminal"),
                        Button::new()
                            .on_click(ctx.callback(|_| message(Action::Cancel)))
                            .content("Cancel edit"),
                    )),
            ),
        ];
        if let Some(name) = &self.name {
            rows.push(KeyedView::new(
                "save-name",
                saved_commands::name_form(
                    name,
                    ctx.callback(|name| message(Action::Name(name))),
                    ctx.callback(|_| message(Action::Save)),
                    ctx.callback(|_| message(Action::CancelSave)),
                ),
            ));
        } else {
            rows.push(KeyedView::new(
                "start-save",
                Button::new()
                    .is_enabled(valid_command_text(&self.text))
                    .on_click(ctx.callback(|_| message(Action::StartSave)))
                    .content("Save as command…"),
            ));
        }
        if let Some(status) = &self.status {
            rows.push(KeyedView::new(
                "save-status",
                TextBlock::new()
                    .text(status)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        StackPanel::new().spacing(6.0).keyed_children(rows)
    }
}

impl TerminalApp {
    pub(super) fn reference_example_action(
        &mut self,
        action: Action,
        auto: Platform,
        origin: Option<Origin>,
        ctx: &ComponentContext<Self>,
    ) {
        let panel = &mut self.command_reference;
        if !panel.open || panel.subpanel_open() {
            return;
        }
        let selected = {
            let results = panel.results(panel.platform.unwrap_or(auto));
            results
                .get(panel.selected)
                .or_else(|| results.first())
                .cloned()
        };
        if let Action::Edit(key, index, target) = action {
            if Some(&key) == selected.as_ref()
                && let Some(example) = panel
                    .library
                    .as_ref()
                    .and_then(|library| library.catalog.page(&key))
                    .and_then(|page| page.examples.get(index))
            {
                panel.draft = Some(Draft {
                    page: key,
                    origin: target.filter(|&target| Some(target) == origin),
                    text: example.command.clone(),
                    name: None,
                    status: None,
                });
                panel.focus = panel.focus.wrapping_add(1);
            }
            return;
        }
        let Some(draft) = panel
            .draft
            .as_mut()
            .filter(|draft| Some(&draft.page) == selected.as_ref())
        else {
            return;
        };
        match action {
            Action::Text(text) => {
                let text = text.chars().take(4096).collect();
                if text != draft.text {
                    draft.text = text;
                    draft.status = None;
                }
            }
            Action::Insert(target) if draft.insertion_target(origin) == Some(target) => {
                let text = draft.text.clone();
                panel.draft = None;
                self.paste_saved_text(target, text, ctx);
            }
            Action::Cancel => panel.draft = None,
            Action::StartSave if draft.name.is_none() && valid_command_text(&draft.text) => {
                draft.name = Some(String::new());
                draft.status = None;
            }
            Action::Name(value) => {
                if let Some(name) = &mut draft.name
                    && *name != value
                {
                    *name = value;
                    draft.status = None;
                }
            }
            Action::Save => {
                let Some(name) = &draft.name else { return };
                let entry = Entry {
                    name: name.clone(),
                    text: draft.text.clone(),
                };
                match saved_commands::create(entry) {
                    Ok(commands) => {
                        self.saved_commands = commands;
                        draft.status = Some(format!("Saved command {}", name.trim()));
                        draft.name = None;
                    }
                    Err(error) => draft.status = Some(format!("{error:#}")),
                }
            }
            Action::CancelSave => {
                draft.name = None;
                draft.status = None;
            }
            _ => {}
        }
    }
}
