use super::*;
use crate::app::command_reference::dotfiles::comparison::code_details;
use std::path::PathBuf;
use w11term::profiles::{
    ToolProfileAudit, ToolProfileAuditRow, ToolProfileAuditStatus, ToolProfileBuildChecks,
};
use w11term::sftp::{
    ToolArchitectureAudit, ToolExecutableSource, ToolExecutableStatus, UtilityArchitecture,
};

mod prepare;
mod rollout;

const CHECKS: [(&str, ToolProfileBuildChecks); 4] = [
    ("Manifest approvals only", ToolProfileBuildChecks::Skip),
    (
        "Verify all manifest architectures",
        ToolProfileBuildChecks::Verify(None),
    ),
    (
        "Verify x86-64 builds",
        ToolProfileBuildChecks::Verify(Some(UtilityArchitecture::X86_64)),
    ),
    (
        "Verify ARM64 builds",
        ToolProfileBuildChecks::Verify(Some(UtilityArchitecture::Aarch64)),
    ),
];

pub enum Action {
    Open,
    Refresh,
    Loaded(Arc<()>, Result<ToolProfileAudit, String>),
    SelectChecks(Option<usize>),
    Filter(String),
    IssuesOnly(bool),
    Edit(String),
    Prepare(String),
    StopPreparing,
    Prepared(Arc<()>, Result<usize, String>),
    Rollout(rollout::Action),
    Back,
}

pub(super) struct AuditPage {
    filter: String,
    issues_only: bool,
    checks: ToolProfileBuildChecks,
    state: State,
    rollout: Option<Box<rollout::Panel>>,
    preparation_result: Option<String>,
}

enum State {
    Loading(Arc<()>, ComponentTask),
    Preparing(prepare::Work),
    Ready(ToolProfileAudit),
    Failed(String),
}

impl AuditPage {
    fn busy(&self) -> bool {
        matches!(self.state, State::Loading(..) | State::Preparing(_))
    }
}

impl Drop for AuditPage {
    fn drop(&mut self) {
        if let State::Loading(_, task) = &self.state {
            task.cancel();
        }
    }
}

fn load(
    catalog: PathBuf,
    id: TabId,
    checks: ToolProfileBuildChecks,
    ctx: &ComponentContext<TerminalApp>,
) -> State {
    let request = Arc::new(());
    let work_request = request.clone();
    let rejected = Message::ToolProfileAudit(
        id,
        Action::Loaded(request.clone(), Err("SSH tool audit unavailable".into())),
    );
    let task = ctx.spawn_background_with_rejection(
        move |cancel| {
            let result = ProfileStore::new(catalog)
                .and_then(|store| {
                    store.audit_tool_profiles_cancellable(None, checks, || cancel.is_cancelled())
                })
                .map_err(|error| format!("{error:#}"));
            Message::ToolProfileAudit(id, Action::Loaded(work_request, result))
        },
        rejected,
    );
    State::Loading(request, task)
}

