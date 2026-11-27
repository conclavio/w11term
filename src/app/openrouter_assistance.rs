use super::*;
use std::{
    fmt::Write as _,
    sync::atomic::{AtomicBool, Ordering},
};
use w11term::{
    openrouter::{self, Model, Preferences, Reply, Review, Task},
    ssh::TriageReport,
    terminal::{Snapshot, viewport_text},
};
use zeroize::Zeroizing;

pub(super) const LABEL: &str = "OpenRouter assistance";
pub(super) const EXPLAIN: &str = "Explain selected text with OpenRouter";
pub(super) const EXPLAIN_VIEWPORT: &str = "Explain visible pane with OpenRouter";
pub(super) const DRAFT: &str = "Draft a Bash command with OpenRouter";

pub enum Action {
    Show(Task),
    ExplainViewport(clipboard_paste::Origin),
    Key(TabId, String),
    SaveKey(TabId),
    RemoveKey(TabId),
    Model(TabId, String),
    Zdr(TabId, bool),
    SaveSettings(TabId),
    Catalog(TabId),
    Edit(TabId, String),
    Review(TabId),
    Send(TabId),
    Cancel(TabId),
    Copy(TabId),
    Ready(TabId, Arc<()>, Result<Outcome, String>),
}

pub enum Outcome {
    Prepared {
        preferences: Option<Result<Preferences, String>>,
        saved_key: Option<Result<bool, String>>,
        evidence: Result<String, &'static str>,
    },
    Key(bool),
    SettingsSaved,
    Models(Vec<Model>),
    Reply(Reply),
}

struct Pending {
    ticket: Arc<()>,
    cancel: Option<Arc<AtomicBool>>,
}

enum Evidence {
    Text(String),
    MissingSelection,
    Viewport(Arc<Snapshot>),
}

impl Evidence {
    fn read(self) -> Result<String, &'static str> {
        match self {
            Self::Text(text) => Ok(text),
            Self::MissingSelection => {
                Err("Select terminal text or paste an excerpt into the preview.")
            }
            Self::Viewport(frame) => {
                let text = viewport_text(&frame)
                    .map_err(|_| "The visible pane exceeds the text capture limit. Select an excerpt to explain.")?;
                if text.is_empty() {
                    return Err("The visible pane contains no text.");
                }
                Ok(text)
            }
        }
    }
}

#[derive(Default)]
pub(super) struct PageState {
    loaded: bool,
    task: Option<Task>,
    prefs: Preferences,
    models: Vec<Model>,
    key: Zeroizing<String>,
    saved_key: bool,
    evidence: String,
    review: Option<Review>,
    reply: Option<Reply>,
    pending: Option<Pending>,
    status: Option<String>,
}

impl Drop for PageState {
    fn drop(&mut self) {
        if let Some(cancel) = self
            .pending
            .as_ref()
            .and_then(|pending| pending.cancel.as_ref())
        {
            cancel.store(true, Ordering::Release);
        }
    }
}

fn message(action: Action) -> Message {
    Message::OpenRouter(action)
}

impl TerminalApp {
    pub(super) fn review_triage_with_openrouter(
        &mut self,
        report: &TriageReport,
        preview_sample: Option<usize>,
        ctx: &ComponentContext<Self>,
    ) {
        self.show_openrouter(
            Task::Triage,
            Evidence::Text(triage_preview(report, preview_sample)),
            ctx,
        );
    }

    pub(super) fn review_triage_comparison_with_openrouter(
        &mut self,
        evidence: String,
        ctx: &ComponentContext<Self>,
    ) {
        self.show_openrouter(Task::TriageComparison, Evidence::Text(evidence), ctx);
    }

