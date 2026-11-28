use super::*;
use serde::Deserialize;
use std::path::PathBuf;
use w11term::events::DotfileSyncStatus;
use w11term::sftp::{DotfileBundle, DotfileShell, validate_zdotdir};

mod comparison;
mod history;
use comparison::{Comparison, code_details, diff_text};

pub enum Action {
    Open(ToolPane),
    Browse,
    Picked(Arc<()>, Result<Option<PathBuf>, String>),
    Shell(Option<usize>),
    Zdotdir(String),
    Preview,
    PreviewHooks,
    Previewed(Arc<()>, Result<Box<Review>, String>),
    Apply,
    ApplyHooks,
    Applied(Arc<()>, Result<Applied, String>),
    History(history::Action),
    Close,
}

#[derive(Deserialize)]
pub struct Review {
    home: String,
    #[serde(default)]
    operation: ReviewOperation,
    bundle_sha256: Option<String>,
    active_bundle_sha256: Option<String>,
    #[serde(default)]
    directories_to_create: Vec<String>,
    files: Vec<Edit>,
    supports_atomic_replace: bool,
    fragment_changes: Option<Comparison>,
    review: String,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ReviewOperation {
    #[default]
    Install,
    RepairDotfileHooks,
}

#[derive(Deserialize)]
struct Edit {
    path: String,
    action: String,
    before_sha256: Option<String>,
    after_sha256: Option<String>,
    hook: Option<String>,
    replaced_hook: Option<String>,
    #[serde(skip)]
    hook_diff: Option<String>,
    #[serde(default)]
    add_newline_before_hook: bool,
}

impl Edit {
    fn blocker(&self, atomic_replace: bool) -> Option<&'static str> {
        if self.after_sha256.is_none() && self.action != "unchanged" {
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
                "replace-hook" => "Replace managed startup hook",
                "unchanged" => "Already current",
                "replace" => "Switch active bundle",
                action => action,
            })
    }
}

impl Review {
    fn prepare(&mut self) {
        for edit in &mut self.files {
            if let (Some(before), Some(hook)) = (edit.replaced_hook.take(), &edit.hook) {
                edit.hook_diff = diff_text(&before, hook, false);
            }
        }
    }

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
        if let Some(id) = &self.bundle_sha256 {
            fields.push(detail("reviewed-version", format!("Reviewed version {id}")));
        }
        if self.operation == ReviewOperation::RepairDotfileHooks {
            fields.push(detail(
                "hook-scope",
                "Repair updates the startup hooks for new shells and keeps the selected bundle."
                    .into(),
            ));
        }
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
            if matches!(edit.action.as_str(), "create" | "append-hook")
                && let Some(hook) = &edit.hook
            {
                if edit.add_newline_before_hook {
                    fields.push(detail(
                        format!("hook-newline-{index}"),
                        format!("A newline will be added before the hook in {}", edit.path),
                    ));
                }
                fields.push(code_details(
                    format!("startup-hook-{index}"),
                    &format!("Startup hook · {}", edit.path),
                    hook,
                ));
            }
            if let Some(diff) = &edit.hook_diff {
                fields.push(code_details(
                    format!("startup-hook-{index}"),
                    &format!("Startup hook changes · {}", edit.path),
                    diff,
                ));
            }
        }
        if let Some(comparison) = &self.fragment_changes {
            comparison.describe(fields);
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
    PreviewHooks,
    Apply,
    ApplyHooks,
    ListVersions,
    SaveVersion,
    ReviewSwitch,
    SwitchVersion,
}

pub(super) struct Installer {
    target: ToolPane,
    account: String,
    profile: Option<String>,
    bundle: Option<PathBuf>,
    shell: DotfileShell,
    zdotdir: String,
    pending: Option<(Arc<()>, Work)>,
    review: Option<Box<Review>>,
    status: Option<String>,
    history: Option<history::Panel>,
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

pub(super) fn login_summary(report: &DotfileSyncReport) -> String {
    let shell = match report.shell {
        DotfileShell::Bash => "Bash",
        DotfileShell::Zsh => "Zsh",
    };
    let outcome = match report.status {
        DotfileSyncStatus::Updated => "Updated",
        DotfileSyncStatus::Current => "Already current",
        DotfileSyncStatus::Held => "Paused by manual selection",
        DotfileSyncStatus::Skipped => "Skipped",
        DotfileSyncStatus::Unconfirmed => "Unconfirmed",
        DotfileSyncStatus::NotRun => "Skipped for this session",
    };
    format!("Dotfiles at login · {shell} · {outcome}")
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
        self.pending.as_ref().is_some_and(|(_, work)| {
            matches!(
                work,
                Work::Apply | Work::ApplyHooks | Work::SaveVersion | Work::SwitchVersion
            )
        })
    }

    fn startup_directory(&self) -> Result<Option<String>, String> {
        let path = self.zdotdir.trim();
        let path = (self.shell == DotfileShell::Zsh && !path.is_empty()).then_some(path);
        validate_zdotdir(self.shell, path).map_err(|error| format!("{error:#}"))?;
        Ok(path.map(str::to_owned))
    }

