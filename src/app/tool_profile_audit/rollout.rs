use super::*;
use std::collections::BTreeSet;
use w11term::{
    options::ToolSync,
    profiles::{ToolManifestBaseline, ToolRolloutReport},
};

#[derive(Clone)]
pub enum Action {
    Open,
    Picked(Arc<()>, Result<Option<Arc<Targets>>, String>),
    Filter(Arc<()>, String),
    Select(Arc<()>, String, bool),
    Review(Arc<()>),
    Apply(Arc<()>),
    Completed(Arc<()>, Result<Arc<ToolRolloutReport>, String>),
    Back(Arc<()>),
    Close(Arc<()>),
}

pub struct Targets {
    candidate: PathBuf,
    profiles: Vec<(String, ToolSync)>,
}

enum Work {
    Picking,
    Choosing,
    Reviewing,
    Reviewed(Arc<ToolRolloutReport>),
    Applying,
    Applied(usize),
}

pub(super) struct Panel {
    ticket: Arc<()>,
    targets: Option<Arc<Targets>>,
    selected: BTreeSet<String>,
    filter: String,
    work: Work,
    task: Option<ComponentTask>,
    error: Option<String>,
}

fn message(id: TabId, action: Action) -> Message {
    Message::ToolProfileAudit(id, super::Action::Rollout(action))
}

impl Drop for Panel {
    fn drop(&mut self) {
        if let Some(task) = &self.task
            && !matches!(self.work, Work::Applying)
        {
            task.cancel();
        }
    }
}

impl Panel {
    fn pick(
        id: TabId,
        owner: usize,
        catalog: PathBuf,
        ctx: &ComponentContext<TerminalApp>,
    ) -> Self {
        let ticket = Arc::new(());
        let request = ticket.clone();
        let rejected = message(
            id,
            Action::Picked(
                ticket.clone(),
                Err("Tool manifest picker unavailable".into()),
            ),
        );
        let task = ctx.spawn_background_with_rejection(
            move |cancel| {
                let picked = (|| -> anyhow::Result<_> {
                    let purpose = file_picker::Purpose::ToolBundle(catalog.clone());
                    let Some(file) = file_picker::pick(owner, purpose)? else {
                        return Ok(None);
                    };
                    if cancel.is_cancelled() {
                        return Ok(None);
                    }
                    let profiles = ProfileStore::new(catalog)?.tool_rollout_targets(&file.path)?;
                    Ok(Some(Arc::new(Targets {
                        candidate: file.path,
                        profiles,
                    })))
                })()
                .map_err(|error| format!("{error:#}"));
                message(id, Action::Picked(request, picked))
            },
            rejected,
        );
        Self {
            ticket,
            targets: None,
            selected: BTreeSet::new(),
            filter: String::new(),
            work: Work::Picking,
            task: Some(task),
            error: None,
        }
    }

    fn request(
        &mut self,
        id: TabId,
        catalog: PathBuf,
        review: Option<String>,
        ctx: &ComponentContext<TerminalApp>,
    ) {
        let Some(targets) = &self.targets else {
            return;
        };
        self.work = if review.is_some() {
            Work::Applying
        } else {
            Work::Reviewing
        };
        self.error = None;
        let ticket = self.ticket.clone();
        let candidate = targets.candidate.clone();
        let selected: Vec<_> = self.selected.iter().cloned().collect();
        let rejected = message(
            id,
            Action::Completed(
                ticket.clone(),
                Err("Tool manifest rollout unavailable".into()),
            ),
        );
        self.task = Some(ctx.spawn_background_with_rejection(
            move |_| {
                let result = ProfileStore::new(catalog)
                    .and_then(|store| store.rollout_tools(&selected, &candidate, review.as_deref()))
                    .map(Arc::new)
                    .map_err(|error| format!("{error:#}"));
                message(id, Action::Completed(ticket, result))
            },
            rejected,
        ));
    }

