use super::*;
use russh::keys::ssh_key::Fingerprint;
use std::path::PathBuf;
use w11term::{
    options::DotfileSync,
    sftp::{DotfileBundle, DotfileShell, validate_dotfiles_home, validate_zdotdir},
};

const TITLE: &str = "SSH dotfiles";

pub(super) fn approved_fingerprint(entered: &str, profile_pin: &str) -> anyhow::Result<String> {
    let profile_pin = profile_pin.trim();
    let entered = entered.trim();
    let fingerprint = if entered.is_empty() {
        profile_pin
    } else {
        entered
    };
    let Ok(fingerprint @ Fingerprint::Sha256(_)) = fingerprint.parse() else {
        anyhow::bail!("Enter the verified SHA256 host-key fingerprint for this account");
    };
    if let Ok(pin) = profile_pin.parse::<Fingerprint>() {
        anyhow::ensure!(
            pin == fingerprint,
            "The fingerprint must match the profile's host-key pin"
        );
    }
    Ok(fingerprint.to_string())
}

#[derive(Clone, Copy)]
pub enum Field {
    Bundle,
    Home,
    Fingerprint,
    Zdotdir,
}

#[derive(Clone)]
pub enum Action {
    Expand(bool),
    Text(Field, String),
    Shell(Option<usize>),
    Browse,
    Picked(Arc<()>, Result<Option<PathBuf>, String>),
    Approve,
    Reviewed(Arc<()>, Box<DotfileSync>, Result<(), String>),
    Disable,
}

#[derive(Clone, Default)]
pub(super) struct Editor {
    expanded: bool,
    bundle: String,
    home: String,
    fingerprint: String,
    zdotdir: String,
    zsh: bool,
    approved: Option<Box<DotfileSync>>,
    checking: Option<Arc<()>>,
    browsing: Option<Arc<()>>,
    error: Option<String>,
}

impl Editor {
    pub(super) fn new(approved: Option<Box<DotfileSync>>) -> Self {
        let Some(settings) = &approved else {
            return Self::default();
        };
        Self {
            bundle: settings.bundle.to_string_lossy().into_owned(),
            home: settings.home.clone(),
            fingerprint: settings.fingerprint.clone(),
            zdotdir: settings.zdotdir.clone().unwrap_or_default(),
            zsh: settings.shell == DotfileShell::Zsh,
            approved,
            ..Default::default()
        }
    }

    pub(super) fn approval(&self) -> Option<Box<DotfileSync>> {
        self.approved.clone()
    }

    fn candidate(&self, draft: &ProfileDraft, base: &Path) -> anyhow::Result<DotfileSync> {
        let (host, user, port) = parse_ssh_address(&draft.host, &draft.user, &draft.port)?;
        let fingerprint = approved_fingerprint(&self.fingerprint, &draft.fingerprint)?;
        anyhow::ensure!(!self.bundle.trim().is_empty(), "Choose a dotfiles bundle");
        let home = self.home.trim();
        validate_dotfiles_home(home)?;
        let shell = if self.zsh {
            DotfileShell::Zsh
        } else {
            DotfileShell::Bash
        };
        let zdotdir = self.zdotdir.trim();
        let zdotdir = (self.zsh && !zdotdir.is_empty()).then_some(zdotdir);
        validate_zdotdir(shell, zdotdir)?;
        Ok(DotfileSync {
            bundle: base.join(self.bundle.trim()),
            shell,
            zdotdir: zdotdir.map(str::to_owned),
            host,
            user,
            port,
            fingerprint,
            home: home.to_owned(),
        })
    }

