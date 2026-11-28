use super::*;
use crate::app::shell_setup::Setup;
use anyhow::Context;
use std::cell::OnceCell;

mod remote_excerpt;
mod shell_integration;

const FRAGMENTS: [&str; 5] = [
    "Shared environment fragment",
    "Bash startup fragment",
    "Zsh startup fragment",
    "Fish environment fragment",
    "Fish startup fragment",
];
const IMPORT_ITEMS: [&str; FRAGMENTS.len()] = [
    "Append to shared environment",
    "Append to Bash fragment",
    "Append to Zsh fragment",
    "Append to Fish environment",
    "Append to Fish fragment",
];

#[derive(Clone)]
pub enum Action {
    Create,
    Revise,
    Loaded(Arc<()>, Result<Box<DotfileBundle>, String>),
    Text(Arc<()>, usize, String),
    Import(Arc<()>, usize),
    Imported(Arc<()>, Result<Option<String>, String>),
    AddSetup(Arc<()>, Setup),
    Excerpt(Arc<()>, Arc<()>, ExcerptAction),
    Save(Arc<()>),
    Saved(Arc<()>, Result<Option<String>, String>),
    Cancel(Arc<()>),
}

#[derive(Clone)]
pub enum ExcerptAction {
    First(String),
    Last(String),
    Append(usize),
    Close,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Loading,
    Ready,
    Importing(usize),
    Saving,
}

#[derive(Clone)]
pub(super) struct Editor {
    ticket: Arc<()>,
    source: Option<PathBuf>,
    fragments: [Option<String>; FRAGMENTS.len()],
    import_menu: OnceCell<Menu>,
    setup_menu: OnceCell<Menu>,
    remote: Option<Box<remote_excerpt::Source>>,
    phase: Phase,
    status: Option<String>,
}

fn message(id: TabId, action: Action) -> Message {
    Message::ProfileDotfiles(id, super::Action::Bundle(action))
}

fn editor_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r")
}

fn fragment_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
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
            import_menu: OnceCell::new(),
            setup_menu: OnceCell::new(),
            remote: None,
            phase,
            status: None,
        }
    }

    fn load(&mut self, bundle: DotfileBundle) {
        self.fragments = [
            bundle.environment,
            bundle.bash,
            bundle.zsh,
            bundle.fish_environment,
            bundle.fish,
        ]
        .map(|text| text.as_deref().map(editor_text));
        self.phase = Phase::Ready;
    }

    fn bundle(&self) -> DotfileBundle {
        let [environment, bash, zsh, fish_environment, fish] = self
            .fragments
            .each_ref()
            .map(|text| text.as_deref().map(fragment_text));
        DotfileBundle {
            version: 1,
            environment,
            bash,
            zsh,
            fish_environment,
            fish,
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

    fn add_setup(&mut self, setup: Setup) {
        let index = match setup.shell() {
            DotfileShell::Bash => 1,
            DotfileShell::Zsh => 2,
            DotfileShell::Fish => 4,
        };
        let existing = fragment_text(self.fragments[index].as_deref().unwrap_or_default());
        self.status = Some(match shell_integration::insert(&existing, setup) {
            Ok(updated) if updated == existing => {
                format!("{} already includes this setup.", FRAGMENTS[index])
            }
            Ok(updated) => {
                self.fragments[index] = Some(editor_text(&updated));
                format!("{} now includes the selected setup.", FRAGMENTS[index])
            }
            Err(error) => format!("{error:#}"),
        });
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
            if let Some(remote) = &self.remote {
                views.push(KeyedView::new(
                    "remote-excerpt",
                    remote.view(id, &self.ticket, self.phase == Phase::Ready, ctx),
                ));
            }
            let menu = self.import_menu.get_or_init(|| {
                let ticket = self.ticket.clone();
                Menu::new(
                    IMPORT_ITEMS.map(|name| MenuItem::item(name, name)),
                    ctx.callback(move |name: String| {
                        IMPORT_ITEMS
                            .iter()
                            .position(|item| *item == name)
                            .map_or(Message::ControlIdle, |index| {
                                message(id, Action::Import(ticket.clone(), index))
                            })
                    }),
                )
            });
            views.push(KeyedView::new(
                "bundle-editor-import",
                Button::new()
                    .automation_name("Import shell fragment")
                    .is_enabled(self.phase == Phase::Ready)
                    .content("Import shell fragment")
                    .menu(menu.clone()),
            ));
            let setup_menu = self.setup_menu.get_or_init(|| {
                let ticket = self.ticket.clone();
                Menu::new(
                    Setup::ALL
                        .map(|setup| MenuItem::item(setup.bundle_label(), setup.bundle_label())),
                    ctx.callback(move |label: String| {
                        Setup::ALL
                            .into_iter()
                            .find(|setup| setup.bundle_label() == label)
                            .map_or(Message::ControlIdle, |setup| {
                                message(id, Action::AddSetup(ticket.clone(), setup))
                            })
                    }),
                )
            });
            views.push(KeyedView::new(
                "bundle-editor-setup",
                Button::new()
                    .automation_name("Add shell setup")
                    .is_enabled(self.phase == Phase::Ready)
                    .content("Add shell setup")
                    .menu(setup_menu.clone()),
            ));
            for (index, name) in FRAGMENTS.into_iter().enumerate() {
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
        if let Some(status) = self.status.as_deref().or(match self.phase {
            Phase::Loading => Some("Reading the selected dotfiles bundle…"),
            Phase::Importing(_) => Some("Reading the selected shell fragment…"),
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
                        .is_enabled(matches!(self.phase, Phase::Loading | Phase::Ready))
                        .on_click(send(Action::Cancel(self.ticket.clone())))
                        .content("Cancel dotfiles editing"),
                )),
        ));
        StackPanel::new().spacing(8.0).keyed_children(views)
    }
}

