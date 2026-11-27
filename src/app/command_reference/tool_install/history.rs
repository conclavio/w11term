use super::*;
use w11term::sftp::{RetainedToolVersion, ToolBundleVersions};

pub enum Action {
    Open,
    Listed(Arc<()>, Result<ToolBundleVersions, String>),
    Select(Option<usize>),
    Review,
    ReviewPrune,
    ReviewPolicy(UpdatePolicy),
    Reviewed(Arc<()>, Result<ReviewedVersion, String>),
    Restore,
    Prune,
    Pruned(Arc<()>, Result<PrunedRelease, String>),
    SetPolicy(UpdatePolicy),
    Switched(Arc<()>, Result<SwitchReport, String>),
    Save,
    Saved(Arc<()>, Result<SavedBundle, String>),
    CopyManifest,
    ReviewProfile,
    Close,
}

pub enum ReviewedVersion {
    Rollback(Box<RollbackReview>),
    UpdatePolicy(UpdatePolicy, PolicyReview),
    Prune(PruneReview),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UpdatePolicy {
    Pause,
    Resume,
}

#[derive(Deserialize)]
pub struct PolicyReview {
    home: String,
    bundle: String,
    bundle_sha256: String,
    ready: bool,
    review: Option<String>,
}

#[derive(Deserialize)]
pub struct PruneReview {
    version_directory: String,
    remaining_bytes: u64,
    incomplete: bool,
    files: Vec<PruneFile>,
    review: String,
}

#[derive(Deserialize)]
struct PruneFile {
    name: String,
    bytes: u64,
}

#[derive(Deserialize)]
pub struct PrunedRelease {
    bundle: String,
    bundle_sha256: String,
    removed_bytes: u64,
}

#[derive(Deserialize)]
pub struct RollbackReview {
    home: String,
    bundle: String,
    bundle_sha256: String,
    current: Current,
    retained: Retained,
    changes: Option<Vec<ToolChange>>,
    bin_matches_active: bool,
    bin_directory_plan: Directory,
    launchers: Vec<(String, String)>,
    ready: bool,
    review: Option<String>,
    supports_atomic_replace: bool,
}

#[derive(Deserialize)]
struct Retained {
    architecture: String,
    bin_directory: String,
}

#[derive(Deserialize)]
struct ToolChange {
    name: String,
    action: ToolChangeKind,
    before: Option<Pin>,
    after: Option<Pin>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ToolChangeKind {
    Add,
    Remove,
    Change,
    Keep,
}

#[derive(Deserialize)]
struct Pin {
    sha256: String,
    bytes: u64,
}

#[derive(Deserialize)]
pub struct SwitchReport {
    bundle: String,
    bundle_sha256: String,
}

#[derive(Deserialize)]
pub struct SavedBundle {
    manifest: PathBuf,
    home: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Work {
    List,
    Review,
    ReviewPrune,
    ReviewPolicy(UpdatePolicy),
    Restore,
    Prune,
    SetPolicy(UpdatePolicy),
    Save,
}

impl Work {
    fn writes(self) -> bool {
        matches!(
            self,
            Self::Restore | Self::Prune | Self::SetPolicy(_) | Self::Save
        )
    }
}

#[derive(Default)]
pub(super) struct Panel {
    versions: Option<ToolBundleVersions>,
    selected: Option<usize>,
    review: Option<ReviewedVersion>,
    pending: Option<(Arc<()>, Work)>,
    status: Option<String>,
    saved: Option<SavedBundle>,
}

fn message(action: Action) -> Message {
    super::message(super::Action::History(action))
}

fn detail(key: &str, text: impl Into<String>) -> KeyedView {
    KeyedView::new(
        key.to_owned(),
        TextBlock::new()
            .text(text.into())
            .text_wrapping(TextWrapping::Wrap),
    )
}

impl Panel {
    fn begin(&mut self, work: Work) -> Arc<()> {
        let ticket = Arc::new(());
        self.pending = Some((ticket.clone(), work));
        self.status = None;
        ticket
    }

    fn finish(&mut self, ticket: &Arc<()>, accepts: impl FnOnce(Work) -> bool) -> Option<Work> {
        self.pending
            .take_if(|(pending, work)| Arc::ptr_eq(pending, ticket) && accepts(*work))
            .map(|(_, work)| work)
    }

