use super::*;
use serde::Deserialize;

pub enum Action {
    Open(String, String),
    Review,
    Apply,
    Finished(Arc<()>, Result<Outcome, String>),
    Back,
}

pub enum Outcome {
    Reviewed(Review),
    Removed,
}

#[derive(Deserialize)]
pub struct Review {
    home: String,
    kept: Vec<KeptTool>,
    launcher: String,
    launcher_status: String,
    review: Option<String>,
}

#[derive(Deserialize)]
struct KeptTool {
    name: String,
    sha256: String,
}

pub(super) struct Panel {
    target: ToolPane,
    account: String,
    bundle: String,
    tool: String,
    review: Option<Review>,
    pending: Option<Arc<()>>,
    status: Option<String>,
    removed: bool,
}

fn message(action: Action) -> Message {
    Message::CommandReference(super::Action::ToolRemoval(action))
}

impl Panel {
    pub(super) fn view(&self, active: Option<ToolPane>, ctx: &ViewContext<TerminalApp>) -> View {
        let connected = active == Some(self.target);
        let available = connected && self.pending.is_none() && !self.removed;
        let click = |label: &str, enabled, action: fn() -> Action| {
            Button::new()
                .is_enabled(enabled)
                .on_click(ctx.callback(move |_| message(action())))
                .content(label)
        };
        let detail = |text: String| {
            TextBlock::new()
                .text(text)
                .text_wrapping(TextWrapping::Wrap)
        };
        let mut fields = vec![
            KeyedView::new(
                "title",
                detail(format!("Remove {} from {}", self.tool, self.bundle)).font_size(18.0),
            ),
            KeyedView::new("account", detail(self.account.clone())),
        ];
        if let Some(review) = &self.review {
            let launcher = match review.launcher_status.as_str() {
                "managed" => format!("Managed launcher becomes inactive\n{}", review.launcher),
                "create" => format!("No launcher at {}", review.launcher),
                _ => format!("Preserve the existing entry at {}", review.launcher),
            };
            fields.extend([
                KeyedView::new("home", detail(format!("SFTP home {}", review.home))),
                KeyedView::new("launcher", detail(launcher)),
                KeyedView::new(
                    "effect",
                    detail("The previous release remains available for rollback. Automatic bundle updates pause.".into()),
                ),
            ]);
            if review.kept.is_empty() {
                fields.push(KeyedView::new(
                    "empty",
                    detail("This is the last tool in the active bundle.".into()),
                ));
            }
            fields.extend(review.kept.iter().map(|tool| {
                KeyedView::new(
                    format!("keep-{}", tool.name),
                    detail(format!("Keep {}\nSHA-256 {}", tool.name, tool.sha256)),
                )
            }));
            if review.review.is_none() {
                fields.push(KeyedView::new(
                    "blocked",
                    detail("Removing a tool requires the server’s atomic rename extension.".into()),
                ));
            }
        }
        let status = if !connected {
            Some("Return to the original SSH pane or start a review on the new connection")
        } else {
            self.status.as_deref()
        };
        if let Some(status) = status {
            fields.push(KeyedView::new("status", detail(status.into())));
        }
        let token = self
            .review
            .as_ref()
            .and_then(|review| review.review.as_ref());
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
                        click("Refresh removal review", available, || Action::Review),
                        click(
                            "Remove reviewed SSH tool",
                            available && token.is_some(),
                            || Action::Apply,
                        ),
                        click("Back to command reference", self.pending.is_none(), || {
                            Action::Back
                        }),
                    )),
            ))
    }
}

impl TerminalApp {
    pub(super) fn tool_removal_action(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        let current = self
            .reference_tool_target()
            .map(|(target, session)| (target, session.input.clone()));
        let panel = &mut self.command_reference;
        let action = match action {
            Action::Open(bundle, tool) => {
                let Some((target, _)) = &current else { return };
                let installed = panel
                    .tools
                    .as_ref()
                    .filter(|lookup| lookup.pane == *target)
                    .and_then(|lookup| lookup.result.as_ref())
                    .and_then(|result| result.as_ref().ok());
                if panel.subpanel_open()
                    || !installed.is_some_and(|tools| {
                        tools
                            .tools
                            .iter()
                            .any(|entry| entry.bundle == bundle && entry.name == tool)
                    })
                {
                    return;
                }
                let Some(Page::Terminal(live)) = self.tabs.active().map(|tab| &tab.page) else {
                    return;
                };
                let Source::Ssh(ssh) = &live.source else {
                    return;
                };
                panel.tool_removal = Some(Panel {
                    target: *target,
                    account: format!("{}@{}:{}", ssh.user, ssh.host, ssh.port),
                    bundle,
                    tool,
                    review: None,
                    pending: None,
                    status: None,
                    removed: false,
                });
                Action::Review
            }
            action => action,
        };
        let Some(removal) = &mut panel.tool_removal else {
            return;
        };
        match action {
            Action::Review | Action::Apply if removal.pending.is_none() && !removal.removed => {
                let Some((target, input)) = current.filter(|(pane, _)| *pane == removal.target)
                else {
                    return;
                };
                let apply = matches!(action, Action::Apply);
                let review = if apply {
                    let Some(token) = removal.review.take().and_then(|review| review.review) else {
                        return;
                    };
                    Some(token)
                } else {
                    removal.review = None;
                    None
                };
                let operation = Operation::RemoveFromToolBundle {
                    name: removal.bundle.clone(),
                    tool: removal.tool.clone(),
                    review,
                };
                let ticket = Arc::new(());
                removal.pending = Some(ticket.clone());
                removal.status = Some(if apply {
                    "Activating the reviewed tool release…".into()
                } else {
                    "Checking the active tool release…".into()
                });
                let rejected = message(Action::Finished(
                    ticket.clone(),
                    Err("Tool removal worker unavailable".into()),
                ));
                ctx.spawn_background_with_rejection(
                    move |_| {
                        let result = run_pane_sftp_job(
                            input,
                            target,
                            operation,
                            Duration::from_secs(600),
                            None,
                        )
                        .and_then(|value| {
                            if apply {
                                return Ok(Outcome::Removed);
                            }
                            serde_json::from_value(value)
                                .map(Outcome::Reviewed)
                                .map_err(|error| error.to_string())
                        });
                        message(Action::Finished(ticket, result))
                    },
                    rejected,
                );
            }
            Action::Finished(ticket, result)
                if removal
                    .pending
                    .as_ref()
                    .is_some_and(|request| Arc::ptr_eq(request, &ticket)) =>
            {
                removal.pending = None;
                removal.status = match result {
                    Ok(Outcome::Reviewed(review)) => {
                        removal.review = Some(review);
                        None
                    }
                    Ok(Outcome::Removed) => {
                        removal.removed = true;
                        panel.tools = None;
                        Some(format!("Removed {} from {}", removal.tool, removal.bundle))
                    }
                    Err(error) => Some(error),
                };
            }
            Action::Back if removal.pending.is_none() => {
                panel.tool_removal = None;
                panel.focus = panel.focus.wrapping_add(1);
            }
            _ => (),
        }
    }
}
