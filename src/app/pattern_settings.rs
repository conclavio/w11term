use super::*;
use w11term::hints::{MAX_PATTERNS, Patterns};

#[derive(Default)]
pub(super) struct PatternPage {
    draft: Option<Draft>,
    error: Option<String>,
}

#[derive(Default)]
struct Draft {
    index: Option<usize>,
    name: String,
    expression: String,
}

pub enum Edit {
    Show,
    Back,
    Add,
    Select(usize),
    Name(String),
    Expression(String),
    Save,
    Remove,
    Cancel,
}

impl TerminalApp {
    pub(super) fn edit_patterns(&mut self, edit: Edit) {
        let Some(AppTab {
            page: Page::Settings(current),
            ..
        }) = self.tabs.active_id().and_then(|id| self.tabs.get_mut(id))
        else {
            return;
        };
        if matches!(edit, Edit::Show) {
            *current = Some(SettingsEditor::Patterns(PatternPage::default()));
            return;
        }
        let Some(SettingsEditor::Patterns(page)) = current else {
            return;
        };
        page.error = None;
        match edit {
            Edit::Back => *current = None,
            Edit::Cancel => page.draft = None,
            Edit::Add => page.draft = Some(Draft::default()),
            Edit::Select(index) => {
                if let Some(rule) = self
                    .appearance
                    .read()
                    .unwrap()
                    .hint_patterns
                    .rules()
                    .get(index)
                {
                    page.draft = Some(Draft {
                        index: Some(index),
                        name: rule.name.clone(),
                        expression: rule.regex.as_str().into(),
                    });
                }
            }
            Edit::Name(name) => {
                if let Some(draft) = &mut page.draft {
                    draft.name = name;
                }
            }
            Edit::Expression(expression) => {
                if let Some(draft) = &mut page.draft {
                    draft.expression = expression;
                }
            }
            Edit::Save | Edit::Remove => {
                let Some(draft) = &page.draft else { return };
                let mut settings = self.appearance.read().unwrap().clone();
                let result = (|| -> anyhow::Result<()> {
                    if matches!(edit, Edit::Remove) {
                        if let Some(index) = draft.index {
                            settings.hint_patterns.remove(index);
                        }
                    } else {
                        settings
                            .hint_patterns
                            .save(draft.index, &draft.name, &draft.expression)?;
                    }
                    settings.save(&Appearance::path()?)
                })();
                match result {
                    Ok(()) => {
                        *self.appearance.write().unwrap() = settings;
                        self.appearance_error = None;
                        page.draft = None;
                    }
                    Err(error) => page.error = Some(format!("{error:#}")),
                }
            }
            Edit::Show => unreachable!(),
        }
    }
}

pub(super) fn view(
    page: &PatternPage,
    patterns: &Patterns,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    let mut rows = vec![
        KeyedView::new(
            "back",
            Button::new()
                .on_click(ctx.callback(|_| Message::Patterns(Edit::Back)))
                .content("Back to settings"),
        ),
        KeyedView::new(
            "heading",
            TextBlock::new().font_size(26.0).text("Hint patterns"),
        ),
    ];
    if let Some(draft) = &page.draft {
        rows.extend([
            KeyedView::new("name-label", TextBlock::new().text("Pattern name")),
            KeyedView::new(
                "name",
                TextBox::new()
                    .automation_name("Pattern name")
                    .placeholder_text("Issue number")
                    .text(&draft.name)
                    .on_text_changed(ctx.callback(|s| Message::Patterns(Edit::Name(s)))),
            ),
            KeyedView::new(
                "expression-label",
                TextBlock::new().text("Regular expression"),
            ),
            KeyedView::new(
                "expression",
                TextBox::new()
                    .automation_name("Regular expression")
                    .placeholder_text(r"\bOPS-[0-9]+\b")
                    .text(&draft.expression)
                    .on_text_changed(ctx.callback(|s| Message::Patterns(Edit::Expression(s)))),
            ),
            KeyedView::new(
                "actions",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .is_enabled(
                                !draft.name.trim().is_empty()
                                    && !draft.expression.is_empty()
                                    && draft
                                        .index
                                        .and_then(|i| patterns.rules().get(i))
                                        .is_none_or(|r| {
                                            r.name != draft.name.trim()
                                                || r.regex.as_str() != draft.expression
                                        }),
                            )
                            .on_click(ctx.callback(|_| Message::Patterns(Edit::Save)))
                            .content("Save pattern"),
                        Button::new()
                            .on_click(ctx.callback(|_| Message::Patterns(Edit::Cancel)))
                            .content("Cancel"),
                    )),
            ),
        ]);
        if draft.index.is_some() {
            rows.push(KeyedView::new(
                "remove",
                Button::new()
                    .on_click(ctx.callback(|_| Message::Patterns(Edit::Remove)))
                    .content("Remove pattern"),
            ));
        }
        if let Some(error) = &page.error {
            rows.push(KeyedView::new(
                "error",
                TextBlock::new()
                    .text(error)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
    } else {
        rows.push(KeyedView::new("guide", TextBlock::new().text("Keyboard hints label matching text in the focused pane. Select a label and press Enter to copy the match.").text_wrapping(TextWrapping::Wrap)));
        rows.push(KeyedView::new(
            "add",
            Button::new()
                .is_enabled(patterns.rules().len() < MAX_PATTERNS)
                .on_click(ctx.callback(|_| Message::Patterns(Edit::Add)))
                .content("Add pattern"),
        ));
        for (i, rule) in patterns.rules().iter().enumerate() {
            rows.push(KeyedView::new(
                format!("rule-{i}"),
                Grid::new()
                    .columns([GridLength::STAR, GridLength::Auto])
                    .column_spacing(16.0)
                    .children((
                        StackPanel::new().children((
                            TextBlock::new().text(&rule.name),
                            TextBlock::new()
                                .text(rule.regex.as_str())
                                .text_wrapping(TextWrapping::Wrap),
                        )),
                        Button::new()
                            .grid_column(1)
                            .automation_name(format!("Edit pattern {}", rule.name))
                            .on_click(ctx.callback(move |_| Message::Patterns(Edit::Select(i))))
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
