use super::*;

pub enum Action {
    Preview,
    Apply,
    Reviewed(Arc<()>, Result<Box<Review>, String>),
    Applied(Arc<()>, Result<(), String>),
}

#[derive(Deserialize)]
pub struct Review {
    shell: String,
    home: String,
    bin_directory: String,
    files: Vec<File>,
    blocked: Option<String>,
    pub(super) ready: bool,
    pub(super) review: Option<String>,
}

#[derive(Deserialize)]
struct File {
    path: String,
    action: String,
    hook: Option<String>,
    replaced_hook: Option<String>,
}

impl Review {
    pub(super) fn describe(&self, fields: &mut Vec<KeyedView>) {
        let detail = |key: String, text: String| {
            KeyedView::new(
                key,
                TextBlock::new()
                    .text(text)
                    .text_wrapping(TextWrapping::Wrap),
            )
        };
        fields.push(detail(
            "path-review-account".into(),
            format!(
                "{} startup files · {}/{}",
                self.shell,
                self.home.trim_end_matches('/'),
                self.bin_directory
            ),
        ));
        for (index, file) in self.files.iter().enumerate() {
            let action = match file.action.as_str() {
                "create" => "Create",
                "append-hook" => "Append managed block to",
                "replace-hook" => "Replace managed block in",
                "unchanged" => "Already current",
                _ => "Blocked",
            };
            fields.push(detail(
                format!("path-file-{index}"),
                format!("{action} · {}", file.path),
            ));
            if let Some(old) = &file.replaced_hook {
                fields.push(detail(
                    format!("path-before-{index}"),
                    format!("Existing managed block\n{old}"),
                ));
            }
            if matches!(
                file.action.as_str(),
                "create" | "append-hook" | "replace-hook"
            ) && let Some(hook) = &file.hook
            {
                fields.push(detail(
                    format!("path-after-{index}"),
                    format!("Proposed managed block\n{hook}"),
                ));
            }
        }
        let status = if let Some(blocked) = &self.blocked {
            blocked.as_str()
        } else if self.ready {
            "The repaired PATH applies to new shells."
        } else {
            "The managed PATH is already current."
        };
        fields.push(detail("path-review-status".into(), status.into()));
    }
}

impl TerminalApp {
    pub(super) fn repair_tool_path(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        let current = self.reference_tool_target().and_then(|(target, session)| {
            let report = session.events.tool_bundle_sync(target.generation)?;
            (report.path == Some(ToolPathSyncStatus::Skipped))
                .then(|| (target, session.input.clone()))
        });
        let Some(installer) = &mut self.command_reference.tool_install else {
            return;
        };
        if installer.history.is_some() {
            return;
        }
        match action {
            Action::Preview | Action::Apply => {
                let target = installer.target;
                let Some((_, input)) = current.filter(|(pane, _)| *pane == target) else {
                    return;
                };
                if installer.pending.is_some() {
                    return;
                }
                let apply = matches!(action, Action::Apply);
                let review = if apply {
                    let Some(token) = installer
                        .path_review
                        .as_ref()
                        .filter(|review| review.ready)
                        .and_then(|review| review.review.clone())
                    else {
                        return;
                    };
                    Some(token)
                } else {
                    None
                };
                installer.path_review = None;
                let ticket = installer.begin(if apply {
                    Work::PathApply
                } else {
                    Work::PathReview
                });
                let rejected = result_message(
                    ticket.clone(),
                    apply,
                    Err("PATH review worker unavailable".into()),
                );
                ctx.spawn_background_with_rejection(
                    move |_| {
                        let operation = Operation::ReviewToolPath { review };
                        let result = run_pane_tool_job(
                            input,
                            target,
                            operation,
                            Duration::from_secs(30),
                            None,
                        );
                        result_message(ticket, apply, result)
                    },
                    rejected,
                );
            }
            Action::Reviewed(ticket, result) if installer.finish(&ticket, Work::PathReview) => {
                match result {
                    Ok(review) => installer.path_review = Some(review),
                    Err(error) => installer.status = Some(error),
                }
            }
            Action::Applied(ticket, result) if installer.finish(&ticket, Work::PathApply) => {
                installer.status = Some(match result {
                    Ok(()) => "Managed PATH repaired for new shells".into(),
                    Err(error) => error,
                });
            }
            _ => (),
        }
    }
}

fn result_message(
    ticket: Arc<()>,
    apply: bool,
    result: Result<serde_json::Value, String>,
) -> Message {
    message(super::Action::Path(if apply {
        Action::Applied(ticket, result.map(|_| ()))
    } else {
        Action::Reviewed(ticket, decode(result))
    }))
}
