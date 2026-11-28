use super::*;
use w11term::{
    options::ToolPath,
    sftp::{DotfileShell, InstalledToolPath},
};

pub enum Action {
    OpenSetup,
    Shell(Option<usize>),
    StartupDirectory(DotfileShell, String),
    Preview,
    PreviewRemoval,
    Apply,
    Reviewed(Arc<()>, Result<Box<Review>, String>),
    Applied(Arc<()>, bool, Result<(), String>),
    Back,
}

pub(super) struct Setup {
    bundle: String,
    sha256: String,
    shell: DotfileShell,
    zdotdir: String,
    fish_config_dir: String,
    pub(super) open: bool,
}

impl Setup {
    pub(super) fn new(bundle: String, sha256: String) -> Self {
        Self {
            bundle,
            sha256,
            shell: DotfileShell::Bash,
            zdotdir: String::new(),
            fish_config_dir: String::new(),
            open: false,
        }
    }

    fn operation(&self, remove: bool, review: Option<String>) -> Result<Operation, String> {
        let (zdotdir, fish_config_dir) =
            profile_dotfiles::startup_fields(self.shell, &self.zdotdir, &self.fish_config_dir)
                .map_err(|error| format!("{error:#}"))?;
        Ok(Operation::ReviewInstalledToolPath {
            installed: InstalledToolPath {
                bundle: self.bundle.clone(),
                bundle_sha256: self.sha256.clone(),
                path: ToolPath {
                    shell: self.shell,
                    zdotdir,
                    fish_config_dir,
                },
                remove,
            },
            review,
        })
    }

    pub(super) fn view(
        &self,
        installer: &Installer,
        connected: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let ready = connected && installer.pending.is_none();
        let click = |label: &str, enabled, action: fn() -> Action| {
            Button::new()
                .is_enabled(enabled)
                .on_click(ctx.callback(move |_| path_message(action())))
                .content(label)
        };
        let mut fields = vec![
            KeyedView::new(
                "title",
                TextBlock::new()
                    .text("Manage PATH for new shells")
                    .font_size(18.0),
            ),
            detail("account".into(), installer.account.clone()),
            KeyedView::new(
                "shell",
                ComboBox::new()
                    .automation_name("PATH setup shell")
                    .items_source(DotfileShell::ALL.map(DotfileShell::label))
                    .selected_index(self.shell.menu_index())
                    .is_enabled(ready)
                    .on_selection_changed(ctx.callback(|index| path_message(Action::Shell(index)))),
            ),
        ];
        if self.shell != DotfileShell::Bash {
            let shell = self.shell;
            let (name, directory) = if shell == DotfileShell::Zsh {
                ("PATH setup Zsh startup directory", &self.zdotdir)
            } else {
                (
                    "PATH setup Fish configuration directory",
                    &self.fish_config_dir,
                )
            };
            fields.push(KeyedView::new(
                name,
                TextBox::new()
                    .automation_name(name)
                    .placeholder_text("Relative to home (optional)")
                    .text(directory)
                    .is_enabled(ready)
                    .on_text_changed(
                        ctx.callback(move |text| {
                            path_message(Action::StartupDirectory(shell, text))
                        }),
                    ),
            ));
        }
        fields.push(KeyedView::new(
            "preview",
            click("Review PATH setup", ready, || Action::Preview),
        ));
        fields.push(KeyedView::new(
            "review-removal",
            click("Review PATH removal", ready, || Action::PreviewRemoval),
        ));
        let mut footer = Vec::with_capacity(2);
        if let Some(review) = &installer.path_review {
            review.describe(&mut fields, true);
            if review.ready {
                footer.push(KeyedView::new(
                    "apply",
                    click(
                        if review.remove {
                            "Remove reviewed PATH blocks"
                        } else {
                            "Apply reviewed PATH setup"
                        },
                        ready && review.review.is_some(),
                        || Action::Apply,
                    ),
                ));
            }
        }
        let pending = installer.pending.as_ref().map(|(_, work)| {
            if *work == Work::PathApply {
                "Updating shell startup files…"
            } else {
                "Reviewing shell startup files…"
            }
        });
        let status = if connected {
            pending.or(installer.status.as_deref())
        } else {
            Some("Return to the original SSH pane or review again on the new connection")
        };
        if let Some(status) = status {
            fields.push(detail("status".into(), status.into()));
        }
        footer.push(KeyedView::new(
            "back",
            click(
                "Back to tool installer",
                installer.pending.is_none(),
                || Action::Back,
            ),
        ));
        Grid::new()
            .rows([GridLength::Star(1.0), GridLength::Auto])
            .keyed_children([
                KeyedView::new(
                    "details",
                    ScrollViewer::new().grid_row(0).content(
                        StackPanel::new()
                            .margin(12.0)
                            .spacing(8.0)
                            .keyed_children(fields),
                    ),
                ),
                KeyedView::new(
                    "footer",
                    StackPanel::new()
                        .grid_row(1)
                        .margin(12.0)
                        .spacing(8.0)
                        .keyed_children(footer),
                ),
            ])
    }
}