    fn show_openrouter(&mut self, task: Task, evidence: Evidence, ctx: &ComponentContext<Self>) {
        self.set_diagnostics_drawer(
            matches!(task, Task::Triage | Task::TriageComparison)
                .then_some(diagnostics::Drawer::Triage),
        );
        self.command_reference.close();
        let current = self
            .tabs
            .entries()
            .iter()
            .find(|tab| matches!(tab.value.page, Page::OpenRouter(_)))
            .map(|tab| tab.id);
        let id = if let Some(id) = current {
            self.tabs.select(id);
            id
        } else {
            self.tabs.insert(AppTab {
                label: LABEL.into(),
                page: Page::OpenRouter(Box::default()),
            })
        };
        self.sync_selection(ctx);
        self.activate(ctx);
        let Some(page) = self
            .openrouter_page(id)
            .filter(|page| page.pending.is_none())
        else {
            return;
        };
        page.review = None;
        page.reply = None;
        page.task = Some(task);
        page.evidence.clear();
        let viewport = matches!(evidence, Evidence::Viewport(_));
        let load_settings = !page.loaded;
        if !viewport && !load_settings {
            page.set_evidence(evidence.read());
            return;
        }
        let status = if viewport {
            "Preparing visible pane text…"
        } else {
            "Loading local OpenRouter settings…"
        };
        let (ticket, _) = page.begin(status, false);
        run(ctx, id, ticket, move |_| {
            Ok(Outcome::Prepared {
                preferences: load_settings
                    .then(|| Preferences::load().map_err(|error| format!("{error:#}"))),
                saved_key: load_settings
                    .then(|| openrouter::has_key().map_err(|error| format!("{error:#}"))),
                evidence: evidence.read(),
            })
        });
    }

    pub(super) fn openrouter_viewport(&self) -> Option<(clipboard_paste::Origin, Arc<Snapshot>)> {
        let Page::Terminal(tab) = &self.tabs.active()?.page else {
            return None;
        };
        let frame = tab.session.frames.load();
        if tab.browsing_sessions() || tab.device_busy || frame.prompt.is_some() {
            return None;
        }
        let origin = clipboard_paste::Origin {
            pane: tab.session.pane_id,
            group: self.tabs.active_group_id()?,
            connection: frame.connection_generation,
        };
        Some((origin, frame))
    }

    fn openrouter_page(&mut self, id: TabId) -> Option<&mut PageState> {
        match &mut self.tabs.get_mut(id)?.page {
            Page::OpenRouter(page) => Some(page),
            _ => None,
        }
    }

    pub(super) fn openrouter_action(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        let id = match &action {
            Action::Show(task) => {
                let evidence = if *task == Task::Explain {
                    self.paste_origin()
                        .and_then(|origin| self.pane_selection_text(origin))
                        .map_or(Evidence::MissingSelection, Evidence::Text)
                } else {
                    Evidence::Text(String::new())
                };
                self.show_openrouter(*task, evidence, ctx);
                return;
            }
            Action::ExplainViewport(target) => {
                if let Some((_, frame)) = self
                    .openrouter_viewport()
                    .filter(|(origin, _)| origin == target)
                {
                    self.show_openrouter(Task::Explain, Evidence::Viewport(frame), ctx);
                } else {
                    self.window_error = Some("The focused pane changed. Choose the pane again to review its visible text.".into());
                }
                return;
            }
            Action::Ready(id, ..)
            | Action::Key(id, _)
            | Action::SaveKey(id)
            | Action::RemoveKey(id)
            | Action::Model(id, _)
            | Action::Zdr(id, _)
            | Action::SaveSettings(id)
            | Action::Catalog(id)
            | Action::Edit(id, _)
            | Action::Review(id)
            | Action::Send(id)
            | Action::Cancel(id)
            | Action::Copy(id) => *id,
        };
        if matches!(action, Action::Send(_)) && self.tabs.active_id() != Some(id) {
            return;
        }
        let Some(page) = self.openrouter_page(id) else {
            return;
        };
        match action {
            Action::Ready(_, ticket, result) => page.finish(ticket, result),
            Action::Cancel(_) => {
                if let Some(cancel) = page
                    .pending
                    .as_ref()
                    .and_then(|pending| pending.cancel.as_ref())
                {
                    cancel.store(true, Ordering::Release);
                    page.pending = None;
                    page.status = Some("Stopped waiting for OpenRouter.".into());
                }
            }
            action if page.pending.is_none() => page.apply(id, action, ctx),
            _ => (),
        }
    }
}

