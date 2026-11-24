use super::*;
use output_recording::PreparedFile;

#[derive(Default)]
pub(super) struct State {
    plan: Option<Box<Plan>>,
    feedback: Option<String>,
}

struct Plan {
    text: Arc<str>,
    targets: Vec<Target>,
}

pub enum Action {
    Pick(Arc<str>, Vec<Target>),
    Picked(Result<Option<PreparedFile>, String>),
}

impl State {
    pub(super) fn pending(&self) -> bool {
        self.plan.is_some()
    }

    pub(super) fn clear_feedback(&mut self) {
        self.feedback = None;
    }

    pub(super) fn feedback(&self) -> View {
        let text = if self.pending() {
            "Choosing an output file…"
        } else if let Some(feedback) = &self.feedback {
            feedback
        } else {
            return View::empty();
        };
        TextBlock::new()
            .text(text)
            .text_wrapping(TextWrapping::Wrap)
            .into()
    }
}

pub(super) fn button(
    id: TabId,
    text: Arc<str>,
    targets: Vec<Target>,
    enabled: bool,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    Button::new()
        .is_enabled(enabled)
        .on_click(ctx.callback(move |_| {
            Message::HostGroup(super::Action::RecordedSend(
                id,
                Action::Pick(text.clone(), targets.clone()),
            ))
        }))
        .content("Send and record…")
}

impl TerminalApp {
    fn recorded_send_state(&mut self, id: TabId) -> Option<&mut State> {
        let Page::HostGroup(page) = &mut self.tabs.get_mut(id)?.page else {
            return None;
        };
        Some(&mut page.recorded_send)
    }

    pub(super) fn handle_recorded_send(
        &mut self,
        id: TabId,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        match action {
            Action::Pick(text, targets) => self.pick_recorded_send(id, text, targets, ctx),
            Action::Picked(result) => self.finish_recorded_send(id, result),
        }
    }

    fn pick_recorded_send(
        &mut self,
        id: TabId,
        text: Arc<str>,
        targets: Vec<Target>,
        ctx: &ComponentContext<Self>,
    ) {
        if !targets.iter().any(|target| target.state.is_ready())
            || !self.group_send_matches(id, &text, &targets, true)
        {
            return;
        }
        let owner = self.keyboard.hwnd().map(|owner| owner.0 as usize);
        let Some(page) = self
            .host_group_mut(id)
            .filter(|page| !page.recorded_send.pending())
        else {
            return;
        };
        let state = &mut page.recorded_send;
        state.clear_feedback();
        let Some(owner) = owner else {
            state.feedback =
                Some("The window is unavailable. Review the command and try again.".into());
            return;
        };
        let run = page.next_submission + 1;
        state.plan = Some(Box::new(Plan { text, targets }));
        let reply = move |result| {
            Message::HostGroup(super::Action::RecordedSend(id, Action::Picked(result)))
        };
        ctx.spawn_background_with_rejection(
            move |cancel| {
                let prepare = || -> anyhow::Result<Option<PreparedFile>> {
                    if cancel.is_cancelled() {
                        return Ok(None);
                    }
                    let chosen = file_picker::pick(
                        owner,
                        file_picker::Purpose::GroupLiveRecording(format!(
                            "ssh-group-output-{run}.txt"
                        )),
                    )?;
                    if cancel.is_cancelled() {
                        return Ok(None);
                    }
                    chosen.map(|file| PreparedFile::open(file.path)).transpose()
                };
                reply(prepare().map_err(|error| format!("{error:#}")))
            },
            reply(Err("File picker unavailable".into())),
        );
    }

    fn finish_recorded_send(&mut self, id: TabId, result: Result<Option<PreparedFile>, String>) {
        let Some(plan) = self
            .recorded_send_state(id)
            .and_then(|state| state.plan.take())
        else {
            return;
        };
        let Plan { text, targets } = *plan;
        let send = || self.group_send_matches(id, &text, &targets, true);
        let transcript = GroupTranscript::default();
        let recording = match result {
            Ok(None) => return,
            Ok(Some(file)) if send() => file
                .start(
                    &transcript,
                    text.clone(),
                    output_recording::names(targets.iter()),
                )
                .map_err(|error| format!("Could not start the recording · {error}")),
            Ok(Some(_)) => Err(
                "The command, hosts or prompts changed. Review them before sending again.".into(),
            ),
            Err(error) => Err(format!("Could not prepare the recording · {error}")),
        };
        match recording {
            Ok(recording) => self.submit_group_command(id, text, targets, transcript, recording),
            Err(error) => {
                if let Some(state) = self.recorded_send_state(id) {
                    state.feedback = Some(error);
                }
            }
        }
    }
}