#[derive(Deserialize)]
pub struct Review {
    shell: String,
    home: String,
    bin_directory: String,
    #[serde(default)]
    directories_to_create: Vec<String>,
    files: Vec<File>,
    blocked: Option<String>,
    pub(super) ready: bool,
    pub(super) review: Option<String>,
    #[serde(default)]
    remove: bool,
}

#[derive(Deserialize)]
struct File {
    path: String,
    action: String,
    hook: Option<String>,
    replaced_hook: Option<String>,
}

fn detail(key: String, text: String) -> KeyedView {
    KeyedView::new(
        key,
        TextBlock::new()
            .text(text)
            .text_wrapping(TextWrapping::Wrap),
    )
}

impl Review {
    pub(super) fn describe(&self, fields: &mut Vec<KeyedView>, setup: bool) {
        fields.push(detail(
            "path-review-account".into(),
            format!(
                "{} startup files · {}/{}",
                self.shell,
                self.home.trim_end_matches('/'),
                self.bin_directory
            ),
        ));
        for (index, path) in self.directories_to_create.iter().enumerate() {
            fields.push(detail(
                format!("path-directory-{index}"),
                format!("Create directory · {path}"),
            ));
        }
        for (index, file) in self.files.iter().enumerate() {
            let action = match file.action.as_str() {
                "create" => "Create",
                "append-hook" => "Append managed block to",
                "replace-hook" => "Replace managed block in",
                "remove-hook" => "Remove managed block from",
                "unchanged" if self.remove => "No managed block in",
                "unchanged" => "Already current",
                _ => "Blocked",
            };
            fields.push(detail(
                format!("path-file-{index}"),
                format!("{action} · {}", file.path),
            ));
            if let Some(old) = &file.replaced_hook {
                fields.push(detail(
                    format!("path-before-{index}"),
                    if self.remove {
                        format!("Block to remove\n{old}")
                    } else {
                        format!("Existing managed block\n{old}")
                    },
                ));
            }
            if matches!(
                file.action.as_str(),
                "create" | "append-hook" | "replace-hook"
            ) && let Some(hook) = &file.hook
            {
                fields.push(detail(
                    format!("path-after-{index}"),
                    format!("Proposed managed block\n{hook}"),
                ));
            }
        }
        let status = if let Some(blocked) = &self.blocked {
            blocked.as_str()
        } else if self.remove && self.ready {
            "Profiles with managed PATH enabled can add this directory again on connect."
        } else if self.remove {
            "There are no matching managed PATH blocks to remove."
        } else if self.ready && setup {
            "The launcher directory will be appended to PATH for new shells."
        } else if self.ready {
            "The repaired PATH applies to new shells."
        } else {
            "The managed PATH is already current."
        };
        fields.push(detail("path-review-status".into(), status.into()));
    }
}

