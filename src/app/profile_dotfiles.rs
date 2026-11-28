use super::*;
use russh::keys::ssh_key::Fingerprint;
use std::path::PathBuf;
use w11term::{
    options::DotfileSync,
    sftp::{
        DotfileBundle, DotfileShell, ExportedDotfileBundle, select_startup_directory,
        validate_dotfiles_home,
    },
};

mod bundle;
mod review;

const TITLE: &str = "SSH dotfiles";

pub(super) fn startup_fields(
    shell: DotfileShell,
    zdotdir: &str,
    fish_config_dir: &str,
) -> anyhow::Result<(Option<String>, Option<String>)> {
    let zdotdir = zdotdir.trim();
    let fish_config_dir = fish_config_dir.trim();
    let zdotdir = (shell == DotfileShell::Zsh && !zdotdir.is_empty()).then_some(zdotdir);
    let fish_config_dir =
        (shell == DotfileShell::Fish && !fish_config_dir.is_empty()).then_some(fish_config_dir);
    select_startup_directory(shell, zdotdir, fish_config_dir)?;
    Ok((
        zdotdir.map(str::to_owned),
        fish_config_dir.map(str::to_owned),
    ))
}

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

pub(super) fn snapshot_fingerprint(
    draft: &ProfileDraft,
    home: &str,
    endpoint: (&str, &str, u16, &str),
) -> anyhow::Result<String> {
    let fingerprint = profile_fingerprint(draft, endpoint)?;
    validate_dotfiles_home(home)?;
    Ok(fingerprint)
}

fn profile_fingerprint(
    draft: &ProfileDraft,
    (host, user, port, fingerprint): (&str, &str, u16, &str),
) -> anyhow::Result<String> {
    let account = parse_ssh_address(&draft.host, &draft.user, &draft.port)?;
    anyhow::ensure!(
        (account.0.as_str(), account.1.as_str(), account.2) == (host, user, port),
        "This profile's SSH account differs from the verified connection"
    );
    approved_fingerprint(fingerprint, &draft.fingerprint)
}

#[derive(Clone, Copy)]
pub enum Field {
    Bundle,
    Home,
    Fingerprint,
    Zdotdir,
    FishConfigDir,
}

#[derive(Clone)]
pub enum Action {
    Expand(bool),
    Text(Field, String),
    Shell(Option<usize>),
    PinVersion(bool),
    Browse,
    Picked(Arc<()>, Result<Option<PathBuf>, String>),
    Preview,
    Previewed(Arc<()>, Result<Arc<review::Report>, String>),
    Approve,
    Reviewed(Arc<()>, Box<DotfileSync>, Result<Option<String>, String>),
    Bundle(bundle::Action),
    Disable,
}