impl super::Editor {
    pub(super) fn stage_remote_excerpt(
        &mut self,
        endpoint: &str,
        path: String,
        text: String,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.checking.is_none()
                && self.browsing.is_none()
                && self
                    .composer
                    .as_ref()
                    .is_none_or(|editor| editor.phase == Phase::Ready),
            "Finish the open dotfiles operation before importing SSH text"
        );
        let remote = remote_excerpt::Source::new(endpoint, path, text)?;
        let editor = self.composer.get_or_insert_with(|| Editor::new(None));
        editor.remote = Some(Box::new(remote));
        editor.status = None;
        self.error = None;
        self.expanded = true;
        Ok(())
    }

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
            | Action::Import(ticket, _)
            | Action::Imported(ticket, _)
            | Action::AddSetup(ticket, _)
            | Action::Excerpt(ticket, ..)
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
                    editor.status = None;
                }
            }
            Action::AddSetup(_, setup) if editor.phase == Phase::Ready => editor.add_setup(setup),
            Action::Import(ticket, index) if editor.phase == Phase::Ready => {
                let Some(owner) = owner else { return };
                let existing =
                    fragment_text(editor.fragments[index].as_deref().unwrap_or_default());
                let current = editor
                    .source
                    .as_deref()
                    .unwrap_or(profile_catalog)
                    .to_owned();
                editor.phase = Phase::Importing(index);
                editor.status = None;
                let rejected = message(
                    id,
                    Action::Imported(ticket.clone(), Err("Fragment import unavailable".into())),
                );
                ctx.spawn_background_with_rejection(
                    move |cancel| {
                        let result = (|| {
                            let Some(file) = file_picker::pick(
                                owner,
                                file_picker::Purpose::DotfileFragment(current),
                            )?
                            else {
                                return Ok(None);
                            };
                            if cancel.is_cancelled() {
                                return Ok(None);
                            }
                            DotfileBundle::append_fragment_from_file(&file.path, &existing)
                                .map(Some)
                        })()
                        .map_err(|error| format!("Importing shell fragment: {error:#}"));
                        message(id, Action::Imported(ticket, result))
                    },
                    rejected,
                );
            }
            Action::Imported(_, result) => {
                if let Phase::Importing(index) = editor.phase {
                    editor.phase = Phase::Ready;
                    match result {
                        Ok(Some(text)) => editor.fragments[index] = Some(editor_text(&text)),
                        Ok(None) => {}
                        Err(error) => editor.status = Some(error),
                    }
                }
            }
            Action::Excerpt(_, source, action) if editor.phase == Phase::Ready => {
                if let Some(remote) = &mut editor.remote
                    && Arc::ptr_eq(&remote.ticket, &source)
                {
                    if matches!(action, ExcerptAction::Close) {
                        editor.remote = None;
                    } else {
                        remote.edit(action, &mut editor.fragments);
                    }
                }
            }
            Action::Save(ticket) if editor.phase == Phase::Ready => {
                let Some(owner) = owner else { return };
                let bundle = editor.bundle();
                if let Err(error) = bundle.validate_for_editing() {
                    editor.status = Some(format!("{error:#}"));
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
                editor.status = None;
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
                    Err(error) => editor.status = Some(error),
                }
            }
            Action::Cancel(_) if matches!(editor.phase, Phase::Loading | Phase::Ready) => {
                self.composer = None
            }
            _ => {}
        }
    }
}
