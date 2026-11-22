use super::*;
use w11term::interaction::Interaction;

pub(super) struct Editor {
    draft: String,
    error: Option<String>,
}

pub enum Edit {
    Show,
    Change(String),
    RestoreDefault,
    Save,
    Cancel,
}

impl TerminalApp {
    pub(super) fn edit_word_delimiters(&mut self, action: Edit) {
        let Some(AppTab {
            page: Page::Settings(editor),
            ..
        }) = self.tabs.active_id().and_then(|id| self.tabs.get_mut(id))
        else {
            return;
        };
        if matches!(action, Edit::Show) {
            *editor = Some(SettingsEditor::WordDelimiters(Editor {
                draft: self
                    .appearance
                    .read()
                    .unwrap()
                    .interaction
                    .word_delimiters
                    .clone(),
                error: None,
            }));
            return;
        }
        let Some(SettingsEditor::WordDelimiters(page)) = editor else {
            return;
        };
        match action {
            Edit::Change(text) => page.draft = text,
            Edit::RestoreDefault => page.draft = Interaction::default().word_delimiters,
            Edit::Save => {
                let mut settings = self.appearance.read().unwrap().clone();
                settings.interaction.word_delimiters.clone_from(&page.draft);
                if let Err(error) = Appearance::path().and_then(|path| settings.save(&path)) {
                    page.error = Some(format!(
                        "Could not save word selection delimiters · {error:#}"
                    ));
                    return;
                }
                self.keyboard.set_preferences(settings.interaction.clone());
                *self.appearance.write().unwrap() = settings;
                self.appearance_error = None;
                *editor = None;
            }
            Edit::Cancel => *editor = None,
            Edit::Show => unreachable!(),
        }
    }
}

pub(super) fn view(page: &Editor, ctx: &ViewContext<TerminalApp>) -> View {
    let description = TextBlock::new()
        .text("These characters separate words for double-click selection and screen-reader word navigation. Whitespace always separates words.")
        .text_wrapping(TextWrapping::Wrap);
    let characters = TextBox::new()
        .automation_name("Delimiter characters")
        .text(&page.draft)
        .on_text_changed(ctx.callback(|text| Message::WordDelimiters(Edit::Change(text))));
    let actions = StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(8.0)
        .children((
            Button::new()
                .on_click(ctx.callback(|_| Message::WordDelimiters(Edit::Save)))
                .content("Save"),
            Button::new()
                .on_click(ctx.callback(|_| Message::WordDelimiters(Edit::RestoreDefault)))
                .content("Restore default delimiters"),
            Button::new()
                .on_click(ctx.callback(|_| Message::WordDelimiters(Edit::Cancel)))
                .content("Cancel"),
        ));
    let mut rows = vec![
        KeyedView::new(
            "heading",
            TextBlock::new()
                .font_size(24.0)
                .text("Word selection delimiters"),
        ),
        KeyedView::new("description", description),
        KeyedView::new("characters", characters),
        KeyedView::new("actions", actions),
    ];
    if let Some(error) = &page.error {
        rows.push(KeyedView::new(
            "error",
            TextBlock::new()
                .text(error)
                .text_wrapping(TextWrapping::Wrap),
        ));
    }
    ScrollViewer::new().content(
        StackPanel::new()
            .margin(32.0)
            .max_width(660.0)
            .spacing(10.0)
            .keyed_children(rows),
    )
}