impl TerminalApp {
    pub(super) fn tool_profile_audit(
        &mut self,
        id: TabId,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        if let Action::Rollout(action) = action {
            self.tool_profile_rollout(id, action, ctx);
            return;
        }
        let Some(Page::Settings(current)) = self.tabs.get_mut(id).map(|tab| &mut tab.page) else {
            return;
        };
        if matches!(action, Action::Open) && current.is_none() {
            let checks = ToolProfileBuildChecks::Skip;
            *current = Some(SettingsEditor::ToolProfileAudit(AuditPage {
                filter: String::new(),
                issues_only: false,
                checks,
                state: load(self.store.path().to_owned(), id, checks, ctx),
                rollout: None,
                preparation_result: None,
            }));
            return;
        }
        let Some(SettingsEditor::ToolProfileAudit(page)) = current else {
            return;
        };
        if page.rollout.is_some() {
            return;
        }
        match action {
            Action::Refresh if !page.busy() => {
                page.preparation_result = None;
                page.state = load(self.store.path().to_owned(), id, page.checks, ctx);
            }
            Action::SelectChecks(Some(index)) if !page.busy() => {
                if let Some(&(_, checks)) = CHECKS.get(index)
                    && checks != page.checks
                {
                    page.checks = checks;
                    page.preparation_result = None;
                    page.state = load(self.store.path().to_owned(), id, checks, ctx);
                }
            }
            Action::Loaded(request, result) if matches!(&page.state, State::Loading(active, _) if Arc::ptr_eq(active, &request)) => {
                page.state = match result {
                    Ok(report) => State::Ready(report),
                    Err(error) => State::Failed(error),
                }
            }
            Action::Filter(filter) => page.filter = filter,
            Action::IssuesOnly(issues_only) => page.issues_only = issues_only,
            Action::Prepare(name) => {
                if let State::Ready(report) = &page.state
                    && let ToolProfileBuildChecks::Verify(architecture) = page.checks
                    && let Some(entry) = report.profiles.iter().find(|entry| entry.profile == name)
                    && prepare::has_downloads(entry)
                    && let Some(digest) = &entry.manifest_sha256
                {
                    page.preparation_result = None;
                    page.state = State::Preparing(prepare::Work::start(
                        id,
                        self.store.path().to_owned(),
                        entry,
                        digest.clone(),
                        architecture,
                        ctx,
                    ));
                }
            }
            Action::StopPreparing => {
                if let State::Preparing(work) = &page.state {
                    work.stop();
                }
            }
            Action::Prepared(ticket, result) => {
                if let State::Preparing(work) = &page.state
                    && Arc::ptr_eq(&work.ticket, &ticket)
                {
                    page.preparation_result = Some(match result {
                        Ok(count) => format!(
                            "Prepared {count} pinned build{} for {}.",
                            if count == 1 { "" } else { "s" },
                            work.profile,
                        ),
                        Err(_) if work.stopping() => "SSH tool preparation cancelled.".into(),
                        Err(error) => {
                            format!("Could not prepare downloads for {}. {error}", work.profile)
                        }
                    });
                    page.state = load(self.store.path().to_owned(), id, page.checks, ctx);
                }
            }
            Action::Edit(name) => {
                self.edit_profile(name, ctx);
                if let Some(id) = self.tabs.active_id() {
                    self.edit_profile_tools(id, profile_tools::Action::Expanded(true), ctx);
                }
            }
            Action::Back => *current = None,
            Action::Open
            | Action::Refresh
            | Action::SelectChecks(_)
            | Action::Loaded(..)
            | Action::Rollout(_) => {}
        }
    }
}

