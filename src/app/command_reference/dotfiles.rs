use super::*;
use serde::Deserialize;
use std::path::PathBuf;
use w11term::sftp::{DotfileBundle, DotfileShell, validate_zdotdir};

pub enum Action {
    Open(ToolPane),
    Browse,
    Picked(Arc<()>, Result<Option<PathBuf>, String>),
    Shell(Option<usize>),
    Zdotdir(String),
    Preview,
    Previewed(Arc<()>, Result<Box<Review>, String>),
    Apply,
    Applied(Arc<()>, Result<Applied, String>),
    Close,
}

#[derive(Deserialize)]
pub struct Review {
    home: String,
    bundle_sha256: String,
    active_bundle_sha256: Option<String>,
    directories_to_create: Vec<String>,
    files: Vec<Edit>,
    supports_atomic_replace: bool,
    review: String,
}

#[derive(Deserialize)]
struct Edit {
    path: String,
    action: String,
    before_sha256: Option<String>,
    after_sha256: Option<String>,
}

impl Edit {
    fn blocker(&self, atomic_replace: bool) -> Option<&'static str> {
        if self.after_sha256.is_none() {
            Some(match self.action.as_str() {
                "hook-conflict" => "Startup hook conflict",
                "payload-conflict" => "Managed fragment conflict",
                "selector-conflict" => "Active bundle selector conflict",
                _ => "File conflict",
            })
        } else if self.before_sha256.is_some()
            && self.before_sha256 != self.after_sha256
            && !atomic_replace
        {
            Some("Server cannot atomically replace")
        } else {
            None
        }
    }

    fn label(&self, atomic_replace: bool) -> &str {
        self.blocker(atomic_replace)
            .unwrap_or(match self.action.as_str() {
                "create" => "Create",
                "append-hook" => "Add startup hook",
                "unchanged" => "Already current",
                "replace" => "Switch active bundle",
                action => action,
            })
    }
}

impl Review {
    fn ready(&self) -> bool {
        self.files
            .iter()
            .all(|edit| edit.blocker(self.supports_atomic_replace).is_none())
    }

    fn describe(&self, fields: &mut Vec<KeyedView>) {
        fields.push(detail("home", format!("SFTP home {}", self.home)));
        if let Some(active) = &self.active_bundle_sha256 {
            fields.push(detail("active", format!("Active version {active}")));
        }
        fields.push(detail(
            "reviewed-version",
            format!("Reviewed version {}", self.bundle_sha256),
        ));
        if !self.ready() {
            fields.push(detail(
                "blocked",
                "Resolve the listed conflicts before applying".into(),
            ));
        }
        for (index, path) in self.directories_to_create.iter().enumerate() {
            fields.push(detail(
                format!("directory-{index}"),
                format!("Create directory · {path}"),
            ));
        }
        for (index, edit) in self.files.iter().enumerate() {
            fields.push(detail(
                format!("file-{index}"),
                format!(
                    "{} · {}",
                    edit.label(self.supports_atomic_replace),
                    edit.path
                ),
            ));
        }
    }
}

#[derive(Deserialize)]
pub struct Applied {
    changed_files: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Work {
    Browse,
    Preview,
    Apply,
}

pub(super) struct Installer {
    target: ToolPane,
    account: String,
    bundle: Option<PathBuf>,
    shell: DotfileShell,
    zdotdir: String,
    pending: Option<(Arc<()>, Work)>,
    review: Option<Box<Review>>,
    status: Option<String>,
}

fn message(action: Action) -> Message {
    Message::CommandReference(super::Action::Dotfiles(action))
}

fn detail(key: impl Into<String>, text: String) -> KeyedView {
    KeyedView::new(
        key.into(),
        TextBlock::new()
            .text(text)
            .text_wrapping(TextWrapping::Wrap),
    )
}

impl Installer {
    fn begin(&mut self, work: Work) -> Arc<()> {
        let ticket = Arc::new(());
        self.pending = Some((ticket.clone(), work));
        self.status = None;
        ticket
    }

    fn finish(&mut self, ticket: &Arc<()>, work: Work) -> bool {
        self.pending
            .take_if(|(pending, kind)| *kind == work && Arc::ptr_eq(pending, ticket))
            .is_some()
    }

    fn applying(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|(_, work)| *work == Work::Apply)
    }

