use super::*;
use std::{borrow::Cow, collections::BTreeSet};
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
                .placeholder_text("Use the default tab title")
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

fn display_label<'a>(
    renamed: Option<&'a TabTitle>,
    fallback: &'a str,
    follow_application: bool,
    application_title: impl FnOnce() -> Option<String>,
) -> Cow<'a, str> {
    if let Some(title) = renamed {
        return Cow::Borrowed(title.as_str());
    }
    if follow_application && let Some(title) = application_title() {
        return Cow::Owned(title);
    }
    Cow::Borrowed(fallback)
}

/// A hidden tab still follows its last focused pane. A sibling cannot change
/// its group's title, and a named group needs no title-change wake at all.
pub(super) fn followed_headers<T>(tabs: &Tabs<T>, enabled: bool) -> BTreeSet<TabId> {
    if !enabled {
        return BTreeSet::new();
    }
    tabs.headers()
        .into_iter()
        .filter(|entry| tabs.title_for(entry.id).is_none())
        .map(|entry| entry.id)
        .collect()
}

pub(super) fn switcher_leaf<T>(headers: &[&w11term::tabs::Tab<T>], group: TabId) -> Option<TabId> {
    Some(headers.iter().find(|tab| tab.group_id() == group)?.id)
}

impl TerminalApp {
    pub(super) fn tab_label(&self, entry: TabId, follow_application: bool) -> Cow<'_, str> {
        let Some(tab) = self.tabs.get(entry) else {
            return Cow::Borrowed("Terminal");
        };
        display_label(
            self.tabs.title_for(entry),
            &tab.label,
            follow_application,
            || {
                let Page::Terminal(pane) = &tab.page else {
                    return None;
                };
                pane.session
                    .frames
                    .load()
                    .application_title()
                    .map(str::to_owned)
            },
        )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_tab_title_obeys_opt_in_and_custom_name_precedence() {
        let renamed = TabTitle::try_from("Incident response".to_owned()).unwrap();
        let no_read = || -> Option<String> { panic!("unused frames should not be read") };
        assert_eq!(display_label(None, "Game", false, no_read), "Game");
        assert_eq!(
            display_label(Some(&renamed), "Game", true, no_read),
            "Incident response"
        );
        assert_eq!(
            display_label(None, "Game", true, || Some("Cockpit".to_owned())),
            "Cockpit"
        );
        assert_eq!(display_label(None, "Settings", true, || None), "Settings");
    }

    #[test]
    fn application_tab_title_watches_remember_focused_leaves_and_skip_named_tabs() {
        let mut tabs = Tabs::default();
        let ssh = tabs.insert("SSH");
        let shell = tabs.split(ssh, "PowerShell", Axis::Columns).unwrap();
        let log = tabs.insert("Log");
        assert_eq!(followed_headers(&tabs, false), BTreeSet::new());
        assert_eq!(followed_headers(&tabs, true), BTreeSet::from([shell, log]));
        tabs.select(ssh);
        tabs.select(log);
        assert_eq!(followed_headers(&tabs, true), BTreeSet::from([ssh, log]));
        tabs.set_group_title(ssh, Some("Operations".to_owned().try_into().unwrap()));
        assert_eq!(followed_headers(&tabs, true), BTreeSet::from([log]));
        tabs.set_group_title(ssh, None);
        assert_eq!(followed_headers(&tabs, true), BTreeSet::from([ssh, log]));
    }

    #[test]
    fn application_tab_title_switcher_keeps_group_if_original_pane_moves_while_open() {
        let mut tabs = Tabs::default();
        let ssh = tabs.insert("SSH");
        let shell = tabs.split(ssh, "PowerShell", Axis::Columns).unwrap();
        tabs.select(ssh);
        let other = tabs.insert("Editor");
        assert_eq!(switcher_leaf(&tabs.headers(), ssh), Some(ssh));
        assert!(tabs.move_to_existing_tab(ssh, other, w11term::pane_layout::Direction::Left));
        assert_eq!(switcher_leaf(&tabs.headers(), ssh), Some(shell));
        assert_eq!(switcher_leaf(&tabs.headers(), other), Some(ssh));
        tabs.remove_group(shell);
        assert_eq!(switcher_leaf(&tabs.headers(), ssh), None);
    }
}
