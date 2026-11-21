use super::*;
use w11term::shortcuts::{Action, BINDINGS, Shortcuts};

#[derive(Default)]
pub(super) struct KeyboardPage {
    draft: Option<(Action, String)>,
    error: Option<String>,
}
pub enum Edit {
    Show,
    Back,
    Select(Action),
    Text(Action, String),
    Save(Action),
    Clear(Action),
    Default(Action),
    Cancel,
}

impl TerminalApp {
    pub(super) fn edit_keyboard(&mut self, edit: Edit) {
        let Some(AppTab {
            page: Page::Settings(page),
            ..
        }) = self.tabs.active_id().and_then(|id| self.tabs.get_mut(id))
        else {
            return;
        };
        match edit {
            Edit::Show => {
                *page = Some(KeyboardPage::default());
                return;
            }
            Edit::Back => {
                *page = None;
                return;
            }
            _ => {}
        }
        let Some(page) = page.as_mut() else {
            return;
        };
        let mut appearance = self.appearance.read().unwrap().clone();
        let action = match edit {
            Edit::Select(action) => {
                page.draft = Some((
                    action,
                    appearance
                        .shortcuts
                        .get(action.binding())
                        .map_or_else(String::new, String::from),
                ));
                page.error = None;
                return;
            }
            Edit::Text(action, value) => {
                if let Some((current, text)) = &mut page.draft
                    && *current == action
                {
                    *text = value;
                    page.error = None;
                }
                return;
            }
            Edit::Cancel => {
                page.draft = None;
                page.error = None;
                return;
            }
            Edit::Save(action) | Edit::Clear(action) | Edit::Default(action) => action,
            Edit::Show | Edit::Back => unreachable!(),
        };
        let Some((current, input)) = &page.draft else {
            return;
        };
        if *current != action {
            return;
        }
        let result = (|| -> anyhow::Result<_> {
            match edit {
                Edit::Save(_) => appearance.shortcuts.set(action, Some(input.parse()?))?,
                Edit::Clear(_) => appearance.shortcuts.set(action, None)?,
                Edit::Default(_) => appearance.shortcuts.restore(action)?,
                _ => unreachable!(),
            }
            let keymap = appearance.shortcuts.keymap()?;
            appearance.save(&Appearance::path()?)?;
            Ok(keymap)
        })();
        match result {
            Ok(keymap) => {
                self.keyboard.set_shortcuts(keymap);
                *self.appearance.write().unwrap() = appearance;
                self.appearance_error = None;
                page.draft = None;
                page.error = None;
            }
            Err(e) => page.error = Some(format!("{e:#}")),
        }
    }
}
pub(super) fn keyboard_view(
    page: &KeyboardPage,
    shortcuts: &Shortcuts,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    let mut rows = vec![
        KeyedView::new(
            "back",
            Button::new()
                .on_click(ctx.callback(|_| Message::Keyboard(Edit::Back)))
                .content("Back to settings"),
        ),
        KeyedView::new("heading", TextBlock::new().text("Keyboard").font_size(26.0)),
    ];
    if let Some((action, input)) = &page.draft {
        let action = *action;
        let label = action.binding().label;
        rows.extend([
            KeyedView::new("action", TextBlock::new().text(label).font_size(20.0)),
            KeyedView::new(
                "guide",
                TextBlock::new()
                    .text("Enter a shortcut such as Ctrl+Alt+Right or F8.")
                    .text_wrapping(TextWrapping::Wrap),
            ),
            KeyedView::new(
                "input",
                TextBox::new()
                    .automation_name(format!("Shortcut for {label}"))
                    .width(320.0)
                    .text(input)
                    .on_text_changed(
                        ctx.callback(move |text| Message::Keyboard(Edit::Text(action, text))),
                    ),
            ),
            KeyedView::new(
                "buttons",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .on_click(ctx.callback(move |_| Message::Keyboard(Edit::Save(action))))
                            .content("Save shortcut"),
                        Button::new()
                            .on_click(ctx.callback(move |_| Message::Keyboard(Edit::Clear(action))))
                            .content("Clear shortcut"),
                        Button::new()
                            .on_click(
                                ctx.callback(move |_| Message::Keyboard(Edit::Default(action))),
                            )
                            .content("Restore default"),
                        Button::new()
                            .on_click(ctx.callback(|_| Message::Keyboard(Edit::Cancel)))
                            .content("Cancel"),
                    )),
            ),
        ]);
        if let Some(error) = &page.error {
            rows.push(KeyedView::new(
                "error",
                TextBlock::new()
                    .text(error)
                    .text_wrapping(TextWrapping::Wrap)
                    .foreground(Color::rgb(255, 150, 130)),
            ));
        }
    } else {
        for binding in BINDINGS {
            let action = binding.action;
            let current = shortcuts
                .get(binding)
                .map_or_else(|| "Unassigned".to_string(), String::from);
            rows.push(KeyedView::new(
                binding.id,
                Grid::new()
                    .columns([GridLength::STAR, GridLength::Auto, GridLength::Auto])
                    .column_spacing(16.0)
                    .children((
                        TextBlock::new()
                            .text(binding.label)
                            .vertical_alignment(VerticalAlignment::Center),
                        TextBlock::new()
                            .grid_column(1)
                            .text(current)
                            .vertical_alignment(VerticalAlignment::Center),
                        Button::new()
                            .grid_column(2)
                            .automation_name(format!("Edit {}", binding.label))
                            .on_click(
                                ctx.callback(move |_| Message::Keyboard(Edit::Select(action))),
                            )
                            .content("Edit…"),
                    )),
            ));
        }
    }
    ScrollViewer::new().content(
        StackPanel::new()
            .margin(32.0)
            .max_width(660.0)
            .spacing(10.0)
            .keyed_children(rows),
    )
}
