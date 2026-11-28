use super::*;
use w11term::sftp::{LocalUtilityBundle, LocalUtilityExecutable};

#[derive(Clone, Copy)]
pub enum Field {
    Name,
    BinDirectory,
    Command,
}

#[derive(Clone)]
pub enum Action {
    Open,
    Revise,
    Loaded(Arc<()>, Result<Box<(PathBuf, UtilityBundle)>, String>),
    Text(Field, String),
    Browse,
    Replace(String, usize),
    Inspected(
        Arc<()>,
        Result<Option<(PickTarget, LocalUtilityExecutable)>, String>,
    ),
    Remove(String, usize),
    Save,
    Saved(Arc<()>, Result<Option<String>, String>),
    Cancel,
}

#[derive(Clone)]
pub enum PickTarget {
    Append(String),
    Replace(String, usize),
}

#[derive(Clone)]
pub(super) struct Editor {
    name: String,
    bin_directory: String,
    command: String,
    bundle: LocalUtilityBundle,
    source: Option<PathBuf>,
    last_file: PathBuf,
    work: Option<Arc<()>>,
    feedback: Option<String>,
}

fn message(id: TabId, action: Action) -> Message {
    Message::ProfileTools(id, super::Action::Local(action))
}

impl Editor {
    pub(super) fn view(&self, id: TabId, ctx: &ViewContext<TerminalApp>) -> View {
        let idle = self.work.is_none();
        let button = |label: &str, enabled, action: Action| {
            Button::new()
                .is_enabled(enabled)
                .on_click(ctx.callback(move |_| message(id, action.clone())))
                .content(label)
        };
        let field = |kind, label: &str, value: &str, hint: &str| {
            StackPanel::new().spacing(4.0).children((
                TextBlock::new().text(label),
                TextBox::new()
                    .is_enabled(idle)
                    .automation_name(label)
                    .placeholder_text(hint)
                    .text(value)
                    .on_text_changed(
                        ctx.callback(move |value| message(id, Action::Text(kind, value))),
                    ),
            ))
        };
        let revision = self.source.is_some();
        let mut fields = vec![KeyedView::new(
            "title",
            TextBlock::new().text(if revision {
                "Revise an SSH tool bundle"
            } else {
                "Create an SSH tool bundle"
            }),
        )];
        if let Some(source) = &self.source {
            fields.push(KeyedView::new(
                "source",
                TextBlock::new()
                    .text(source.display().to_string())
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        fields.extend([
            KeyedView::new(
                "name",
                field(
                    Field::Name,
                    if revision {
                        "Bundle name"
                    } else {
                        "New bundle name"
                    },
                    &self.name,
                    "my-tools",
                ),
            ),
            KeyedView::new(
                "bin",
                field(
                    Field::BinDirectory,
                    "Remote launcher directory",
                    &self.bin_directory,
                    "Relative to the remote home",
                ),
            ),
            KeyedView::new(
                "command",
                field(
                    Field::Command,
                    "Linux command name",
                    &self.command,
                    "Name used at the shell",
                ),
            ),
            KeyedView::new(
                "add",
                button(
                    "Add Linux executable",
                    idle && !self.command.trim().is_empty(),
                    Action::Browse,
                ),
            ),
        ]);
        for tool in self.bundle.tools() {
            let name = &tool.name;
            for (index, build) in tool.builds.iter().enumerate() {
                let architecture = review::build_label(&tool.builds, index);
                let source = review::build_source(build);
                fields.push(KeyedView::new(
                    format!("{name}/{index}"),
                    StackPanel::new().spacing(4.0).children((
                        TextBlock::new().text(format!("{name} · {architecture}")),
                        TextBlock::new()
                            .text(format!("{source}\nSHA-256 {}", build.sha256))
                            .text_wrapping(TextWrapping::Wrap),
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(8.0)
                            .children((
                                button(
                                    &format!("Replace {name} {architecture}"),
                                    idle,
                                    Action::Replace(name.clone(), index),
                                ),
                                button(
                                    &format!("Remove {name} {architecture}"),
                                    idle,
                                    Action::Remove(name.clone(), index),
                                ),
                            )),
                    )),
                ));
            }
        }
        if let Some(feedback) = &self.feedback {
            fields.push(KeyedView::new(
                "status",
                TextBlock::new()
                    .text(feedback)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        fields.push(KeyedView::new(
            "actions",
            StackPanel::new().spacing(8.0).children((
                button(
                    if revision {
                        "Save bundle revision"
                    } else {
                        "Save new tool bundle"
                    },
                    idle && !self.bundle.is_empty() && !self.name.trim().is_empty(),
                    Action::Save,
                ),
                button(
                    if revision {
                        "Cancel bundle revision"
                    } else {
                        "Cancel bundle creation"
                    },
                    idle,
                    Action::Cancel,
                ),
            )),
        ));
        StackPanel::new().spacing(8.0).keyed_children(fields)
    }

    fn suggestion(&self) -> String {
        if let Some(source) = &self.source {
            let stem = source
                .file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| stem.encode_utf16().count() <= 120)
                .unwrap_or("tools");
            format!("{stem}-revision.json")
        } else {
            "tools.json".into()
        }
    }

    fn begin(&mut self) -> Arc<()> {
        let ticket = Arc::new(());
        self.work = Some(ticket.clone());
        self.feedback = None;
        ticket
    }

    fn finish(&mut self, request: &Arc<()>) -> bool {
        self.work
            .take_if(|work| Arc::ptr_eq(work, request))
            .is_some()
    }
}

impl super::Editor {
    pub(super) fn edit_local_bundle(
        &mut self,
        id: TabId,
        action: Action,
        owner: Option<usize>,
        catalog: &Path,
        ctx: &ComponentContext<TerminalApp>,
    ) {
        if matches!(action, Action::Open) {
            if self.work.is_none() && self.starter.is_none() && self.local.is_none() {
                self.local = Some(Editor {
                    name: "my-tools".into(),
                    bin_directory: ".local/bin".into(),
                    command: String::new(),
                    bundle: LocalUtilityBundle::default(),
                    source: None,
                    last_file: catalog.into(),
                    work: None,
                    feedback: None,
                });
            }
            return;
        }
        match action {
            Action::Revise => {
                if self.work.is_some()
                    || self.starter.is_some()
                    || self.local.is_some()
                    || self.bundle.trim().is_empty()
                {
                    return;
                }
                let source = catalog
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join(self.bundle.trim());
                let ticket = Arc::new(());
                self.work = Some(ticket.clone());
                self.feedback = Some("Loading selected tool bundle…".into());
                let rejected = message(
                    id,
                    Action::Loaded(ticket.clone(), Err("Bundle editor unavailable".into())),
                );
                ctx.spawn_background_with_rejection(
                    move |_| {
                        let result = UtilityBundle::load(&source)
                            .map(|bundle| Box::new((source, bundle)))
                            .map_err(|error| {
                                format!("Reading the selected tool bundle: {error:#}")
                            });
                        message(id, Action::Loaded(ticket, result))
                    },
                    rejected,
                );
                return;
            }
            Action::Loaded(ticket, result) => {
                if self
                    .work
                    .take_if(|work| Arc::ptr_eq(work, &ticket))
                    .is_none()
                {
                    return;
                }
                match result {
                    Ok(loaded) => {
                        let (source, bundle) = *loaded;
                        let last_file = if ToolBundlePreset::from_managed_path(&source).is_some() {
                            catalog.into()
                        } else {
                            source.clone()
                        };
                        self.feedback = None;
                        self.local = Some(Editor {
                            name: bundle.name,
                            bin_directory: bundle.bin_directory,
                            command: String::new(),
                            bundle: LocalUtilityBundle::from_tools(bundle.tools),
                            source: Some(source),
                            last_file,
                            work: None,
                            feedback: None,
                        });
                    }
                    Err(error) => self.feedback = Some(error),
                }
                return;
            }
            _ => (),
        }
        let Some(local) = &mut self.local else { return };
        match action {
            Action::Text(field, value) if local.work.is_none() => {
                let slot = match field {
                    Field::Name => &mut local.name,
                    Field::BinDirectory => &mut local.bin_directory,
                    Field::Command => &mut local.command,
                };
                if *slot != value {
                    *slot = value;
                    local.feedback = None;
                }
            }
            Action::Remove(name, index) if local.work.is_none() => {
                local.bundle.remove(&name, index);
                local.feedback = None;
            }
            Action::Browse | Action::Replace(..) if local.work.is_none() => {
                let Some(owner) = owner else { return };
                let target = match action {
                    Action::Replace(name, index) => PickTarget::Replace(name, index),
                    _ if !local.command.trim().is_empty() => {
                        PickTarget::Append(local.command.trim().to_owned())
                    }
                    _ => return,
                };
                let current = local.last_file.clone();
                let ticket = local.begin();
                let rejected = message(
                    id,
                    Action::Inspected(ticket.clone(), Err("Executable picker unavailable".into())),
                );
                ctx.spawn_background_with_rejection(
                    move |scope| {
                        let result = (|| -> anyhow::Result<_> {
                            let Some(file) = file_picker::pick(
                                owner,
                                file_picker::Purpose::ToolExecutable(current),
                            )?
                            else {
                                return Ok(None);
                            };
                            if scope.is_cancelled() {
                                return Ok(None);
                            }
                            let executable = LocalUtilityExecutable::inspect(&file.path)?;
                            Ok(Some((target, executable)))
                        })()
                        .map_err(|error| format!("{error:#}"));
                        message(id, Action::Inspected(ticket, result))
                    },
                    rejected,
                );
            }
            Action::Inspected(ticket, result) if local.finish(&ticket) => match result {
                Ok(Some((target, executable))) => {
                    local.last_file.clone_from(&executable.path);
                    let result = match target {
                        PickTarget::Append(name) => local.bundle.add(&name, executable),
                        PickTarget::Replace(name, index) => {
                            local.bundle.replace(&name, index, executable)
                        }
                    };
                    if let Err(error) = result {
                        local.feedback = Some(format!("{error:#}"));
                    }
                }
                Ok(None) => (),
                Err(error) => local.feedback = Some(error),
            },
            Action::Save if local.work.is_none() && !local.bundle.is_empty() => {
                let Some(owner) = owner else { return };
                let bundle = local.bundle.clone();
                let name = local.name.trim().to_owned();
                let bin_directory = local.bin_directory.trim().to_owned();
                let current = local.last_file.clone();
                let suggestion = local.suggestion();
                let ticket = local.begin();
                let rejected = message(
                    id,
                    Action::Saved(ticket.clone(), Err("Bundle save unavailable".into())),
                );
                ctx.spawn_background_with_rejection(
                    move |scope| {
                        let result = (|| -> anyhow::Result<_> {
                            let Some(file) = file_picker::pick(
                                owner,
                                file_picker::Purpose::SaveToolBundle {
                                    current,
                                    suggestion,
                                },
                            )?
                            else {
                                return Ok(None);
                            };
                            if scope.is_cancelled() {
                                return Ok(None);
                            }
                            let path = file.path.to_str().ok_or_else(|| {
                                anyhow::anyhow!("The destination cannot be represented as text")
                            })?;
                            bundle.save(&file.path, &name, &bin_directory)?;
                            Ok(Some(path.to_owned()))
                        })()
                        .map_err(|error| format!("{error:#}"));
                        message(id, Action::Saved(ticket, result))
                    },
                    rejected,
                );
            }
            Action::Saved(ticket, result) if local.finish(&ticket) => match result {
                Ok(Some(path)) => {
                    self.local = None;
                    self.bundle = path;
                    self.edited();
                    self.feedback = Some("Bundle saved. Review it to approve sync.".into());
                }
                Ok(None) => (),
                Err(error) => local.feedback = Some(error),
            },
            Action::Cancel if local.work.is_none() => self.local = None,
            _ => (),
        }
    }
}