    pub(super) fn view(
        &self,
        id: TabId,
        draft: &ProfileDraft,
        base: &Path,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let send =
            |action: Action| ctx.callback(move |_| Message::ProfileDotfiles(id, action.clone()));
        let field = |which, label: &str, value: &str, placeholder: &str| {
            StackPanel::new().spacing(4.0).children((
                TextBlock::new().text(label),
                TextBox::new()
                    .text(value)
                    .placeholder_text(placeholder)
                    .automation_name(label)
                    .on_text_changed(ctx.callback(move |text| {
                        Message::ProfileDotfiles(id, Action::Text(which, text))
                    })),
            ))
        };
        let current = self.approved.as_deref().is_some_and(|approved| {
            self.candidate(draft, base)
                .is_ok_and(|candidate| candidate == *approved)
        });
        let (header, status) = match &self.approved {
            Some(settings) if current => (
                "SSH dotfiles · approved",
                format!(
                    "Approved for {}@{}:{} · {}",
                    settings.user, settings.host, settings.port, settings.home
                ),
            ),
            Some(_) => (
                "SSH dotfiles · review changes",
                "Approve the edited dotfiles settings to update the saved approval".into(),
            ),
            None => (TITLE, "Sync off".into()),
        };
        let mut content = vec![
            KeyedView::new(
                "bundle",
                field(
                    Field::Bundle,
                    "Dotfiles bundle",
                    &self.bundle,
                    "Path to a dotfiles JSON bundle",
                ),
            ),
            KeyedView::new(
                "browse",
                Button::new()
                    .is_enabled(self.browsing.is_none())
                    .on_click(send(Action::Browse))
                    .content("Browse for dotfiles bundle"),
            ),
            KeyedView::new(
                "shell",
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new().text("Dotfiles shell"),
                    ComboBox::new()
                        .automation_name("Dotfiles shell")
                        .items_source(["Bash", "Zsh"])
                        .selected_index(usize::from(self.zsh))
                        .on_selection_changed(ctx.callback(move |index| {
                            Message::ProfileDotfiles(id, Action::Shell(index))
                        })),
                )),
            ),
            KeyedView::new(
                "home",
                field(
                    Field::Home,
                    "Remote SFTP home",
                    &self.home,
                    "/home/operator",
                ),
            ),
        ];
        if self.zsh {
            content.push(KeyedView::new(
                "zdotdir",
                field(
                    Field::Zdotdir,
                    "Zsh startup directory (optional)",
                    &self.zdotdir,
                    "Relative to the remote home",
                ),
            ));
        }
        content.push(KeyedView::new(
            "fingerprint",
            field(
                Field::Fingerprint,
                "Dotfiles host-key fingerprint",
                &self.fingerprint,
                "Uses the profile host fingerprint when blank",
            ),
        ));
        if let Ok((host, user, port)) = parse_ssh_address(&draft.host, &draft.user, &draft.port) {
            content.push(KeyedView::new(
                "destination",
                TextBlock::new()
                    .text(format!("Upload on connect to {user}@{host}:{port}"))
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        content.extend([
            KeyedView::new(
                "actions",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .is_enabled(self.checking.is_none())
                            .on_click(send(Action::Approve))
                            .content("Approve dotfiles sync"),
                        Button::new()
                            .is_enabled(self.approved.is_some())
                            .on_click(send(Action::Disable))
                            .content("Disable dotfiles sync"),
                    )),
            ),
            KeyedView::new(
                "status",
                TextBlock::new()
                    .text(self.error.as_deref().unwrap_or(&status))
                    .text_wrapping(TextWrapping::Wrap),
            ),
        ]);
        Expander::new()
            .automation_name(TITLE)
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .is_expanded(self.expanded)
            .on_is_expanded_changed(
                ctx.callback(move |expanded| {
                    Message::ProfileDotfiles(id, Action::Expand(expanded))
                }),
            )
            .slots([
                SlotView::new(ExpanderSlot::Header, TextBlock::new().text(header)),
                SlotView::new(
                    ExpanderSlot::Content,
                    StackPanel::new().spacing(8.0).keyed_children(content),
                ),
            ])
    }
}

