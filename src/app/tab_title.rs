use super::*;
use w11term::tabs::TabTitle;

pub enum Action {
    Show,
    Edit(TabId, String),
    Finish(TabId, ContentDialogResult),
}

pub(super) struct Editor {
    group: TabId,
    text: String,
    has_title: bool,
}

impl Editor {
    fn title(&self) -> Result<Option<TabTitle>, &'static str> {
        let value = self.text.trim();
        if value.is_empty() {
            return Ok(None);
        }
        TabTitle::try_from(value.to_owned()).map(Some)
    }

    pub(super) fn view(&self, ctx: &ViewContext<TerminalApp>) -> View {
        let group = self.group;
        let title = self.title();
        let mut fields = vec![KeyedView::new(
            "title",
            TextBox::new()
                .automation_name("Tab name")
                .placeholder_text("Use the focused pane’s label")
                .text(&self.text)
                .on_text_changed(ctx.callback(move |s| Message::TabTitle(Action::Edit(group, s)))),
        )];
        if let Err(error) = title {
            fields.push(KeyedView::new("error", TextBlock::new().text(error)));
        }
        ContentDialog::new()
            .title("Rename tab")
            .primary_button_text("Save")
            .is_primary_button_enabled(title.is_ok())
            .secondary_button_text("Use default")
            .is_secondary_button_enabled(self.has_title)
            .close_button_text("Cancel")
            .is_open(true)
            .on_closed(ctx.callback(move |r| Message::TabTitle(Action::Finish(group, r))))
            .content(
                StackPanel::new()
                    .spacing(8.0)
                    .min_width(300.0)
                    .keyed_children(fields),
            )
    }
}

impl TerminalApp {
    pub(super) fn tab_label(&self, entry: TabId) -> &str {
        self.tabs
            .title_for(entry)
            .map(TabTitle::as_str)
            .or_else(|| self.tabs.get(entry).map(|tab| tab.label.as_str()))
            .unwrap_or("Terminal")
    }

    pub(super) fn edit_tab_title(&mut self, action: Action) {
        match action {
            Action::Show => {
                let Some(id) = self.tabs.active_id() else {
                    return;
                };
                if self.title_editor.is_some()
                    || !matches!(self.tabs.get(id), Some(AppTab { page: Page::Terminal(tab), .. })
                        if tab.session.frames.load().prompt.is_none())
                {
                    return;
                }
                let title = self.tabs.title_for(id);
                self.title_editor = Some(Editor {
                    group: self.tabs.group_id(id).expect("pane has a tab"),
                    text: title.map_or_else(String::new, |t| t.as_str().to_owned()),
                    has_title: title.is_some(),
                });
                self.keyboard.set_modal(true);
            }
            Action::Edit(group, text) => {
                if let Some(editor) = &mut self.title_editor
                    && editor.group == group
                {
                    editor.text = text;
                }
            }
            Action::Finish(group, result) => {
                if !self.title_editor.as_ref().is_some_and(|e| e.group == group) {
                    return;
                }
                let editor = self.title_editor.take().unwrap();
                let change = match result {
                    ContentDialogResult::Primary => editor.title().ok(),
                    ContentDialogResult::Secondary => Some(None),
                    _ => None,
                };
                if let Some(title) = change {
                    self.tabs.set_group_title(group, title);
                    self.publish_panes();
                }
                self.keyboard.set_modal(false);
                if let Some(id) = self.tabs.active_id() {
                    self.connection_ui(id);
                }
            }
        }
    }
}
