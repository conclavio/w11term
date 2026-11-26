use super::*;
use std::{borrow::Cow, collections::BTreeSet};
use w11term::tabs::TabTitle;

pub(super) const RENAME_PANE: &str = "Rename focused pane…";

pub enum Action {
    Show,
    ShowGroup(TabId),
    ShowPane(u64, TabId),
    SaveProfile(clipboard_paste::Origin),
    Edit(u64, String),
    Finish(u64, ContentDialogResult),
}

pub(super) fn show_pane(target: Option<(u64, TabId)>) -> Message {
    target.map_or(Message::ControlIdle, |(pane, group)| {
        Message::TabTitle(Action::ShowPane(pane, group))
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Group(TabId),
    Pane { pane: u64, group: TabId },
    Profile(clipboard_paste::Origin),
}

impl Target {
    fn matches_focus(self, actual_pane: u64, actual_group: TabId) -> bool {
        match self {
            Self::Group(group) => group == actual_group,
            Self::Pane { pane, group } => pane == actual_pane && group == actual_group,
            Self::Profile(origin) => origin.pane == actual_pane && origin.group == actual_group,
        }
    }

    fn apply(self, tabs: &mut Tabs<AppTab>, title: Option<TabTitle>) {
        match self {
            Self::Group(group) => tabs.set_group_title(group, title),
            Self::Pane { .. } => {
                if let (Some(active), Some(group)) = (tabs.active_id(), tabs.active_group_id())
                    && let Some(AppTab {
                        page: Page::Terminal(live),
                        ..
                    }) = tabs.get_mut(active)
                    && self.matches_focus(live.session.pane_id, group)
                    && live.session.prompts.current().is_none()
                    && live.session.frames.load().prompt.is_none()
                {
                    live.pane_name = title;
                }
            }
            Self::Profile(_) => unreachable!("profile edits use the profile store"),
        }
    }
}

pub(super) struct Editor {
    target: Target,
    serial: u64,
    text: String,
    has_title: bool,
    reopen: Option<Target>,
    error: Option<String>,
}

impl Editor {
    pub(super) fn key(&self) -> String {
        format!("rename-title-{}", self.serial)
    }

    fn title(&self) -> Result<Option<TabTitle>, &'static str> {
        let value = self.text.trim();
        if value.is_empty() {
            return Ok(None);
        }
        TabTitle::try_from(value.to_owned()).map(Some)
    }

    pub(super) fn view(&self, ctx: &ViewContext<TerminalApp>) -> View {
        let serial = self.serial;
        let (heading, label, placeholder) = match self.target {
            Target::Group(_) => ("Rename tab", "Tab name", "Use the default tab title"),
            Target::Pane { .. } => (
                "Rename focused pane",
                "Pane name",
                "Use the default pane name",
            ),
            Target::Profile(_) => (
                "Save SSH connection as profile",
                "Profile name",
                "Profile name",
            ),
        };
        let profile = matches!(self.target, Target::Profile(_));
        let title_error = if profile { None } else { self.title().err() };
        let can_save = title_error.is_none() && (!profile || !self.text.trim().is_empty());
        let mut fields = vec![KeyedView::new(
            "title",
            TextBox::new()
                .automation_name(label)
                .placeholder_text(placeholder)
                .text(&self.text)
                .on_text_changed(ctx.callback(move |s| Message::TabTitle(Action::Edit(serial, s)))),
        )];
        if let Some(error) = self.error.as_deref().or(title_error) {
            fields.push(KeyedView::new(
                "error",
                TextBlock::new()
                    .text(error)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        let dialog = ContentDialog::new()
            .title(heading)
            .primary_button_text(if profile { "Save profile" } else { "Save" })
            .is_primary_button_enabled(can_save)
            .close_button_text("Cancel")
            .is_open(true)
            .on_closed(ctx.callback(move |r| Message::TabTitle(Action::Finish(serial, r))));
        let dialog = if profile {
            dialog
        } else {
            dialog
                .secondary_button_text("Use default")
                .is_secondary_button_enabled(self.has_title)
        };
        dialog.content(
            StackPanel::new()
                .spacing(8.0)
                .min_width(300.0)
                .keyed_children(fields),
        )
    }
}

fn display_label<'a>(
    renamed: Option<&'a TabTitle>,
    pane_name: Option<&'a TabTitle>,
    fallback: &'a str,
    follow_application: bool,
    application_title: impl FnOnce() -> Option<String>,
) -> Cow<'a, str> {
    if let Some(title) = renamed.or(pane_name) {
        return Cow::Borrowed(title.as_str());
    }
    if follow_application && let Some(title) = application_title() {
        return Cow::Owned(title);
    }
    Cow::Borrowed(fallback)
}

/// A hidden tab still follows its last focused pane. A sibling cannot change
/// its group's title, and an explicitly named group or pane needs no wake.
pub(super) fn followed_headers<T>(
    tabs: &Tabs<T>,
    enabled: bool,
    named: impl Fn(&T) -> bool,
) -> BTreeSet<TabId> {
    if !enabled {
        return BTreeSet::new();
    }
    tabs.headers()
        .into_iter()
        .filter(|entry| tabs.title_for(entry.id).is_none() && !named(&entry.value))
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
            tab.pane_name(),
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

    fn show_title_editor(&mut self, target: Target) {
        let id = match target {
            Target::Group(group) => switcher_leaf(&self.tabs.headers(), group),
            _ => self.tabs.active_id(),
        };
        let Some(id) = id else {
            return;
        };
        let Some(AppTab {
            page: Page::Terminal(live),
            ..
        }) = self.tabs.get(id)
        else {
            return;
        };
        let group = self.tabs.group_id(id).expect("pane has a tab");
        let blocked = |live: &LiveTab| {
            live.session.frames.load().prompt.is_some()
                || live.session.prompts.current().is_some()
                || live.files.entry_dialog_open()
        };
        if !target.matches_focus(live.session.pane_id, group)
            || matches!(target, Target::Profile(origin) if self.saveable_ssh_origin() != Some(origin))
            || blocked(live)
            || (self.tabs.active_id() != Some(id)
                && matches!(self.tabs.active().map(|tab| &tab.page), Some(Page::Terminal(active)) if blocked(active)))
            || self.clipboard.is_open()
        {
            return;
        }
        if let Some(editor) = &mut self.title_editor {
            // WinUI can accept another menu click after hiding the dialog but
            // before delivering Closed. Finish the original edit first.
            editor.reopen = Some(target);
            return;
        }
        let title = match target {
            Target::Pane { .. } => live.pane_name.as_ref(),
            Target::Group(_) => self.tabs.title_for(id),
            Target::Profile(_) => None,
        };
        self.name_edit_serial = self.name_edit_serial.wrapping_add(1);
        self.title_editor = Some(Editor {
            target,
            serial: self.name_edit_serial,
            text: title.map_or_else(String::new, |t| t.as_str().to_owned()),
            has_title: title.is_some(),
            reopen: None,
            error: None,
        });
        self.keyboard.set_modal(true);
    }

    pub(super) fn edit_tab_title(&mut self, action: Action, context: &ComponentContext<Self>) {
        match action {
            Action::Show => {
                if let Some(group) = self.tabs.active_group_id() {
                    self.show_title_editor(Target::Group(group));
                }
            }
            Action::ShowGroup(group) => self.show_title_editor(Target::Group(group)),
            Action::ShowPane(pane, group) => self.show_title_editor(Target::Pane { pane, group }),
            Action::SaveProfile(origin) => self.show_title_editor(Target::Profile(origin)),
            Action::Edit(serial, text) => {
                if let Some(editor) = &mut self.title_editor
                    && editor.serial == serial
                    && editor.text != text
                {
                    editor.text = text;
                    editor.error = None;
                }
            }
            Action::Finish(serial, result) => {
                let Some(mut editor) = self.title_editor.take_if(|e| e.serial == serial) else {
                    return;
                };
                if let Target::Profile(origin) = editor.target {
                    if result == ContentDialogResult::Primary {
                        match self.save_live_ssh_profile(origin, editor.text.trim(), context) {
                            Ok(()) => return,
                            Err(error) if self.saveable_ssh_origin() == Some(origin) => {
                                self.name_edit_serial = self.name_edit_serial.wrapping_add(1);
                                editor.serial = self.name_edit_serial;
                                editor.error = Some(format!("{error:#}"));
                                self.title_editor = Some(editor);
                                return;
                            }
                            Err(error) => self.window_error = Some(format!("{error:#}")),
                        }
                    }
                } else if let Some(title) = match result {
                    ContentDialogResult::Primary => editor.title().ok(),
                    ContentDialogResult::Secondary => Some(None),
                    _ => None,
                } {
                    editor.target.apply(&mut self.tabs, title);
                    self.header.borrow_mut().take();
                    self.publish_panes();
                }
                self.keyboard.set_modal(false);
                if let Some(id) = self.tabs.active_id() {
                    self.connection_ui(id);
                }
                if let Some(target) = editor.reopen {
                    self.show_title_editor(target);
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
        let pane_name = TabTitle::try_from("Tail logs".to_owned()).unwrap();
        let no_read = || -> Option<String> { panic!("unused frames should not be read") };
        assert_eq!(display_label(None, None, "Game", false, no_read), "Game");
        assert_eq!(
            display_label(Some(&renamed), Some(&pane_name), "Game", true, no_read),
            "Incident response"
        );
        assert_eq!(
            display_label(None, None, "Game", true, || Some("Cockpit".to_owned())),
            "Cockpit"
        );
        assert_eq!(
            display_label(None, None, "Settings", true, || None),
            "Settings"
        );
        for application_titles in [false, true] {
            assert_eq!(
                display_label(None, Some(&pane_name), "Game", application_titles, no_read),
                "Tail logs"
            );
        }
    }

    #[test]
    fn application_tab_title_watches_remember_focused_leaves_and_skip_named_tabs() {
        let mut tabs = Tabs::default();
        let ssh = tabs.insert("SSH");
        let shell = tabs.split(ssh, "PowerShell", Axis::Columns).unwrap();
        let log = tabs.insert("Log");
        assert_eq!(followed_headers(&tabs, false, |_| false), BTreeSet::new());
        assert_eq!(
            followed_headers(&tabs, true, |_| false),
            BTreeSet::from([shell, log])
        );
        tabs.select(ssh);
        tabs.select(log);
        assert_eq!(
            followed_headers(&tabs, true, |_| false),
            BTreeSet::from([ssh, log])
        );
        assert_eq!(
            followed_headers(&tabs, true, |label| *label == "SSH"),
            BTreeSet::from([log])
        );
        tabs.set_group_title(ssh, Some("Operations".to_owned().try_into().unwrap()));
        assert_eq!(
            followed_headers(&tabs, true, |_| false),
            BTreeSet::from([log])
        );
        tabs.set_group_title(ssh, None);
        assert_eq!(
            followed_headers(&tabs, true, |_| false),
            BTreeSet::from([ssh, log])
        );
    }

    #[test]
    fn pane_editor_requires_the_same_session_and_group_and_validates_only_the_name() {
        let target = Target::Pane { pane: 11, group: 7 };
        assert!(!target.matches_focus(12, 7), "focus moved to a sibling");
        assert!(!target.matches_focus(11, 8), "the original pane moved");
        assert!(
            !target.matches_focus(22, 7),
            "a replacement reused a layout position"
        );
        assert!(target.matches_focus(11, 7));
        let mut editor = Editor {
            target,
            serial: 4,
            text: "   ".into(),
            has_title: true,
            reopen: None,
            error: None,
        };
        assert_eq!(editor.title().unwrap(), None);
        editor.text = "  Build 雪  ".into();
        assert_eq!(editor.title().unwrap().unwrap().as_str(), "Build 雪");
        editor.text = "a\nb".into();
        assert!(editor.title().is_err());
        editor.text = "z".repeat(129);
        assert!(editor.title().is_err());
    }

    #[test]
    fn pane_rename_survives_moves_and_ignores_a_finish_for_a_different_focus_or_group() {
        let mut tabs = Tabs::default();
        let first = live_tab("shell".into(), Source::Demo, false);
        let pane = first.pane_id().unwrap();
        let first = tabs.insert(first);
        let other = tabs
            .split(
                first,
                live_tab("shell".into(), Source::Demo, false),
                Axis::Rows,
            )
            .unwrap();
        let target = Target::Pane { pane, group: first };
        let name = TabTitle::try_from("Build".to_owned()).unwrap();

        target.apply(&mut tabs, Some(name.clone()));
        assert!(tabs.get(other).unwrap().pane_name().is_none());
        tabs.select(first);
        target.apply(&mut tabs, Some(name.clone()));
        assert_eq!(tabs.get(first).unwrap().pane_name(), Some(&name));
        assert!(tabs.title_for(first).is_none());
        assert!(tabs.move_active_to_new_tab());
        target.apply(&mut tabs, None);
        assert_eq!(tabs.get(first).unwrap().pane_name(), Some(&name));
        let group = tabs.active_group_id().unwrap();
        Target::Pane { pane, group }.apply(&mut tabs, None);
        assert!(tabs.get(first).unwrap().pane_name().is_none());
        assert_eq!(tabs.get(first).unwrap().label, "shell");
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