    fn selected(&self) -> Option<&RetainedToolVersion> {
        self.versions.as_ref()?.versions.get(self.selected?)
    }

    fn token(&self, work: Work) -> Option<&str> {
        match (self.review.as_ref()?, work) {
            (ReviewedVersion::Rollback(review), Work::Restore)
                if review.ready
                    && review.current.version.as_deref() != Some(&review.bundle_sha256) =>
            {
                review.review.as_deref()
            }
            (ReviewedVersion::UpdatePolicy(reviewed, review), Work::SetPolicy(policy))
                if *reviewed == policy && review.ready =>
            {
                review.review.as_deref()
            }
            (ReviewedVersion::Prune(review), Work::Prune) => Some(&review.review),
            _ => None,
        }
    }

    pub(super) fn view(
        &self,
        account: &str,
        has_profile: bool,
        active: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let available = active && self.pending.is_none();
        let writing = self.pending.as_ref().is_some_and(|(_, kind)| kind.writes());
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
                    .text("SSH tool bundle versions")
                    .font_size(18.0),
            ),
            detail("account", account),
            KeyedView::new(
                "refresh",
                button("Refresh versions", available, || Action::Open),
            ),
        ];
        let status = if let Some((_, work)) = &self.pending {
            Some(match work {
                Work::List => "Reading retained tool bundles…",
                Work::Review => "Verifying retained tool bundle…",
                Work::ReviewPrune => "Checking the inactive release files…",
                Work::ReviewPolicy(_) => "Reviewing automatic updates…",
                Work::Restore => "Restoring reviewed tool bundle…",
                Work::Prune => "Deleting the reviewed inactive release…",
                Work::SetPolicy(UpdatePolicy::Pause) => "Pausing automatic updates…",
                Work::SetPolicy(UpdatePolicy::Resume) => "Resuming automatic updates…",
                Work::Save => "Saving the pinned Linux executables on Windows…",
            })
        } else if !active {
            Some("Return to the original SSH pane or open a new review")
        } else {
            self.status.as_deref()
        };
        if let Some(status) = status {
            fields.push(detail("status", status));
        }
        if let Some(history) = &self.versions {
            if history.versions.is_empty() {
                fields.push(detail("empty", "No retained tool bundles found"));
            } else {
                fields.push(KeyedView::new(
                    "versions",
                    ComboBox::new()
                        .automation_name("Retained tool bundle versions")
                        .is_enabled(available)
                        .placeholder_text("Choose a version")
                        .items_source(history.versions.iter().map(version_label))
                        .selected_index(self.selected)
                        .on_selection_changed(ctx.callback(|index| message(Action::Select(index)))),
                ));
            }
            if history.skipped_bundles != 0 {
                fields.push(detail(
                    "skipped",
                    format!("Bundles skipped ({})", history.skipped_bundles),
                ));
            }
            if history.omitted_versions != 0 {
                fields.push(detail(
                    "omitted",
                    format!("Older versions omitted ({})", history.omitted_versions),
                ));
            }
        }
        if let Some(selected) = self.selected() {
            fields.push(detail(
                "digest",
                format!("SHA-256 {}", selected.bundle_sha256),
            ));
            if let Some(time) = selected
                .modified
                .and_then(|time| chrono::DateTime::from_timestamp(i64::from(time), 0))
            {
                fields.push(detail(
                    "modified",
                    format!("Modified {}", time.format("%Y-%m-%d %H:%M UTC")),
                ));
            }
            if !selected.active {
                fields.push(KeyedView::new(
                    "review-deletion",
                    button("Review release deletion", available, || Action::ReviewPrune),
                ));
            }
        }
        match &self.review {
            Some(ReviewedVersion::Rollback(review)) => review.describe(&mut fields),
            Some(ReviewedVersion::UpdatePolicy(policy, review)) => {
                review.describe(*policy, &mut fields)
            }
            Some(ReviewedVersion::Prune(review)) => review.describe(&mut fields),
            None => (),
        }
        let actions = if let Some(row) = self.selected().filter(|row| row.active) {
            let (policy, review, confirm) = if row.held {
                (
                    UpdatePolicy::Resume,
                    "Review update resumption",
                    "Resume automatic updates",
                )
            } else {
                (
                    UpdatePolicy::Pause,
                    "Review update pause",
                    "Pause automatic updates",
                )
            };
            StackPanel::new().spacing(8.0).children((
                Button::new()
                    .is_enabled(available)
                    .on_click(ctx.callback(move |_| message(Action::ReviewPolicy(policy))))
                    .content(review),
                Button::new()
                    .is_enabled(available && self.token(Work::SetPolicy(policy)).is_some())
                    .on_click(ctx.callback(move |_| message(Action::SetPolicy(policy))))
                    .content(confirm),
            ))
        } else {
            let (label, work, confirm): (&str, Work, fn() -> Action) =
                if matches!(self.review, Some(ReviewedVersion::Prune(_))) {
                    ("Delete reviewed release", Work::Prune, || Action::Prune)
                } else {
                    ("Restore reviewed version", Work::Restore, || {
                        Action::Restore
                    })
                };
            StackPanel::new().spacing(8.0).children((
                button(
                    "Review rollback",
                    available && self.selected().is_some_and(|row| !row.active),
                    || Action::Review,
                ),
                button(label, available && self.token(work).is_some(), confirm),
            ))
        };
        if self.selected().is_some_and(|row| row.active) {
            fields.push(KeyedView::new(
                "save-pinned-bundle",
                button("Save pinned bundle on Windows", available, || Action::Save),
            ));
        }
        if let Some(saved) = &self.saved {
            fields.push(detail(
                "saved-manifest",
                saved.manifest.display().to_string(),
            ));
            if has_profile {
                fields.push(KeyedView::new(
                    "review-saved-for-profile",
                    button("Review in this SSH profile", available, || {
                        Action::ReviewProfile
                    }),
                ));
            }
            fields.push(KeyedView::new(
                "copy-manifest",
                button("Copy saved manifest path", !writing, || {
                    Action::CopyManifest
                }),
            ));
        }
        Grid::new()
            .rows([GridLength::Star(1.0), GridLength::Auto])
            .children((
                ScrollViewer::new().content(
                    StackPanel::new()
                        .margin(12.0)
                        .spacing(8.0)
                        .keyed_children(fields),
                ),
                StackPanel::new()
                    .grid_row(1)
                    .margin(12.0)
                    .spacing(8.0)
                    .children((
                        actions,
                        button("Back to tool installer", !writing, || Action::Close),
                    )),
            ))
    }
}

fn version_label(row: &RetainedToolVersion) -> String {
    let digest = row.bundle_sha256.get(..12).unwrap_or(&row.bundle_sha256);
    let suffix = if row.held {
        " · Active · Updates paused"
    } else if row.active {
        " · Active"
    } else {
        ""
    };
    format!("{} · {digest}{suffix}", row.bundle)
}

impl PolicyReview {
    fn describe(&self, policy: UpdatePolicy, fields: &mut Vec<KeyedView>) {
        fields.push(detail(
            "update-destination",
            format!(
                "{} · Home {}\nActive SHA-256 {}",
                self.bundle, self.home, self.bundle_sha256
            ),
        ));
        fields.push(detail(
            "update-effect",
            if !self.ready {
                "Changing automatic updates requires the server's atomic rename extension."
            } else {
                match policy {
                    UpdatePolicy::Pause => {
                        "Keep this release installed and skip this bundle on future SSH logins."
                    }
                    UpdatePolicy::Resume => {
                        "Approved SSH profiles can sync this bundle on their next connection."
                    }
                }
            },
        ));
    }
}

impl PruneReview {
    fn describe(&self, fields: &mut Vec<KeyedView>) {
        fields.extend([
            detail("prune-path", &self.version_directory),
            detail(
                "prune-effect",
                "Deleting this release permanently removes it from rollback history.",
            ),
            detail(
                "prune-size",
                format!(
                    "Files to remove {} · {} bytes",
                    self.files.len(),
                    self.remaining_bytes
                ),
            ),
        ]);
        if self.incomplete {
            fields.push(detail(
                "prune-incomplete",
                "This release has missing files. Confirm to finish the remaining cleanup.",
            ));
        }
        fields.extend(self.files.iter().map(|file| {
            detail(
                &format!("prune-file-{}", file.name),
                format!("{} · {} bytes", file.name, file.bytes),
            )
        }));
    }
}

impl RollbackReview {
    fn describe(&self, fields: &mut Vec<KeyedView>) {
        fields.push(detail(
            "destination",
            format!(
                "{} · Linux {} · Home {}\nLauncher directory {}",
                self.bundle, self.retained.architecture, self.home, self.retained.bin_directory
            ),
        ));
        if let Some(changes) = &self.changes {
            fields.push(detail(
                "rollback-comparison",
                if self.current.version.is_some() {
                    "Changes from the active release"
                } else {
                    "Commands in the selected release"
                },
            ));
            fields.extend(changes.iter().map(ToolChange::view));
        }
        if let Some(path) = &self.bin_directory_plan.blocked_at {
            fields.push(detail(
                "directory",
                format!("Launcher directory blocked by {path}"),
            ));
        }
        if self.current.conflict {
            fields.push(detail(
                "selector",
                "The active tool bundle selector is invalid",
            ));
        } else if self.current.version.as_deref() == Some(&self.bundle_sha256) {
            fields.push(detail(
                "selector",
                "This version is already active. Refresh versions.",
            ));
        } else if !self.bin_matches_active {
            fields.push(detail(
                "selector",
                "The selected release uses a different launcher directory",
            ));
        } else if self.current.version.is_some() && !self.supports_atomic_replace {
            fields.push(detail(
                "selector",
                "Switching an active release requires the server's atomic rename extension",
            ));
        }
        for (index, (path, action)) in self.launchers.iter().enumerate() {
            let label = match action.as_str() {
                "conflict" => "Existing entry at",
                "create" => "Create",
                _ => "Reuse",
            };
            fields.push(detail(
                &format!("launcher-{index}"),
                format!("{label} {path}"),
            ));
        }
        fields.push(detail(
            "effect",
            if self.ready {
                "Restoring this version pauses automatic bundle updates."
            } else {
                "Resolve the conflicts before restoring this version."
            },
        ));
    }
}

impl ToolChange {
    fn view(&self) -> KeyedView {
        let (verb, before, after) = match self.action {
            ToolChangeKind::Add => ("Restore", None, self.after.as_ref()),
            ToolChangeKind::Remove => ("Deactivate", self.before.as_ref(), None),
            ToolChangeKind::Change => (
                "Replace pinned build for",
                self.before.as_ref(),
                self.after.as_ref(),
            ),
            ToolChangeKind::Keep => ("Keep", self.before.as_ref(), None),
        };
        let mut fields = vec![detail("command", format!("{verb} {}", self.name))];
        for (key, label, pin) in [("before", "Active", before), ("after", "Restored", after)] {
            if let Some(pin) = pin {
                fields.push(KeyedView::new(
                    key,
                    TextBlock::new()
                        .text(format!(
                            "{label} SHA-256 {} · {} bytes",
                            pin.sha256, pin.bytes
                        ))
                        .text_wrapping(TextWrapping::Wrap)
                        .is_text_selection_enabled(true),
                ));
            }
        }
        KeyedView::new(
            format!("change-{}", self.name),
            StackPanel::new().spacing(2.0).keyed_children(fields),
        )
    }
}

impl TerminalApp {
    fn review_saved_tool_bundle_for_profile(&mut self, ctx: &ComponentContext<Self>) {
        let Some(installer) = self.command_reference.tool_install.as_ref() else {
            return;
        };
        let Some(history) = installer
            .history
            .as_ref()
            .filter(|history| history.pending.is_none())
        else {
            return;
        };
        let Some(saved) = &history.saved else {
            return;
        };
        let Some((pane, session)) = self
            .reference_tool_target()
            .filter(|(pane, _)| *pane == installer.target)
        else {
            return;
        };
        let Some(profile) = self
            .tabs
            .active()
            .and_then(AppTab::ssh_profile_name)
            .filter(|name| Some(*name) == installer.profile.as_deref())
        else {
            return;
        };
        let Some(connection) = session.ssh_connection_at(pane.generation) else {
            return;
        };
        let (manifest, home) = (saved.manifest.clone(), saved.home.clone());
        self.open_tool_snapshot_profile(profile.to_owned(), &manifest, &home, &connection, ctx);
    }

