use super::*;
use std::path::PathBuf;
use w11term::{
    options::{ToolPath, ToolSync},
    sftp::{
        DotfileShell, ToolBundlePreset, UtilityBundle, builtin_tool_names,
        prepare_builtin_tool_bundle, validate_dotfiles_home, validate_zdotdir,
    },
};

const TITLE: &str = "SSH tools";

#[derive(Clone, Copy)]
pub enum Field {
    Bundle,
    Home,
    Fingerprint,
    Zdotdir,
}

#[derive(Clone)]
pub enum Action {
    Expanded(bool),
    Text(Field, String),
    PathMode(Option<usize>),
    Browse,
    Picked(Arc<()>, Result<Option<String>, String>),
    Preset(Option<usize>),
    Tool(&'static str, bool),
    Starter,
    StarterFinished(Arc<()>, Result<String, String>),
    Review,
    Reviewed(Arc<()>, Result<Box<ToolSync>, String>),
    Approve,
    Disable,
}

#[derive(Clone, Default)]
pub(super) struct Editor {
    expanded: bool,
    bundle: String,
    home: String,
    fingerprint: String,
    zdotdir: String,
    path_mode: usize,
    approved: Option<Box<ToolSync>>,
    reviewed: Option<Box<ToolSync>>,
    work: Option<Arc<()>>,
    preset: ToolBundlePreset,
    starter: Option<(Arc<()>, ComponentTask)>,
    feedback: Option<String>,
}

impl Editor {
    pub(super) fn new(approved: Option<Box<ToolSync>>) -> Self {
        let Some(settings) = approved.as_ref() else {
            return Self::default();
        };
        let (path_mode, zdotdir) =
            settings
                .add_to_path
                .as_ref()
                .map_or((0, String::new()), |path| {
                    (
                        if path.shell == DotfileShell::Zsh {
                            2
                        } else {
                            1
                        },
                        path.zdotdir.clone().unwrap_or_default(),
                    )
                });
        Self {
            bundle: settings.bundle.to_string_lossy().into_owned(),
            preset: ToolBundlePreset::from_managed_path(&settings.bundle).unwrap_or_default(),
            home: settings.home.clone(),
            fingerprint: settings.fingerprint.clone(),
            zdotdir,
            path_mode,
            approved,
            ..Self::default()
        }
    }

    pub(super) fn approval(&self) -> Option<Box<ToolSync>> {
        self.approved.clone()
    }

    fn candidate(&self, draft: &ProfileDraft, base: &Path) -> anyhow::Result<ToolSync> {
        let (host, user, port) = parse_ssh_address(&draft.host, &draft.user, &draft.port)?;
        let fingerprint =
            profile_dotfiles::approved_fingerprint(&self.fingerprint, &draft.fingerprint)?;
        anyhow::ensure!(!self.bundle.trim().is_empty(), "Choose an SSH tool bundle");
        let home = self.home.trim();
        validate_dotfiles_home(home)?;
        let add_to_path = match self.path_mode {
            1 => Some(ToolPath {
                shell: DotfileShell::Bash,
                zdotdir: None,
            }),
            2 => {
                let zdotdir = self.zdotdir.trim();
                let zdotdir = (!zdotdir.is_empty()).then_some(zdotdir);
                validate_zdotdir(DotfileShell::Zsh, zdotdir)?;
                Some(ToolPath {
                    shell: DotfileShell::Zsh,
                    zdotdir: zdotdir.map(str::to_owned),
                })
            }
            _ => None,
        };
        Ok(ToolSync {
            bundle: base.join(self.bundle.trim()),
            name: String::new(),
            bin_directory: String::new(),
            host,
            user,
            port,
            fingerprint,
            home: home.into(),
            add_to_path,
        })
    }

    fn agrees(&self, scope: &ToolSync, draft: &ProfileDraft, base: &Path) -> bool {
        self.candidate(draft, base).is_ok_and(|mut current| {
            current.name.clone_from(&scope.name);
            current.bin_directory.clone_from(&scope.bin_directory);
            current == *scope
        })
    }

    fn edited(&mut self) {
        self.reviewed = None;
        self.work = None;
        self.feedback = None;
    }