impl TerminalApp {
    pub(super) fn repair_tool_path(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        let current = self.reference_tool_target().map(|(target, session)| {
            let repairable = session
                .events
                .tool_bundle_sync(target.generation)
                .is_some_and(|report| report.path == Some(ToolPathSyncStatus::Skipped));
            (target, session.input.clone(), repairable)
        });
        let Some(installer) = &mut self.command_reference.tool_install else {
            return;
        };
        if installer.history.is_some() {
            return;
        }
        let target = installer.target;
        let current = current.filter(|(pane, _, _)| *pane == target);
        let idle = installer.pending.is_none();
        match action {
            Action::OpenSetup if idle && current.is_some() => {
                if let Some(setup) = &mut installer.path_setup {
                    setup.open = true;
                    installer.path_review = None;
                    installer.status = None;
                }
            }
            Action::Back if idle => {
                if let Some(setup) = &mut installer.path_setup {
                    setup.open = false;
                    installer.path_review = None;
                }
            }
            Action::Shell(Some(index)) if idle && current.is_some() => {
                if let Some(&shell) = DotfileShell::ALL.get(index)
                    && let Some(setup) = installer.path_setup.as_mut().filter(|setup| setup.open)
                    && setup.shell != shell
                {
                    setup.shell = shell;
                    installer.path_review = None;
                    installer.status = None;
                }
            }
            Action::StartupDirectory(shell, text) if idle && current.is_some() => {
                if let Some(setup) = installer
                    .path_setup
                    .as_mut()
                    .filter(|setup| setup.open && setup.shell == shell)
                {
                    let directory = match shell {
                        DotfileShell::Zsh => &mut setup.zdotdir,
                        DotfileShell::Fish => &mut setup.fish_config_dir,
                        DotfileShell::Bash => return,
                    };
                    if *directory != text {
                        *directory = text;
                        installer.path_review = None;
                        installer.status = None;
                    }
                }
            }
            Action::Preview | Action::PreviewRemoval | Action::Apply => {
                let Some((_, input, repairable)) = current else {
                    return;
                };
                if !idle {
                    return;
                }
                let apply = matches!(action, Action::Apply);
                let remove = matches!(action, Action::PreviewRemoval)
                    || (apply
                        && installer
                            .path_review
                            .as_ref()
                            .is_some_and(|review| review.remove));
                let review = if apply {
                    let Some(token) = installer
                        .path_review
                        .as_ref()
                        .filter(|review| review.ready)
                        .and_then(|review| review.review.clone())
                    else {
                        return;
                    };
                    Some(token)
                } else {
                    None
                };
                let operation =
                    if let Some(setup) = installer.path_setup.as_ref().filter(|setup| setup.open) {
                        match setup.operation(remove, review) {
                            Ok(operation) => operation,
                            Err(error) => {
                                installer.status = Some(error);
                                return;
                            }
                        }
                    } else if repairable && !remove {
                        Operation::ReviewToolPath { review }
                    } else {
                        return;
                    };
                installer.path_review = None;
                let ticket = installer.begin(if apply {
                    Work::PathApply
                } else {
                    Work::PathReview
                });
                let rejected = result_message(
                    ticket.clone(),
                    apply,
                    remove,
                    Err("PATH review worker unavailable".into()),
                );
                ctx.spawn_background_with_rejection(
                    move |_| {
                        let result = run_pane_sftp_job(
                            input,
                            target,
                            operation,
                            Duration::from_secs(30),
                            None,
                        );
                        result_message(ticket, apply, remove, result)
                    },
                    rejected,
                );
            }
            Action::Reviewed(ticket, result) if installer.finish(&ticket, Work::PathReview) => {
                match result {
                    Ok(review) => installer.path_review = Some(review),
                    Err(error) => installer.status = Some(error),
                }
            }
            Action::Applied(ticket, remove, result)
                if installer.finish(&ticket, Work::PathApply) =>
            {
                installer.status = Some(match result {
                    Ok(()) if remove => "Managed PATH blocks removed for new shells".into(),
                    Ok(())
                        if installer
                            .path_setup
                            .as_ref()
                            .is_some_and(|setup| setup.open) =>
                    {
                        "Managed PATH added for new shells".into()
                    }
                    Ok(()) => "Managed PATH repaired for new shells".into(),
                    Err(error) => error,
                });
            }
            _ => (),
        }
    }
}

fn path_message(action: Action) -> Message {
    message(super::Action::Path(action))
}

fn result_message(
    ticket: Arc<()>,
    apply: bool,
    remove: bool,
    result: Result<serde_json::Value, String>,
) -> Message {
    path_message(if apply {
        Action::Applied(ticket, remove, result.map(|_| ()))
    } else {
        Action::Reviewed(ticket, decode(result))
    })
}
