use super::*;
use std::path::Path;
use w11term::profiles::LocalProfileSnapshot;

pub(super) struct Draft {
    original: Option<LocalProfileSnapshot>,
    name: String,
    executable: String,
    args: Vec<String>,
    cwd: String,
    scrollback_lines: String,
    powershell_integration: bool,
    error: String,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            original: None,
            name: String::new(),
            executable: String::new(),
            args: Vec::new(),
            cwd: String::new(),
            scrollback_lines: ScrollbackLines::default().to_string(),
            powershell_integration: false,
            error: String::new(),
        }
    }
}

impl Draft {
    fn from_options(name: String, options: &LocalOptions) -> Self {
        Self {
            name,
            executable: options.executable.display().to_string(),
            cwd: options
                .cwd
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            args: options.args.clone(),
            scrollback_lines: options.scrollback_lines.to_string(),
            powershell_integration: options.powershell_integration,
            ..Self::default()
        }
    }

    fn options(&self) -> anyhow::Result<LocalOptions> {
        Ok(LocalOptions {
            executable: self.executable.trim().into(),
            args: self.args.clone(),
            cwd: (!self.cwd.trim().is_empty()).then(|| self.cwd.trim().into()),
            scrollback_lines: self.scrollback_lines.parse()?,
            powershell_integration: self.powershell_integration && self.supports_integration(),
        })
    }

    fn supports_integration(&self) -> bool {
        LocalOptions::supports_powershell_integration(Path::new(self.executable.trim()), &self.args)
    }
}

#[derive(Clone, Copy)]
pub enum Field {
    Name,
    Executable,
    Cwd,
    ScrollbackLines,
    Argument(usize),
}
pub enum Action {
    Open(Option<String>),
    Field(TabId, Field, String),
    AddArgument(TabId),
    RemoveArgument(TabId, usize),
    PowerShellIntegration(TabId, bool),
    Save(TabId),
}

impl TerminalApp {
    pub(super) fn local_profile_action(
        &mut self,
        action: Action,
        context: &ComponentContext<Self>,
    ) {
        let id = match action {
            Action::Open(name) => {
                let draft = if let Some(name) = name {
                    match self.store.inspect_local(&name) {
                        Ok(base) => {
                            let mut draft = Draft::from_options(name, &base.options);
                            draft.original = Some(base);
                            draft
                        }
                        Err(error) => {
                            self.profile_error = Some(format!("{error:#}"));
                            self.utility(true, context);
                            return;
                        }
                    }
                } else {
                    Draft::default()
                };
                self.tabs.insert(AppTab {
                    label: "Local shell profile".into(),
                    page: Page::LocalProfile(draft),
                });
                self.sync_selection(context);
                self.activate(context);
                return;
            }
            Action::Save(id)
            | Action::Field(id, ..)
            | Action::AddArgument(id)
            | Action::PowerShellIntegration(id, _)
            | Action::RemoveArgument(id, _) => id,
        };
        let Some(AppTab {
            page: Page::LocalProfile(draft),
            ..
        }) = self.tabs.get_mut(id)
        else {
            return;
        };
        match action {
            Action::Field(_, field, value) => match field {
                Field::Name => draft.name = value,
                Field::Executable => draft.executable = value,
                Field::Cwd => draft.cwd = value,
                Field::ScrollbackLines => {
                    if draft.scrollback_lines != value {
                        draft.scrollback_lines = value;
                        draft.error.clear();
                    }
                }
                Field::Argument(index) => {
                    if let Some(arg) = draft.args.get_mut(index) {
                        *arg = value;
                    }
                }
            },
            Action::AddArgument(_) => draft.args.push(String::new()),
            Action::PowerShellIntegration(_, enabled) => draft.powershell_integration = enabled,
            Action::RemoveArgument(_, index) => {
                if index < draft.args.len() {
                    draft.args.remove(index);
                }
            }
            Action::Save(_) => {
                let result = draft
                    .options()
                    .and_then(|shell| {
                        self.store
                            .save_local(draft.original.as_ref(), draft.name.trim(), &shell)
                    })
                    .and_then(|_| self.store.catalog());
                match result {
                    Ok(catalog) => {
                        let original = draft.original.as_ref().map(|base| base.name.clone());
                        let name = draft.name.trim().to_owned();
                        self.catalog = catalog;
                        if let Some(old) = original {
                            if self.default_profile.as_ref() == Some(&old)
                                && matches!(self.default_source, Source::Local(_))
                            {
                                self.default_profile = Some(name.clone());
                                self.default_label = name.clone();
                                self.default_source =
                                    Source::Local(self.catalog.local[&name].clone());
                            }
                            let live: Vec<_> = self
                                .tabs
                                .entries()
                                .iter()
                                .filter(|entry| entry.value.profile_name() == Some(&old))
                                .map(|entry| entry.id)
                                .collect();
                            for id in live {
                                if let Some(AppTab {
                                    label,
                                    page: Page::Terminal(pane),
                                }) = self.tabs.get_mut(id)
                                    && matches!(pane.source, Source::Local(_))
                                {
                                    *label = name.clone();
                                    pane.profile_name = Some(name.clone());
                                }
                            }
                        }
                        self.profile_error = None;
                        self.utility(true, context);
                        self.close_tab(id, context);
                    }
                    Err(error) => draft.error = format!("{error:#}"),
                }
            }
            Action::Open(_) => unreachable!(),
        }
    }