pub(super) fn view(id: TabId, page: &AuditPage, ctx: &ViewContext<TerminalApp>) -> View {
    if let Some(rollout) = &page.rollout {
        return rollout.view(id, ctx);
    }
    let busy = page.busy();
    let mut fields = vec![
        KeyedView::new(
            "title",
            TextBlock::new()
                .text("SSH tool approval audit")
                .font_size(24.0),
        ),
        KeyedView::new(
            "actions",
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    Button::new()
                        .on_click(
                            ctx.callback(move |_| Message::ToolProfileAudit(id, Action::Back)),
                        )
                        .content("Back to settings"),
                    Button::new()
                        .is_enabled(!busy)
                        .on_click(
                            ctx.callback(move |_| Message::ToolProfileAudit(id, Action::Refresh)),
                        )
                        .content("Refresh audit"),
                )),
        ),
        KeyedView::new(
            "rollout-actions",
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    Button::new()
                        .is_enabled(!busy)
                        .on_click(ctx.callback(move |_| {
                            Message::ToolProfileAudit(id, Action::Rollout(rollout::Action::Open))
                        }))
                        .content("Roll out tool manifest"),
                    Button::new()
                        .is_enabled(!busy)
                        .on_click(ctx.callback(move |_| {
                            Message::ToolProfileAudit(
                                id,
                                Action::Rollout(rollout::Action::OpenReceipt),
                            )
                        }))
                        .content("Open tool rollout receipt…"),
                )),
        ),
        KeyedView::new(
            "checks",
            StackPanel::new().spacing(4.0).children((
                TextBlock::new().text("Audit scope"),
                ComboBox::new()
                    .automation_name("SSH tool audit scope")
                    .is_enabled(!busy)
                    .items_source(CHECKS.iter().map(|(label, _)| *label))
                    .selected_index(CHECKS.iter().position(|(_, checks)| *checks == page.checks))
                    .on_selection_changed(ctx.callback(move |index| {
                        Message::ToolProfileAudit(id, Action::SelectChecks(index))
                    })),
            )),
        ),
        KeyedView::new(
            "filter",
            TextBox::new()
                .automation_name("Filter audited SSH profiles")
                .placeholder_text("Filter by profile name or SSH address")
                .text(&page.filter)
                .on_text_changed(
                    ctx.callback(move |text| Message::ToolProfileAudit(id, Action::Filter(text))),
                ),
        ),
        KeyedView::new(
            "issues",
            CheckBox::new()
                .automation_name("Show only tool sync issues")
                .is_checked(page.issues_only)
                .on_is_checked_changed(ctx.callback(move |checked| {
                    Message::ToolProfileAudit(id, Action::IssuesOnly(checked))
                }))
                .content("Show only tool sync issues"),
        ),
    ];
    if let Some(result) = &page.preparation_result {
        fields.push(note("preparation-result", result));
    }
    match &page.state {
        State::Loading(..) => fields.push(note(
            "status",
            match page.checks {
                ToolProfileBuildChecks::Skip => "Checking saved SSH tool approvals…",
                ToolProfileBuildChecks::Verify(_) => "Verifying local and cached SSH tools…",
            },
        )),
        State::Preparing(work) => {
            let stopping = work.stopping();
            fields.push(note(
                "status",
                if stopping {
                    format!("Stopping tool preparation for {}…", work.profile)
                } else {
                    format!("Preparing pinned downloads for {}…", work.profile)
                },
            ));
            fields.push(KeyedView::new(
                "stop-preparing",
                Button::new()
                    .is_enabled(!stopping)
                    .on_click(
                        ctx.callback(move |_| Message::ToolProfileAudit(id, Action::StopPreparing)),
                    )
                    .content("Stop preparing tools"),
            ));
        }
        State::Failed(error) => fields.push(note("error", error)),
        State::Ready(report) => {
            let query = profile_query::Query::new(&page.filter);
            let mut rows = Vec::new();
            for entry in &report.profiles {
                let (label, issue) = status_label(&entry.status);
                if page.issues_only
                    && !issue
                    && entry.path_error.is_none()
                    && !entry
                        .build_checks
                        .iter()
                        .flatten()
                        .any(|check| !check.locally_available)
                {
                    continue;
                }
                let address = host_groups::ssh_address(&entry.user, &entry.host, entry.port);
                if query.rank(&entry.profile, &address).is_none() {
                    continue;
                }
                rows.push(row(id, entry, &address, label, ctx));
            }
            if rows.is_empty() {
                fields.push(note(
                    "status",
                    if report.profiles.is_empty() {
                        "No saved SSH profiles."
                    } else {
                        "No SSH profiles match these filters."
                    },
                ));
            } else {
                fields.push(note(
                    "status",
                    format!(
                        "Showing {} of {} SSH profiles.",
                        rows.len(),
                        report.profiles.len()
                    ),
                ));
                fields.extend(rows);
            }
        }
    }
    ScrollViewer::new().content(
        StackPanel::new()
            .margin(32.0)
            .max_width(820.0)
            .spacing(10.0)
            .keyed_children(fields),
    )
}

fn status_label(status: &ToolProfileAuditStatus) -> (&'static str, bool) {
    use ToolProfileAuditStatus::*;
    match status {
        Disabled => ("Tool sync off", false),
        NotRun => ("Profile uses alternate startup", false),
        InvalidSettings => ("Invalid tool approval", true),
        ManifestError => ("Manifest could not be loaded", true),
        ManifestScopeMismatch => ("Manifest destination changed", true),
        MatchesPin => ("Manifest matches its pin", false),
        PinMismatch => ("Manifest differs from its pin", true),
        FollowsFile => ("Follows the manifest file", false),
    }
}