impl PageState {
    fn set_evidence(&mut self, evidence: Result<String, &'static str>) {
        match evidence {
            Ok(text) => {
                // CRLF pairs can halve the byte count. Leave enough input to
                // fill the limit, including one UTF-8 character at the boundary.
                let prefix = clip(&text, 2 * openrouter::MAX_INPUT + 4);
                let clipped = prefix.len() < text.len();
                let text = display_safe(prefix)
                    .replace("\r\n", "\r")
                    .replace('\n', "\r");
                self.status = (clipped || text.len() > openrouter::MAX_INPUT).then(|| {
                    "The preview includes the first 16 KiB of the selected evidence.".into()
                });
                self.evidence = clip(&text, openrouter::MAX_INPUT).to_owned();
            }
            Err(error) => self.status = Some(error.into()),
        }
    }

    fn begin(&mut self, status: &str, cancellable: bool) -> (Arc<()>, Option<Arc<AtomicBool>>) {
        let ticket = Arc::new(());
        let cancel = cancellable.then(|| Arc::new(AtomicBool::new(false)));
        self.pending = Some(Pending {
            ticket: ticket.clone(),
            cancel: cancel.clone(),
        });
        self.status = Some(status.into());
        (ticket, cancel)
    }

    fn finish(&mut self, ticket: Arc<()>, result: Result<Outcome, String>) {
        if self
            .pending
            .take_if(|pending| Arc::ptr_eq(&ticket, &pending.ticket))
            .is_none()
        {
            return;
        }
        self.status = None;
        match result {
            Ok(Outcome::Prepared {
                preferences,
                saved_key,
                evidence,
            }) => {
                self.loaded = true;
                self.set_evidence(evidence);
                if let Some(preferences) = preferences {
                    match preferences {
                        Ok(prefs) => self.prefs = prefs,
                        Err(error) => self.status = Some(error),
                    }
                }
                if let Some(saved_key) = saved_key {
                    match saved_key {
                        Ok(saved) => self.saved_key = saved,
                        Err(error) => self.status = Some(error),
                    }
                }
            }
            Ok(Outcome::Key(saved)) => {
                self.saved_key = saved;
                self.status = Some(
                    if saved {
                        "OpenRouter API key saved"
                    } else {
                        "Saved OpenRouter API key removed"
                    }
                    .into(),
                );
            }
            Ok(Outcome::SettingsSaved) => self.status = Some("OpenRouter settings saved".into()),
            Ok(Outcome::Models(models)) => {
                self.status = Some(format!("Loaded {} OpenRouter models", models.len()));
                self.models = models;
            }
            Ok(Outcome::Reply(reply)) => self.reply = Some(reply),
            Err(error) => self.status = Some(error),
        }
    }

