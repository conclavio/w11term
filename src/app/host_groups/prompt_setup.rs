use super::*;
use w11term::options::GroupPromptPattern;

#[derive(Default)]
pub(super) struct State {
    revision: u64,
    draft: Option<Draft>,
}

struct Draft {
    pane: u64,
    name: String,
    pattern: Option<GroupPromptPattern>,
    text: String,
    pending: Option<Pending>,
    feedback: String,
}

struct Pending {
    replacement: Option<GroupPromptPattern>,
    reply: oneshot::Receiver<Result<(), &'static str>>,
}

pub enum Action {
    Open(u64),
    Form(u64, Form),
}

pub enum Form {
    Back,
    Text(String),
    Apply,
    Copy(shell_setup::Setup),
}

impl State {
    pub(super) fn is_open(&self) -> bool {
        self.draft.is_some()
    }

    fn draft_mut(&mut self, revision: u64) -> Option<&mut Draft> {
        self.draft.as_mut().filter(|_| self.revision == revision)
    }

    pub(super) fn poll(&mut self) {
        let Some(draft) = &mut self.draft else { return };
        let Some(pending) = &mut draft.pending else {
            return;
        };
        draft.feedback = match pending.reply.try_recv() {
            Ok(Ok(())) => {
                draft.pattern = pending.replacement.take();
                draft.text = draft
                    .pattern
                    .as_ref()
                    .map_or("", GroupPromptPattern::as_str)
                    .into();
                if draft.pattern.is_some() {
                    "Prompt pattern applied. Wait for a freshly printed matching prompt.".into()
                } else {
                    "Prompt matching disabled for this pane.".into()
                }
            }
            Ok(Err(error)) => error.into(),
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Closed) => "The pane closed before confirming the change.".into(),
        };
        draft.pending = None;
    }

    pub(super) fn view(&self, id: TabId, ctx: &ViewContext<TerminalApp>) -> Option<View> {
        let draft = self.draft.as_ref()?;
        let pane = draft.pane;
        let revision = self.revision;
        let action = move |form| {
            Message::HostGroup(super::Action::PromptSetup(id, Action::Form(revision, form)))
        };
        let editable = draft.pending.is_none();
        let saved = draft
            .pattern
            .as_ref()
            .map_or("", GroupPromptPattern::as_str);
        let integrations = shell_setup::Setup::INTEGRATIONS.into_iter().map(|shell| {
            KeyedView::new(
                shell.label(),
                Button::new()
                    .on_click(ctx.callback(move |_| action(Form::Copy(shell))))
                    .content(shell.label()),
            )
        });
        Some(ScrollViewer::new().content(StackPanel::new().margin(16.0).spacing(10.0).children((
            TextBlock::new().font_size(24.0).text("SSH prompt setup"),
            TextBlock::new().text(&draft.name).text_wrapping(TextWrapping::Wrap),
            TextBlock::new()
                .text("Copy the integration for your shell, then paste and run it at the remote prompt.")
                .text_wrapping(TextWrapping::Wrap),
            StackPanel::new().spacing(5.0).keyed_children(integrations),
            TextBlock::new().text("Match an appliance prompt").font_size(18.0),
            TextBlock::new()
                .text("Use a regex for the complete prompt line up to the cursor. Leave it blank to disable matching. Shell integration markers take precedence.")
                .text_wrapping(TextWrapping::Wrap),
            TextBox::new()
                .automation_name("Pane prompt regex")
                .placeholder_text("rack-[12]>>")
                .is_enabled(editable)
                .text(&draft.text)
                .on_text_changed(ctx.callback(move |text| action(Form::Text(text)))),
            StackPanel::new().spacing(8.0).orientation(Orientation::Horizontal).children((
                Button::new()
                    .style(ButtonStyle::Accent)
                    .is_enabled(editable && draft.text != saved)
                    .on_click(ctx.callback(move |_| action(Form::Apply)))
                    .content("Apply to pane"),
                Button::new()
                    .on_click(ctx.callback(move |_| Message::HostGroup(super::Action::Visit(id, pane))))
                    .content("Open pane"),
                Button::new()
                    .on_click(ctx.callback(move |_| action(Form::Back)))
                    .content("Back to host group"),
            )),
            TextBlock::new().text(&draft.feedback).text_wrapping(TextWrapping::Wrap),
        ))))
    }
}

