use super::*;
use anyhow::Context;

#[derive(Clone)]
pub enum Action {
    Create,
    Revise,
    Loaded(Arc<()>, Result<Box<DotfileBundle>, String>),
    Text(Arc<()>, usize, String),
    Save(Arc<()>),
    Saved(Arc<()>, Result<Option<String>, String>),
    Cancel(Arc<()>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Loading,
    Ready,
    Saving,
}

#[derive(Clone)]
pub(super) struct Editor {
    ticket: Arc<()>,
    source: Option<PathBuf>,
    fragments: [Option<String>; 3],
    phase: Phase,
    error: Option<String>,
}

fn message(id: TabId, action: Action) -> Message {
    Message::ProfileDotfiles(id, super::Action::Bundle(action))
}

impl Editor {
    fn new(source: Option<PathBuf>) -> Self {
        let phase = if source.is_some() {
            Phase::Loading
        } else {
            Phase::Ready
        };
        Self {
            ticket: Arc::new(()),
            source,
            fragments: Default::default(),
            phase,
            error: None,
        }
    }

    fn load(&mut self, bundle: DotfileBundle) {
        self.fragments = [bundle.environment, bundle.bash, bundle.zsh]
            .map(|text| text.map(|text| text.replace("\r\n", "\n").replace('\n', "\r")));
        self.phase = Phase::Ready;
    }

    fn bundle(&self) -> DotfileBundle {
        let [environment, bash, zsh] = self.fragments.each_ref().map(|text| {
            text.as_ref()
                .map(|text| text.replace("\r\n", "\n").replace('\r', "\n"))
        });
        DotfileBundle {
            version: 1,
            environment,
            bash,
            zsh,
        }
    }

    fn suggestion(&self) -> String {
        if let Some(source) = &self.source {
            let stem = source
                .file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| stem.encode_utf16().count() <= 120)
                .unwrap_or("dotfiles");
            format!("{stem}-revision.json")
        } else {
            "dotfiles.json".into()
        }
    }

