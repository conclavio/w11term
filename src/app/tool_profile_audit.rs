use super::*;
use crate::app::command_reference::dotfiles::comparison::code_details;
use std::path::PathBuf;
use w11term::profiles::{ToolProfileAudit, ToolProfileAuditRow, ToolProfileAuditStatus};

pub enum Action {
    Open,
    Refresh,
    Loaded(Arc<()>, Result<ToolProfileAudit, String>),
    Filter(String),
    IssuesOnly(bool),
    Edit(String),
    Back,
}

pub(super) struct AuditPage {
    filter: String,
    issues_only: bool,
    state: State,
}

enum State {
    Loading(Arc<()>),
    Ready(ToolProfileAudit),
    Failed(String),
}

fn load(catalog: PathBuf, id: TabId, ctx: &ComponentContext<TerminalApp>) -> State {
    let request = Arc::new(());
    let state = State::Loading(request.clone());
    let rejected = Message::ToolProfileAudit(
        id,
        Action::Loaded(request.clone(), Err("SSH tool audit unavailable".into())),
    );
    ctx.spawn_background_with_rejection(
        move |_| {
            let result = ProfileStore::new(catalog)
                .and_then(|store| store.audit_tool_profiles(None))
                .map_err(|error| format!("{error:#}"));
            Message::ToolProfileAudit(id, Action::Loaded(request, result))
        },
        rejected,
    );
    state
}

impl TerminalApp {
    pub(super) fn tool_profile_audit(
        &mut self,
        id: TabId,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        let Some(Page::Settings(current)) = self.tabs.get_mut(id).map(|tab| &mut tab.page) else {
            return;
        };
        if matches!(action, Action::Open) && current.is_none() {
            *current = Some(SettingsEditor::ToolProfileAudit(AuditPage {
                filter: String::new(),
                issues_only: false,
                state: load(self.store.path().to_owned(), id, ctx),
            }));
            return;
        }
        let Some(SettingsEditor::ToolProfileAudit(page)) = current else {
            return;
        };
        match action {
            Action::Refresh if !matches!(page.state, State::Loading(_)) => {
                page.state = load(self.store.path().to_owned(), id, ctx);
            }
            Action::Loaded(request, result) if matches!(&page.state, State::Loading(active) if Arc::ptr_eq(active, &request)) => {
                page.state = match result {
                    Ok(report) => State::Ready(report),
                    Err(error) => State::Failed(error),
                }
            }
            Action::Filter(filter) => page.filter = filter,
            Action::IssuesOnly(issues_only) => page.issues_only = issues_only,
            Action::Edit(name) => {
                self.edit_profile(name, ctx);
                if let Some(id) = self.tabs.active_id() {
                    self.edit_profile_tools(id, profile_tools::Action::Expanded(true), ctx);
                }
            }
            Action::Back => *current = None,
            Action::Open | Action::Refresh | Action::Loaded(..) => {}
        }
    }
}

pub(super) fn view(id: TabId, page: &AuditPage, ctx: &ViewContext<TerminalApp>) -> View {
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
                        .is_enabled(!matches!(page.state, State::Loading(_)))
                        .on_click(
                            ctx.callback(move |_| Message::ToolProfileAudit(id, Action::Refresh)),
                        )
                        .content("Refresh audit"),
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
    match &page.state {
        State::Loading(_) => fields.push(note("status", "Checking saved SSH tool approvals…")),
        State::Failed(error) => fields.push(note("error", error)),
        State::Ready(report) => {
            let query = profile_query::Query::new(&page.filter);
            let mut rows = Vec::new();
            for entry in &report.profiles {
                let (label, issue) = status_label(&entry.status);
                if page.issues_only && !issue && entry.path_error.is_none() {
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
    KeyedView::new(
        format!("profile-{}", entry.profile),
        StackPanel::new().spacing(4.0).keyed_children(fields),
    )
}

fn note(key: &'static str, text: impl Into<String>) -> KeyedView {
    KeyedView::new(
        key,
        TextBlock::new()
            .text(text.into())
            .text_wrapping(TextWrapping::Wrap),
    )
}