    pub(super) fn view(&self, current: Option<ToolPane>, ctx: &ViewContext<TerminalApp>) -> View {
        let connected = current == Some(self.target);
        let ready = connected && self.pending.is_none();
        let button = |label: &str, enabled, action: fn() -> Action| {
            Button::new()
                .is_enabled(enabled)
                .on_click(ctx.callback(move |_| message(action())))
                .content(label)
        };
        let mut fields = vec![
            KeyedView::new(
                "heading",
                TextBlock::new().text("Apply SSH dotfiles").font_size(18.0),
            ),
            detail("account", self.account.clone()),
            detail(
                "source",
                self.bundle.as_ref().map_or_else(
                    || "Choose a local dotfiles bundle".into(),
                    |path| path.display().to_string(),
                ),
            ),
            KeyedView::new(
                "browse",
                button("Browse for dotfiles bundle", ready, || Action::Browse),
            ),
            KeyedView::new(
                "shell",
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new().text("Target shell"),
                    ComboBox::new()
                        .automation_name("Target dotfiles shell")
                        .items_source(["Bash", "Zsh"])
                        .selected_index(usize::from(self.shell == DotfileShell::Zsh))
                        .is_enabled(ready)
                        .on_selection_changed(ctx.callback(|index| message(Action::Shell(index)))),
                )),
            ),
        ];
        if self.shell == DotfileShell::Zsh {
            fields.push(KeyedView::new(
                "zdotdir",
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new().text("Zsh startup directory (optional)"),
                    TextBox::new()
                        .automation_name("Dotfiles Zsh startup directory")
                        .placeholder_text("Relative to the SFTP home")
                        .text(&self.zdotdir)
                        .is_enabled(ready)
                        .on_text_changed(ctx.callback(|text| message(Action::Zdotdir(text)))),
                )),
            ));
        }
        fields.push(KeyedView::new(
            "preview",
            button(
                "Review dotfiles changes",
                ready && self.bundle.is_some(),
                || Action::Preview,
            ),
        ));
        if let Some(review) = &self.review {
            review.describe(&mut fields);
            fields.push(KeyedView::new(
                "apply",
                button("Apply reviewed dotfiles", ready && review.ready(), || {
                    Action::Apply
                }),
            ));
        }
        if !connected {
            fields.push(detail(
                "connection",
                "Return to the original SSH connection, or reopen this panel after reconnecting"
                    .into(),
            ));
        }
        if let Some(status) = self.status.clone().or_else(|| {
            self.pending.as_ref().map(|(_, work)| {
                match work {
                    Work::Browse => "Selecting a local dotfiles bundle…",
                    Work::Preview => "Reading remote dotfiles…",
                    Work::Apply => "Applying reviewed dotfiles…",
                }
                .to_owned()
            })
        }) {
            fields.push(detail("status", status));
        }
        fields.push(KeyedView::new(
            "back",
            button("Back to command reference", !self.applying(), || {
                Action::Close
            }),
        ));
        ScrollViewer::new().content(
            StackPanel::new()
                .margin(12.0)
                .spacing(8.0)
                .keyed_children(fields),
        )
    }
}

