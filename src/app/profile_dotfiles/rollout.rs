use super::*;
use std::collections::BTreeSet;
use w11term::profiles::{DotfileRolloutReport, ProfileStore};

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
    Completed(Arc<()>, Result<Arc<DotfileRolloutReport>, String>),
    Close(Arc<()>),
}

#[derive(Clone)]
enum Work {
    Loading,
    Editing,
    Reviewing,
    Reviewed(Arc<DotfileRolloutReport>),
    Applying,
    Applied(usize),
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
                    .map(Arc::new)
                    .map_err(|error| format!("{error:#}"));
                message(id, Action::Completed(ticket, result))
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
            Work::Applied(count) => fields.push(label(
                "status",
                format!("Updated dotfiles in {count} saved SSH profiles."),
            )),
            Work::Reviewed(report) => {
                for entry in &report.profiles {
                    let old = &entry.previous;
                    let policy = match (&old.bundle_sha256, &entry.next_pin) {
                        (Some(previous), Some(_)) => format!("Previous pin {previous}"),
                        _ => "Follows the bundle file".into(),
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
                        )),
                    ));
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
                .is_enabled(!matches!(self.work, Work::Applying))
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
            Action::Review(_)
                if matches!(panel.work, Work::Editing) && !panel.selected.is_empty() =>
            {
                panel.request(id, catalog, None, ctx)
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
                match result {
                    Ok(report) if report.applied => {
                        panel.work = Work::Applied(report.changed_profiles);
                        self.refresh_rollout_origin(id, &report);
                        self.refresh_profiles();
                    }
                    Ok(report) => panel.work = Work::Reviewed(report),
                    Err(error) => {
                        panel.work = Work::Editing;
                        panel.error = Some(error);
                    }
                }
            }
            Action::Close(_) if !matches!(panel.work, Work::Applying) => editor.rollout = None,
            _ => {}
        }
    }

    fn refresh_rollout_origin(&mut self, id: TabId, report: &DotfileRolloutReport) {
        let Some(Page::NewProfile(draft)) = self.tabs.get_mut(id).map(|tab| &mut tab.page) else {
            return;
        };
        let Some(base) = draft.origin.editing() else {
            return;
        };
        let Some(entry) = report
            .profiles
            .iter()
            .find(|entry| entry.changed && entry.profile == base.name)
        else {
            return;
        };
        if base.options.dotfiles.as_deref() != Some(&entry.previous) {
            return;
        }
        let mut expected = base.options.clone();
        let mut approval = entry.previous.clone();
        approval.bundle = report.candidate_path.clone();
        approval.bundle_sha256 = entry.next_pin.clone();
        expected.dotfiles = Some(Box::new(approval));
        let Ok((_, saved)) = self.store.inspect_catalog(&base.name) else {
            return;
        };
        if saved.options != expected {
            return;
        }
        if draft.dotfiles.approved == base.options.dotfiles {
            draft.dotfiles.approved = saved.options.dotfiles.clone();
            draft.dotfiles.snapshot_version = None;
            draft.dotfiles.preview = review::Preview::Closed;
        }
        draft.origin = DraftOrigin::Edit(saved);
    }
}
