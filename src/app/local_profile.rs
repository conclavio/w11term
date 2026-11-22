use super::*;
use std::path::Path;
use w11term::profiles::{LocalProfileRemoval, LocalProfileSnapshot};

pub(super) struct Draft {
    origin: DraftOrigin<LocalProfileSnapshot>,
    confirm_removal: bool,
    name: String,
    executable: String,
    args: Vec<String>,
    cwd: String,
    color_scheme: ColorScheme,
    scrollback_lines: String,
    powershell_integration: bool,
    error: String,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            origin: DraftOrigin::New,
            confirm_removal: false,
            name: String::new(),
            executable: String::new(),
            args: Vec::new(),
            cwd: String::new(),
            color_scheme: ColorScheme::default(),
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
            color_scheme: options.color_scheme,
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
            color_scheme: self.color_scheme,
            scrollback_lines: self.scrollback_lines.parse()?,
            powershell_integration: self.powershell_integration && self.supports_integration(),
        })
    }

    fn supports_integration(&self) -> bool {
        LocalOptions::supports_powershell_integration(Path::new(self.executable.trim()), &self.args)
    }

    pub(super) fn set_color_scheme(&mut self, scheme: ColorScheme) {
        if self.color_scheme != scheme {
            self.color_scheme = scheme;
            self.error.clear();
        }
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
    Duplicate(TabId),
    RequestRemoval(TabId),
    ConfirmRemoval(TabId, bool),
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
                            draft.origin = DraftOrigin::Edit(base);
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
            | Action::Duplicate(id)
            | Action::RequestRemoval(id)
            | Action::ConfirmRemoval(id, _)
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
            Action::Duplicate(_) => {
                let Some(base) = draft.origin.editing() else {
                    return;
                };
                let mut copy =
                    Draft::from_options(self.catalog.copy_name(&base.name), &base.options);
                let label = format!("Duplicate {}", base.name);
                copy.origin = DraftOrigin::Copy(base.clone());
                self.tabs.insert(AppTab {
                    label,
                    page: Page::LocalProfile(copy),
                });
                self.sync_selection(context);
                self.activate(context);
            }
            Action::RequestRemoval(_) => {
                draft.confirm_removal = draft
                    .origin
                    .editing()
                    .and_then(LocalProfileSnapshot::removal)
                    .is_some();
            }
            Action::ConfirmRemoval(_, accept) => {
                if !std::mem::take(&mut draft.confirm_removal) || !accept {
                    return;
                }
                let Some(base) = draft.origin.editing() else {
                    return;
                };
                let name = base.name.clone();
                match self.store.remove_local(base) {
                    Ok(catalog) => self.catalog = catalog,
                    Err(error) => {
                        draft.error = format!("{error:#}");
                        return;
                    }
                }
                let fallback = self.catalog.local.get(&name);
                if self.default_profile.as_ref() == Some(&name)
                    && matches!(self.default_source, Source::Local(_))
                {
                    if let Some(shell) = fallback {
                        self.default_source = Source::Local(shell.clone());
                    } else {
                        self.default_profile = None;
                        self.default_source = Source::Demo;
                        self.default_label = LAB.into();
                    }
                }
                if fallback.is_none() {
                    let linked: Vec<_> = self
                        .tabs
                        .entries()
                        .iter()
                        .filter(|entry| entry.value.profile_name() == Some(&name))
                        .map(|entry| entry.id)
                        .collect();
                    for linked in linked {
                        if let Some(AppTab {
                            page: Page::Terminal(pane),
                            ..
                        }) = self.tabs.get_mut(linked)
                            && matches!(pane.source, Source::Local(_))
                        {
                            pane.profile_name = None;
                        }
                    }
                }
                self.profile_error = None;
                self.utility(true, context);
                self.close_tab(id, context);
            }
            Action::Save(_) => {
                let result = draft
                    .options()
                    .and_then(|shell| {
                        let name = draft.name.trim();
                        match &draft.origin {
                            DraftOrigin::Copy(source) => {
                                self.store.duplicate_local(source, name, &shell)
                            }
                            origin => self.store.save_local(origin.editing(), name, &shell),
                        }
                    })
                    .and_then(|_| self.store.catalog());
                match result {
                    Ok(catalog) => {
                        let original = draft.origin.editing().map(|base| base.name.clone());
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
            color_scheme_editor(id, draft.color_scheme, context),
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
                .content(if matches!(draft.origin, DraftOrigin::Copy(_)) {
                    "Save copy"
                } else {
                    "Save profile"
                }),
        );
        if let Some(base) = draft.origin.editing() {
            if draft.confirm_removal
                && let Some(action) = base.removal()
            {
                let (message, confirm) = match action {
                    LocalProfileRemoval::Delete => (
                        format!(
                            "Delete saved profile “{}”? Open shells keep running.",
                            base.name
                        ),
                        "Delete saved profile",
                    ),
                    LocalProfileRemoval::RestoreDefaults => (
                        format!(
                            "Restore the installed settings for “{}”? Open shells keep running.",
                            base.name
                        ),
                        "Restore defaults",
                    ),
                };
                fields.push(
                    TextBlock::new()
                        .text(message)
                        .text_wrapping(TextWrapping::Wrap)
                        .into(),
                );
                fields.push(
                    Button::new()
                        .on_click(context.callback(move |_| {
                            Message::LocalProfile(Action::ConfirmRemoval(id, false))
                        }))
                        .content("Keep profile"),
                );
                fields.push(
                    Button::new()
                        .automation_name(format!("{confirm} {}", base.name))
                        .on_click(context.callback(move |_| {
                            Message::LocalProfile(Action::ConfirmRemoval(id, true))
                        }))
                        .content(confirm),
                );
            } else {
                fields.push(
                    Button::new()
                        .on_click(
                            context.callback(move |_| Message::LocalProfile(Action::Duplicate(id))),
                        )
                        .content("Duplicate saved profile…"),
                );
                if let Some(action) = base.removal() {
                    fields.push(
                        Button::new()
                            .on_click(context.callback(move |_| {
                                Message::LocalProfile(Action::RequestRemoval(id))
                            }))
                            .content(match action {
                                LocalProfileRemoval::Delete => "Delete profile…",
                                LocalProfileRemoval::RestoreDefaults => "Restore defaults…",
                            }),
                    );
                }
            }
        }
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
    fn local_scheme_edits_round_trip_without_changing_the_launch() {
        let mut draft = Draft {
            executable: "powershell.exe".into(),
            args: vec!["-NoLogo".into()],
            cwd: r"C:\work".into(),
            ..Default::default()
        };
        let original = draft.options().unwrap();
        assert_eq!(original.color_scheme, ColorScheme::Default);
        for scheme in ColorScheme::ALL {
            draft.set_color_scheme(scheme);
            let saved = draft.options().unwrap();
            assert_eq!(
                saved,
                LocalOptions {
                    color_scheme: scheme,
                    ..original.clone()
                }
            );
            let reopened = Draft::from_options("PowerShell".into(), &saved);
            assert_eq!(reopened.options().unwrap(), saved);
        }
    }

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
