use super::*;
use clipboard_paste::Origin;
use w11term::saved_commands::{Entry, valid_command_text};

mod template;

pub enum Action {
    Edit(Key, usize, Option<Origin>),
    Text(String),
    Insert(Origin),
    Cancel,
    StartSave,
    Name(String),
    Save,
    CancelSave,
    BeginFill,
    FillText(usize, String),
    FillOption(usize, usize),
    ApplyFill,
    CancelFill,
}

pub(super) struct Draft {
    pub(super) page: Key,
    origin: Option<Origin>,
    text: String,
    name: Option<String>,
    status: Option<String>,
    template: Option<template::Form>,
    filling: Option<template::Form>,
}

fn message(action: Action) -> Message {
    Message::CommandReference(super::Action::Example(action))
}

impl Draft {
    fn insertion_target(&self, origin: Option<Origin>) -> Option<Origin> {
        self.origin.filter(|&target| {
            self.filling.is_none() && Some(target) == origin && catalog::insertable(&self.text)
        })
    }

    pub(super) fn focus_option(&self) -> bool {
        self.filling
            .as_ref()
            .is_some_and(template::Form::options_only)
    }

    pub(super) fn view(
        &self,
        origin: Option<Origin>,
        field: &ElementRef<TextBox>,
        option_field: &ElementRef<ComboBox>,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        if let Some(form) = &self.filling {
            return form.view(field, option_field, ctx);
        }
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
            if self.template.is_some() {
                rows.push(KeyedView::new(
                    "fill",
                    Button::new()
                        .on_click(ctx.callback(|_| message(Action::BeginFill)))
                        .content("Fill placeholders…"),
                ));
            }
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
                    template: template::Form::parse(&example.command),
                    filling: None,
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
            Action::Text(text) if draft.filling.is_none() => {
                let text = text.chars().take(4096).collect();
                if text != draft.text {
                    draft.text = text;
                    draft.status = None;
                    draft.template = None;
                }
            }
            Action::Insert(target) if draft.insertion_target(origin) == Some(target) => {
                let text = draft.text.clone();
                panel.draft = None;
                self.paste_saved_text(target, text, ctx);
            }
            Action::Cancel => panel.draft = None,
            Action::StartSave
                if draft.filling.is_none()
                    && draft.name.is_none()
                    && valid_command_text(&draft.text) =>
            {
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
            Action::BeginFill if draft.filling.is_none() && draft.name.is_none() => {
                if let Some(form) = &draft.template {
                    draft.filling = Some(form.clone());
                    panel.focus = panel.focus.wrapping_add(1);
                }
            }
            Action::FillText(index, text) => {
                if let Some(form) = &mut draft.filling {
                    form.set_text(index, text);
                }
            }
            Action::FillOption(index, choice) => {
                if let Some(form) = &mut draft.filling {
                    form.set_option(index, choice);
                }
            }
            Action::ApplyFill => {
                if let Some(text) = draft.filling.as_ref().and_then(template::Form::preview) {
                    draft.text = text;
                    draft.template = draft.filling.take();
                    draft.status = None;
                    panel.focus = panel.focus.wrapping_add(1);
                }
            }
            Action::CancelFill if draft.filling.is_some() => {
                draft.filling = None;
                panel.focus = panel.focus.wrapping_add(1);
            }
            _ => {}
        }
    }
}
