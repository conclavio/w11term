use super::*;
use crate::app::command_reference::dotfiles::comparison::{code_details, diff_text};
use std::collections::BTreeSet;
use w11term::profiles::{
    DotfileRestoreStatus, DotfileRollbackReport, DotfileRolloutBaseline, DotfileRolloutReport,
    ProfileStore,
};

type Targets = Vec<(String, DotfileSync)>;

#[derive(Clone)]
pub enum Action {
    Open,
    Loaded(Arc<()>, Result<Arc<Targets>, String>),
    Filter(Arc<()>, String),
    Select(Arc<()>, String, bool),
    Review(Arc<()>),
    Back(Arc<()>),
    Apply(Arc<()>),
    Completed(Arc<()>, Result<Arc<ReviewedRollout>, String>),
    ReviewRollback(Arc<()>),
    ApplyRollback(Arc<()>),
    RollbackCompleted(Arc<()>, Result<Arc<DotfileRollbackReport>, String>),
    Close(Arc<()>),
}

#[derive(Clone)]
enum Work {
    Loading,
    Editing,
    Reviewing,
    Reviewed(Arc<ReviewedRollout>),
    Applying,
    Applied(Arc<ReviewedRollout>),
    Rollback {
        applied: Arc<ReviewedRollout>,
        state: Undo,
    },
    RolledBack(usize),
}

#[derive(Clone)]
enum Undo {
    Reviewing,
    Reviewed(Arc<DotfileRollbackReport>),
    Applying,
}

#[derive(Clone)]
pub(super) struct Panel {
    ticket: Arc<()>,
    candidate: PathBuf,
    targets: Arc<Targets>,
    selected: BTreeSet<String>,
    query: String,
    work: Work,
    error: Option<String>,
}

pub struct ReviewedRollout {
    report: DotfileRolloutReport,
    changes: Vec<[Option<String>; 2]>,
}

impl ReviewedRollout {
    fn new(report: DotfileRolloutReport) -> Self {
        let changes = if report.applied {
            Vec::new()
        } else {
            report
                .profiles
                .iter()
                .map(|entry| {
                    [0, 1].map(|index| {
                        diff_text(
                            entry
                                .before_fragments
                                .as_deref()
                                .map_or("", |before| before[index].as_str()),
                            &entry.candidate_fragments[index],
                            entry.before_fragments.is_none(),
                        )
                    })
                })
                .collect()
        };
        Self { report, changes }
    }
}

fn message(id: TabId, action: Action) -> Message {
    Message::ProfileDotfiles(id, super::Action::Rollout(action))
}