impl TerminalApp {
    pub(super) fn reference_dotfiles_action(
        &mut self,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        let current = self
            .reference_tool_target()
            .map(|(target, session)| (target, session.input.clone()));
        if let Action::Open(target) = action {
            if current.as_ref().is_none_or(|(pane, _)| *pane != target)
                || !self.command_reference.open
                || self.command_reference.subpanel_open()
            {
                return;
            }
            let Some(Page::Terminal(live)) = self.tabs.active().map(|tab| &tab.page) else {
                return;
            };
            let Source::Ssh(ssh) = &live.source else {
                return;
            };
            let settings = ssh.dotfiles.as_deref();
            self.command_reference.draft = None;
            self.command_reference.dotfiles = Some(Installer {
                target,
                account: format!("{}@{}:{}", ssh.user, ssh.host, ssh.port),
                bundle: settings.map(|settings| settings.bundle.clone()),
                shell: settings.map_or(DotfileShell::Bash, |settings| settings.shell),
                zdotdir: settings
                    .and_then(|settings| settings.zdotdir.clone())
                    .unwrap_or_default(),
                pending: None,
                review: None,
                status: None,
            });
            return;
        }
        let owner = self.keyboard.hwnd().map(|window| window.0 as usize);
        let panel = &mut self.command_reference;
        let Some(installer) = &mut panel.dotfiles else {
            return;
        };
        let target = installer.target;
        let connected = current.as_ref().is_some_and(|(pane, _)| *pane == target);
        let idle = connected && installer.pending.is_none();
        match action {
            Action::Browse if idle => {
                let Some(owner) = owner else { return };
                let path = installer
                    .bundle
                    .clone()
                    .unwrap_or_else(|| self.store.path().to_owned());
                let ticket = installer.begin(Work::Browse);
                let rejected = message(Action::Picked(
                    ticket.clone(),
                    Err("File picker unavailable".into()),
                ));
                ctx.spawn_background_with_rejection(
                    move |scope| {
                        let result = if scope.is_cancelled() {
                            Ok(None)
                        } else {
                            file_picker::pick(owner, file_picker::Purpose::DotfilesBundle(path))
                                .map(|selected| selected.map(|file| file.path))
                                .map_err(|error| format!("{error:#}"))
                        };
                        message(Action::Picked(ticket, result))
                    },
                    rejected,
                );
            }
            Action::Picked(ticket, result) => {
                if installer.finish(&ticket, Work::Browse) {
                    match result {
                        Ok(Some(path)) => {
                            installer.bundle = Some(path);
                            installer.review = None;
                        }
                        Ok(None) => {}
                        Err(error) => installer.status = Some(error),
                    }
                }
            }
            Action::Shell(Some(index @ 0..=1)) if idle => {
                let shell = if index == 0 {
                    DotfileShell::Bash
                } else {
                    DotfileShell::Zsh
                };
                if installer.shell != shell {
                    installer.shell = shell;
                    installer.review = None;
                    installer.status = None;
                }
            }
            Action::Zdotdir(text) if idle && installer.shell == DotfileShell::Zsh => {
                if installer.zdotdir != text {
                    installer.zdotdir = text;
                    installer.review = None;
                    installer.status = None;
                }
            }
            Action::Preview | Action::Apply if idle => {
                let Some((_, input)) = current else { return };
                let Some(path) = installer.bundle.clone() else {
                    return;
                };
                let apply = matches!(action, Action::Apply);
                let review = if apply {
                    let Some(plan) = installer.review.take_if(|plan| plan.ready()) else {
                        return;
                    };
                    Some(plan.review)
                } else {
                    installer.review = None;
                    None
                };
                let shell = installer.shell;
                let zdotdir = installer.zdotdir.trim();
                let zdotdir =
                    (shell == DotfileShell::Zsh && !zdotdir.is_empty()).then_some(zdotdir);
                if let Err(error) = validate_zdotdir(shell, zdotdir) {
                    installer.status = Some(format!("{error:#}"));
                    return;
                }
                let zdotdir = zdotdir.map(str::to_owned);
                let ticket = installer.begin(if apply { Work::Apply } else { Work::Preview });
                let rejected = if apply {
                    message(Action::Applied(
                        ticket.clone(),
                        Err("Dotfiles worker unavailable. Review again before applying.".into()),
                    ))
                } else {
                    message(Action::Previewed(
                        ticket.clone(),
                        Err("Dotfiles review worker unavailable".into()),
                    ))
                };
                ctx.spawn_background_with_rejection(
                    move |scope| {
                        let result = (|| {
                            let bundle = DotfileBundle::load(&path)
                                .map_err(|error| format!("Reading dotfiles bundle: {error:#}"))?;
                            if scope.is_cancelled() {
                                return Err("Dotfiles operation cancelled".into());
                            }
                            let operation = match review {
                                Some(review) => Operation::ApplyDotfiles {
                                    bundle,
                                    shell,
                                    zdotdir,
                                    review,
                                },
                                None => Operation::PreviewDotfiles {
                                    bundle,
                                    shell,
                                    zdotdir,
                                },
                            };
                            let timeout = Duration::from_secs(if apply { 120 } else { 30 });
                            run_pane_sftp_job(input, target, operation, timeout, None)
                        })();
                        if apply {
                            message(Action::Applied(ticket, decode(result)))
                        } else {
                            message(Action::Previewed(ticket, decode(result)))
                        }
                    },
                    rejected,
                );
            }
            Action::Previewed(ticket, result) => {
                if installer.finish(&ticket, Work::Preview) {
                    match result {
                        Ok(review) => installer.review = Some(review),
                        Err(error) => installer.status = Some(error),
                    }
                }
            }
            Action::Applied(ticket, result) => {
                if installer.finish(&ticket, Work::Apply) {
                    installer.status = Some(match result {
                        Ok(applied) if applied.changed_files.is_empty() => {
                            "Dotfiles are already current".into()
                        }
                        Ok(applied) => format!(
                            "Dotfiles applied · {} files changed",
                            applied.changed_files.len()
                        ),
                        Err(error) => error,
                    });
                }
            }
            Action::Close if !installer.applying() => {
                panel.dotfiles = None;
                panel.focus = panel.focus.wrapping_add(1);
            }
            _ => {}
        }
    }
}

fn decode<T: serde::de::DeserializeOwned>(
    result: Result<serde_json::Value, String>,
) -> Result<T, String> {
    serde_json::from_value(result?).map_err(|error| format!("Reading the dotfiles report: {error}"))
}