    pub(super) fn view(
        &self,
        id: TabId,
        draft: &ProfileDraft,
        base: &Path,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let send =
            |action: Action| ctx.callback(move |_| Message::ProfileTools(id, action.clone()));
        let field = |which, label: &str, value: &str, hint: &str| {
            StackPanel::new().spacing(4.0).children((
                TextBlock::new().text(label),
                TextBox::new()
                    .text(value)
                    .automation_name(label)
                    .placeholder_text(hint)
                    .on_text_changed(ctx.callback(move |text| {
                        Message::ProfileTools(id, Action::Text(which, text))
                    })),
            ))
        };
        let reviewed = self
            .reviewed
            .as_ref()
            .filter(|settings| self.agrees(settings, draft, base));
        let approved = self
            .approved
            .as_ref()
            .filter(|settings| self.agrees(settings, draft, base));
        let status = if self.starter.is_some() {
            "Preparing verified Linux builds for x86-64 and ARM64…".into()
        } else if let Some(feedback) = &self.feedback {
            feedback.clone()
        } else if let Some(settings) = reviewed {
            format!(
                "{} → {}@{}:{} · {}/{}",
                settings.name,
                settings.user,
                settings.host,
                settings.port,
                settings.home.trim_end_matches('/'),
                settings.bin_directory
            )
        } else if let Some(settings) = approved {
            format!("Approved tool bundle: {}", settings.name)
        } else if self.approved.is_some() {
            "Review the edited settings before approving them".into()
        } else {
            "Tool sync off".into()
        };
        let idle = self.work.is_none() && self.starter.is_none();
        let custom = match &self.preset {
            ToolBundlePreset::Custom(names) => Some(names),
            _ => None,
        };
        let mut fields = vec![
            KeyedView::new(
                "bundle",
                field(
                    Field::Bundle,
                    "Tool bundle manifest",
                    &self.bundle,
                    "Windows path to a bundle JSON file",
                ),
            ),
            KeyedView::new(
                "browse",
                Button::new()
                    .is_enabled(idle)
                    .on_click(send(Action::Browse))
                    .content("Browse for tool bundle"),
            ),
            KeyedView::new(
                "builtin",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        ComboBox::new()
                            .automation_name("Built-in Linux tool bundle")
                            .is_enabled(idle)
                            .items_source(ToolBundlePreset::LABELS)
                            .selected_index(self.preset.index())
                            .on_selection_changed(ctx.callback(move |index| {
                                Message::ProfileTools(id, Action::Preset(index))
                            })),
                        Button::new()
                            .is_enabled(
                                self.work.is_none() && custom.is_none_or(|names| !names.is_empty()),
                            )
                            .on_click(send(Action::Starter))
                            .content(if self.starter.is_some() {
                                "Cancel tool download".into()
                            } else if custom.is_some() {
                                "Use selected tools".into()
                            } else {
                                format!("Use {}", self.preset.label())
                            }),
                    )),
            ),
        ];
        if let Some(selected) = custom {
            let names = builtin_tool_names();
            let columns = 3;
            fields.push(KeyedView::new(
                "custom-tools",
                Grid::new()
                    .columns(vec![GridLength::Star(1.0); columns])
                    .rows(vec![GridLength::Auto; names.len().div_ceil(columns)])
                    .column_spacing(8.0)
                    .keyed_children(names.iter().enumerate().map(|(index, name)| {
                        let name = name.as_str();
                        KeyedView::new(
                            name,
                            CheckBox::new()
                                .grid_row((index / columns) as i32)
                                .grid_column((index % columns) as i32)
                                .automation_name(format!("Include {name}"))
                                .is_enabled(idle)
                                .is_checked(selected.contains(name))
                                .on_is_checked_changed(ctx.callback(move |value| {
                                    Message::ProfileTools(id, Action::Tool(name, value))
                                }))
                                .content(name),
                        )
                    })),
            ));
        }
        fields.extend([
            KeyedView::new(
                "home",
                field(
                    Field::Home,
                    "Tool bundle SFTP home",
                    &self.home,
                    "/home/operator",
                ),
            ),
            KeyedView::new(
                "fingerprint",
                field(
                    Field::Fingerprint,
                    "Tool bundle host-key fingerprint",
                    &self.fingerprint,
                    "Uses the profile host fingerprint when blank",
                ),
            ),
            KeyedView::new(
                "path",
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new().text("Shell PATH"),
                    ComboBox::new()
                        .automation_name("Tool bundle shell PATH")
                        .items_source(["Manual PATH setup", "Manage Bash PATH", "Manage Zsh PATH"])
                        .selected_index(self.path_mode)
                        .on_selection_changed(ctx.callback(move |index| {
                            Message::ProfileTools(id, Action::PathMode(index))
                        })),
                )),
            ),
        ]);
        if self.path_mode == 2 {
            fields.push(KeyedView::new(
                "zdotdir",
                field(
                    Field::Zdotdir,
                    "Tool PATH Zsh startup directory",
                    &self.zdotdir,
                    "Relative to the remote home",
                ),
            ));
        }
        fields.extend([
            KeyedView::new(
                "actions",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .is_enabled(idle)
                            .on_click(send(Action::Review))
                            .content("Review tool bundle"),
                        Button::new()
                            .is_enabled(self.starter.is_none() && reviewed.is_some())
                            .on_click(send(Action::Approve))
                            .content("Approve tool sync"),
                        Button::new()
                            .is_enabled(self.approved.is_some())
                            .on_click(send(Action::Disable))
                            .content("Disable tool sync"),
                    )),
            ),
            KeyedView::new(
                "status",
                TextBlock::new()
                    .text(status)
                    .text_wrapping(TextWrapping::Wrap),
            ),
        ]);
        Expander::new()
            .automation_name(TITLE)
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .is_expanded(self.expanded)
            .on_is_expanded_changed(
                ctx.callback(move |expanded| Message::ProfileTools(id, Action::Expanded(expanded))),
            )
            .slots([
                SlotView::new(ExpanderSlot::Header, TextBlock::new().text(TITLE)),
                SlotView::new(
                    ExpanderSlot::Content,
                    StackPanel::new().spacing(8.0).keyed_children(fields),
                ),
            ])
    }
}