pub(super) fn button(id: TabId, target: &Target, ctx: &ViewContext<TerminalApp>) -> View {
    if !target.connected || target.state.is_authenticating() {
        return View::empty();
    }
    let pane = target.pane;
    Button::new()
        .automation_name(format!("Prompt setup for pane {pane}"))
        .on_click(ctx.callback(move |_| {
            Message::HostGroup(super::Action::PromptSetup(id, Action::Open(pane)))
        }))
        .content("Prompt setup…")
}

impl TerminalApp {
    pub(super) fn handle_group_prompt_setup(&mut self, id: TabId, action: Action) {
        match action {
            Action::Open(pane) => self.open_group_prompt_setup(id, pane),
            Action::Form(revision, Form::Apply) => self.apply_group_prompt_pattern(id, revision),
            Action::Form(revision, form) => {
                let Some(page) = self.host_group_mut(id) else {
                    return;
                };
                let Some(draft) = page.prompt_setup.draft_mut(revision) else {
                    return;
                };
                match form {
                    Form::Back => page.prompt_setup.draft = None,
                    Form::Text(text) if draft.pending.is_none() && text != draft.text => {
                        draft.text = text;
                        draft.feedback.clear();
                    }
                    Form::Copy(shell) => {
                        draft.feedback = match shell.copy() {
                            Ok(()) => "Copied shell integration.".into(),
                            Err(error) => format!("Could not copy shell integration · {error}"),
                        };
                    }
                    _ => {}
                }
            }
        }
    }

    fn open_group_prompt_setup(&mut self, id: TabId, pane: u64) {
        if self.host_group_command_target() != Some(id) {
            return;
        }
        let roster = self.host_group_targets();
        let Some(page) = self.host_group_mut(id) else {
            return;
        };
        let Some(target) = page.shown(&roster).find(|target| {
            target.pane == pane && target.connected && !target.state.is_authenticating()
        }) else {
            return;
        };
        let Some(Page::Terminal(live)) = self
            .tab_for_pane(pane)
            .and_then(|id| self.tabs.get(id))
            .map(|tab| &tab.page)
        else {
            return;
        };
        let pattern = live.session.events.group_prompt_pattern();
        let text = pattern
            .as_ref()
            .map_or("", GroupPromptPattern::as_str)
            .to_owned();
        if let Some(page) = self.host_group_mut(id) {
            page.prompt_setup.revision += 1;
            page.prompt_setup.draft = Some(Draft {
                pane,
                name: target.name.clone(),
                pattern,
                text,
                pending: None,
                feedback: String::new(),
            });
        }
    }

    fn apply_group_prompt_pattern(&mut self, id: TabId, revision: u64) {
        let Some(draft) = self
            .host_group_mut(id)
            .and_then(|page| page.prompt_setup.draft_mut(revision))
        else {
            return;
        };
        if draft.pending.is_some() {
            return;
        }
        let replacement = match (!draft.text.trim().is_empty())
            .then(|| GroupPromptPattern::try_from(draft.text.clone()))
            .transpose()
        {
            Ok(pattern) => pattern,
            Err(error) => {
                draft.feedback = error.to_string();
                return;
            }
        };
        let pane = draft.pane;
        let expected = draft.pattern.clone();
        let (reply, receive) = oneshot::channel();
        let input = self
            .tab_for_pane(pane)
            .and_then(|id| self.tabs.get(id))
            .and_then(|tab| match &tab.page {
                Page::Terminal(live) if live.session.is_direct_ssh_shell() => {
                    Some(&live.session.input)
                }
                _ => None,
            });
        let request = Command::SetGroupPrompt {
            expected,
            replacement: replacement.clone(),
            reply,
        };
        let queued = input.is_some_and(|input| input.send(request).is_ok());
        if let Some(draft) = self
            .host_group_mut(id)
            .and_then(|page| page.prompt_setup.draft_mut(revision))
        {
            if queued {
                draft.pending = Some(Pending {
                    replacement,
                    reply: receive,
                });
                draft.feedback = "Applying prompt pattern…".into();
            } else {
                draft.feedback = "The SSH pane has closed.".into();
            }
        }
    }
}
