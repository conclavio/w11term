use super::*;
use w11term::ssh::agent::{Key, list_keys};

#[derive(Clone, PartialEq)]
struct Input {
    label: &'static str,
    fingerprint: String,
    on_changed: Callback<String>,
}

#[derive(Default)]
enum Picker {
    #[default]
    Idle,
    Loading,
    Ready(Vec<Key>),
    Failed(String),
}

enum Action {
    Load,
    Loaded(Result<Vec<Key>, String>),
}

pub(super) fn field(label: &'static str, fingerprint: &str, on_changed: Callback<String>) -> View {
    View::component::<Picker>(Input {
        label,
        fingerprint: fingerprint.into(),
        on_changed,
    })
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
            Action::Load if !matches!(self, Self::Loading) => {
                *self = Self::Loading;
                context.spawn_background_with_rejection(
                    |_| {
                        let keys = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .context("could not start the SSH agent reader")
                            .and_then(|runtime| runtime.block_on(list_keys()));
                        Action::Loaded(keys.map_err(|error| format!("{error:#}")))
                    },
                    Action::Loaded(Err("Could not deliver the list of SSH agent keys.".into())),
                );
            }
            Action::Loaded(Ok(keys)) => *self = Self::Ready(keys),
            Action::Loaded(Err(error)) => *self = Self::Failed(error),
            Action::Load => {}
        }
    }

    fn view(&self, input: &Input, context: &mut ViewContext<Self>) -> View {
        let button = match self {
            Self::Idle => "Load agent keys",
            Self::Loading => "Loading agent keys…",
            Self::Ready(_) | Self::Failed(_) => "Refresh agent keys",
        };
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
                            .is_enabled(!matches!(self, Self::Loading))
                            .on_click(context.callback(|_| Action::Load))
                            .content(button),
                    )),
            ),
        ];
        let status = match self {
            Self::Ready(keys) if keys.is_empty() => Some("The SSH agent has no plain keys."),
            Self::Failed(error) => Some(error.as_str()),
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
        if let Self::Ready(keys) = self
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
