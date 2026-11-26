use super::*;
use w11term::ssh::agent::{Key, list_keys};

#[derive(Clone, PartialEq)]
struct Input {
    label: &'static str,
    fingerprint: String,
    pipe: String,
    on_changed: Callback<String>,
}

#[derive(Default)]
struct Picker {
    request: u64,
    pipe: String,
    state: State,
}

#[derive(Default)]
enum State {
    #[default]
    Idle,
    Loading,
    Ready(Vec<Key>),
    Failed(String),
}

enum Action {
    Load(String),
    Loaded(u64, Result<Vec<Key>, String>),
}

pub(super) fn field(
    label: &'static str,
    fingerprint: &str,
    pipe: &str,
    on_changed: Callback<String>,
) -> View {
    View::component::<Picker>(Input {
        label,
        fingerprint: fingerprint.into(),
        pipe: pipe.trim().into(),
        on_changed,
    })
}

pub(super) fn pipe_field(label: &str, pipe: &str, on_changed: Callback<String>) -> View {
    StackPanel::new().spacing(5.0).children((
        TextBlock::new().text(label),
        TextBox::new()
            .automation_name(label)
            .placeholder_text(r"\\.\pipe\name or the default when blank")
            .text(pipe)
            .on_text_changed(on_changed),
    ))
}

pub(super) fn optional_pin(label: &str, fingerprint: &str, on_changed: Callback<String>) -> View {
    StackPanel::new().spacing(5.0).children((
        TextBlock::new().text(label),
        TextBox::new()
            .automation_name(label)
            .placeholder_text("Use the certificate's key when blank")
            .text(fingerprint)
            .on_text_changed(on_changed),
    ))
}

impl Component for Picker {
    type Input = Input;
    type Message = Action;

    fn create(_: &Input, _: &ComponentContext<Self>) -> Self {
        Self::default()
    }

    fn update(&mut self, action: Action, context: &ComponentContext<Self>) {
        match action {
            Action::Load(pipe) if pipe != self.pipe || !matches!(self.state, State::Loading) => {
                self.pipe.clone_from(&pipe);
                self.state = State::Loading;
                self.request += 1;
                let request = self.request;
                context.spawn_background_with_rejection(
                    move |_| {
                        let keys = AgentPipe::optional(&pipe).and_then(|source| {
                            tokio::runtime::Builder::new_current_thread()
                                .enable_all()
                                .build()
                                .context("could not start the SSH agent reader")
                                .and_then(|runtime| runtime.block_on(list_keys(source.as_ref())))
                        });
                        Action::Loaded(request, keys.map_err(|error| format!("{error:#}")))
                    },
                    Action::Loaded(
                        request,
                        Err("Could not deliver the list of SSH agent keys.".into()),
                    ),
                );
            }
            Action::Loaded(request, results) if request == self.request => {
                self.state = match results {
                    Ok(keys) => State::Ready(keys),
                    Err(error) => State::Failed(error),
                };
            }
            _ => {}
        }
    }

    fn view(&self, input: &Input, context: &mut ViewContext<Self>) -> View {
        let state = if self.pipe == input.pipe {
            &self.state
        } else {
            &State::Idle
        };
        let button = match state {
            State::Idle => "Load agent keys",
            State::Loading => "Loading agent keys…",
            State::Ready(_) | State::Failed(_) => "Refresh agent keys",
        };
        let pipe = input.pipe.clone();
        let mut fields = vec![
            KeyedView::new("label", TextBlock::new().text(input.label)),
            KeyedView::new(
                "editor",
                Grid::new()
                    .columns([GridLength::STAR, GridLength::Auto])
                    .column_spacing(8.0)
                    .children((
                        TextBox::new()
                            .automation_name(input.label)
                            .placeholder_text("SHA256:…")
                            .text(&input.fingerprint)
                            .on_text_changed(input.on_changed.clone()),
                        Button::new()
                            .grid_column(1)
                            .is_enabled(!matches!(state, State::Loading))
                            .on_click(context.callback(move |_| Action::Load(pipe.clone())))
                            .content(button),
                    )),
            ),
        ];
        let status = match state {
            State::Ready(keys) if keys.is_empty() => Some("The SSH agent has no plain keys."),
            State::Failed(error) => Some(error.as_str()),
            _ => None,
        };
        if let Some(status) = status {
            fields.push(KeyedView::new(
                "status",
                TextBlock::new()
                    .text(status)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        if let State::Ready(keys) = state
            && !keys.is_empty()
        {
            let fingerprints = keys
                .iter()
                .map(|key| key.fingerprint.as_str().to_owned())
                .collect::<Vec<_>>();
            let selected = fingerprints
                .iter()
                .position(|key| key == &input.fingerprint);
            let on_changed = input.on_changed.clone();
            fields.push(KeyedView::new(
                "keys",
                ComboBox::new()
                    .automation_name("Keys in the SSH agent")
                    .placeholder_text("Choose a key")
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .items_source(keys.iter().map(key_label).collect::<Vec<_>>())
                    .selected_index(selected)
                    .on_selection_changed(Callback::new(move |index: Option<usize>| {
                        if let Some(key) = index.and_then(|index| fingerprints.get(index)) {
                            let _ = on_changed.call(key.clone());
                        }
                    })),
            ));
        }
        StackPanel::new().spacing(5.0).keyed_children(fields)
    }
}

fn key_label(key: &Key) -> String {
    let name = if key.comment.is_empty() {
        &key.algorithm
    } else {
        &key.comment
    };
    format!("{name} · {}", key.fingerprint.as_str())
}