impl TerminalApp {
    pub(super) fn edit_profile_tools(
        &mut self,
        id: TabId,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        let owner = self.keyboard.hwnd().map(|handle| handle.0 as usize);
        let catalog = self.store.path();
        let base = catalog.parent().unwrap_or(Path::new("."));
        let Some(AppTab {
            page: Page::NewProfile(draft),
            ..
        }) = self.tabs.get_mut(id)
        else {
            return;
        };
        match action {
            Action::Expanded(expanded) => draft.tool_bundle.expanded = expanded,
            Action::PathMode(Some(index)) if index != draft.tool_bundle.path_mode && index < 3 => {
                draft.tool_bundle.path_mode = index;
                draft.tool_bundle.edited();
            }
            Action::PathMode(_) => (),
            Action::Preset(Some(index))
                if draft.tool_bundle.work.is_none() && draft.tool_bundle.starter.is_none() =>
            {
                let editor = &mut draft.tool_bundle;
                if index == editor.preset.index() {
                    return;
                }
                editor.preset = match index {
                    0 => ToolBundlePreset::Essentials,
                    1 => ToolBundlePreset::Admin,
                    2 => match ToolBundlePreset::from_managed_path(Path::new(&editor.bundle)) {
                        Some(preset @ ToolBundlePreset::Custom(_)) => preset,
                        _ => ToolBundlePreset::Custom(Default::default()),
                    },
                    _ => return,
                };
                editor.edited();
            }
            Action::Preset(_) => (),
            Action::Tool(name, checked) => {
                let editor = &mut draft.tool_bundle;
                if editor.work.is_none()
                    && editor.starter.is_none()
                    && let ToolBundlePreset::Custom(names) = &mut editor.preset
                {
                    let changed = if checked {
                        names.insert(name.to_owned())
                    } else {
                        names.remove(name)
                    };
                    if changed {
                        editor.edited();
                    }
                }
            }
            Action::Text(field, value) => {
                let editor = &mut draft.tool_bundle;
                let slot = match field {
                    Field::Bundle => &mut editor.bundle,
                    Field::Home => &mut editor.home,
                    Field::Fingerprint => &mut editor.fingerprint,
                    Field::Zdotdir => &mut editor.zdotdir,
                };
                if *slot != value {
                    *slot = value;
                    editor.edited();
                    if matches!(field, Field::Bundle)
                        && let Some((_, task)) = editor.starter.take()
                    {
                        task.cancel();
                    }
                }
            }
            Action::Browse
                if draft.tool_bundle.work.is_none() && draft.tool_bundle.starter.is_none() =>
            {
                let Some(owner) = owner else { return };
                let text = draft.tool_bundle.bundle.trim();
                let current: PathBuf = if text.is_empty() {
                    catalog.into()
                } else {
                    base.join(text)
                };
                let request = Arc::new(());
                draft.tool_bundle.work = Some(request.clone());
                let rejected = Message::ProfileTools(
                    id,
                    Action::Picked(request.clone(), Err("File picker unavailable".into())),
                );
                ctx.spawn_background_with_rejection(
                    move |cancel| {
                        let result = (|| -> anyhow::Result<Option<String>> {
                            if cancel.is_cancelled() {
                                return Ok(None);
                            }
                            let selected = file_picker::pick(
                                owner,
                                file_picker::Purpose::ToolBundle(current),
                            )?;
                            selected
                                .map(|file| {
                                    file.path.into_os_string().into_string().map_err(|_| {
                                        anyhow::anyhow!(
                                            "The selected path cannot be represented as text"
                                        )
                                    })
                                })
                                .transpose()
                        })()
                        .map_err(|error| format!("{error:#}"));
                        Message::ProfileTools(id, Action::Picked(request, result))
                    },
                    rejected,
                );
            }
            Action::Starter if draft.tool_bundle.work.is_none() => {
                let editor = &mut draft.tool_bundle;
                if let Some((_, task)) = editor.starter.take() {
                    task.cancel();
                    editor.feedback = Some("Tool download cancelled".into());
                    return;
                }
                editor.feedback = None;
                let preset = editor.preset.clone();
                let request = Arc::new(());
                let rejected = Message::ProfileTools(
                    id,
                    Action::StarterFinished(
                        request.clone(),
                        Err("Tool download unavailable".into()),
                    ),
                );
                let work_request = request.clone();
                let task = ctx.spawn_background_with_rejection(
                    move |cancel| {
                        let result = prepare_builtin_tool_bundle(&preset, || cancel.is_cancelled())
                            .and_then(|path| {
                                path.into_os_string().into_string().map_err(|_| {
                                    anyhow::anyhow!(
                                        "The starter manifest path cannot be represented as text"
                                    )
                                })
                            })
                            .map_err(|error| format!("{error:#}"));
                        Message::ProfileTools(id, Action::StarterFinished(work_request, result))
                    },
                    rejected,
                );
                editor.starter = Some((request, task));
            }
            Action::StarterFinished(request, result) => {
                let editor = &mut draft.tool_bundle;
                if editor
                    .starter
                    .take_if(|(current, _)| Arc::ptr_eq(current, &request))
                    .is_none()
                {
                    return;
                }
                match result {
                    Ok(path) => {
                        editor.bundle = path;
                        editor.edited();
                        editor.feedback = Some(match &editor.preset {
                            ToolBundlePreset::Custom(_) => {
                                "Selected tools are ready. Review the bundle to approve sync."
                                    .into()
                            }
                            preset => format!(
                                "{} is ready. Review the bundle to approve sync.",
                                preset.label()
                            ),
                        });
                    }
                    Err(error) => editor.feedback = Some(error),
                }
            }
            Action::Review
                if draft.tool_bundle.work.is_none() && draft.tool_bundle.starter.is_none() =>
            {
                let mut settings = match draft.tool_bundle.candidate(draft, base) {
                    Ok(settings) => Box::new(settings),
                    Err(error) => {
                        draft.tool_bundle.feedback = Some(format!("{error:#}"));
                        return;
                    }
                };
                draft.tool_bundle.edited();
                let request = Arc::new(());
                draft.tool_bundle.work = Some(request.clone());
                let rejected = Message::ProfileTools(
                    id,
                    Action::Reviewed(request.clone(), Err("Bundle review unavailable".into())),
                );
                ctx.spawn_background_with_rejection(
                    move |_| {
                        let result = UtilityBundle::load(&settings.bundle)
                            .map(move |bundle| {
                                settings.name = bundle.name;
                                settings.bin_directory = bundle.bin_directory;
                                settings
                            })
                            .map_err(|error| format!("{error:#}"));
                        Message::ProfileTools(id, Action::Reviewed(request, result))
                    },
                    rejected,
                );
            }
            Action::Picked(request, result) => {
                let editor = &mut draft.tool_bundle;
                if editor
                    .work
                    .take_if(|current| Arc::ptr_eq(current, &request))
                    .is_none()
                {
                    return;
                }
                match result {
                    Ok(Some(path)) => {
                        editor.bundle = path;
                        editor.edited();
                    }
                    Ok(None) => (),
                    Err(error) => editor.feedback = Some(error),
                }
            }
            Action::Reviewed(request, result) => {
                if draft
                    .tool_bundle
                    .work
                    .take_if(|current| Arc::ptr_eq(current, &request))
                    .is_none()
                {
                    return;
                }
                match result {
                    Ok(scope) if draft.tool_bundle.agrees(&scope, draft, base) => {
                        draft.tool_bundle.reviewed = Some(scope)
                    }
                    Ok(_) => (),
                    Err(error) => draft.tool_bundle.feedback = Some(error),
                }
            }
            Action::Approve if draft.tool_bundle.starter.is_none() => {
                if let Some(scope) = draft.tool_bundle.reviewed.take()
                    && draft.tool_bundle.agrees(&scope, draft, base)
                {
                    draft.tool_bundle.approved = Some(scope);
                    draft.tool_bundle.feedback = None;
                    draft.saved = false;
                }
            }
            Action::Disable => {
                draft.tool_bundle.edited();
                draft.tool_bundle.approved = None;
                draft.saved = false;
            }
            Action::Browse | Action::Review | Action::Starter | Action::Approve => (),
        }
    }
}