    fn apply(&mut self, id: TabId, action: Action, ctx: &ComponentContext<TerminalApp>) {
        match action {
            Action::Key(_, value) => self.key = Zeroizing::new(value),
            Action::SaveKey(_) => {
                let key = std::mem::take(&mut self.key);
                self.review = None;
                let (ticket, _) = self.begin("Protecting the OpenRouter key with Windows…", false);
                run(ctx, id, ticket, move |_| {
                    openrouter::save_key(&key)?;
                    Ok(Outcome::Key(true))
                });
            }
            Action::RemoveKey(_) => {
                self.review = None;
                let (ticket, _) = self.begin("Removing the saved OpenRouter key…", false);
                run(ctx, id, ticket, |_| {
                    openrouter::remove_key()?;
                    Ok(Outcome::Key(false))
                });
            }
            Action::Model(_, model) if self.prefs.model != model => {
                self.review = None;
                self.prefs.model = model;
            }
            Action::Zdr(_, require) if self.prefs.require_zdr != require => {
                self.review = None;
                self.prefs.require_zdr = require;
            }
            Action::SaveSettings(_) => {
                let prefs = self.prefs.clone();
                let (ticket, _) = self.begin("Saving OpenRouter settings…", false);
                run(ctx, id, ticket, move |_| {
                    prefs.save()?;
                    Ok(Outcome::SettingsSaved)
                });
            }
            Action::Catalog(_) => {
                let (ticket, cancel) = self.begin("Fetching the OpenRouter model catalog…", true);
                let cancel = cancel.unwrap();
                run(ctx, id, ticket, move |stopped| {
                    openrouter::models(|| stopped() || cancel.load(Ordering::Acquire))
                        .map(Outcome::Models)
                });
            }
            Action::Edit(_, evidence) if self.evidence != evidence => {
                self.review = None;
                self.evidence = evidence;
            }
            Action::Review(_) => {
                self.status = None;
                self.review = None;
                let result = Review::prepare(
                    self.task.unwrap_or(Task::Draft),
                    self.prefs.clone(),
                    self.evidence.replace("\r\n", "\n").replace('\r', "\n"),
                );
                match result {
                    Ok(review) => self.review = Some(review),
                    Err(error) => self.status = Some(format!("{error:#}")),
                }
            }
            Action::Send(_) if self.saved_key => {
                let Some(review) = self.review.take() else {
                    return;
                };
                self.reply = None;
                let (ticket, cancel) = self.begin("Waiting for OpenRouter…", true);
                let cancel = cancel.unwrap();
                run(ctx, id, ticket, move |stopped| {
                    review
                        .submit(|| stopped() || cancel.load(Ordering::Acquire))
                        .map(Outcome::Reply)
                });
            }
            Action::Copy(_) => {
                if let Some(reply) = &self.reply {
                    self.status = Some(
                        match w11term::clipboard::write(&display_safe(&reply.text)) {
                            Ok(()) => "Copied the response".into(),
                            Err(error) => format!("{error:#}"),
                        },
                    );
                }
            }
            _ => (),
        }
    }

