use super::*;
use w11term::sftp::{LocalUtilityBundle, LocalUtilityExecutable, UtilityArchitecture};

#[derive(Clone, Copy)]
pub enum Field {
    Name,
    BinDirectory,
    Command,
}

#[derive(Clone)]
pub enum Action {
    Open,
    Text(Field, String),
    Browse,
    Inspected(
        Arc<()>,
        Result<Option<(String, LocalUtilityExecutable)>, String>,
    ),
    Remove(String, UtilityArchitecture),
    Save,
    Saved(Arc<()>, Result<Option<String>, String>),
    Cancel,
}

#[derive(Clone)]
pub(super) struct Editor {
    name: String,
    bin_directory: String,
    command: String,
    bundle: LocalUtilityBundle,
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
        let mut fields = vec![
            KeyedView::new("title", TextBlock::new().text("Create an SSH tool bundle")),
            KeyedView::new(
                "name",
                field(Field::Name, "New bundle name", &self.name, "my-tools"),
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
        ];
        for (name, build) in self.bundle.executables() {
            let platform = match build.architecture {
                UtilityArchitecture::X86_64 => "x86-64",
                UtilityArchitecture::Aarch64 => "ARM64",
            };
            fields.push(KeyedView::new(
                format!("{name}/{platform}"),
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new().text(format!("{name} · {platform}")),
                    TextBlock::new()
                        .text(format!(
                            "{}\nSHA-256 {}",
                            build.path.display(),
                            build.sha256
                        ))
                        .text_wrapping(TextWrapping::Wrap),
                    button(
                        &format!("Remove {name} {platform}"),
                        idle,
                        Action::Remove(name.to_owned(), build.architecture),
                    ),
                )),
            ));
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
                    "Save new tool bundle",
                    idle && !self.bundle.is_empty() && !self.name.trim().is_empty(),
                    Action::Save,
                ),
                button("Cancel bundle creation", idle, Action::Cancel),
            )),
        ));
        StackPanel::new().spacing(8.0).keyed_children(fields)
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
                    last_file: catalog.into(),
                    work: None,
                    feedback: None,
                });
            }
            return;
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
            Action::Remove(name, architecture) if local.work.is_none() => {
                local.bundle.remove(&name, architecture);
                local.feedback = None;
            }
            Action::Browse if local.work.is_none() && !local.command.trim().is_empty() => {
                let Some(owner) = owner else { return };
                let name = local.command.trim().to_owned();
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
                            Ok(Some((name, executable)))
                        })()
                        .map_err(|error| format!("{error:#}"));
                        message(id, Action::Inspected(ticket, result))
                    },
                    rejected,
                );
            }
            Action::Inspected(ticket, result) if local.finish(&ticket) => match result {
                Ok(Some((name, executable))) => {
                    local.last_file.clone_from(&executable.path);
                    if let Err(error) = local.bundle.add(&name, executable) {
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
                                file_picker::Purpose::SaveToolBundle(current),
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
