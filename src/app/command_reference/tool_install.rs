use super::*;
use serde::Deserialize;
use std::path::PathBuf;
use w11term::sftp::UtilityBundle;

mod history;
mod path_repair;

pub enum Action {
    Open(ToolPane),
    Browse,
    Picked(Arc<()>, Result<Option<PathBuf>, String>),
    Preview,
    Previewed(Arc<()>, Result<Box<Review>, String>),
    Apply,
    Applied(Arc<()>, Result<Installation, String>),
    History(history::Action),
    Path(path_repair::Action),
    Close,
}

#[derive(Deserialize)]
pub struct Review {
    bundle: String,
    host: Host,
    bin_directory: String,
    path_status: String,
    ready: bool,
    review: Option<String>,
    tools: Vec<Tool>,
    current: Current,
    bin_directory_plan: Directory,
    managed_directory_plan: Directory,
    bin_matches_active: bool,
    supports_atomic_replace: bool,
}

#[derive(Deserialize)]
struct Host {
    architecture: String,
    home: String,
}

#[derive(Deserialize)]
struct Current {
    conflict: bool,
    version: Option<String>,
}

#[derive(Deserialize)]
struct Directory {
    blocked_at: Option<String>,
}

#[derive(Deserialize)]
struct Tool {
    name: String,
    destination: String,
    launcher: String,
    earlier_executable: Option<String>,
    selected_build: Option<usize>,
    builds: Vec<Build>,
}

#[derive(Deserialize)]
struct Build {
    reason: Option<String>,
}