fn row(
    id: TabId,
    entry: &ToolProfileAuditRow,
    address: &str,
    status: &str,
    ctx: &ViewContext<TerminalApp>,
) -> KeyedView {
    let profile = entry.profile.clone();
    let edit = ctx.callback(move |_| Message::ToolProfileAudit(id, Action::Edit(profile.clone())));
    let mut fields = vec![
        KeyedView::new(
            "header",
            Grid::new()
                .columns([GridLength::STAR, GridLength::Auto])
                .column_spacing(8.0)
                .children((
                    TextBlock::new()
                        .text(format!("{} · {address}", entry.profile))
                        .text_wrapping(TextWrapping::Wrap),
                    Button::new()
                        .grid_column(1)
                        .automation_name(format!("Edit SSH tools for {}", entry.profile))
                        .on_click(edit)
                        .content("Edit SSH tools…"),
                )),
        ),
        note("status", status),
    ];
    if let Some(error) = &entry.detail {
        fields.push(note("detail", error));
    }
    if let Some(error) = &entry.path_error {
        fields.push(note("path-error", format!("Shell PATH · {error}")));
    }
    if let Some(bundle) = &entry.bundle {
        let mut details = vec![bundle.display().to_string()];
        if let Some(pin) = &entry.pinned_manifest_sha256 {
            details.push(format!("Pinned SHA-256 {pin}"));
        }
        if let Some(digest) = &entry.manifest_sha256 {
            details.push(format!("Current SHA-256 {digest}"));
        }
        fields.push(code_details(
            "manifest".into(),
            &format!("Manifest details for {}", entry.profile),
            &details.join("\n"),
        ));
    }
    for check in entry.build_checks.iter().flatten() {
        let architecture = profile_tools::architecture_label(check.architecture);
        let available = check
            .commands
            .iter()
            .filter(|command| command.locally_available)
            .count();
        fields.push(note(
            format!("build-summary-{architecture}"),
            format!(
                "{architecture} · {available} of {} commands ready locally",
                check.commands.len()
            ),
        ));
        fields.push(code_details(
            format!("builds-{architecture}"),
            &format!("{architecture} builds for {}", entry.profile),
            &build_details(check),
        ));
    }
    if prepare::has_downloads(entry) {
        let profile = entry.profile.clone();
        fields.push(KeyedView::new(
            "prepare",
            Button::new()
                .automation_name(format!("Prepare pinned downloads for {profile}"))
                .on_click(ctx.callback(move |_| {
                    Message::ToolProfileAudit(id, Action::Prepare(profile.clone()))
                }))
                .content("Prepare pinned downloads"),
        ));
    }
    KeyedView::new(
        format!("profile-{}", entry.profile),
        StackPanel::new().spacing(4.0).keyed_children(fields),
    )
}

fn build_details(check: &ToolArchitectureAudit) -> String {
    let mut lines = Vec::new();
    for command in &check.commands {
        if command.builds.is_empty() {
            lines.push(format!("{} · No build in manifest", command.name));
            continue;
        }
        lines.push(command.name.clone());
        for build in &command.builds {
            let status = match build.status {
                ToolExecutableStatus::Verified => "Verified",
                ToolExecutableStatus::Missing => "Missing",
                ToolExecutableStatus::Error => "Could not verify",
            };
            let source = match build.source {
                ToolExecutableSource::LocalFile => "local file",
                ToolExecutableSource::DownloadCache => "cached download",
            };
            lines.push(format!("{status} {source}"));
            if let Some(path) = &build.path {
                lines.push(path.display().to_string());
            }
            lines.push(format!("Executable SHA-256 {}", build.pinned_sha256));
            if let Some(interpreter) = &build.interpreter {
                lines.push(format!("Interpreter {interpreter}"));
            }
            if let Some(glibc) = &build.minimum_glibc {
                lines.push(format!("Minimum glibc {glibc}"));
            }
            if let Some(detail) = &build.detail {
                lines.push(detail.clone());
            }
        }
        lines.push(String::new());
    }
    lines.join("\n").trim_end().to_owned()
}

fn note(key: impl Into<Key>, text: impl Into<String>) -> KeyedView {
    KeyedView::new(
        key,
        TextBlock::new()
            .text(text.into())
            .text_wrapping(TextWrapping::Wrap),
    )
}
