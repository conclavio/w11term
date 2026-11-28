use super::*;
use w11term::sftp::{
    DotfileTarget, DotfileVersionState, DotfileVersions, ExportedDotfileBundle,
    RetainedDotfileVersion,
};

pub enum Action {
    Open,
    Refresh,
    Select(Option<usize>),
    Save,
    Saved(Arc<()>, Result<Option<ExportedDotfileBundle>, String>),
    ReviewProfile,
    ReviewRestore,
    ReviewDisable,
    ReviewResume,
    Apply,
    Reviewed(Arc<()>, Result<Box<SwitchReview>, String>),
    Completed(Arc<()>, Result<serde_json::Value, String>),
    Back,
}

#[derive(Default)]
pub(super) struct Panel {
    listing: Option<DotfileVersions>,
    selected: Option<usize>,
    review: Option<Box<SwitchReview>>,
    saved: Option<ExportedDotfileBundle>,
}

#[derive(Deserialize)]
pub struct SwitchReview {
    home: String,
    target: DotfileTarget,
    active_bundle_sha256: Option<String>,
    selector: Edit,
    supports_atomic_replace: bool,
    fragment_changes: Comparison,
    review: String,
}

#[derive(Deserialize)]
struct Switched {
    target: DotfileTarget,
    changed: bool,
}

fn send(action: Action) -> Message {
    message(super::Action::History(action))
}

fn short_id(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
}

fn version_label(version: &RetainedDotfileVersion, active: Option<&str>) -> String {
    let mut label = short_id(&version.bundle_sha256).to_owned();
    if active == Some(version.bundle_sha256.as_str()) {
        label.push_str(" · Active");
    }
    if let Some(datetime) = version
        .modified
        .and_then(|time| chrono::DateTime::from_timestamp(i64::from(time), 0))
    {
        label.push_str(&format!(" · {}", datetime.format("%Y-%m-%d %H:%M UTC")));
    }
    label
}

impl Panel {
    fn selected_id(&self) -> Option<&str> {
        let listing = self.listing.as_ref()?;
        Some(&listing.versions.get(self.selected?)?.bundle_sha256)
    }

    fn can_disable(&self) -> bool {
        self.listing.as_ref().is_some_and(|listing| {
            matches!(
                listing.state,
                DotfileVersionState::Active
                    | DotfileVersionState::Paused
                    | DotfileVersionState::AwaitingSync
            )
        })
    }

    fn can_resume(&self) -> bool {
        self.listing.as_ref().is_some_and(|listing| {
            matches!(
                listing.state,
                DotfileVersionState::Paused | DotfileVersionState::Disabled
            )
        })
    }