    pub(super) fn view(&self, id: TabId, ctx: &ViewContext<TerminalApp>) -> View {
        let mut views = vec![KeyedView::new(
            "bundle-editor-title",
            TextBlock::new().text(if self.source.is_some() {
                "Revise dotfiles bundle"
            } else {
                "Create a dotfiles bundle"
            }),
        )];
        if let Some(source) = &self.source {
            views.push(KeyedView::new(
                "bundle-editor-source",
                TextBlock::new()
                    .text(source.display().to_string())
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        if self.phase != Phase::Loading {
            for (index, name) in [
                "Shared environment fragment",
                "Bash startup fragment",
                "Zsh startup fragment",
            ]
            .into_iter()
            .enumerate()
            {
                let ticket = self.ticket.clone();
                let text = self.fragments[index].as_deref().unwrap_or_default();
                views.push(KeyedView::new(
                    name,
                    StackPanel::new().spacing(4.0).children((
                        TextBlock::new().text(name),
                        TextBox::new()
                            .automation_name(name)
                            .accepts_return(true)
                            .text_wrapping(TextWrapping::Wrap)
                            .min_height(76.0)
                            .max_height(150.0)
                            .text(text)
                            .is_enabled(self.phase == Phase::Ready)
                            .on_text_changed(ctx.callback(move |text| {
                                message(id, Action::Text(ticket.clone(), index, text))
                            })),
                    )),
                ));
            }
        }
        if let Some(status) = self.error.as_deref().or(match self.phase {
            Phase::Loading => Some("Reading the selected dotfiles bundle…"),
            Phase::Saving => Some("Saving the dotfiles bundle…"),
            _ => None,
        }) {
            views.push(KeyedView::new(
                "bundle-editor-status",
                TextBlock::new()
                    .text(status)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        let send = |action: Action| ctx.callback(move |_| message(id, action.clone()));
        views.push(KeyedView::new(
            "bundle-editor-actions",
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    Button::new()
                        .is_enabled(self.phase == Phase::Ready)
                        .on_click(send(Action::Save(self.ticket.clone())))
                        .content(if self.source.is_some() {
                            "Save dotfiles revision"
                        } else {
                            "Save dotfiles bundle"
                        }),
                    Button::new()
                        .is_enabled(self.phase != Phase::Saving)
                        .on_click(send(Action::Cancel(self.ticket.clone())))
                        .content("Cancel dotfiles editing"),
                )),
        ));
        StackPanel::new().spacing(8.0).keyed_children(views)
    }
}

impl super::Editor {
    pub(super) fn edit_bundle(
        &mut self,
        id: TabId,
        action: Action,
        owner: Option<usize>,
        profile_catalog: &Path,
        ctx: &ComponentContext<TerminalApp>,
    ) {
        if matches!(action, Action::Create | Action::Revise) {
            if self.composer.is_some() || self.checking.is_some() || self.browsing.is_some() {
                return;
            }
            let source = if matches!(action, Action::Revise) {
                let path = self.bundle.trim();
                if path.is_empty() {
                    return;
                }
                Some(
                    profile_catalog
                        .parent()
                        .unwrap_or(Path::new("."))
                        .join(path),
                )
            } else {
                None
            };
            let editor = Editor::new(source.clone());
            let ticket = editor.ticket.clone();
            self.composer = Some(editor);
            self.error = None;
            if let Some(source) = source {
                let rejected = message(
                    id,
                    Action::Loaded(ticket.clone(), Err("Bundle editor unavailable".into())),
                );
                ctx.spawn_background_with_rejection(
                    move |_| {
                        let result = DotfileBundle::load(&source)
                            .and_then(|bundle| {
                                bundle.validate_for_editing()?;
                                Ok(Box::new(bundle))
                            })
                            .map_err(|error| format!("{error:#}"));
                        message(id, Action::Loaded(ticket, result))
                    },
                    rejected,
                );
            }
            return;
        }
        let Some(editor) = &mut self.composer else {
            return;
        };
        let ticket = match &action {
            Action::Loaded(ticket, _)
            | Action::Text(ticket, ..)
            | Action::Save(ticket)
            | Action::Saved(ticket, _)
            | Action::Cancel(ticket) => ticket,
            Action::Create | Action::Revise => unreachable!(),
        };
        if !Arc::ptr_eq(&editor.ticket, ticket) {
            return;
        }
        match action {
            Action::Loaded(_, result) if editor.phase == Phase::Loading => match result {
                Ok(bundle) => editor.load(*bundle),
                Err(error) => {
                    self.error = Some(error);
                    self.composer = None;
                }
            },
            Action::Text(_, index, text) if editor.phase == Phase::Ready => {
                let fragment = &mut editor.fragments[index];
                if fragment.as_deref().unwrap_or_default() != text {
                    *fragment = (!text.is_empty()).then_some(text);
                    editor.error = None;
                }
            }
            Action::Save(ticket) if editor.phase == Phase::Ready => {
                let Some(owner) = owner else { return };
                let bundle = editor.bundle();
                if let Err(error) = bundle.validate_for_editing() {
                    editor.error = Some(format!("{error:#}"));
                    return;
                }
                let purpose = file_picker::Purpose::SaveDotfilesBundle {
                    current: editor
                        .source
                        .as_deref()
                        .unwrap_or(profile_catalog)
                        .to_owned(),
                    suggestion: editor.suggestion(),
                };
                editor.phase = Phase::Saving;
                editor.error = None;
                let rejected = message(
                    id,
                    Action::Saved(ticket.clone(), Err("Bundle save unavailable".into())),
                );
                ctx.spawn_background_with_rejection(
                    move |cancel| {
                        let result = (|| {
                            let Some(file) = file_picker::pick(owner, purpose)? else {
                                return Ok(None);
                            };
                            if cancel.is_cancelled() {
                                return Ok(None);
                            }
                            let path = file
                                .path
                                .to_str()
                                .context("The selected path cannot be represented as text")?;
                            bundle.save_new(&file.path)?;
                            Ok::<_, anyhow::Error>(Some(path.to_owned()))
                        })()
                        .map_err(|error| format!("{error:#}"));
                        message(id, Action::Saved(ticket, result))
                    },
                    rejected,
                );
            }
            Action::Saved(_, result) if editor.phase == Phase::Saving => {
                editor.phase = Phase::Ready;
                match result {
                    Ok(Some(path)) => {
                        self.select_bundle(path);
                        self.composer = None;
                    }
                    Ok(None) => {}
                    Err(error) => editor.error = Some(error),
                }
            }
            Action::Cancel(_) if editor.phase != Phase::Saving => self.composer = None,
            _ => {}
        }
    }
}