    pub(super) fn view(&self, id: TabId, ctx: &ViewContext<TerminalApp>) -> View {
        let send = |action: Action| ctx.callback(move |_| message(id, action.clone()));
        let mut fields = vec![KeyedView::new(
            "heading",
            TextBlock::new()
                .font_size(24.0)
                .text("Roll out tool manifest"),
        )];
        if let Some(targets) = &self.targets {
            fields.push(note("candidate", targets.candidate.display().to_string()));
        }
        if let Some(error) = &self.error {
            fields.push(note("error", error));
        }
        match &self.work {
            Work::Picking => fields.push(note("status", "Choosing a tool manifest…")),
            Work::Reviewing => fields.push(note("status", "Checking selected tool approvals…")),
            Work::Applying => fields.push(note("status", "Saving the reviewed tool rollout…")),
            Work::Applied(count) => fields.push(note(
                "status",
                format!("Updated tool manifests in {count} saved SSH profiles."),
            )),
            Work::Choosing => {
                if let Some(targets) = &self.targets {
                    if targets.profiles.is_empty() {
                        fields.push(note("status", "No saved SSH profiles approve this bundle name and launcher directory."));
                    } else {
                        fields.push(KeyedView::new(
                            "filter",
                            TextBox::new()
                                .automation_name("Filter tool rollout profiles")
                                .placeholder_text("Filter by profile name or SSH address")
                                .text(&self.filter)
                                .on_text_changed(ctx.callback({
                                    let ticket = self.ticket.clone();
                                    move |text| message(id, Action::Filter(ticket.clone(), text))
                                })),
                        ));
                        let query = profile_query::Query::new(&self.filter);
                        let mut visible = 0;
                        for (name, settings) in &targets.profiles {
                            let address = host_groups::ssh_address(
                                &settings.user,
                                &settings.host,
                                settings.port,
                            );
                            if query.rank(name, &address).is_none() {
                                continue;
                            }
                            visible += 1;
                            let selected = name.clone();
                            let ticket = self.ticket.clone();
                            fields.push(KeyedView::new(
                                format!("profile-{name}"),
                                CheckBox::new()
                                    .automation_name(format!("Tool rollout profile {name}"))
                                    .is_checked(self.selected.contains(name))
                                    .on_is_checked_changed(ctx.callback(move |checked| {
                                        message(
                                            id,
                                            Action::Select(
                                                ticket.clone(),
                                                selected.clone(),
                                                checked,
                                            ),
                                        )
                                    }))
                                    .content(format!("{name} · {address}")),
                            ));
                        }
                        if visible == 0 {
                            fields.push(note(
                                "status",
                                "No eligible SSH profiles match this filter.",
                            ));
                        }
                        fields.push(note(
                            "selected",
                            format!("{} selected", self.selected.len()),
                        ));
                        fields.push(KeyedView::new(
                            "review",
                            Button::new()
                                .is_enabled(!self.selected.is_empty())
                                .on_click(send(Action::Review(self.ticket.clone())))
                                .content("Review selected tool rollout"),
                        ));
                    }
                }
            }
            Work::Reviewed(report) => {
                fields.push(note(
                    "digest",
                    format!(
                        "Proposed manifest SHA-256 {}",
                        report.candidate_manifest_sha256
                    ),
                ));
                fields.push(note(
                    "commands",
                    format!(
                        "Proposed commands · {}",
                        report.candidate_commands.join(", ")
                    ),
                ));
                fields.extend(report.profiles.iter().map(review_row));
                fields.push(KeyedView::new(
                    "review-actions",
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            Button::new()
                                .is_enabled(report.changed_profiles > 0)
                                .on_click(send(Action::Apply(self.ticket.clone())))
                                .content("Apply reviewed tool rollout"),
                            Button::new()
                                .on_click(send(Action::Back(self.ticket.clone())))
                                .content("Back to selected profiles"),
                        )),
                ));
            }
        }
        fields.push(KeyedView::new(
            "close",
            Button::new()
                .is_enabled(!matches!(self.work, Work::Applying))
                .on_click(send(Action::Close(self.ticket.clone())))
                .content("Close tool rollout"),
        ));
        ScrollViewer::new().content(
            StackPanel::new()
                .margin(32.0)
                .max_width(820.0)
                .spacing(10.0)
                .keyed_children(fields),
        )
    }
}

fn review_row(entry: &w11term::profiles::ToolRolloutProfile) -> KeyedView {
    let before = &entry.previous;
    let address = host_groups::ssh_address(&before.user, &before.host, before.port);
    let policy = entry
        .next_pin
        .as_deref()
        .map_or("Follows the manifest file", |_| "Keeps a manifest pin");
    let baseline = match &entry.baseline {
        ToolManifestBaseline::Pinned(_) => "Compared with the approved pinned manifest",
        ToolManifestBaseline::FollowsFile(_) => "Compared with the previous manifest file",
        ToolManifestBaseline::PinMismatch(_) => "The previous manifest differs from its pin",
        ToolManifestBaseline::ScopeMismatch(_) => {
            "The previous manifest destination differs from its approval"
        }
        ToolManifestBaseline::Unavailable => "The previous manifest could not be loaded",
    };
    let mut fields = vec![
        note("name", format!("{} · {address}", entry.profile)),
        note(
            "path",
            format!("Previous manifest · {}", before.bundle.display()),
        ),
        note("policy", policy),
        note("baseline", baseline),
    ];
    if !entry.changed {
        fields.push(note(
            "unchanged",
            "This profile already uses the selected manifest.",
        ));
    }
    if let Some(changes) = &entry.changes {
        for (key, label, commands) in [
            ("added", "Added commands", &changes.added),
            ("removed", "Removed commands", &changes.removed),
            ("changed", "Changed builds", &changes.changed),
        ] {
            if !commands.is_empty() {
                fields.push(note(key, format!("{label} · {}", commands.join(", "))));
            }
        }
    }
    KeyedView::new(
        format!("review-{}", entry.profile),
        StackPanel::new().spacing(4.0).keyed_children(fields),
    )
}