    pub(super) fn view(&self, id: TabId, ctx: &ViewContext<TerminalApp>) -> View {
        let idle = self.pending.is_none() && self.loaded;
        let button = |label: &str, enabled, action: fn(TabId) -> Action| {
            Button::new()
                .is_enabled(enabled)
                .on_click(ctx.callback(move |_| message(action(id))))
                .content(label)
        };
        let mut fields = vec![
            KeyedView::new("heading", TextBlock::new().text(LABEL).font_size(22.0)),
            line(
                "key-state",
                if self.saved_key {
                    "OpenRouter API key is stored with Windows protection"
                } else {
                    "Save an OpenRouter API key to send a request"
                },
            ),
            KeyedView::new(
                "key",
                PasswordBox::new()
                    .automation_name("OpenRouter API key")
                    .password(self.key.as_str())
                    .is_enabled(idle)
                    .password_reveal_mode(PasswordRevealMode::Hidden)
                    .on_password_changed(
                        ctx.callback(move |value| message(Action::Key(id, value))),
                    ),
            ),
            KeyedView::new(
                "key-controls",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        button(
                            "Save OpenRouter key",
                            idle && !self.key.is_empty(),
                            Action::SaveKey,
                        ),
                        button(
                            "Remove OpenRouter key",
                            idle && self.saved_key,
                            Action::RemoveKey,
                        ),
                    )),
            ),
            line("model-label", "Model ID"),
            KeyedView::new(
                "model",
                TextBox::new()
                    .automation_name("OpenRouter model ID")
                    .text(&self.prefs.model)
                    .placeholder_text("provider/model")
                    .is_enabled(idle)
                    .on_text_changed(ctx.callback(move |value| message(Action::Model(id, value)))),
            ),
            KeyedView::new(
                "catalog",
                button("Refresh model catalog", idle, Action::Catalog),
            ),
        ];
        if !self.models.is_empty() {
            let labels = self
                .models
                .iter()
                .map(|model| display_safe(&format!("{} · {}", model.name, model.id)))
                .collect::<Vec<_>>();
            let models = self
                .models
                .iter()
                .map(|model| model.id.clone())
                .collect::<Vec<_>>();
            let selected = models.iter().position(|model| model == &self.prefs.model);
            fields.push(KeyedView::new(
                "model-catalog",
                ComboBox::new()
                    .automation_name("OpenRouter model catalog")
                    .items_source(labels)
                    .selected_index(selected)
                    .is_enabled(idle)
                    .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                        index
                            .and_then(|index| models.get(index).cloned())
                            .map_or(Message::ControlIdle, |model| {
                                message(Action::Model(id, model))
                            })
                    })),
            ));
        }
        fields.extend([
            KeyedView::new(
                "zdr",
                CheckBox::new()
                    .is_checked(self.prefs.require_zdr)
                    .is_enabled(idle)
                    .on_is_checked_changed(
                        ctx.callback(move |checked| message(Action::Zdr(id, checked))),
                    )
                    .content("Require Zero Data Retention routing"),
            ),
            KeyedView::new(
                "save-settings",
                button(
                    "Save OpenRouter settings",
                    idle && !self.prefs.model.is_empty(),
                    Action::SaveSettings,
                ),
            ),
            line(
                "task",
                match self.task.unwrap_or(Task::Draft) {
                    Task::Explain => "Explain terminal output",
                    Task::Draft => "Describe the Bash task",
                    Task::Triage => "Review Linux performance evidence",
                    Task::TriageComparison => "Review Linux performance changes",
                },
            ),
            KeyedView::new(
                "evidence",
                TextBox::new()
                    .automation_name("OpenRouter request preview")
                    .height(170.0)
                    .accepts_return(true)
                    .text_wrapping(TextWrapping::Wrap)
                    .is_enabled(idle)
                    .text(&self.evidence)
                    .on_text_changed(ctx.callback(move |text| message(Action::Edit(id, text)))),
            ),
            KeyedView::new(
                "review",
                button(
                    "Review OpenRouter request",
                    idle && !self.evidence.trim().is_empty(),
                    Action::Review,
                ),
            ),
        ]);
        if let Some(review) = &self.review {
            fields.extend([
                line(
                    "destination",
                    format!("Destination {}", Review::destination()),
                ),
                line("review-model", format!("Model {}", review.model())),
                line(
                    "review-privacy",
                    if review.require_zdr() {
                        "Zero Data Retention required · Provider data collection denied"
                    } else {
                        "Provider data collection denied"
                    },
                ),
                line(
                    "review-bytes",
                    format!("{} bytes of reviewed input", review.bytes()),
                ),
                KeyedView::new(
                    "send",
                    button(
                        "Send reviewed request",
                        idle && self.saved_key,
                        Action::Send,
                    ),
                ),
            ]);
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.cancel.is_some())
        {
            fields.push(KeyedView::new(
                "cancel",
                button("Stop waiting", true, Action::Cancel),
            ));
        }
        if let Some(status) = &self.status {
            fields.push(line("status", status));
        }
        if let Some(reply) = &self.reply {
            fields.push(line(
                "reply-model",
                reply.model.as_deref().map_or_else(
                    || "OpenRouter response".into(),
                    |model| format!("Response from {}", display_safe(model)),
                ),
            ));
            let reply_text = display_safe(&reply.text);
            fields.push(KeyedView::new(
                "reply",
                TextBlock::new()
                    .automation_id("openrouter-response")
                    .text(reply_text)
                    .is_text_selection_enabled(true)
                    .text_wrapping(TextWrapping::Wrap),
            ));
            if let Some(usage) = &reply.usage {
                let mut pieces = Vec::new();
                for (value, label) in [
                    (usage.prompt_tokens, "input"),
                    (usage.completion_tokens, "output"),
                    (usage.total_tokens, "total"),
                ] {
                    if let Some(tokens) = value {
                        pieces.push(format!("{tokens} {label}"));
                    }
                }
                if !pieces.is_empty() {
                    fields.push(line("usage", format!("Tokens {}", pieces.join(" · "))));
                }
            }
            fields.push(KeyedView::new(
                "copy",
                button("Copy response", idle, Action::Copy),
            ));
        }
        ScrollViewer::new().content(
            StackPanel::new()
                .spacing(8.0)
                .margin(12.0)
                .max_width(760.0)
                .keyed_children(fields),
        )
    }
}

