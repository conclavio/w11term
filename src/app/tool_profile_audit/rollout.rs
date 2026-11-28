use super::*;
use std::collections::{BTreeMap, BTreeSet};
use w11term::{
    options::ToolSync,
    profiles::{
        ToolManifestBaseline, ToolRollbackReport, ToolRolloutBuildPolicy, ToolRolloutBuildReport,
        ToolRolloutReport,
    },
};

mod candidate;
mod rollback;

#[derive(Clone)]
pub enum Action {
    Open,
    OpenReceipt,
    Picked(Arc<()>, Result<Option<Picked>, String>),
    Filter(Arc<()>, String),
    Select(Arc<()>, String, bool),
    SelectBuildRequirement(Arc<()>, Option<usize>),
    Review(Arc<()>),
    CheckCandidate(Arc<()>),
    PrepareCandidate(Arc<()>, UtilityArchitecture),
    CandidateChecked(Arc<()>, Arc<()>, candidate::ChecksResult),
    Apply(Arc<()>),
    Completed(Arc<()>, Result<Arc<ToolRolloutReport>, String>),
    SaveReceipt(Arc<()>),
    ReceiptSaved(Arc<()>, Result<Option<PathBuf>, String>),
    ReviewRollback(Arc<()>),
    ApplyRollback(Arc<()>),
    RollbackCompleted(Arc<()>, Result<Arc<ToolRollbackReport>, String>),
    Back(Arc<()>),
    Close(Arc<()>),
}

pub struct Targets {
    candidate: PathBuf,
    profiles: Vec<(String, ToolSync)>,
}

#[derive(Clone)]
pub enum Picked {
    Manifest(Arc<Targets>),
    Receipt(PathBuf),
}

#[derive(Clone, Copy)]
enum Picker {
    Manifest,
    Receipt,
}

enum Work {
    Picking(Picker),
    PickFailed(Picker),
    Choosing,
    Reviewing,
    Reviewed(Arc<ToolRolloutReport>),
    Applying,
    Applied(Arc<ToolRolloutReport>),
    SavingReceipt(Arc<ToolRolloutReport>),
    Rollback(rollback::Source, rollback::Stage),
    RolledBack(usize),
}

pub(super) struct Panel {
    ticket: Arc<()>,
    targets: Option<Arc<Targets>>,
    selected: BTreeSet<String>,
    filter: String,
    work: Work,
    task: Option<ComponentTask>,
    error: Option<String>,
    saved_receipt: Option<PathBuf>,
    candidate_checks: candidate::Checks,
    build_requirement: usize,
}

fn message(id: TabId, action: Action) -> Message {
    Message::ToolProfileAudit(id, super::Action::Rollout(action))
}

impl Drop for Panel {
    fn drop(&mut self) {
        if let Some(task) = &self.task
            && !self.applying()
        {
            task.cancel();
        }
    }
}

impl Panel {
    fn applying(&self) -> bool {
        matches!(
            self.work,
            Work::Applying | Work::Rollback(_, rollback::Stage::Applying)
        )
    }

    fn can_close(&self) -> bool {
        !self.applying() && !matches!(self.work, Work::Picking(_) | Work::SavingReceipt(_))
    }

    fn can_apply(&self, report: &ToolRolloutReport) -> bool {
        report.changed_profiles > 0
            && !self.candidate_checks.busy()
            && (candidate::REQUIREMENTS[self.build_requirement].1.is_none()
                || self.candidate_checks.ready())
    }