impl TerminalApp {
    pub(super) fn tool_profile_rollout(
        &mut self,
        id: TabId,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        if let Action::Completed(_, Ok(report)) = &action
            && report.applied
        {
            self.adopt_tool_rollout(report);
            self.refresh_profiles();
        }
        let owner = self.keyboard.hwnd().map(|window| window.0 as usize);
        let catalog = self.store.path().to_owned();
        let Some(Page::Settings(Some(SettingsEditor::ToolProfileAudit(page)))) =
            self.tabs.get_mut(id).map(|tab| &mut tab.page)
        else {
            return;
        };
        if matches!(action, Action::Open) {
            if page.rollout.is_none()
                && !matches!(page.state, State::Loading(..))
                && let Some(owner) = owner
            {
                page.rollout = Some(Box::new(Panel::pick(id, owner, catalog, ctx)));
            }
            return;
        }
        let Some(panel) = &mut page.rollout else {
            return;
        };
        let request = match &action {
            Action::Picked(ticket, _)
            | Action::Filter(ticket, _)
            | Action::Select(ticket, ..)
            | Action::Review(ticket)
            | Action::Apply(ticket)
            | Action::Completed(ticket, _)
            | Action::Back(ticket)
            | Action::Close(ticket) => ticket,
            Action::Open => unreachable!(),
        };
        if !Arc::ptr_eq(&panel.ticket, request) {
            return;
        }
        match action {
            Action::Picked(_, result) if matches!(panel.work, Work::Picking) => {
                panel.task = None;
                panel.work = Work::Choosing;
                match result {
                    Ok(Some(targets)) => panel.targets = Some(targets),
                    Ok(None) => page.rollout = None,
                    Err(error) => panel.error = Some(error),
                }
            }
            Action::Filter(_, filter) if matches!(panel.work, Work::Choosing) => {
                panel.filter = filter
            }
            Action::Select(_, name, checked) if matches!(panel.work, Work::Choosing) => {
                if checked {
                    panel.selected.insert(name);
                } else {
                    panel.selected.remove(&name);
                }
            }
            Action::Review(_)
                if matches!(panel.work, Work::Choosing) && !panel.selected.is_empty() =>
            {
                panel.request(id, catalog, None, ctx);
            }
            Action::Apply(_) => {
                if let Work::Reviewed(report) = &panel.work
                    && report.changed_profiles > 0
                {
                    panel.request(id, catalog, Some(report.review.clone()), ctx);
                }
            }
            Action::Completed(_, result)
                if matches!(panel.work, Work::Reviewing | Work::Applying) =>
            {
                panel.task = None;
                panel.work = match result {
                    Ok(report) if report.applied => Work::Applied(report.changed_profiles),
                    Ok(report) => Work::Reviewed(report),
                    Err(error) => {
                        panel.error = Some(error);
                        Work::Choosing
                    }
                };
            }
            Action::Back(_) if matches!(panel.work, Work::Reviewed(_)) => {
                panel.work = Work::Choosing
            }
            Action::Close(_) if !matches!(panel.work, Work::Applying) => {
                if matches!(panel.work, Work::Applied(_)) {
                    page.state = load(catalog, id, page.checks, ctx);
                }
                page.rollout = None;
            }
            _ => {}
        }
    }

    fn adopt_tool_rollout(&mut self, report: &ToolRolloutReport) {
        let ids: Vec<_> = self.tabs.entries().iter().map(|tab| tab.id).collect();
        let base_path = self.store.path().parent().unwrap_or(Path::new("."));
        for id in ids {
            let Some(Page::NewProfile(draft)) = self.tabs.get_mut(id).map(|tab| &mut tab.page)
            else {
                continue;
            };
            let Some(base) = draft.origin.editing() else {
                continue;
            };
            let Some(entry) = report
                .profiles
                .iter()
                .find(|entry| entry.changed && entry.profile == base.name)
            else {
                continue;
            };
            if base.options.tool_bundle.as_deref() != Some(&entry.previous) {
                continue;
            }
            let mut next = entry.previous.clone();
            next.bundle = report.candidate_path.clone();
            next.manifest_sha256 = entry.next_pin.clone();
            let mut expected = base.options.clone();
            expected.tool_bundle = Some(Box::new(next.clone()));
            let Ok((_, saved)) = self.store.inspect_catalog(&base.name) else {
                continue;
            };
            if saved.options == expected {
                draft
                    .tool_bundle
                    .adopt_rollout(&entry.previous, &next, base_path);
                draft.origin = DraftOrigin::Edit(saved);
            }
        }
    }
}