    pub(super) fn local_profile_view(
        &self,
        id: TabId,
        draft: &Draft,
        context: &ViewContext<Self>,
    ) -> View {
        let edit =
            |field, label: &str, value: &str| {
                TextBox::new()
                    .automation_name(label)
                    .placeholder_text(label)
                    .text(value)
                    .on_text_changed(context.callback(move |value| {
                        Message::LocalProfile(Action::Field(id, field, value))
                    }))
            };
        let mut fields = vec![
            TextBlock::new()
                .text("Local shell profile")
                .font_size(26.0)
                .into(),
            TextBlock::new().text("Profile name").into(),
            edit(Field::Name, "Profile name", &draft.name).into(),
            TextBlock::new().text("Executable").into(),
            edit(Field::Executable, "Executable", &draft.executable).into(),
            TextBlock::new()
                .text("Starting directory (optional)")
                .into(),
            edit(Field::Cwd, "Starting directory", &draft.cwd).into(),
            scrollback_editor(
                &draft.scrollback_lines,
                context.callback(move |value| {
                    Message::LocalProfile(Action::Field(id, Field::ScrollbackLines, value))
                }),
            ),
            TextBlock::new().text("Arguments").into(),
        ];
        for (index, arg) in draft.args.iter().enumerate() {
            fields.push(
                Grid::new()
                    .columns([GridLength::STAR, GridLength::Auto])
                    .children((
                        edit(
                            Field::Argument(index),
                            &format!("Argument {}", index + 1),
                            arg,
                        ),
                        Button::new()
                            .grid_column(1)
                            .on_click(context.callback(move |_| {
                                Message::LocalProfile(Action::RemoveArgument(id, index))
                            }))
                            .content("Remove"),
                    )),
            );
        }
        fields.push(
            Button::new()
                .on_click(context.callback(move |_| Message::LocalProfile(Action::AddArgument(id))))
                .content("Add argument"),
        );
        if draft.supports_integration() {
            fields.push(
                CheckBox::new()
                    .automation_name("Enable command history and completion alerts in PowerShell")
                    .is_checked(draft.powershell_integration)
                    .on_is_checked_changed(context.callback(move |enabled| {
                        Message::LocalProfile(Action::PowerShellIntegration(id, enabled))
                    }))
                    .content("Enable command history and completion alerts in PowerShell"),
            );
        }
        fields.push(
            Button::new()
                .on_click(context.callback(move |_| Message::LocalProfile(Action::Save(id))))
                .content("Save profile"),
        );
        if !draft.error.is_empty() {
            fields.push(
                TextBlock::new()
                    .text(&draft.error)
                    .text_wrapping(TextWrapping::Wrap)
                    .into(),
            );
        }
        ScrollViewer::new().content(
            StackPanel::new()
                .width(520.0)
                .margin(32.0)
                .spacing(10.0)
                .horizontal_alignment(HorizontalAlignment::Left)
                .keyed_children(
                    fields
                        .into_iter()
                        .enumerate()
                        .map(|(index, field)| KeyedView::new(index.to_string(), field)),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_scrollback_editor_preserves_limits_and_rejects_invalid_changes() {
        let mut draft = Draft::default();
        assert_eq!(draft.scrollback_lines, "10000");
        assert_eq!(
            draft.options().unwrap().scrollback_lines,
            ScrollbackLines::default()
        );
        for limit in ["0", "32767"] {
            draft.scrollback_lines = limit.into();
            let saved = draft.options().unwrap();
            let reopened = Draft::from_options("PowerShell".into(), &saved);
            assert_eq!(reopened.scrollback_lines, limit);
            assert_eq!(
                reopened.options().unwrap().scrollback_lines,
                saved.scrollback_lines
            );
        }
        for invalid in ["", "-1", "2.5", "32768"] {
            draft.scrollback_lines = invalid.into();
            assert!(draft.options().is_err());
        }
    }
}