    fn pick(
        id: TabId,
        owner: usize,
        catalog: PathBuf,
        picker: Picker,
        ctx: &ComponentContext<TerminalApp>,
    ) -> Self {
        let ticket = Arc::new(());
        let request = ticket.clone();
        let rejected = message(
            id,
            Action::Picked(
                ticket.clone(),
                Err("SSH tool rollout picker unavailable".into()),
            ),
        );
        let task = ctx.spawn_background_with_rejection(
            move |cancel| {
                let picked = (|| -> anyhow::Result<_> {
                    let purpose = match picker {
                        Picker::Manifest => file_picker::Purpose::ToolBundle(catalog.clone()),
                        Picker::Receipt => rollback::receipt_picker(catalog.clone(), false),
                    };
                    let Some(file) = file_picker::pick(owner, purpose)? else {
                        return Ok(None);
                    };
                    if cancel.is_cancelled() {
                        return Ok(None);
                    }
                    match picker {
                        Picker::Manifest => {
                            let profiles =
                                ProfileStore::new(catalog)?.tool_rollout_targets(&file.path)?;
                            Ok(Some(Picked::Manifest(Arc::new(Targets {
                                candidate: file.path,
                                profiles,
                            }))))
                        }
                        Picker::Receipt => Ok(Some(Picked::Receipt(file.path))),
                    }
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
            work: Work::Picking(picker),
            task: Some(task),
            error: None,
            saved_receipt: None,
            candidate_checks: candidate::Checks::default(),
            build_requirement: 0,
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
        self.candidate_checks = candidate::Checks::default();
        self.error = None;
        let ticket = self.ticket.clone();
        let candidate = targets.candidate.clone();
        let selected: Vec<_> = self.selected.iter().cloned().collect();
        let policy = candidate::REQUIREMENTS[self.build_requirement].1;
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
                    .and_then(|store| {
                        store.rollout_tools_with_builds(
                            &selected,
                            &candidate,
                            review.as_deref(),
                            policy,
                        )
                    })
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
            TextBlock::new().font_size(24.0).text(match self.work {
                Work::Picking(Picker::Receipt)
                | Work::PickFailed(Picker::Receipt)
                | Work::Rollback(..)
                | Work::RolledBack(_) => "Roll back tool manifest",
                _ => "Roll out tool manifest",
            }),
        )];
        if let Some(targets) = &self.targets {
            fields.push(note("candidate", targets.candidate.display().to_string()));
        }
        if let Some(error) = &self.error {
            fields.push(note("error", error));
        }
        match &self.work {
            Work::Picking(Picker::Manifest) => {
                fields.push(note("status", "Choosing a tool manifest…"))
            }
            Work::Picking(Picker::Receipt) => {
                fields.push(note("status", "Choosing a tool rollout receipt…"))
            }
            Work::PickFailed(_) => {}
            Work::Reviewing => fields.push(note("status", "Checking selected tool approvals…")),
            Work::Applying => fields.push(note("status", "Saving the reviewed tool rollout…")),
            Work::Applied(report) => {
                fields.push(note(
                    "status",
                    format!(
                        "Updated tool manifests in {} saved SSH profiles.",
                        report.changed_profiles
                    ),
                ));
                if let Some(path) = &self.saved_receipt {
                    fields.push(note(
                        "saved-receipt",
                        format!("Saved rollout receipt · {}", path.display()),
                    ));
                }
                fields.push(KeyedView::new(
                    "applied-actions",
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            Button::new()
                                .on_click(send(Action::SaveReceipt(self.ticket.clone())))
                                .content("Save tool rollout receipt…"),
                            Button::new()
                                .on_click(send(Action::ReviewRollback(self.ticket.clone())))
                                .content("Review tool rollback"),
                        )),
                ));
            }
            Work::SavingReceipt(_) => {
                fields.push(note("status", "Saving the tool rollout receipt…"))
            }
            Work::Rollback(source, stage) => {
                rollback::describe(&mut fields, source, stage, &self.ticket, send);
            }
            Work::RolledBack(count) => fields.push(note(
                "status",
                format!("Restored tool manifests in {count} saved SSH profiles."),
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
                            "build-requirement",
                            StackPanel::new().spacing(4.0).children((
                                TextBlock::new().text("Candidate build requirement"),
                                ComboBox::new()
                                    .automation_name("Candidate build requirement")
                                    .items_source(candidate::REQUIREMENTS.map(|(label, _)| label))
                                    .selected_index(Some(self.build_requirement))
                                    .on_selection_changed(ctx.callback({
                                        let ticket = self.ticket.clone();
                                        move |index| {
                                            message(
                                                id,
                                                Action::SelectBuildRequirement(
                                                    ticket.clone(),
                                                    index,
                                                ),
                                            )
                                        }
                                    })),
                            )),
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
                self.candidate_checks
                    .describe(&mut fields, &self.ticket, send);
                let (requirement, policy) = candidate::REQUIREMENTS[self.build_requirement];
                if policy.is_some() {
                    fields.push(note("selected-build-requirement", requirement));
                    if !self.candidate_checks.busy() && !self.candidate_checks.ready() {
                        fields.push(note(
                            "build-requirement-unmet",
                            "Prepare or repair the required builds before applying the rollout.",
                        ));
                    }
                }
                fields.extend(report.profiles.iter().map(review_row));
                fields.push(KeyedView::new(
                    "review-actions",
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            Button::new()
                                .is_enabled(self.can_apply(report))
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
                .is_enabled(self.can_close())
                .on_click(send(Action::Close(self.ticket.clone())))
                .content(
                    if matches!(
                        self.work,
                        Work::Rollback(..)
                            | Work::RolledBack(_)
                            | Work::PickFailed(Picker::Receipt)
                    ) {
                        "Close tool rollback"
                    } else {
                        "Close tool rollout"
                    },
                ),
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
            self.adopt_tool_bindings(report.profiles.iter().filter(|entry| entry.changed).map(
                |entry| {
                    let mut next = entry.previous.clone();
                    next.bundle.clone_from(&report.candidate_path);
                    next.manifest_sha256.clone_from(&entry.next_pin);
                    (entry.profile.as_str(), &entry.previous, next)
                },
            ));
            self.refresh_profiles();
        }
        if let Action::RollbackCompleted(_, Ok(report)) = &action
            && report.applied
        {
            self.adopt_tool_bindings(report.profiles.iter().map(|entry| {
                (
                    entry.profile.as_str(),
                    &entry.current,
                    entry.restore.clone(),
                )
            }));
            self.refresh_profiles();
        }
        let owner = self.keyboard.hwnd().map(|window| window.0 as usize);
        let catalog = self.store.path().to_owned();
        let Some(Page::Settings(Some(SettingsEditor::ToolProfileAudit(page)))) =
            self.tabs.get_mut(id).map(|tab| &mut tab.page)
        else {
            return;
        };
        if matches!(action, Action::Open | Action::OpenReceipt) {
            if page.rollout.is_none()
                && !page.busy()
                && let Some(owner) = owner
            {
                let picker = if matches!(action, Action::OpenReceipt) {
                    Picker::Receipt
                } else {
                    Picker::Manifest
                };
                page.rollout = Some(Box::new(Panel::pick(id, owner, catalog, picker, ctx)));
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
            | Action::SelectBuildRequirement(ticket, _)
            | Action::Review(ticket)
            | Action::CheckCandidate(ticket)
            | Action::PrepareCandidate(ticket, _)
            | Action::CandidateChecked(ticket, ..)
            | Action::Apply(ticket)
            | Action::Completed(ticket, _)
            | Action::SaveReceipt(ticket)
            | Action::ReceiptSaved(ticket, _)
            | Action::ReviewRollback(ticket)
            | Action::ApplyRollback(ticket)
            | Action::RollbackCompleted(ticket, _)
            | Action::Back(ticket)
            | Action::Close(ticket) => ticket,
            Action::Open | Action::OpenReceipt => unreachable!(),
        };
        if !Arc::ptr_eq(&panel.ticket, request) {
            return;
        }
        match action {
            Action::Picked(_, result) => {
                let Work::Picking(picker) = panel.work else {
                    return;
                };
                panel.task = None;
                match result {
                    Ok(Some(Picked::Manifest(targets))) => {
                        panel.targets = Some(targets);
                        panel.work = Work::Choosing;
                    }
                    Ok(Some(Picked::Receipt(path))) => {
                        panel.request_rollback(
                            id,
                            catalog,
                            rollback::Source::Receipt(path),
                            None,
                            ctx,
                        );
                    }
                    Ok(None) => page.rollout = None,
                    Err(error) => {
                        panel.work = Work::PickFailed(picker);
                        panel.error = Some(error);
                    }
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
            Action::SelectBuildRequirement(_, Some(index))
                if matches!(panel.work, Work::Choosing)
                    && index < candidate::REQUIREMENTS.len() =>
            {
                panel.build_requirement = index;
            }
            Action::Review(_)
                if matches!(panel.work, Work::Choosing) && !panel.selected.is_empty() =>
            {
                panel.request(id, catalog, None, ctx);
            }
            Action::Apply(_) => {
                if let Work::Reviewed(report) = &panel.work
                    && panel.can_apply(report)
                {
                    panel.request(id, catalog, Some(report.review.clone()), ctx);
                }
            }
            Action::CheckCandidate(_) => {
                if let Work::Reviewed(report) = &panel.work
                    && !panel.candidate_checks.busy()
                {
                    panel.candidate_checks.start(
                        id,
                        panel.ticket.clone(),
                        report.clone(),
                        None,
                        ctx,
                    );
                }
            }
            Action::PrepareCandidate(_, architecture) => {
                if let Work::Reviewed(report) = &panel.work
                    && !panel.candidate_checks.busy()
                    && panel.candidate_checks.can_prepare(architecture)
                {
                    panel.candidate_checks.start(
                        id,
                        panel.ticket.clone(),
                        report.clone(),
                        Some(architecture),
                        ctx,
                    );
                }
            }
            Action::CandidateChecked(_, ticket, result) => {
                panel.candidate_checks.finish(&ticket, result);
            }
            Action::Completed(_, result)
                if matches!(panel.work, Work::Reviewing | Work::Applying) =>
            {
                panel.task = None;
                panel.work = match result {
                    Ok(report) if report.applied => Work::Applied(report),
                    Ok(report) => {
                        if let Some(builds) = &report.candidate_builds {
                            panel.candidate_checks.use_review(builds);
                        } else {
                            panel.candidate_checks.start(
                                id,
                                panel.ticket.clone(),
                                report.clone(),
                                None,
                                ctx,
                            );
                        }
                        Work::Reviewed(report)
                    }
                    Err(error) => {
                        panel.error = Some(error);
                        Work::Choosing
                    }
                };
            }
            Action::Back(_) if matches!(panel.work, Work::Reviewed(_)) => {
                panel.candidate_checks = candidate::Checks::default();
                panel.work = Work::Choosing
            }
            Action::Back(_) => {
                if let Work::Rollback(
                    rollback::Source::Applied(report),
                    rollback::Stage::Ready | rollback::Stage::Reviewed(_),
                ) = &panel.work
                {
                    panel.work = Work::Applied(report.clone());
                    panel.error = None;
                }
            }
            Action::SaveReceipt(_) => {
                if let Work::Applied(report) = &panel.work
                    && let Some(owner) = owner
                {
                    panel.save_receipt(id, owner, report.clone(), ctx);
                }
            }
            Action::ReceiptSaved(_, result) => {
                let Work::SavingReceipt(report) = &panel.work else {
                    return;
                };
                panel.task = None;
                panel.work = Work::Applied(report.clone());
                match result {
                    Ok(Some(path)) => panel.saved_receipt = Some(path),
                    Ok(None) => {}
                    Err(error) => panel.error = Some(error),
                }
            }
            Action::ReviewRollback(_) => {
                let source = match &panel.work {
                    Work::Applied(report) => rollback::Source::Applied(report.clone()),
                    Work::Rollback(source, rollback::Stage::Ready) => source.clone(),
                    _ => return,
                };
                panel.request_rollback(id, catalog, source, None, ctx);
            }
            Action::ApplyRollback(_) => {
                if let Work::Rollback(source, rollback::Stage::Reviewed(report)) = &panel.work {
                    panel.request_rollback(
                        id,
                        catalog,
                        source.clone(),
                        Some(report.review.clone()),
                        ctx,
                    );
                }
            }
            Action::RollbackCompleted(_, result) => {
                let Work::Rollback(source, rollback::Stage::Reviewing | rollback::Stage::Applying) =
                    &panel.work
                else {
                    return;
                };
                panel.task = None;
                panel.work = match result {
                    Ok(report) if report.applied => Work::RolledBack(report.changed_profiles),
                    Ok(report) => Work::Rollback(source.clone(), rollback::Stage::Reviewed(report)),
                    Err(error) => {
                        panel.error = Some(error);
                        Work::Rollback(source.clone(), rollback::Stage::Ready)
                    }
                };
            }
            Action::Close(_) if panel.can_close() => {
                if matches!(
                    panel.work,
                    Work::Applied(_)
                        | Work::Rollback(rollback::Source::Applied(_), _)
                        | Work::RolledBack(_)
                ) {
                    page.state = load(catalog, id, page.checks, ctx);
                }
                page.rollout = None;
            }
            _ => {}
        }
    }

    fn adopt_tool_bindings<'a>(
        &mut self,
        bindings: impl Iterator<Item = (&'a str, &'a ToolSync, ToolSync)>,
    ) {
        let bindings: BTreeMap<_, _> = bindings
            .map(|(name, before, after)| (name, (before, after)))
            .collect();
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
            let Some(&(before, ref after)) = bindings.get(base.name.as_str()) else {
                continue;
            };
            if base.options.tool_bundle.as_deref() != Some(before) {
                continue;
            }
            let mut expected = base.options.clone();
            expected.tool_bundle = Some(Box::new(after.clone()));
            let Ok((_, saved)) = self.store.inspect_catalog(&base.name) else {
                continue;
            };
            if saved.options == expected {
                draft.tool_bundle.adopt_rollout(before, after, base_path);
                draft.origin = DraftOrigin::Edit(saved);
            }
        }
    }
}
