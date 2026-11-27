use super::*;
use w11term::sftp::{RetainedToolVersion, ToolBundleVersions};

pub enum Action {
    Open,
    Listed(Arc<()>, Result<ToolBundleVersions, String>),
    Select(Option<usize>),
    Review,
    Reviewed(Arc<()>, Result<Box<RollbackReview>, String>),
    Restore,
    Restored(Arc<()>, Result<Restored, String>),
    Close,
}

#[derive(Deserialize)]
pub struct RollbackReview {
    home: String,
    bundle: String,
    bundle_sha256: String,
    current: Current,
    retained: Retained,
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
    tools: Vec<RetainedTool>,
}

#[derive(Deserialize)]
struct RetainedTool {
    name: String,
}

#[derive(Deserialize)]
pub struct Restored {
    bundle: String,
    bundle_sha256: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Work {
    List,
    Review,
    Restore,
}

#[derive(Default)]
pub(super) struct Panel {
    versions: Option<ToolBundleVersions>,
    selected: Option<usize>,
    review: Option<Box<RollbackReview>>,
    pending: Option<(Arc<()>, Work)>,
    status: Option<String>,
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

    fn finish(&mut self, ticket: &Arc<()>, work: Work) -> bool {
        self.pending
            .take_if(|(pending, kind)| *kind == work && Arc::ptr_eq(pending, ticket))
            .is_some()
    }

    fn selected(&self) -> Option<&RetainedToolVersion> {
        self.versions.as_ref()?.versions.get(self.selected?)
    }

    fn token(&self) -> Option<&str> {
        let review = self.review.as_ref().filter(|review| {
            review.ready && review.current.version.as_deref() != Some(&review.bundle_sha256)
        })?;
        review.review.as_deref()
    }

    pub(super) fn view(&self, account: &str, active: bool, ctx: &ViewContext<TerminalApp>) -> View {
        let available = active && self.pending.is_none();
        let restoring = self
            .pending
            .as_ref()
            .is_some_and(|(_, kind)| *kind == Work::Restore);
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
                Work::Restore => "Restoring reviewed tool bundle…",
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
        }
        if let Some(review) = &self.review {
            review.describe(&mut fields);
        }
        fields.push(KeyedView::new(
            "actions",
            StackPanel::new().spacing(8.0).children((
                button(
                    "Review rollback",
                    available && self.selected().is_some_and(|row| !row.active),
                    || Action::Review,
                ),
                button(
                    "Restore reviewed version",
                    available && self.token().is_some(),
                    || Action::Restore,
                ),
                button("Back to tool installer", !restoring, || Action::Close),
            )),
        ));
        Border::new().padding(12.0).content(
            ScrollViewer::new().content(StackPanel::new().spacing(8.0).keyed_children(fields)),
        )
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

impl RollbackReview {
    fn describe(&self, fields: &mut Vec<KeyedView>) {
        fields.push(detail(
            "destination",
            format!(
                "{} · Linux {} · Home {}\nLauncher directory {}",
                self.bundle, self.retained.architecture, self.home, self.retained.bin_directory
            ),
        ));
        let commands: Vec<_> = self
            .retained
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        fields.push(detail(
            "commands",
            format!("Retained commands {}", commands.join(", ")),
        ));
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
                "Restoring this version pauses automatic bundle updates until you apply a bundle again."
            } else {
                "Resolve the conflicts before restoring this version."
            },
        ));
    }
}

impl TerminalApp {
    pub(super) fn tool_bundle_history(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        let current = self
            .reference_tool_target()
            .map(|(pane, session)| (pane, session.input.clone()));
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
                    .is_some_and(|(_, work)| *work == Work::Restore)
                {
                    installer.history = None;
                }
            }
            Action::Open | Action::Review | Action::Restore => {
                let Some(input) = input else { return };
                if history.pending.is_some() {
                    return;
                }
                let work = match action {
                    Action::Open => Work::List,
                    Action::Review => Work::Review,
                    _ => Work::Restore,
                };
                let operation = if work == Work::List {
                    history.versions = None;
                    history.selected = None;
                    Operation::ToolBundleVersions
                } else {
                    let Some(row) = history.selected().filter(|row| !row.active) else {
                        return;
                    };
                    let review = if work == Work::Restore {
                        let Some(token) = history.token() else { return };
                        Some(token.to_owned())
                    } else {
                        None
                    };
                    Operation::RollbackToolBundle {
                        name: row.bundle.clone(),
                        bundle_sha256: row.bundle_sha256.clone(),
                        review,
                    }
                };
                history.review = None;
                let ticket = history.begin(work);
                let rejected = result_message(
                    ticket.clone(),
                    work,
                    Err("Tool history worker unavailable".into()),
                );
                ctx.spawn_background_with_rejection(
                    move |_| {
                        let timeout =
                            Duration::from_secs(if work == Work::List { 30 } else { 600 });
                        result_message(
                            ticket,
                            work,
                            run_pane_tool_job(input, target, operation, timeout, None),
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
                }
            }
            Action::Listed(ticket, result) if history.finish(&ticket, Work::List) => match result {
                Ok(versions) => history.versions = Some(versions),
                Err(error) => history.status = Some(error),
            },
            Action::Reviewed(ticket, result) if history.finish(&ticket, Work::Review) => {
                match result {
                    Ok(review) => history.review = Some(review),
                    Err(error) => history.status = Some(error),
                }
            }
            Action::Restored(ticket, result) if history.finish(&ticket, Work::Restore) => {
                history.status = Some(match result {
                    Ok(restored) => {
                        reference.tools = None;
                        installer.review = None;
                        installer.status = None;
                        if let Some(versions) = &mut history.versions {
                            for row in versions
                                .versions
                                .iter_mut()
                                .filter(|row| row.bundle == restored.bundle)
                            {
                                row.active = row.bundle_sha256 == restored.bundle_sha256;
                                row.held = row.active;
                            }
                        }
                        format!("Restored tool bundle {}", restored.bundle)
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
        Work::Review => Action::Reviewed(ticket, decode(result)),
        Work::Restore => Action::Restored(ticket, decode(result)),
    })
}