impl TerminalApp {
    pub(super) fn edit_profile_dotfiles(
        &mut self,
        id: TabId,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        let owner = self.keyboard.hwnd();
        let base = self.store.path().parent().unwrap_or(Path::new("."));
        let Some(AppTab {
            page: Page::NewProfile(draft),
            ..
        }) = self.tabs.get_mut(id)
        else {
            return;
        };
        match action {
            Action::Approve => {
                if draft.dotfiles.checking.is_some() {
                    return;
                }
                let settings = match draft.dotfiles.candidate(draft, base) {
                    Ok(settings) => Box::new(settings),
                    Err(error) => {
                        draft.dotfiles.error = Some(format!("{error:#}"));
                        return;
                    }
                };
                let request = Arc::new(());
                draft.dotfiles.checking = Some(request.clone());
                draft.dotfiles.error = None;
                let rejected = Message::ProfileDotfiles(
                    id,
                    Action::Reviewed(
                        request.clone(),
                        settings.clone(),
                        Err("Bundle validation unavailable".into()),
                    ),
                );
                ctx.spawn_background_with_rejection(
                    move |_| {
                        let result = DotfileBundle::load(&settings.bundle)
                            .and_then(|bundle| bundle.validate(settings.shell))
                            .map_err(|err| format!("Checking the dotfiles bundle: {err:#}"));
                        Message::ProfileDotfiles(id, Action::Reviewed(request, settings, result))
                    },
                    rejected,
                );
            }
            Action::Reviewed(request, settings, result) => {
                if draft
                    .dotfiles
                    .checking
                    .take_if(|current| Arc::ptr_eq(current, &request))
                    .is_none()
                {
                    return;
                }
                if !draft
                    .dotfiles
                    .candidate(draft, base)
                    .is_ok_and(|current| current == *settings)
                {
                    return;
                }
                match result {
                    Ok(()) => {
                        draft.dotfiles.approved = Some(settings);
                        draft.dotfiles.error = None;
                        draft.saved = false;
                    }
                    Err(error) => draft.dotfiles.error = Some(error),
                }
            }
            Action::Browse => {
                if draft.dotfiles.browsing.is_some() {
                    return;
                }
                let Some(owner) = owner else {
                    return;
                };
                let current = if draft.dotfiles.bundle.trim().is_empty() {
                    self.store.path().to_owned()
                } else {
                    base.join(draft.dotfiles.bundle.trim())
                };
                let owner = owner.0 as usize;
                let request = Arc::new(());
                draft.dotfiles.browsing = Some(request.clone());
                let rejected = Message::ProfileDotfiles(
                    id,
                    Action::Picked(request.clone(), Err("File picker unavailable".into())),
                );
                ctx.spawn_background_with_rejection(
                    move |cancel| {
                        let result = if cancel.is_cancelled() {
                            Ok(None)
                        } else {
                            file_picker::pick(owner, file_picker::Purpose::DotfilesBundle(current))
                                .map(|chosen| chosen.map(|file| file.path))
                                .map_err(|error| format!("{error:#}"))
                        };
                        Message::ProfileDotfiles(id, Action::Picked(request, result))
                    },
                    rejected,
                );
            }
            Action::Picked(request, result) => {
                if draft
                    .dotfiles
                    .browsing
                    .take_if(|current| Arc::ptr_eq(current, &request))
                    .is_none()
                {
                    return;
                }
                match result {
                    Ok(Some(path)) => match path.into_os_string().into_string() {
                        Ok(path) => {
                            draft.dotfiles.bundle = path;
                            draft.dotfiles.checking = None;
                            draft.dotfiles.error = None;
                        }
                        Err(_) => {
                            draft.dotfiles.error =
                                Some("The selected path cannot be represented as text".into())
                        }
                    },
                    Ok(None) => {}
                    Err(error) => draft.dotfiles.error = Some(error),
                }
            }
            Action::Expand(expanded) => draft.dotfiles.expanded = expanded,
            Action::Text(field, text) => {
                let editor = &mut draft.dotfiles;
                let target = match field {
                    Field::Bundle => &mut editor.bundle,
                    Field::Home => &mut editor.home,
                    Field::Fingerprint => &mut editor.fingerprint,
                    Field::Zdotdir => &mut editor.zdotdir,
                };
                if *target != text {
                    *target = text;
                    if matches!(field, Field::Bundle) {
                        editor.browsing = None;
                    }
                    editor.checking = None;
                    editor.error = None;
                }
            }
            Action::Shell(Some(index)) => {
                if draft.dotfiles.zsh != (index == 1) {
                    draft.dotfiles.zsh = index == 1;
                    draft.dotfiles.checking = None;
                    draft.dotfiles.error = None;
                }
            }
            Action::Shell(None) => {}
            Action::Disable => {
                draft.dotfiles.approved = None;
                draft.dotfiles.checking = None;
                draft.dotfiles.error = None;
                draft.saved = false;
            }
        }
    }
}