#[derive(Clone, Default)]
pub(super) struct Editor {
    expanded: bool,
    bundle: String,
    home: String,
    fingerprint: String,
    zdotdir: String,
    fish_config_dir: String,
    shell: DotfileShell,
    approved: Option<Box<DotfileSync>>,
    pin_version: bool,
    snapshot_version: Option<String>,
    checking: Option<Arc<()>>,
    preview: review::Preview,
    browsing: Option<Arc<()>>,
    composer: Option<bundle::Editor>,
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
            fish_config_dir: settings.fish_config_dir.clone().unwrap_or_default(),
            shell: settings.shell,
            pin_version: settings.bundle_sha256.is_some(),
            approved,
            ..Default::default()
        }
    }

    pub(super) fn approval(&self) -> Option<Box<DotfileSync>> {
        self.approved.clone()
    }

    fn select_bundle(&mut self, path: String) {
        self.bundle = path;
        self.snapshot_version = None;
        self.checking = None;
        self.preview = review::Preview::Closed;
        self.error = None;
    }

    fn stage_snapshot(
        draft: &mut ProfileDraft,
        saved: ExportedDotfileBundle,
        zdotdir: Option<String>,
        fish_config_dir: Option<String>,
        endpoint: (&str, &str, u16, &str),
    ) -> anyhow::Result<()> {
        let fingerprint = snapshot_fingerprint(draft, &saved.home, endpoint)?;
        select_startup_directory(saved.shell, zdotdir.as_deref(), fish_config_dir.as_deref())?;
        let path = saved
            .local
            .into_os_string()
            .into_string()
            .map_err(|_| anyhow::anyhow!("The saved path cannot be represented as text"))?;
        let editor = &mut draft.dotfiles;
        anyhow::ensure!(
            editor.checking.is_none() && editor.browsing.is_none() && editor.composer.is_none(),
            "Finish the open dotfiles edit before selecting the saved bundle"
        );
        editor.select_bundle(path);
        editor.expanded = true;
        editor.home = saved.home;
        editor.fingerprint = fingerprint;
        editor.shell = saved.shell;
        editor.zdotdir = zdotdir.unwrap_or_default();
        editor.fish_config_dir = fish_config_dir.unwrap_or_default();
        editor.pin_version = true;
        editor.snapshot_version = Some(saved.bundle_sha256);
        Ok(())
    }

    fn candidate(&self, draft: &ProfileDraft, base: &Path) -> anyhow::Result<DotfileSync> {
        let (host, user, port) = parse_ssh_address(&draft.host, &draft.user, &draft.port)?;
        let fingerprint = approved_fingerprint(&self.fingerprint, &draft.fingerprint)?;
        anyhow::ensure!(!self.bundle.trim().is_empty(), "Choose a dotfiles bundle");
        let home = self.home.trim();
        validate_dotfiles_home(home)?;
        let shell = self.shell;
        let (zdotdir, fish_config_dir) =
            startup_fields(shell, &self.zdotdir, &self.fish_config_dir)?;
        Ok(DotfileSync {
            bundle: base.join(self.bundle.trim()),
            shell,
            bundle_sha256: self
                .approved
                .as_ref()
                .filter(|_| self.pin_version)
                .and_then(|approval| approval.bundle_sha256.clone()),
            zdotdir,
            fish_config_dir,
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
            self.pin_version == approved.bundle_sha256.is_some()
                && self
                    .candidate(draft, base)
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
        if let Some(composer) = &self.composer {
            return self.section(id, header, composer.view(id, ctx), ctx);
        }
        let can_compose =
            self.browsing.is_none() && self.checking.is_none() && !self.preview.loading();
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
                "bundle-actions",
                StackPanel::new().spacing(6.0).children((
                    Button::new()
                        .is_enabled(self.browsing.is_none())
                        .on_click(send(Action::Browse))
                        .content("Browse for dotfiles bundle"),
                    Button::new()
                        .is_enabled(can_compose)
                        .on_click(send(Action::Bundle(bundle::Action::Create)))
                        .content("Create dotfiles bundle"),
                    Button::new()
                        .is_enabled(can_compose && !self.bundle.trim().is_empty())
                        .on_click(send(Action::Bundle(bundle::Action::Revise)))
                        .content("Revise selected dotfiles bundle"),
                )),
            ),
            KeyedView::new(
                "shell",
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new().text("Dotfiles shell"),
                    ComboBox::new()
                        .automation_name("Dotfiles shell")
                        .items_source(DotfileShell::ALL.map(DotfileShell::label))
                        .selected_index(self.shell.menu_index())
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
        if self.shell == DotfileShell::Zsh {
            content.push(KeyedView::new(
                "zdotdir",
                field(
                    Field::Zdotdir,
                    "Zsh startup directory (optional)",
                    &self.zdotdir,
                    "Relative to the remote home",
                ),
            ));
        } else if self.shell == DotfileShell::Fish {
            content.push(KeyedView::new(
                "fish-config-dir",
                field(
                    Field::FishConfigDir,
                    "Fish configuration directory (optional)",
                    &self.fish_config_dir,
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
        if let Some(version) = &self.snapshot_version {
            content.push(KeyedView::new(
                "snapshot-version",
                TextBlock::new()
                    .text(format!("Saved dotfile version {version}"))
                    .text_wrapping(TextWrapping::Wrap),
            ));
        } else if let Some(version) = self
            .approved
            .as_deref()
            .filter(|_| current)
            .and_then(|settings| settings.bundle_sha256.as_deref())
        {
            content.push(KeyedView::new(
                "pinned-version",
                TextBlock::new()
                    .text(format!("Pinned dotfile version {version}"))
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        content.push(KeyedView::new(
            "preview-button",
            Button::new()
                .is_enabled(can_compose && !self.bundle.trim().is_empty())
                .on_click(send(Action::Preview))
                .content("Preview dotfile changes"),
        ));
        self.preview.describe(&mut content);
        content.extend([
            KeyedView::new(
                "pin-version",
                CheckBox::new()
                    .automation_name("Pin approved dotfile version")
                    .is_checked(self.pin_version)
                    .on_is_checked_changed(
                        ctx.callback(move |pin| {
                            Message::ProfileDotfiles(id, Action::PinVersion(pin))
                        }),
                    )
                    .content("Pin approved dotfile version"),
            ),
            KeyedView::new(
                "actions",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .is_enabled(self.checking.is_none() && self.preview.can_approve())
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
        self.section(
            id,
            header,
            StackPanel::new().spacing(8.0).keyed_children(content),
            ctx,
        )
    }

    fn section(
        &self,
        id: TabId,
        header: &str,
        content: View,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
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
                SlotView::new(ExpanderSlot::Content, content),
            ])
    }
}

impl TerminalApp {
    pub(super) fn open_profile_for_snapshot(
        &mut self,
        profile: &str,
        ctx: &ComponentContext<Self>,
    ) -> Option<(TabId, &mut ProfileDraft)> {
        self.command_reference.close();
        self.edit_profile(profile.to_owned(), ctx);
        let id = self.tabs.active_id()?;
        let Page::NewProfile(draft) = &mut self.tabs.get_mut(id)?.page else {
            return None;
        };
        draft
            .origin
            .editing()
            .is_some_and(|base| base.name == profile)
            .then_some((id, draft))
    }

    pub(super) fn open_dotfile_snapshot_profile(
        &mut self,
        profile: &str,
        saved: ExportedDotfileBundle,
        zdotdir: Option<String>,
        fish_config_dir: Option<String>,
        connection: &w11term::ssh::ConnectionShare,
        ctx: &ComponentContext<Self>,
    ) {
        let Some((_, draft)) = self.open_profile_for_snapshot(profile, ctx) else {
            return;
        };
        if let Err(error) = Editor::stage_snapshot(
            draft,
            saved,
            zdotdir,
            fish_config_dir,
            connection.verified_endpoint(),
        ) {
            draft.error = Some(format!("{error:#}"));
        }
    }

    pub(super) fn open_profile_dotfile_excerpt(
        &mut self,
        profile: &str,
        endpoint: (&str, &str, u16, &str),
        path: String,
        text: String,
        ctx: &ComponentContext<Self>,
    ) {
        let Some((_, draft)) = self.open_profile_for_snapshot(profile, ctx) else {
            return;
        };
        let (host, user, port, _) = endpoint;
        if let Err(error) = profile_fingerprint(draft, endpoint).and_then(|_| {
            draft
                .dotfiles
                .stage_remote_excerpt(&format!("{user}@{host}:{port}"), path, text)
        }) {
            draft.error = Some(format!("{error:#}"));
        }
    }

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
            Action::Bundle(action) => draft.dotfiles.edit_bundle(
                id,
                action,
                owner.map(|window| window.0 as usize),
                self.store.path(),
                ctx,
            ),
            Action::Preview => {
                let editor = &mut draft.dotfiles;
                if editor.checking.is_some()
                    || editor.preview.loading()
                    || editor.browsing.is_some()
                    || editor.composer.is_some()
                    || editor.bundle.trim().is_empty()
                {
                    return;
                }
                let path = base.join(editor.bundle.trim());
                let shell = editor.shell;
                let snapshot = editor.snapshot_version.clone();
                let approved = editor.approved.clone().map(|mut settings| {
                    settings.bundle = base.join(&settings.bundle);
                    settings
                });
                let request = Arc::new(());
                editor.preview = review::Preview::Loading(request.clone());
                editor.error = None;
                let rejected = Message::ProfileDotfiles(
                    id,
                    Action::Previewed(request.clone(), Err("Bundle preview unavailable".into())),
                );
                ctx.spawn_background_with_rejection(
                    move |_| {
                        let result = review::Report::read(
                            &path,
                            shell,
                            snapshot.as_deref(),
                            approved.as_deref(),
                        )
                        .map(Arc::new)
                        .map_err(|err| format!("Previewing dotfile changes: {err:#}"));
                        Message::ProfileDotfiles(id, Action::Previewed(request, result))
                    },
                    rejected,
                );
            }
            Action::Previewed(request, result) => {
                let editor = &mut draft.dotfiles;
                if !matches!(&editor.preview, review::Preview::Loading(current) if Arc::ptr_eq(current, &request))
                {
                    return;
                }
                editor.preview = match result {
                    Ok(report) => review::Preview::Ready(report),
                    Err(error) => {
                        editor.error = Some(error);
                        review::Preview::Failed
                    }
                };
            }
            Action::Approve => {
                if draft.dotfiles.checking.is_some() || !draft.dotfiles.preview.can_approve() {
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
                let snapshot_version = draft.dotfiles.snapshot_version.clone();
                let preview_version = draft.dotfiles.preview.digest();
                let pin_version = draft.dotfiles.pin_version;
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
                            .and_then(|bundle| {
                                if let Some(version) = preview_version {
                                    use anyhow::Context;
                                    bundle.verify_version(settings.shell, &version).context(
                                        "The bundle changed since the preview. Preview it again",
                                    )?;
                                    return Ok(pin_version.then_some(version));
                                }
                                match (snapshot_version, pin_version) {
                                    (Some(version), pin) => {
                                        bundle.verify_version(settings.shell, &version)?;
                                        Ok(pin.then_some(version))
                                    }
                                    (None, true) => {
                                        bundle.content_version(settings.shell).map(Some)
                                    }
                                    (None, false) => bundle.validate(settings.shell).map(|()| None),
                                }
                            })
                            .map_err(|err| format!("Checking the dotfiles bundle: {err:#}"));
                        Message::ProfileDotfiles(id, Action::Reviewed(request, settings, result))
                    },
                    rejected,
                );
            }
            Action::Reviewed(request, mut settings, result) => {
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
                    Ok(version) => {
                        settings.bundle_sha256 = version;
                        draft.dotfiles.approved = Some(settings);
                        draft.dotfiles.snapshot_version = None;
                        draft.dotfiles.preview = review::Preview::Closed;
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
                            draft.dotfiles.select_bundle(path);
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
                    Field::FishConfigDir => &mut editor.fish_config_dir,
                };
                if *target != text {
                    *target = text;
                    if matches!(field, Field::Bundle) {
                        editor.browsing = None;
                        editor.snapshot_version = None;
                        editor.preview = review::Preview::Closed;
                    }
                    editor.checking = None;
                    editor.error = None;
                }
            }
            Action::Shell(index) => {
                if let Some(shell) = index.and_then(|index| DotfileShell::ALL.get(index).copied())
                    && draft.dotfiles.shell != shell
                {
                    draft.dotfiles.shell = shell;
                    draft.dotfiles.snapshot_version = None;
                    draft.dotfiles.preview = review::Preview::Closed;
                    draft.dotfiles.checking = None;
                    draft.dotfiles.error = None;
                }
            }
            Action::PinVersion(pin) => {
                if draft.dotfiles.pin_version != pin {
                    draft.dotfiles.pin_version = pin;
                    draft.dotfiles.checking = None;
                    draft.dotfiles.error = None;
                }
            }
            Action::Disable => {
                draft.dotfiles.approved = None;
                draft.dotfiles.checking = None;
                draft.dotfiles.preview = review::Preview::Closed;
                draft.dotfiles.error = None;
                draft.saved = false;
            }
        }
    }
}