#[derive(Deserialize)]
pub struct Installation {
    bundle: String,
    changed: bool,
    launchers_created: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Work {
    Browse,
    Preview,
    Apply,
    PathReview,
    PathApply,
}

impl Work {
    fn writing(self) -> bool {
        matches!(self, Self::Apply | Self::PathApply)
    }
}

pub(super) struct Installer {
    target: ToolPane,
    account: String,
    manifest: Option<PathBuf>,
    review: Option<Box<Review>>,
    pending: Option<(Arc<()>, Work)>,
    status: Option<String>,
    history: Option<history::Panel>,
    path_review: Option<Box<path_repair::Review>>,
}

fn message(action: Action) -> Message {
    Message::CommandReference(super::Action::ToolInstall(action))
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

    fn token(&self) -> Option<&str> {
        let review = self.review.as_ref().filter(|review| review.ready)?;
        review.review.as_deref()
    }

    pub(super) fn view(
        &self,
        active: Option<ToolPane>,
        tool_sync: Option<&ToolBundleSyncReport>,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        if let Some(history) = &self.history {
            return history.view(&self.account, active == Some(self.target), ctx);
        }
        let available = active == Some(self.target) && self.pending.is_none();
        let applying = self
            .pending
            .as_ref()
            .is_some_and(|(_, kind)| kind.writing());
        let button = |label: &str, enabled, action: fn() -> Action| {
            Button::new()
                .is_enabled(enabled)
                .on_click(ctx.callback(move |_| message(action())))
                .content(label)
        };
        let mut fields = vec![
            KeyedView::new(
                "heading",
                TextBlock::new().text("Install SSH tools").font_size(18.0),
            ),
            KeyedView::new(
                "account",
                TextBlock::new()
                    .text(&self.account)
                    .text_wrapping(TextWrapping::Wrap),
            ),
        ];
        if active == Some(self.target)
            && let Some(report) = tool_sync
        {
            fields.push(KeyedView::new(
                "tool-login-sync",
                TextBlock::new()
                    .text(tool_sync_summary(report))
                    .text_wrapping(TextWrapping::Wrap),
            ));
            if let Some(path) = report.path {
                let status = match path {
                    ToolPathSyncStatus::Updated => "Managed PATH updated",
                    ToolPathSyncStatus::Current => "Managed PATH already current",
                    ToolPathSyncStatus::Skipped => "Managed PATH skipped",
                };
                fields.push(KeyedView::new(
                    "tool-login-path",
                    TextBlock::new().text(status),
                ));
            }
            if let Some(detail) = &report.detail {
                fields.push(KeyedView::new(
                    "tool-login-detail",
                    TextBlock::new()
                        .text(detail)
                        .text_wrapping(TextWrapping::Wrap),
                ));
            }
            if report.path == Some(ToolPathSyncStatus::Skipped) {
                fields.push(KeyedView::new(
                    "review-path",
                    button("Review PATH repair", available, || {
                        Action::Path(path_repair::Action::Preview)
                    }),
                ));
            }
        }
        if active == Some(self.target)
            && let Some(review) = &self.path_review
        {
            review.describe(&mut fields);
            if review.ready {
                fields.push(KeyedView::new(
                    "apply-path",
                    button(
                        "Apply reviewed PATH repair",
                        available && review.review.is_some(),
                        || Action::Path(path_repair::Action::Apply),
                    ),
                ));
            }
        }
        fields.extend([
            KeyedView::new(
                "history",
                button("Bundle versions", available, || {
                    Action::History(history::Action::Open)
                }),
            ),
            KeyedView::new(
                "manifest",
                TextBlock::new()
                    .automation_id("ssh-tool-install-manifest")
                    .text(self.manifest.as_deref().map_or_else(
                        || "Choose a bundle manifest".into(),
                        |path| path.display().to_string(),
                    ))
                    .text_wrapping(TextWrapping::Wrap),
            ),
            KeyedView::new(
                "choose-and-review",
                StackPanel::new().spacing(8.0).children((
                    button("Browse for tool bundle", available, || Action::Browse),
                    button(
                        "Review bundle",
                        available && self.manifest.is_some(),
                        || Action::Preview,
                    ),
                )),
            ),
        ]);
        let status = if let Some((_, work)) = &self.pending {
            Some(match work {
                Work::Browse => "Choosing a manifest…",
                Work::Preview => "Reviewing tool bundle…",
                Work::Apply => "Installing reviewed tool bundle…",
                Work::PathReview => "Reviewing tool PATH…",
                Work::PathApply => "Updating shell startup files…",
            })
        } else if active != Some(self.target) {
            Some("Return to the original SSH pane or open a new review")
        } else {
            self.status.as_deref()
        };
        if let Some(status) = status {
            fields.push(KeyedView::new(
                "status",
                TextBlock::new()
                    .text(status)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        if let Some(review) = &self.review {
            review.describe(&mut fields);
        }
        fields.push(KeyedView::new(
            "install-actions",
            StackPanel::new().spacing(8.0).children((
                button(
                    "Install reviewed bundle",
                    available && self.token().is_some(),
                    || Action::Apply,
                ),
                button("Back to command reference", !applying, || Action::Close),
            )),
        ));
        Border::new().padding(12.0).content(
            ScrollViewer::new().content(StackPanel::new().spacing(8.0).keyed_children(fields)),
        )
    }
}

impl Review {
    fn describe(&self, fields: &mut Vec<KeyedView>) {
        let detail = |key: &str, text: String| {
            KeyedView::new(
                key.to_owned(),
                TextBlock::new()
                    .text(text)
                    .text_wrapping(TextWrapping::Wrap),
            )
        };
        fields.push(detail(
            "destination",
            format!(
                "{} · Linux {} · Home {}\nLaunchers will be in {}",
                self.bundle, self.host.architecture, self.host.home, self.bin_directory
            ),
        ));
        let path_status = match self.path_status.as_str() {
            "present" => "Launcher directory is in the probed PATH",
            "present-precedence-partial" => {
                "Launcher directory is in PATH. Relative entries can affect which command runs."
            }
            _ => "Launcher directory is missing from the probed PATH",
        };
        fields.push(detail("path", path_status.to_owned()));
        if !self.ready {
            fields.push(detail(
                "blocked",
                "Resolve the conflicts before installing".into(),
            ));
        }
        for (key, directory) in [
            ("bin-conflict", &self.bin_directory_plan),
            ("storage-conflict", &self.managed_directory_plan),
        ] {
            if let Some(path) = &directory.blocked_at {
                fields.push(detail(key, format!("Blocked by {path}")));
            }
        }
        if self.current.conflict || !self.bin_matches_active {
            fields.push(detail(
                "active-conflict",
                "The active tool bundle has a conflicting selector or launcher directory".into(),
            ));
        } else if !self.ready && self.current.version.is_some() && !self.supports_atomic_replace {
            fields.push(detail(
                "atomic-replace",
                "Switching an active release requires the server's atomic rename extension".into(),
            ));
        }
        for tool in &self.tools {
            let mut text = match tool.launcher.as_str() {
                "conflict" => format!("{} · Existing entry at {}", tool.name, tool.destination),
                "managed" => format!("{} · Reuse {}", tool.name, tool.destination),
                _ => format!("{} · Create {}", tool.name, tool.destination),
            };
            if let Some(path) = &tool.earlier_executable {
                text.push_str(&format!("\nEarlier executable in PATH is {path}"));
            }
            if tool.selected_build.is_none() {
                let reason = tool
                    .builds
                    .iter()
                    .find_map(|build| build.reason.as_deref())
                    .unwrap_or("No compatible Linux build in the manifest");
                text.push_str(&format!("\n{reason}"));
            }
            fields.push(detail(&format!("tool-{}", tool.name), text));
        }
    }
}

impl TerminalApp {
    pub(super) fn tool_install_action(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        if let Action::History(action) = action {
            self.tool_bundle_history(action, ctx);
            return;
        }
        if let Action::Path(action) = action {
            self.repair_tool_path(action, ctx);
            return;
        }
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
            let manifest = ssh.tool_bundle.as_ref().map(|tools| tools.bundle.clone());
            self.command_reference.draft = None;
            self.command_reference.tool_install = Some(Installer {
                target,
                account: format!("{}@{}:{}", ssh.user, ssh.host, ssh.port),
                manifest,
                review: None,
                pending: None,
                status: None,
                history: None,
                path_review: None,
            });
            return;
        }

        let owner = self.keyboard.hwnd().map(|window| window.0 as usize);
        let catalog_path = self.store.path().to_path_buf();
        let panel = &mut self.command_reference;
        let Some(installer) = &mut panel.tool_install else {
            return;
        };
        if installer.history.is_some() {
            return;
        }
        let target = installer.target;
        match action {
            Action::Open(_) => unreachable!("opening handled above"),
            Action::History(_) => unreachable!("history handled above"),
            Action::Path(_) => unreachable!("PATH repair handled above"),
            Action::Close => {
                if !installer
                    .pending
                    .as_ref()
                    .is_some_and(|(_, work)| work.writing())
                {
                    panel.tool_install = None;
                    panel.focus = panel.focus.wrapping_add(1);
                }
            }
            Action::Browse => {
                if installer.pending.is_some()
                    || current.as_ref().is_none_or(|(pane, _)| *pane != target)
                {
                    return;
                }
                let Some(owner) = owner else { return };
                let path = installer.manifest.clone().unwrap_or(catalog_path);
                let ticket = installer.begin(Work::Browse);
                let rejected = message(Action::Picked(
                    ticket.clone(),
                    Err("File picker unavailable".into()),
                ));
                ctx.spawn_background_with_rejection(
                    move |_| {
                        let result =
                            file_picker::pick(owner, file_picker::Purpose::ToolBundle(path))
                                .map(|selected| selected.map(|file| file.path))
                                .map_err(|error| format!("{error:#}"));
                        message(Action::Picked(ticket, result))
                    },
                    rejected,
                );
            }
            Action::Picked(ticket, result) => {
                if !installer.finish(&ticket, Work::Browse) {
                    return;
                }
                match result {
                    Ok(Some(path)) => {
                        installer.manifest = Some(path);
                        installer.review = None;
                    }
                    Ok(None) => (),
                    Err(error) => installer.status = Some(error),
                }
            }
            Action::Preview | Action::Apply => {
                let Some((_, input)) = current.filter(|(pane, _)| *pane == target) else {
                    return;
                };
                if installer.pending.is_some() {
                    return;
                }
                let Some(path) = installer.manifest.clone() else {
                    return;
                };
                let apply = matches!(action, Action::Apply);
                let token = if apply {
                    let Some(token) = installer.token() else {
                        return;
                    };
                    Some(token.to_owned())
                } else {
                    None
                };
                installer.review = None;
                let ticket = installer.begin(if apply { Work::Apply } else { Work::Preview });
                let rejected = if apply {
                    message(Action::Applied(
                        ticket.clone(),
                        Err("Installation worker unavailable. Review the bundle again.".into()),
                    ))
                } else {
                    message(Action::Previewed(
                        ticket.clone(),
                        Err("Review worker unavailable".into()),
                    ))
                };
                ctx.spawn_background_with_rejection(
                    move |scope| {
                        let result = (|| -> Result<serde_json::Value, String> {
                            let bundle = UtilityBundle::load(&path)
                                .map_err(|error| format!("Reading the tool bundle: {error:#}"))?;
                            if scope.is_cancelled() {
                                return Err("Tool bundle operation cancelled".into());
                            }
                            let operation = match token {
                                Some(review) => Operation::ApplyToolBundle { bundle, review },
                                None => Operation::PreviewToolBundle { bundle },
                            };
                            let timeout = Duration::from_secs(if apply { 600 } else { 30 });
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
                        Ok(installation) => {
                            panel.tools = None;
                            if installation.changed || installation.launchers_created != 0 {
                                format!("Installed tool bundle {}", installation.bundle)
                            } else {
                                format!("Tool bundle {} is already active", installation.bundle)
                            }
                        }
                        Err(error) => error,
                    });
                }
            }
        }
    }
}

fn decode<T: serde::de::DeserializeOwned>(
    value: Result<serde_json::Value, String>,
) -> Result<T, String> {
    serde_json::from_value(value?)
        .map_err(|error| format!("Reading the tool bundle report: {error}"))
}