fn run(
    ctx: &ComponentContext<TerminalApp>,
    id: TabId,
    ticket: Arc<()>,
    job: impl FnOnce(&dyn Fn() -> bool) -> anyhow::Result<Outcome> + Send + 'static,
) {
    let rejected = message(Action::Ready(
        id,
        ticket.clone(),
        Err("OpenRouter worker unavailable".into()),
    ));
    ctx.spawn_background_with_rejection(
        move |scope| {
            let result = job(&|| scope.is_cancelled()).map_err(|error| format!("{error:#}"));
            message(Action::Ready(id, ticket, result))
        },
        rejected,
    );
}

fn line(key: &str, text: impl Into<String>) -> KeyedView {
    KeyedView::new(
        key,
        TextBlock::new()
            .text(text.into())
            .text_wrapping(TextWrapping::Wrap),
    )
}

pub(super) fn clip(text: &str, bytes: usize) -> &str {
    &text[..text.floor_char_boundary(bytes.min(text.len()))]
}

fn display_safe(text: &str) -> String {
    text.chars().fold(String::new(), |mut buffer, c| {
        if (c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
            || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            buffer.extend(c.escape_default());
        } else {
            buffer.push(c);
        }
        buffer
    })
}

fn triage_preview(report: &TriageReport, selected: Option<usize>) -> String {
    let mut text = String::from("Linux performance report\n\nChecks and their outcomes\n");
    for (probe, sample) in report.target.probes.iter().zip(&report.collection.samples) {
        let status = serde_json::to_string(&sample.status).unwrap_or_default();
        let exit = sample
            .exit_status
            .map_or_else(|| "unavailable".into(), |code| code.to_string());
        let _ = writeln!(
            text,
            "{}: {} · {} · Exit {exit}",
            clip(&sample.id, 48),
            clip(&probe.command, 180),
            status.trim_matches('"')
        );
    }
    text.push_str("\nLocally observed findings\n");
    for finding in report.findings.iter().take(40) {
        let reference = finding
            .stdout_line
            .map_or_else(String::new, |line| format!(" line {line}"));
        let _ = writeln!(
            text,
            "{}{reference}: {} · {}",
            clip(&finding.sample_id, 48),
            clip(&finding.label, 100),
            clip(&finding.value, 180)
        );
    }
    if let Some(sample) = selected.and_then(|index| report.collection.samples.get(index)) {
        for (stream, output) in [("stdout", &sample.stdout), ("stderr", &sample.stderr)] {
            if output.text.is_empty() {
                continue;
            }
            let _ = writeln!(text, "\n{} {stream} excerpt", clip(&sample.id, 48));
            for (line, row) in clip(&output.text, 1500).lines().take(20).enumerate() {
                let _ = writeln!(text, "{}: {}", line + 1, clip(row, 200));
            }
        }
    }
    text
}