impl Panel {
    fn new(candidate: PathBuf) -> Self {
        Self {
            ticket: Arc::new(()),
            candidate,
            targets: Arc::default(),
            selected: BTreeSet::new(),
            query: String::new(),
            work: Work::Loading,
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
        self.work = if review.is_some() {
            Work::Applying
        } else {
            Work::Reviewing
        };
        self.error = None;
        let ticket = self.ticket.clone();
        let candidate = self.candidate.clone();
        let profiles: Vec<_> = self.selected.iter().cloned().collect();
        let rejected = message(
            id,
            Action::Completed(ticket.clone(), Err("Dotfile rollout unavailable".into())),
        );
        ctx.spawn_background_with_rejection(
            move |_| {
                let result = ProfileStore::new(catalog)
                    .and_then(|store| {
                        store.rollout_dotfiles(&profiles, &candidate, review.as_deref())
                    })
                    .map(ReviewedRollout::new)
                    .map(Arc::new)
                    .map_err(|error| format!("{error:#}"));
                message(id, Action::Completed(ticket, result))
            },
            rejected,
        );
    }

    fn request_rollback(
        &mut self,
        id: TabId,
        applied: Arc<ReviewedRollout>,
        review: Option<String>,
        ctx: &ComponentContext<TerminalApp>,
    ) {
        let state = if review.is_some() {
            Undo::Applying
        } else {
            Undo::Reviewing
        };
        self.work = Work::Rollback {
            applied: applied.clone(),
            state,
        };
        self.error = None;
        let ticket = self.ticket.clone();
        let rejected = message(
            id,
            Action::RollbackCompleted(ticket.clone(), Err("Dotfile rollback unavailable".into())),
        );
        ctx.spawn_background_with_rejection(
            move |_| {
                let result = applied
                    .report
                    .rollback(review.as_deref())
                    .map(Arc::new)
                    .map_err(|error| format!("{error:#}"));
                message(id, Action::RollbackCompleted(ticket, result))
            },
            rejected,
        );
    }

    pub(super) fn view(&self, id: TabId, ctx: &ViewContext<TerminalApp>) -> View {
        let send = |action: Action| ctx.callback(move |_| message(id, action.clone()));
        let mut fields = vec![
            label("heading", "Roll out dotfile bundle"),
            label("candidate", self.candidate.display().to_string()),
        ];
        match &self.work {
            Work::Loading => fields.push(label("status", "Reading saved SSH profiles…")),
            Work::Reviewing => fields.push(label("status", "Checking the selected profiles…")),
            Work::Applying => fields.push(label("status", "Saving the reviewed rollout…")),
            Work::Applied(applied) => {
                fields.push(label(
                    "status",
                    format!(
                        "Updated dotfiles in {} saved SSH profiles.",
                        applied.report.changed_profiles
                    ),
                ));
                fields.push(KeyedView::new(
                    "review-rollback",
                    Button::new()
                        .on_click(send(Action::ReviewRollback(self.ticket.clone())))
                        .content("Review rollback"),
                ));
            }
            Work::Rollback { state, .. } => match state {
                Undo::Reviewing => {
                    fields.push(label("status", "Checking the previous dotfile approvals…"))
                }
                Undo::Applying => {
                    fields.push(label("status", "Restoring the reviewed dotfile approvals…"))
                }
                Undo::Reviewed(rollback) => {
                    for entry in &rollback.profiles {
                        let restore = &entry.restore;
                        let policy = restore.bundle_sha256.as_deref().map_or_else(
                            || "Follows the bundle file".into(),
                            |pin| format!("Restore pin {pin}"),
                        );
                        let status = match entry.restore_status {
                            DotfileRestoreStatus::BundleUnavailable => {
                                "The previous bundle is unavailable for this shell."
                            }
                            DotfileRestoreStatus::FollowsFile => {
                                "The previous bundle is available."
                            }
                            DotfileRestoreStatus::MatchesPin => {
                                "The previous bundle matches its pin."
                            }
                            DotfileRestoreStatus::PinMismatch => {
                                "The previous bundle differs from its restored pin."
                            }
                        };
                        fields.push(KeyedView::new(
                            format!("rollback-{}", entry.profile),
                            StackPanel::new().spacing(3.0).children((
                                TextBlock::new()
                                    .text(profile_label(&entry.profile, restore))
                                    .text_wrapping(TextWrapping::Wrap),
                                TextBlock::new()
                                    .text(format!("Restore {}", restore.bundle.display()))
                                    .text_wrapping(TextWrapping::Wrap),
                                TextBlock::new()
                                    .text(policy)
                                    .text_wrapping(TextWrapping::Wrap),
                                TextBlock::new()
                                    .text(status)
                                    .text_wrapping(TextWrapping::Wrap),
                            )),
                        ));
                    }
                    fields.push(KeyedView::new(
                        "rollback-actions",
                        StackPanel::new().spacing(6.0).children((
                            Button::new()
                                .on_click(send(Action::ApplyRollback(self.ticket.clone())))
                                .content("Apply reviewed rollback"),
                            Button::new()
                                .on_click(send(Action::Back(self.ticket.clone())))
                                .content("Back to rollout result"),
                        )),
                    ));
                }
            },
            Work::RolledBack(count) => fields.push(label(
                "status",
                format!("Restored dotfiles in {count} saved SSH profiles."),
            )),
            Work::Reviewed(review) => {
                let report = &review.report;
                for (entry, changes) in report.profiles.iter().zip(&review.changes) {
                    let old = &entry.previous;
                    let policy = match (&old.bundle_sha256, &entry.next_pin) {
                        (Some(previous), Some(_)) => format!("Previous pin {previous}"),
                        _ => "Follows the bundle file".into(),
                    };
                    let baseline = match &entry.baseline {
                        DotfileRolloutBaseline::Pinned(_) => {
                            "Compared with the approved pinned version."
                        }
                        DotfileRolloutBaseline::File(_) => {
                            "Compared with the approved bundle file on disk."
                        }
                        DotfileRolloutBaseline::Unavailable => {
                            "The approved bundle could not be verified. Showing proposed fragments."
                        }
                    };
                    fields.push(KeyedView::new(
                        format!("review-{}", entry.profile),
                        StackPanel::new().spacing(3.0).children((
                            TextBlock::new()
                                .text(profile_label(&entry.profile, old))
                                .text_wrapping(TextWrapping::Wrap),
                            TextBlock::new()
                                .text(format!("{} · {}", old.home, old.bundle.display()))
                                .text_wrapping(TextWrapping::Wrap),
                            TextBlock::new()
                                .text(policy)
                                .text_wrapping(TextWrapping::Wrap),
                            TextBlock::new()
                                .text(format!("Proposed digest {}", entry.candidate_bundle_sha256))
                                .text_wrapping(TextWrapping::Wrap),
                            TextBlock::new()
                                .text(baseline)
                                .text_wrapping(TextWrapping::Wrap),
                        )),
                    ));
                    if changes.iter().all(Option::is_none) {
                        fields.push(KeyedView::new(
                            format!("unchanged-{}", entry.profile),
                            TextBlock::new().text("No dotfile fragment lines changed."),
                        ));
                    }
                    for (role, lines) in ["environment", "prompt"].into_iter().zip(changes) {
                        if let Some(lines) = lines {
                            fields.push(code_details(
                                format!("changes-{role}-{}", entry.profile),
                                &format!("{} {role} changes", entry.profile),
                                lines,
                            ));
                        }
                    }
                }
                if report.changed_profiles == 0 {
                    fields.push(label(
                        "unchanged",
                        "The selected profiles already use this revision.",
                    ));
                }
                fields.push(KeyedView::new(
                    "review-actions",
                    StackPanel::new().spacing(6.0).children((
                        Button::new()
                            .is_enabled(report.changed_profiles > 0)
                            .on_click(send(Action::Apply(self.ticket.clone())))
                            .content("Apply reviewed rollout"),
                        Button::new()
                            .on_click(send(Action::Back(self.ticket.clone())))
                            .content("Change rollout selection"),
                    )),
                ));
            }
            Work::Editing if self.targets.is_empty() && self.error.is_none() => {
                fields.push(label(
                    "empty",
                    "No saved approvals match the shells in this bundle.",
                ));
            }
            Work::Editing => {
                if !self.targets.is_empty() {
                    let ticket = self.ticket.clone();
                    fields.push(KeyedView::new(
                        "filter",
                        TextBox::new()
                            .automation_name("Filter rollout profiles")
                            .placeholder_text("Filter profiles")
                            .text(&self.query)
                            .on_text_changed(ctx.callback(move |query| {
                                message(id, Action::Filter(ticket.clone(), query))
                            })),
                    ));
                    let query = self.query.trim().to_lowercase();
                    for (name, settings) in self.targets.iter() {
                        let label = profile_label(name, settings);
                        if !label.to_lowercase().contains(&query) {
                            continue;
                        }
                        let target = name.clone();
                        let ticket = self.ticket.clone();
                        fields.push(KeyedView::new(
                            format!("target-{name}"),
                            CheckBox::new()
                                .automation_name(format!("Rollout profile {name}"))
                                .is_checked(self.selected.contains(name))
                                .on_is_checked_changed(ctx.callback(move |checked| {
                                    message(
                                        id,
                                        Action::Select(ticket.clone(), target.clone(), checked),
                                    )
                                }))
                                .content(label),
                        ));
                    }
                    fields.push(label(
                        "selected-count",
                        format!("{} selected", self.selected.len()),
                    ));
                    fields.push(KeyedView::new(
                        "review",
                        Button::new()
                            .is_enabled(!self.selected.is_empty())
                            .on_click(send(Action::Review(self.ticket.clone())))
                            .content("Review selected rollout profiles"),
                    ));
                }
            }
        }
        if let Some(error) = &self.error {
            fields.push(label("error", error.clone()));
        }
        fields.push(KeyedView::new(
            "close",
            Button::new()
                .is_enabled(!matches!(
                    self.work,
                    Work::Applying
                        | Work::Rollback {
                            state: Undo::Applying,
                            ..
                        }
                ))
                .on_click(send(Action::Close(self.ticket.clone())))
                .content("Close dotfile rollout"),
        ));
        StackPanel::new().spacing(6.0).keyed_children(fields)
    }
}

fn label(key: &'static str, text: impl Into<String>) -> KeyedView {
    KeyedView::new(
        key,
        TextBlock::new()
            .text(text.into())
            .text_wrapping(TextWrapping::Wrap),
    )
}

fn profile_label(name: &str, settings: &DotfileSync) -> String {
    format!(
        "{name} · {}@{}:{} · {}",
        settings.user,
        settings.host,
        settings.port,
        settings.shell.label()
    )
}

impl TerminalApp {
    pub(super) fn edit_profile_rollout(
        &mut self,
        id: TabId,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        let catalog = self.store.path().to_owned();
        let Some(Page::NewProfile(draft)) = self.tabs.get_mut(id).map(|tab| &mut tab.page) else {
            return;
        };
        let editor = &mut draft.dotfiles;
        if matches!(action, Action::Open) {
            if editor.rollout.is_some()
                || editor.composer.is_some()
                || editor.checking.is_some()
                || editor.browsing.is_some()
                || editor.preview.loading()
                || editor.bundle.trim().is_empty()
            {
                return;
            }
            let base = catalog.parent().unwrap_or(Path::new("."));
            let candidate = base.join(editor.bundle.trim());
            let panel = Panel::new(candidate.clone());
            let ticket = panel.ticket.clone();
            let rejected = message(
                id,
                Action::Loaded(
                    ticket.clone(),
                    Err("Could not load saved SSH profiles".into()),
                ),
            );
            editor.rollout = Some(Box::new(panel));
            ctx.spawn_background_with_rejection(
                move |_| {
                    let result = ProfileStore::new(catalog)
                        .and_then(|store| store.dotfile_rollout_targets(&candidate))
                        .map(Arc::new)
                        .map_err(|error| format!("{error:#}"));
                    message(id, Action::Loaded(ticket, result))
                },
                rejected,
            );
            return;
        }
        let Some(panel) = &mut editor.rollout else {
            return;
        };
        let ticket = match &action {
            Action::Loaded(ticket, _)
            | Action::Filter(ticket, _)
            | Action::Select(ticket, ..)
            | Action::Review(ticket)
            | Action::Back(ticket)
            | Action::Apply(ticket)
            | Action::Completed(ticket, _)
            | Action::ReviewRollback(ticket)
            | Action::ApplyRollback(ticket)
            | Action::RollbackCompleted(ticket, _)
            | Action::Close(ticket) => ticket,
            Action::Open => unreachable!(),
        };
        if !Arc::ptr_eq(&panel.ticket, ticket) {
            return;
        }
        match action {
            Action::Loaded(_, result) if matches!(panel.work, Work::Loading) => {
                panel.work = Work::Editing;
                match result {
                    Ok(targets) => panel.targets = targets,
                    Err(error) => panel.error = Some(error),
                }
            }
            Action::Filter(_, query) if matches!(panel.work, Work::Editing) => panel.query = query,
            Action::Select(_, name, checked) if matches!(panel.work, Work::Editing) => {
                if checked {
                    panel.selected.insert(name);
                } else {
                    panel.selected.remove(&name);
                }
            }
            Action::Back(_) if matches!(panel.work, Work::Reviewed(_)) => {
                panel.work = Work::Editing
            }
            Action::Back(_) => {
                if let Work::Rollback {
                    applied,
                    state: Undo::Reviewed(_),
                } = &panel.work
                {
                    panel.work = Work::Applied(applied.clone());
                }
            }
            Action::Review(_)
                if matches!(panel.work, Work::Editing) && !panel.selected.is_empty() =>
            {
                panel.request(id, catalog, None, ctx)
            }
            Action::Apply(_) => {
                if let Work::Reviewed(review) = &panel.work
                    && review.report.changed_profiles > 0
                {
                    panel.request(id, catalog, Some(review.report.review.clone()), ctx);
                }
            }
            Action::Completed(_, result)
                if matches!(panel.work, Work::Reviewing | Work::Applying) =>
            {
                match result {
                    Ok(review) if review.report.applied => {
                        panel.work = Work::Applied(review.clone());
                        self.refresh_dotfile_origin(
                            id,
                            review
                                .report
                                .profiles
                                .iter()
                                .filter(|entry| entry.changed)
                                .map(|entry| {
                                    let mut after = entry.previous.clone();
                                    after.bundle = review.report.candidate_path.clone();
                                    after.bundle_sha256 = entry.next_pin.clone();
                                    (entry.profile.as_str(), &entry.previous, after)
                                }),
                        );
                        self.refresh_profiles();
                    }
                    Ok(review) => panel.work = Work::Reviewed(review),
                    Err(error) => {
                        panel.work = Work::Editing;
                        panel.error = Some(error);
                    }
                }
            }
            Action::ReviewRollback(_) => {
                if let Work::Applied(applied) = &panel.work {
                    panel.request_rollback(id, applied.clone(), None, ctx);
                }
            }
            Action::ApplyRollback(_) => {
                if let Work::Rollback {
                    applied,
                    state: Undo::Reviewed(rollback),
                } = &panel.work
                {
                    panel.request_rollback(id, applied.clone(), Some(rollback.review.clone()), ctx);
                }
            }
            Action::RollbackCompleted(_, result) => {
                let Work::Rollback {
                    applied,
                    state: Undo::Reviewing | Undo::Applying,
                } = &panel.work
                else {
                    return;
                };
                match result {
                    Ok(rollback) if rollback.applied => {
                        panel.work = Work::RolledBack(rollback.changed_profiles);
                        self.refresh_dotfile_origin(
                            id,
                            rollback.profiles.iter().map(|entry| {
                                (
                                    entry.profile.as_str(),
                                    &entry.current,
                                    entry.restore.clone(),
                                )
                            }),
                        );
                        self.refresh_profiles();
                    }
                    Ok(rollback) => {
                        panel.work = Work::Rollback {
                            applied: applied.clone(),
                            state: Undo::Reviewed(rollback),
                        }
                    }
                    Err(error) => {
                        panel.work = Work::Applied(applied.clone());
                        panel.error = Some(error);
                    }
                }
            }
            Action::Close(_)
                if !matches!(
                    panel.work,
                    Work::Applying
                        | Work::Rollback {
                            state: Undo::Applying,
                            ..
                        }
                ) =>
            {
                editor.rollout = None
            }
            _ => {}
        }
    }