    pub(super) fn view(
        &self,
        installer: &Installer,
        connected: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let idle = connected && installer.pending.is_none();
        let button = |label: &str, enabled, action: fn() -> Action| {
            Button::new()
                .is_enabled(enabled)
                .on_click(ctx.callback(move |_| send(action())))
                .content(label)
        };
        let shell = installer.shell.label();
        let mut fields = vec![
            KeyedView::new(
                "title",
                TextBlock::new().text("Dotfile versions").font_size(18.0),
            ),
            detail("target", format!("{} · {shell}", installer.account)),
            KeyedView::new(
                "refresh",
                button("Refresh dotfile versions", idle, || Action::Refresh),
            ),
        ];
        if let Some(listing) = &self.listing {
            fields.push(detail("home", format!("SFTP home {}", listing.home)));
            let state = match listing.state {
                DotfileVersionState::Inactive => "No managed version active",
                DotfileVersionState::AwaitingSync => {
                    "Waiting for an approved profile to sync managed dotfiles"
                }
                DotfileVersionState::Active => "Managed dotfiles active",
                DotfileVersionState::Paused => "Automatic dotfile sync paused",
                DotfileVersionState::Disabled => "Managed dotfiles disabled",
                DotfileVersionState::Conflict => "The active bundle selector has a conflict",
            };
            fields.push(detail("state", state.into()));
            if let Some(active) = &listing.active_bundle_sha256 {
                fields.push(detail("active", format!("Active version {active}")));
            }
            if listing.versions.is_empty() {
                fields.push(detail("empty", "No retained dotfile versions".into()));
            } else {
                fields.push(KeyedView::new(
                    "versions",
                    ComboBox::new()
                        .automation_name("Retained dotfile versions")
                        .items_source(
                            listing
                                .versions
                                .iter()
                                .map(|row| {
                                    version_label(row, listing.active_bundle_sha256.as_deref())
                                })
                                .collect::<Vec<_>>(),
                        )
                        .selected_index(self.selected)
                        .is_enabled(idle)
                        .on_selection_changed(ctx.callback(|index| send(Action::Select(index)))),
                ));
            }
            if listing.omitted_versions != 0 {
                fields.push(detail(
                    "omitted",
                    format!("{} older versions omitted", listing.omitted_versions),
                ));
            }
        }
        if let Some(id) = self.selected_id() {
            fields.push(detail("selected", format!("Selected version {id}")));
        }
        fields.extend([
            KeyedView::new(
                "save-bundle",
                button(
                    "Save selected dotfile bundle",
                    idle && self.selected_id().is_some(),
                    || Action::Save,
                ),
            ),
            KeyedView::new(
                "review-restore",
                button(
                    "Review dotfile restore",
                    idle && self.selected_id().is_some(),
                    || Action::ReviewRestore,
                ),
            ),
            KeyedView::new(
                "review-disable",
                button(
                    "Review disabling managed dotfiles",
                    idle && self.can_disable(),
                    || Action::ReviewDisable,
                ),
            ),
        ]);
        if self.saved.is_some() && installer.profile.is_some() {
            fields.push(KeyedView::new(
                "review-saved-in-profile",
                button("Review saved dotfiles in this SSH profile", idle, || {
                    Action::ReviewProfile
                }),
            ));
        }
        if self.can_resume() {
            fields.push(KeyedView::new(
                "review-resume",
                button("Review resuming automatic dotfile sync", idle, || {
                    Action::ReviewResume
                }),
            ));
        }
        let mut footer = Vec::with_capacity(2);
        if let Some(review) = &self.review {
            fields.push(detail(
                "review-home",
                format!("Reviewed SFTP home {}", review.home),
            ));
            if let Some(current) = &review.active_bundle_sha256 {
                fields.push(detail(
                    "review-active",
                    format!("Reviewed active version {current}"),
                ));
            }
            let paused = "This choice pauses automatic dotfile sync on reconnect.";
            let (change, effect, button_label) = match &review.target {
                DotfileTarget::Bundle { sha256 } => (
                    format!("Restore version {sha256}"),
                    paused,
                    "Restore reviewed dotfiles",
                ),
                DotfileTarget::Disabled => (
                    "Disable managed dotfiles for new shells".into(),
                    paused,
                    "Disable reviewed dotfiles",
                ),
                DotfileTarget::Automatic => (
                    if review.active_bundle_sha256.is_some() {
                        "Keep the reviewed version active".into()
                    } else {
                        "Keep managed fragments inactive until a bundle is installed".into()
                    },
                    "Approved profiles can sync on their next SSH connection.",
                    "Resume automatic dotfile sync",
                ),
            };
            fields.push(detail("change", change));
            fields.push(detail("sync-effect", effect.into()));
            fields.push(detail(
                "selector",
                format!(
                    "{} · {}",
                    review.selector.label(review.supports_atomic_replace),
                    review.selector.path
                ),
            ));
            review.fragment_changes.describe(&mut fields);
            footer.push(KeyedView::new(
                "apply-switch",
                button(
                    button_label,
                    idle && review
                        .selector
                        .blocker(review.supports_atomic_replace)
                        .is_none(),
                    || Action::Apply,
                ),
            ));
        }
        if !connected {
            fields.push(detail(
                "connection",
                "Return to the original SSH connection, or reopen this panel after reconnecting"
                    .into(),
            ));
        }
        if let Some(status) = installer.status.as_deref().or_else(|| {
            installer.pending.as_ref().map(|(_, work)| match work {
                Work::ListVersions => "Reading retained dotfile versions…",
                Work::SaveVersion => "Saving the verified dotfile bundle…",
                Work::ReviewSwitch => "Verifying the dotfile change…",
                Work::SwitchVersion => "Applying the reviewed dotfile change…",
                _ => unreachable!("history only runs version operations"),
            })
        }) {
            fields.push(detail("status", status.into()));
        }
        footer.push(KeyedView::new(
            "back",
            button("Back to dotfile installer", !installer.applying(), || {
                Action::Back
            }),
        ));
        Grid::new()
            .rows([GridLength::Star(1.0), GridLength::Auto])
            .keyed_children([
                KeyedView::new(
                    "versions",
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

impl TerminalApp {
    fn review_saved_dotfiles_for_profile(&mut self, ctx: &ComponentContext<Self>) {
        let Some(installer) = self
            .command_reference
            .dotfiles
            .as_ref()
            .filter(|installer| installer.pending.is_none())
        else {
            return;
        };
        let Some(saved) = installer
            .history
            .as_ref()
            .and_then(|history| history.saved.clone())
        else {
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
            .map(str::to_owned)
        else {
            return;
        };
        let Some(connection) = session.ssh_connection_at(pane.generation) else {
            return;
        };
        match installer.startup_directory() {
            Ok(zdotdir) => {
                self.open_dotfile_snapshot_profile(&profile, saved, zdotdir, &connection, ctx)
            }
            Err(error) => {
                if let Some(installer) = &mut self.command_reference.dotfiles {
                    installer.status = Some(error);
                }
            }
        }
    }

    pub(super) fn dotfile_history_action(
        &mut self,
        action: Action,
        current: Option<(ToolPane, tokio::sync::mpsc::UnboundedSender<Command>)>,
        ctx: &ComponentContext<Self>,
    ) {
        if matches!(action, Action::ReviewProfile) {
            self.review_saved_dotfiles_for_profile(ctx);
            return;
        }
        let owner = self.keyboard.hwnd().map(|window| window.0 as usize);
        let Some(installer) = &mut self.command_reference.dotfiles else {
            return;
        };
        let target = installer.target;
        let input = current
            .filter(|(pane, _)| *pane == target)
            .map(|(_, input)| input);
        let idle = input.is_some() && installer.pending.is_none();
        let shell = installer.shell;
        let list = || (Work::ListVersions, Operation::ListDotfileVersions { shell });
        let request = if matches!(action, Action::Open) {
            if !idle || installer.history.is_some() {
                return;
            }
            installer.history = Some(Panel::default());
            Some(list())
        } else {
            let Some(history) = &mut installer.history else {
                return;
            };
            match action {
                Action::Refresh if idle => {
                    *history = Panel::default();
                    Some(list())
                }
                Action::Select(index) if idle => {
                    let selected = index.filter(|&index| {
                        history
                            .listing
                            .as_ref()
                            .is_some_and(|list| index < list.versions.len())
                    });
                    if history.selected != selected {
                        history.selected = selected;
                        history.review = None;
                        history.saved = None;
                        installer.status = None;
                    }
                    None
                }
                Action::Save if idle => {
                    let (Some(id), Some(owner), Some(input)) =
                        (history.selected_id(), owner, input)
                    else {
                        return;
                    };
                    let id = id.to_owned();
                    let current = installer
                        .bundle
                        .as_deref()
                        .unwrap_or(self.store.path())
                        .to_owned();
                    history.saved = None;
                    installer.save_dotfile_version(owner, current, id, input, ctx);
                    return;
                }
                Action::Saved(ticket, result) => {
                    if installer.finish(&ticket, Work::SaveVersion) {
                        installer.status = match result {
                            Ok(Some(saved)) => {
                                let status =
                                    format!("Saved dotfile bundle · {}", saved.local.display());
                                installer.history.as_mut().unwrap().saved = Some(saved);
                                Some(status)
                            }
                            Ok(None) => None,
                            Err(error) => Some(error),
                        };
                    }
                    None
                }
                Action::ReviewRestore
                | Action::ReviewDisable
                | Action::ReviewResume
                | Action::Apply
                    if idle =>
                {
                    let (target, review) = match action {
                        Action::ReviewRestore => {
                            let Some(id) = history.selected_id() else {
                                return;
                            };
                            (
                                DotfileTarget::Bundle {
                                    sha256: id.to_owned(),
                                },
                                None,
                            )
                        }
                        Action::ReviewDisable if history.can_disable() => {
                            (DotfileTarget::Disabled, None)
                        }
                        Action::ReviewResume if history.can_resume() => {
                            (DotfileTarget::Automatic, None)
                        }
                        Action::Apply => {
                            let Some(reviewed) = history.review.take_if(|review| {
                                review
                                    .selector
                                    .blocker(review.supports_atomic_replace)
                                    .is_none()
                            }) else {
                                return;
                            };
                            (reviewed.target, Some(reviewed.review))
                        }
                        _ => return,
                    };
                    history.review = None;
                    let zdotdir = match installer.startup_directory() {
                        Ok(path) => path,
                        Err(error) => {
                            installer.status = Some(error);
                            return;
                        }
                    };
                    let work = if review.is_some() {
                        Work::SwitchVersion
                    } else {
                        Work::ReviewSwitch
                    };
                    Some((
                        work,
                        Operation::SwitchDotfiles {
                            target,
                            shell: installer.shell,
                            zdotdir,
                            review,
                        },
                    ))
                }
                Action::Reviewed(ticket, result) => {
                    if installer.finish(&ticket, Work::ReviewSwitch) {
                        match result {
                            Ok(review) => installer.history.as_mut().unwrap().review = Some(review),
                            Err(error) => installer.status = Some(error),
                        }
                    }
                    None
                }
                Action::Completed(ticket, result) => {
                    let Some((_, work @ (Work::ListVersions | Work::SwitchVersion))) =
                        installer.pending.as_ref()
                    else {
                        return;
                    };
                    let work = *work;
                    if !installer.finish(&ticket, work) {
                        return;
                    }
                    let history = installer.history.as_mut().unwrap();
                    match work {
                        Work::ListVersions => match decode(result) {
                            Ok(listing) => history.listing = Some(listing),
                            Err(error) => installer.status = Some(error),
                        },
                        Work::SwitchVersion => match decode::<Switched>(result) {
                            Ok(switched) => {
                                let message = match (switched.changed, switched.target) {
                                    (true, DotfileTarget::Bundle { sha256 }) => {
                                        format!("Restored dotfile version {}", short_id(&sha256))
                                    }
                                    (true, DotfileTarget::Disabled) => {
                                        "Disabled managed dotfiles".into()
                                    }
                                    (true, DotfileTarget::Automatic) => {
                                        "Automatic dotfile sync resumed".into()
                                    }
                                    (false, _) => "No dotfiles change needed".into(),
                                };
                                *history = Panel::default();
                                if let Some(input) = input {
                                    installer.run_history(list(), input, ctx);
                                }
                                installer.status = Some(message);
                                return;
                            }
                            Err(error) => installer.status = Some(error),
                        },
                        _ => unreachable!(),
                    }
                    None
                }
                Action::Back if !installer.applying() => {
                    installer.history = None;
                    installer.pending = None;
                    installer.status = None;
                    None
                }
                _ => None,
            }
        };
        if let Some((work, operation)) = request
            && let Some(input) = input
        {
            installer.run_history((work, operation), input, ctx);
        }
    }
}

impl Installer {
    fn save_dotfile_version(
        &mut self,
        owner: usize,
        current: PathBuf,
        bundle_sha256: String,
        input: tokio::sync::mpsc::UnboundedSender<Command>,
        ctx: &ComponentContext<TerminalApp>,
    ) {
        let target = self.target;
        let shell = self.shell;
        let name = shell.name();
        let purpose = file_picker::Purpose::SaveDotfilesBundle {
            current,
            suggestion: format!("dotfiles-{name}-{}.json", short_id(&bundle_sha256)),
        };
        let ticket = self.begin(Work::SaveVersion);
        let rejected = send(Action::Saved(
            ticket.clone(),
            Err("Dotfile bundle save unavailable".into()),
        ));
        ctx.spawn_background_with_rejection(
            move |scope| {
                let result = (|| {
                    let file =
                        file_picker::pick(owner, purpose).map_err(|error| format!("{error:#}"))?;
                    let Some(file) = file.filter(|_| !scope.is_cancelled()) else {
                        return Ok(None);
                    };
                    let operation = Operation::ExportDotfileBundle {
                        shell,
                        bundle_sha256,
                        local: file.path,
                    };
                    decode::<ExportedDotfileBundle>(run_pane_sftp_job(
                        input,
                        target,
                        operation,
                        Duration::from_secs(30),
                        None,
                    ))
                    .map(Some)
                })();
                send(Action::Saved(ticket, result))
            },
            rejected,
        );
    }

    fn run_history(
        &mut self,
        (work, operation): (Work, Operation),
        input: tokio::sync::mpsc::UnboundedSender<Command>,
        ctx: &ComponentContext<TerminalApp>,
    ) {
        let target = self.target;
        if work == Work::SwitchVersion {
            self.review = None;
        }
        let ticket = self.begin(work);
        let complete = move |ticket, result| {
            if work == Work::ReviewSwitch {
                send(Action::Reviewed(ticket, decode(result)))
            } else {
                send(Action::Completed(ticket, result))
            }
        };
        let rejected = complete(
            ticket.clone(),
            Err("Dotfile version worker unavailable".into()),
        );
        ctx.spawn_background_with_rejection(
            move |scope| {
                let result = if scope.is_cancelled() {
                    Err("Dotfile version operation cancelled".into())
                } else {
                    run_pane_sftp_job(input, target, operation, Duration::from_secs(30), None)
                };
                complete(ticket, result)
            },
            rejected,
        );
    }
}
