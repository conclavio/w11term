use super::*;
use serde::Deserialize;
use std::path::PathBuf;
use w11term::sftp::{
    UtilityArchitecture, UtilityBundle, UtilityDownload, builtin_tool_bundle, builtin_tool_names,
    fetch_tool_bundle_cancellable,
};

mod history;
mod path_repair;

pub enum Action {
    Open(ToolPane, Option<String>),
    Browse,
    Picked(Arc<()>, Result<Option<PathBuf>, String>),
    Preview,
    Download,
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
    #[serde(default)]
    automatic_sync_paused: bool,
}

#[derive(Deserialize)]
struct Host {
    architecture: UtilityArchitecture,
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
    retained: Option<Retained>,
}

#[derive(Deserialize)]
struct Build {
    architecture: UtilityArchitecture,
    status: String,
    release: Option<String>,
    sha256: String,
    download: Option<UtilityDownload>,
    reason: Option<String>,
}

#[derive(Deserialize)]
struct Retained {
    sha256: String,
}

#[derive(Deserialize)]
pub struct Installation {
    bundle: String,
    bundle_sha256: String,
    changed: bool,
    launchers_created: usize,
    #[serde(default)]
    automatic_sync_paused: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Work {
    Browse,
    Preview,
    Download,
    Apply,
    PathReview,
    PathApply,
}

impl Work {
    fn writing(self) -> bool {
        matches!(self, Self::Apply | Self::PathApply)
    }
}

#[derive(Clone)]
enum BundleSource {
    Manifest(Option<PathBuf>),
    BuiltIn(String),
}

impl BundleSource {
    fn command(&self) -> Option<&str> {
        match self {
            Self::BuiltIn(name) => Some(name),
            Self::Manifest(_) => None,
        }
    }
}

pub(super) struct Installer {
    target: ToolPane,
    account: String,
    source: BundleSource,
    review: Option<Box<Review>>,
    pending: Option<(Arc<()>, Work)>,
    status: Option<String>,
    history: Option<history::Panel>,
    path_review: Option<Box<path_repair::Review>>,
    path_setup: Option<path_repair::Setup>,
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
        if let Some(setup) = self.path_setup.as_ref().filter(|setup| setup.open) {
            return setup.view(self, active == Some(self.target), ctx);
        }
        let available = active == Some(self.target) && self.pending.is_none();
        let command = self.source.command();
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
                TextBlock::new()
                    .text(command.map_or_else(
                        || "Install SSH tools".into(),
                        |name| format!("Install {name}"),
                    ))
                    .font_size(18.0),
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
            review.describe(&mut fields, false);
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
        fields.push(KeyedView::new(
            "history",
            button("Bundle versions", available, || {
                Action::History(history::Action::Open)
            }),
        ));
        if let BundleSource::Manifest(manifest) = &self.source {
            fields.extend([
                KeyedView::new(
                    "manifest",
                    TextBlock::new()
                        .automation_id("ssh-tool-install-manifest")
                        .text(manifest.as_deref().map_or_else(
                            || "Choose a bundle manifest".into(),
                            |path| path.display().to_string(),
                        ))
                        .text_wrapping(TextWrapping::Wrap),
                ),
                KeyedView::new(
                    "choose-and-review",
                    StackPanel::new().spacing(8.0).children((
                        button("Browse for tool bundle", available, || Action::Browse),
                        button("Review bundle", available && manifest.is_some(), || {
                            Action::Preview
                        }),
                    )),
                ),
            ]);
        } else {
            fields.push(KeyedView::new(
                "review-command",
                button("Review tool again", available, || Action::Preview),
            ));
        }
        let status = if let Some((_, work)) = &self.pending {
            Some(match work {
                Work::Browse => "Choosing a manifest…",
                Work::Preview => "Reviewing tool bundle…",
                Work::Download => "Downloading and verifying the pinned Linux build…",
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
        if self.path_setup.is_some() {
            fields.push(KeyedView::new(
                "setup-tool-path",
                button("Manage PATH for new shells", available, || {
                    Action::Path(path_repair::Action::OpenSetup)
                }),
            ));
        }
        if let Some(review) = &self.review {
            let download = command.and_then(|name| review.downloadable(name));
            review.describe(&mut fields, command, download);
            if download.is_some() {
                fields.push(KeyedView::new(
                    "download-build",
                    button("Download pinned Linux build", available, || {
                        Action::Download
                    }),
                ));
            }
        }
        let install = command.map_or_else(
            || "Install reviewed bundle".into(),
            |name| format!("Install reviewed {name}"),
        );
        fields.push(KeyedView::new(
            "install-actions",
            StackPanel::new().spacing(8.0).children((
                button(&install, available && self.token().is_some(), || {
                    Action::Apply
                }),
                button("Back to command reference", !applying, || Action::Close),
            )),
        ));
        Border::new().padding(12.0).content(
            ScrollViewer::new().content(StackPanel::new().spacing(8.0).keyed_children(fields)),
        )
    }
}

impl Review {
    fn downloadable(&self, name: &str) -> Option<&Build> {
        if self.ready
            || self.current.conflict
            || !self.bin_matches_active
            || self.bin_directory_plan.blocked_at.is_some()
            || self.managed_directory_plan.blocked_at.is_some()
            || (self.current.version.is_some() && !self.supports_atomic_replace)
            || self.tools.iter().any(|tool| tool.launcher == "conflict")
        {
            return None;
        }
        let tool = self.tools.iter().find(|tool| tool.name == name)?;
        if tool.retained.is_some() || tool.selected_build.is_some() {
            return None;
        }
        tool.builds.iter().find(|build| {
            build.architecture == self.host.architecture
                && build.status == "invalid"
                && build.download.is_some()
        })
    }

    fn describe(
        &self,
        fields: &mut Vec<KeyedView>,
        command: Option<&str>,
        download: Option<&Build>,
    ) {
        let detail = |key: &str, text: String| {
            KeyedView::new(
                key.to_owned(),
                TextBlock::new()
                    .text(text)
                    .text_wrapping(TextWrapping::Wrap),
            )
        };
        let architecture = match self.host.architecture {
            UtilityArchitecture::X86_64 => "x86_64",
            UtilityArchitecture::Aarch64 => "aarch64",
        };
        fields.push(detail(
            "destination",
            format!(
                "{} · Linux {} · Home {}\nLaunchers will be in {}",
                self.bundle, architecture, self.host.home, self.bin_directory
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
        if let Some(build) = download {
            fields.push(detail(
                "awaiting-build",
                format!("Download the pinned {architecture} build to continue"),
            ));
            fields.push(detail("download-pin", format!("SHA-256 {}", build.sha256)));
        } else if !self.ready {
            fields.push(detail(
                "blocked",
                "Resolve the conflicts before installing".into(),
            ));
        }
        if self.automatic_sync_paused {
            fields.push(detail(
                "sync-hold",
                "Automatic sync will be paused for this bundle after installation.".into(),
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
                "managed" if tool.retained.is_some() => {
                    format!("{} · Keep {}", tool.name, tool.destination)
                }
                "managed" => format!("{} · Reuse {}", tool.name, tool.destination),
                _ => format!("{} · Create {}", tool.name, tool.destination),
            };
            if let Some(path) = &tool.earlier_executable {
                text.push_str(&format!("\nEarlier executable in PATH is {path}"));
            }
            if let Some(retained) = &tool.retained {
                text.push_str(&format!(
                    "\nKeep installed binary · SHA-256 {}",
                    retained.sha256
                ));
            } else if let Some(build) = tool.selected_build.and_then(|index| tool.builds.get(index))
            {
                if command == Some(tool.name.as_str()) {
                    if let Some(release) = &build.release {
                        text.push_str(&format!("\n{release}"));
                    }
                    text.push_str(&format!("\nSHA-256 {}", build.sha256));
                }
            } else if download.is_none() || command != Some(tool.name.as_str()) {
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
        let action = match action {
            Action::Open(target, command) => {
                if current.as_ref().is_none_or(|(pane, _)| *pane != target)
                    || !self.command_reference.open
                    || self.command_reference.subpanel_open()
                    || command
                        .as_ref()
                        .is_some_and(|name| !builtin_tool_names().contains(name))
                {
                    return;
                }
                let Some(Page::Terminal(live)) = self.tabs.active().map(|tab| &tab.page) else {
                    return;
                };
                let Source::Ssh(ssh) = &live.source else {
                    return;
                };
                let source = command.map_or_else(
                    || {
                        BundleSource::Manifest(
                            ssh.tool_bundle.as_ref().map(|tools| tools.bundle.clone()),
                        )
                    },
                    BundleSource::BuiltIn,
                );
                let review_now = source.command().is_some();
                self.command_reference.draft = None;
                self.command_reference.tool_install = Some(Installer {
                    target,
                    account: format!("{}@{}:{}", ssh.user, ssh.host, ssh.port),
                    source,
                    review: None,
                    pending: None,
                    status: None,
                    history: None,
                    path_review: None,
                    path_setup: None,
                });
                if !review_now {
                    return;
                }
                Action::Preview
            }
            action => action,
        };

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
            Action::Open(..) => unreachable!("opening handled above"),
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
                let BundleSource::Manifest(manifest) = &installer.source else {
                    return;
                };
                let Some(owner) = owner else { return };
                let path = manifest.clone().unwrap_or(catalog_path);
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
                        installer.source = BundleSource::Manifest(Some(path));
                        installer.review = None;
                        installer.path_setup = None;
                    }
                    Ok(None) => (),
                    Err(error) => installer.status = Some(error),
                }
            }
            Action::Preview | Action::Download | Action::Apply => {
                let Some((_, input)) = current.filter(|(pane, _)| *pane == target) else {
                    return;
                };
                if installer.pending.is_some() {
                    return;
                }
                let source = installer.source.clone();
                if matches!(source, BundleSource::Manifest(None)) {
                    return;
                }
                let work = match action {
                    Action::Apply => Work::Apply,
                    Action::Download => Work::Download,
                    _ => Work::Preview,
                };
                let download = if work == Work::Download {
                    let Some(build) = installer.review.as_ref().and_then(|review| {
                        source.command().and_then(|name| review.downloadable(name))
                    }) else {
                        return;
                    };
                    Some(build.architecture)
                } else {
                    None
                };
                let apply = work == Work::Apply;
                let token = if apply {
                    let Some(token) = installer.token() else {
                        return;
                    };
                    Some(token.to_owned())
                } else {
                    None
                };
                if work != Work::Download {
                    installer.review = None;
                }
                installer.path_setup = None;
                let ticket = installer.begin(work);
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
                            let operation = match source {
                                BundleSource::Manifest(Some(path)) => {
                                    let bundle = UtilityBundle::load(&path).map_err(|error| {
                                        format!("Reading the tool bundle: {error:#}")
                                    })?;
                                    match token {
                                        Some(review) => {
                                            Operation::ApplyToolBundle { bundle, review }
                                        }
                                        None => Operation::PreviewToolBundle { bundle },
                                    }
                                }
                                BundleSource::BuiltIn(name) => {
                                    let bundle = builtin_tool_bundle(&name).map_err(|error| {
                                        format!("Reading the built-in command: {error:#}")
                                    })?;
                                    if let Some(architecture) = download {
                                        fetch_tool_bundle_cancellable(
                                            &bundle,
                                            Some(architecture),
                                            || scope.is_cancelled(),
                                        )
                                        .map_err(
                                            |error| {
                                                format!("Downloading the pinned build: {error:#}")
                                            },
                                        )?;
                                    }
                                    Operation::AddToToolBundle {
                                        bundle,
                                        review: token,
                                    }
                                }
                                BundleSource::Manifest(None) => {
                                    unreachable!("a manifest is selected")
                                }
                            };
                            if scope.is_cancelled() {
                                return Err("Tool bundle operation cancelled".into());
                            }
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
                if installer.finish(&ticket, Work::Preview)
                    || installer.finish(&ticket, Work::Download)
                {
                    match result {
                        Ok(review) => {
                            if let Some(command) = installer.source.command()
                                && let Some(id) = &review.current.version
                                && !review.current.conflict
                                && review
                                    .tools
                                    .iter()
                                    .any(|tool| tool.name == command && tool.retained.is_some())
                            {
                                installer.path_setup = Some(path_repair::Setup::new(
                                    review.bundle.clone(),
                                    id.clone(),
                                ));
                            }
                            installer.review = Some(review);
                        }
                        Err(error) => installer.status = Some(error),
                    }
                }
            }
            Action::Applied(ticket, result) => {
                if installer.finish(&ticket, Work::Apply) {
                    installer.status = Some(match result {
                        Ok(installation) => {
                            panel.tools = None;
                            installer.path_setup = Some(path_repair::Setup::new(
                                installation.bundle.clone(),
                                installation.bundle_sha256,
                            ));
                            let changed =
                                installation.changed || installation.launchers_created != 0;
                            if let Some(name) = installer.source.command() {
                                let mut status = if changed {
                                    format!("Installed {name}")
                                } else {
                                    format!("{name} is already installed")
                                };
                                if installation.automatic_sync_paused {
                                    status.push_str("\nAutomatic bundle sync is paused.");
                                }
                                status
                            } else if changed {
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