    pub(super) fn view(
        &self,
        current: Option<ToolPane>,
        login_sync: Option<&DotfileSyncReport>,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let connected = current == Some(self.target);
        if let Some(history) = &self.history {
            return history.view(self, connected, ctx);
        }
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
        ];
        if let Some(report) = login_sync.filter(|_| connected) {
            fields.push(detail("login-sync", login_summary(report)));
            if let Some(reason) = &report.detail {
                fields.push(detail("login-sync-detail", reason.clone()));
            }
        }
        fields.extend([
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
        ]);
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
            "versions",
            button("Dotfile versions", ready, || {
                Action::History(history::Action::Open)
            }),
        ));
        fields.push(KeyedView::new(
            "preview",
            button(
                "Review dotfiles changes",
                ready && self.bundle.is_some(),
                || Action::Preview,
            ),
        ));
        fields.push(KeyedView::new(
            "preview-hooks",
            button("Review startup hook repair", ready, || Action::PreviewHooks),
        ));
        let mut footer = Vec::with_capacity(2);
        if let Some(review) = &self.review {
            review.describe(&mut fields);
            let (label, action): (&str, fn() -> Action) =
                if review.operation == ReviewOperation::RepairDotfileHooks {
                    ("Repair reviewed startup hooks", || Action::ApplyHooks)
                } else {
                    ("Apply reviewed dotfiles", || Action::Apply)
                };
            footer.push(KeyedView::new(
                "apply",
                button(label, ready && review.ready(), action),
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
                    Work::PreviewHooks => "Reviewing startup hooks…",
                    Work::Apply => "Applying reviewed dotfiles…",
                    Work::ApplyHooks => "Repairing reviewed startup hooks…",
                    Work::ListVersions
                    | Work::SaveVersion
                    | Work::ReviewSwitch
                    | Work::SwitchVersion => {
                        unreachable!("the history view handles version operations")
                    }
                }
                .to_owned()
            })
        }) {
            fields.push(detail("status", status));
        }
        footer.push(KeyedView::new(
            "back",
            button("Back to command reference", !self.applying(), || {
                Action::Close
            }),
        ));
        Grid::new()
            .rows([GridLength::Star(1.0), GridLength::Auto])
            .keyed_children([
                KeyedView::new(
                    "review",
                    ScrollViewer::new().grid_row(0).content(
                        StackPanel::new()
                            .margin(12.0)
                            .spacing(8.0)
                            .keyed_children(fields),
                    ),
                ),
                KeyedView::new(
                    "actions",
                    StackPanel::new()
                        .grid_row(1)
                        .margin(12.0)
                        .spacing(8.0)
                        .keyed_children(footer),
                ),
            ])
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
        if let Action::History(action) = action {
            self.dotfile_history_action(action, current, ctx);
            return;
        }
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
                profile: live.profile_name.clone(),
                bundle: settings.map(|settings| settings.bundle.clone()),
                shell: settings.map_or(DotfileShell::Bash, |settings| settings.shell),
                zdotdir: settings
                    .and_then(|settings| settings.zdotdir.clone())
                    .unwrap_or_default(),
                pending: None,
                review: None,
                status: None,
                history: None,
            });
            return;
        }
        let owner = self.keyboard.hwnd().map(|window| window.0 as usize);
        let panel = &mut self.command_reference;
        let Some(installer) = &mut panel.dotfiles else {
            return;
        };
        if installer.history.is_some() {
            return;
        }
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
            Action::Preview | Action::PreviewHooks | Action::Apply | Action::ApplyHooks if idle => {
                let Some((_, input)) = current else { return };
                let work = match action {
                    Action::PreviewHooks => Work::PreviewHooks,
                    Action::ApplyHooks => Work::ApplyHooks,
                    Action::Apply => Work::Apply,
                    _ => Work::Preview,
                };
                let apply = matches!(work, Work::Apply | Work::ApplyHooks);
                let repairing = matches!(work, Work::PreviewHooks | Work::ApplyHooks);
                let path = if repairing {
                    None
                } else {
                    let Some(path) = installer.bundle.clone() else {
                        return;
                    };
                    Some(path)
                };
                let review = if apply {
                    let Some(plan) = installer.review.take_if(|plan| {
                        plan.ready()
                            && (plan.operation == ReviewOperation::RepairDotfileHooks) == repairing
                    }) else {
                        return;
                    };
                    Some(plan.review)
                } else {
                    installer.review = None;
                    None
                };
                let shell = installer.shell;
                let zdotdir = match installer.startup_directory() {
                    Ok(path) => path,
                    Err(error) => {
                        installer.status = Some(error);
                        return;
                    }
                };
                let ticket = installer.begin(work);
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
                            if scope.is_cancelled() {
                                return Err("Dotfiles operation cancelled".into());
                            }
                            let operation = if let Some(path) = path {
                                let bundle = DotfileBundle::load(&path).map_err(|error| {
                                    format!("Reading dotfiles bundle: {error:#}")
                                })?;
                                match review {
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
                                }
                            } else {
                                Operation::RepairDotfileHooks {
                                    shell,
                                    zdotdir,
                                    review,
                                }
                            };
                            let timeout = Duration::from_secs(if apply { 120 } else { 30 });
                            run_pane_sftp_job(input, target, operation, timeout, None)
                        })();
                        if apply {
                            message(Action::Applied(ticket, decode(result)))
                        } else {
                            let prepared = decode(result).map(|mut review: Box<Review>| {
                                review.prepare();
                                review
                            });
                            message(Action::Previewed(ticket, prepared))
                        }
                    },
                    rejected,
                );
            }
            Action::Previewed(ticket, result) => {
                if installer.finish(&ticket, Work::Preview)
                    || installer.finish(&ticket, Work::PreviewHooks)
                {
                    match result {
                        Ok(review) => installer.review = Some(review),
                        Err(error) => installer.status = Some(error),
                    }
                }
            }
            Action::Applied(ticket, result) => {
                let repairing = installer.finish(&ticket, Work::ApplyHooks);
                if repairing || installer.finish(&ticket, Work::Apply) {
                    installer.status = Some(match result {
                        Ok(applied) if repairing => match applied.changed_files.len() {
                            0 => "Startup hooks are already current".into(),
                            1 => "Startup hooks repaired · 1 file changed".into(),
                            count => format!("Startup hooks repaired · {count} files changed"),
                        },
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