    fn refresh_dotfile_origin<'a>(
        &mut self,
        id: TabId,
        mut changes: impl Iterator<Item = (&'a str, &'a DotfileSync, DotfileSync)>,
    ) {
        let Some(Page::NewProfile(draft)) = self.tabs.get_mut(id).map(|tab| &mut tab.page) else {
            return;
        };
        let Some(base) = draft.origin.editing() else {
            return;
        };
        let Some((_, before, after)) = changes.find(|(name, _, _)| *name == base.name) else {
            return;
        };
        if base.options.dotfiles.as_deref() != Some(before) {
            return;
        }
        let mut expected = base.options.clone();
        expected.dotfiles = Some(Box::new(after));
        let Ok((_, saved)) = self.store.inspect_catalog(&base.name) else {
            return;
        };
        if saved.options != expected {
            return;
        }
        if draft.dotfiles.approved == base.options.dotfiles {
            let base_dir = self.store.path().parent().unwrap_or(Path::new("."));
            if base_dir.join(draft.dotfiles.bundle.trim()) == before.bundle
                && let Some(restored) = saved.options.dotfiles.as_ref()
            {
                draft.dotfiles.bundle = restored.bundle.to_string_lossy().into_owned();
            }
            draft.dotfiles.approved = saved.options.dotfiles.clone();
            draft.dotfiles.snapshot_version = None;
            draft.dotfiles.preview = review::Preview::Closed;
        }
        draft.origin = DraftOrigin::Edit(saved);
    }
}