    pub(super) fn tool_bundle_history(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        if matches!(action, Action::ReviewProfile) {
            self.review_saved_tool_bundle_for_profile(ctx);
            return;
        }
        let current = self
            .reference_tool_target()
            .map(|(pane, session)| (pane, session.input.clone()));
        let directory = self.store.path().with_file_name("tool-bundle-snapshots");
        let reference = &mut self.command_reference;
        let Some(installer) = &mut reference.tool_install else {
            return;
        };
        let target = installer.target;
        let input = current
            .filter(|(pane, _)| *pane == target)
            .map(|(_, input)| input);
        if matches!(action, Action::Open) && input.is_some() && installer.pending.is_none() {
            installer.history.get_or_insert_with(Panel::default);
        }
        let Some(history) = &mut installer.history else {
            return;
        };
        match action {
            Action::Close => {
                if !history
                    .pending
                    .as_ref()
                    .is_some_and(|(_, work)| work.writes())
                {
                    installer.history = None;
                }
            }
            Action::Open
            | Action::Review
            | Action::ReviewPrune
            | Action::ReviewPolicy(_)
            | Action::Restore
            | Action::Prune
            | Action::SetPolicy(_)
            | Action::Save => {
                let Some(input) = input else { return };
                if history.pending.is_some() {
                    return;
                }
                let work = match action {
                    Action::Open => Work::List,
                    Action::Review => Work::Review,
                    Action::ReviewPrune => Work::ReviewPrune,
                    Action::Prune => Work::Prune,
                    Action::ReviewPolicy(policy) => Work::ReviewPolicy(policy),
                    Action::SetPolicy(policy) => Work::SetPolicy(policy),
                    Action::Save => Work::Save,
                    _ => Work::Restore,
                };
                let operation = if work == Work::List {
                    history.versions = None;
                    history.selected = None;
                    history.saved = None;
                    Operation::ToolBundleVersions
                } else if work == Work::Save {
                    let Some(row) = history.selected().filter(|row| row.active) else {
                        return;
                    };
                    Operation::SaveInstalledToolBundle {
                        name: row.bundle.clone(),
                        bundle_sha256: row.bundle_sha256.clone(),
                        directory,
                    }
                } else {
                    let policy = match work {
                        Work::ReviewPolicy(policy) | Work::SetPolicy(policy) => Some(policy),
                        _ => None,
                    };
                    let Some(row) = history.selected().filter(|row| match policy {
                        Some(UpdatePolicy::Pause) => row.active && !row.held,
                        Some(UpdatePolicy::Resume) => row.active && row.held,
                        None => !row.active,
                    }) else {
                        return;
                    };
                    let review = if work.writes() {
                        let Some(token) = history.token(work) else {
                            return;
                        };
                        Some(token.to_owned())
                    } else {
                        None
                    };
                    match policy {
                        Some(UpdatePolicy::Pause) => Operation::PauseToolBundleUpdates {
                            name: row.bundle.clone(),
                            bundle_sha256: row.bundle_sha256.clone(),
                            review,
                        },
                        Some(UpdatePolicy::Resume) => Operation::ResumeToolBundleUpdates {
                            name: row.bundle.clone(),
                            bundle_sha256: row.bundle_sha256.clone(),
                            review,
                        },
                        None if matches!(work, Work::ReviewPrune | Work::Prune) => {
                            Operation::PruneToolBundleRelease {
                                name: row.bundle.clone(),
                                bundle_sha256: row.bundle_sha256.clone(),
                                review,
                            }
                        }
                        None => Operation::RollbackToolBundle {
                            name: row.bundle.clone(),
                            bundle_sha256: row.bundle_sha256.clone(),
                            review,
                        },
                    }
                };
                if work == Work::Save {
                    history.saved = None;
                }
                history.review = None;
                let ticket = history.begin(work);
                let rejected = result_message(
                    ticket.clone(),
                    work,
                    Err("Tool history worker unavailable".into()),
                );
                ctx.spawn_background_with_rejection(
                    move |_| {
                        let timeout = Duration::from_secs(
                            if matches!(
                                work,
                                Work::Review
                                    | Work::Restore
                                    | Work::ReviewPrune
                                    | Work::Prune
                                    | Work::Save
                            ) {
                                600
                            } else {
                                30
                            },
                        );
                        result_message(
                            ticket,
                            work,
                            run_pane_sftp_job(input, target, operation, timeout, None),
                        )
                    },
                    rejected,
                );
            }
            Action::Select(Some(index)) if input.is_some() && history.pending.is_none() => {
                if history.selected != Some(index)
                    && history
                        .versions
                        .as_ref()
                        .is_some_and(|history| index < history.versions.len())
                {
                    history.selected = Some(index);
                    history.review = None;
                    history.status = None;
                    history.saved = None;
                }
            }
            Action::Listed(ticket, result)
                if history.finish(&ticket, |work| work == Work::List).is_some() =>
            {
                match result {
                    Ok(versions) => history.versions = Some(versions),
                    Err(error) => history.status = Some(error),
                }
            }
            Action::Reviewed(ticket, result) => {
                if history
                    .finish(&ticket, |work| {
                        matches!(
                            work,
                            Work::Review | Work::ReviewPrune | Work::ReviewPolicy(_)
                        )
                    })
                    .is_none()
                {
                    return;
                }
                match result {
                    Ok(review) => history.review = Some(review),
                    Err(error) => history.status = Some(error),
                }
            }
            Action::Saved(ticket, result)
                if history.finish(&ticket, |work| work == Work::Save).is_some() =>
            {
                history.status = Some(match result {
                    Ok(saved) => {
                        history.saved = Some(saved);
                        "Saved pinned tool bundle on Windows".into()
                    }
                    Err(error) => error,
                });
            }
            Action::CopyManifest if history.pending.is_none() => {
                if let Some(saved) = &history.saved {
                    history.status = Some(
                        match w11term::clipboard::write(&saved.manifest.to_string_lossy()) {
                            Ok(()) => "Copied the saved manifest path".into(),
                            Err(error) => format!("Copying the saved manifest path: {error:#}"),
                        },
                    );
                }
            }
            Action::Switched(ticket, result) => {
                let Some(work) = history.finish(&ticket, |work| {
                    matches!(work, Work::Restore | Work::SetPolicy(_))
                }) else {
                    return;
                };
                history.status = Some(match result {
                    Ok(switch) => {
                        if work == Work::Restore {
                            reference.tools = None;
                        }
                        let held = work != Work::SetPolicy(UpdatePolicy::Resume);
                        installer.review = None;
                        installer.path_setup = None;
                        installer.status = None;
                        if let Some(versions) = &mut history.versions {
                            for row in versions
                                .versions
                                .iter_mut()
                                .filter(|row| row.bundle == switch.bundle)
                            {
                                row.active = row.bundle_sha256 == switch.bundle_sha256;
                                row.held = row.active && held;
                            }
                        }
                        match work {
                            Work::Restore => format!("Restored tool bundle {}", switch.bundle),
                            Work::SetPolicy(UpdatePolicy::Pause) => {
                                format!("Paused automatic updates for {}", switch.bundle)
                            }
                            Work::SetPolicy(UpdatePolicy::Resume) => {
                                format!("Resumed automatic updates for {}", switch.bundle)
                            }
                            _ => unreachable!("only version and policy changes complete here"),
                        }
                    }
                    Err(error) => error,
                });
            }
            Action::Pruned(ticket, result)
                if history
                    .finish(&ticket, |work| work == Work::Prune)
                    .is_some() =>
            {
                history.status = Some(match result {
                    Ok(removed) => {
                        if let Some(versions) = &mut history.versions {
                            versions.versions.retain(|row| {
                                row.bundle != removed.bundle
                                    || row.bundle_sha256 != removed.bundle_sha256
                            });
                        }
                        history.selected = None;
                        format!("Deleted retained release · {} bytes", removed.removed_bytes)
                    }
                    Err(error) => error,
                });
            }
            _ => {}
        }
    }
}

fn result_message(
    ticket: Arc<()>,
    work: Work,
    result: Result<serde_json::Value, String>,
) -> Message {
    message(match work {
        Work::List => Action::Listed(ticket, decode(result)),
        Work::Review => Action::Reviewed(ticket, decode(result).map(ReviewedVersion::Rollback)),
        Work::ReviewPrune => Action::Reviewed(ticket, decode(result).map(ReviewedVersion::Prune)),
        Work::ReviewPolicy(policy) => Action::Reviewed(
            ticket,
            decode(result).map(|report| ReviewedVersion::UpdatePolicy(policy, report)),
        ),
        Work::Restore | Work::SetPolicy(_) => Action::Switched(ticket, decode(result)),
        Work::Prune => Action::Pruned(ticket, decode(result)),
        Work::Save => Action::Saved(ticket, decode(result)),
    })
}
