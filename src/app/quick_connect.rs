use super::*;

pub(super) const LABEL: &str = "Quick connect…";

#[derive(Default)]
pub(super) struct Draft {
    destination: String,
    pub(super) authentication: usize,
    pub(super) identity: String,
    pub(super) error: Option<String>,
    input: ElementRef<TextBox>,
}

pub enum Action {
    Focus(TabId, u8),
    Address(TabId, String),
    Authentication(TabId, Option<usize>),
    Identity(TabId, String),
    Connect(TabId),
}

impl TerminalApp {
    pub(super) fn open_quick_connect(&mut self, context: &ComponentContext<Self>) {
        let existing = self
            .tabs
            .entries()
            .iter()
            .find(|entry| matches!(entry.value.page, Page::QuickConnect(_)))
            .map(|entry| entry.id);
        let id = match existing {
            Some(id) => {
                self.tabs.select(id);
                id
            }
            None => {
                let id = self.tabs.insert(AppTab {
                    label: "Quick connect".into(),
                    page: Page::QuickConnect(Draft::default()),
                });
                self.sync_selection(context);
                id
            }
        };
        self.activate(context);
        // The new-tab flyout restores focus to its owner when it closes.
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_millis(200));
            Message::QuickConnect(Action::Focus(id, 0))
        });
    }

    fn focus_quick_connect(&self, id: TabId, attempt: u8, context: &ComponentContext<Self>) {
        if self.tabs.active_id() != Some(id) {
            return;
        }
        let Some(Page::QuickConnect(draft)) = self.tabs.get(id).map(|tab| &tab.page) else {
            return;
        };
        let sender = context.sender();
        let queued = draft.input.request_focus_result(move |result| {
            if !result.unwrap_or(false) && attempt < 4 {
                sender.send(Message::QuickConnect(Action::Focus(id, attempt + 1)));
            }
        });
        if !queued && attempt < 4 {
            context.spawn_background(move |_| {
                std::thread::sleep(Duration::from_millis(20));
                Message::QuickConnect(Action::Focus(id, attempt + 1))
            });
        }
    }

    pub(super) fn edit_quick_connect(&mut self, action: Action, context: &ComponentContext<Self>) {
        let id = match &action {
            Action::Focus(id, attempt) => return self.focus_quick_connect(*id, *attempt, context),
            Action::Address(id, _)
            | Action::Authentication(id, _)
            | Action::Identity(id, _)
            | Action::Connect(id) => *id,
        };
        let active = self.tabs.active_id() == Some(id);
        let Some(tab) = self.tabs.get_mut(id) else {
            return;
        };
        let Page::QuickConnect(draft) = &mut tab.page else {
            return;
        };
        match action {
            Action::Address(_, value) if draft.destination != value => {
                draft.destination = value;
                draft.error = None;
            }
            Action::Authentication(_, Some(index)) if draft.authentication != index => {
                draft.authentication = index;
                draft.error = None;
            }
            Action::Identity(_, value) if draft.identity != value => {
                draft.identity = value;
                draft.error = None;
            }
            Action::Connect(_) if active => {
                let result =
                    SshOptions::from_destination(&draft.destination).and_then(|mut options| {
                        options.authentication =
                            ssh_authentication(draft.authentication, &draft.identity)?;
                        options.prepare_profile_connection(self.store.path())?;
                        Ok(options)
                    });
                match result {
                    Ok(options) => {
                        let policy = self.appearance.read().unwrap().pane_preferences();
                        let source = Source::Ssh(options);
                        *tab = live_tab(source_label(&source), source, policy);
                        self.sync_selection(context);
                        self.activate(context);
                    }
                    Err(error) => draft.error = Some(format!("{error:#}")),
                }
            }
            _ => {}
        }
    }
}

pub(super) fn view(
    id: TabId,
    draft: &Draft,
    can_browse: bool,
    context: &ViewContext<TerminalApp>,
) -> View {
    let mut fields =
        vec![
            KeyedView::new(
                "title",
                TextBlock::new().text("Quick connect").font_size(22.0),
            ),
            KeyedView::new(
                "destination",
                StackPanel::new().spacing(5.0).children((
                    TextBlock::new().text("SSH destination"),
                    TextBox::new()
                        .automation_name("SSH destination")
                        .element_ref(&draft.input)
                        .placeholder_text("user@host:port or ssh://user@host:port")
                        .text(&draft.destination)
                        .on_text_changed(context.callback(move |value| {
                            Message::QuickConnect(Action::Address(id, value))
                        })),
                )),
            ),
            KeyedView::new(
                "authentication",
                StackPanel::new().spacing(5.0).children((
                    TextBlock::new().text("Authentication"),
                    ComboBox::new()
                        .automation_name("Authentication")
                        .items_source(SSH_AUTH_LABELS)
                        .selected_index(draft.authentication)
                        .horizontal_alignment(HorizontalAlignment::Stretch)
                        .on_selection_changed(context.callback(move |value| {
                            Message::QuickConnect(Action::Authentication(id, value))
                        })),
                )),
            ),
        ];
    if draft.authentication == PRIVATE_KEY_AUTH {
        fields.push(KeyedView::new(
            "identity",
            identity_picker::field(
                identity_picker::Target::Endpoint(id),
                &draft.identity,
                can_browse,
                context.callback(move |value| Message::QuickConnect(Action::Identity(id, value))),
                context,
            ),
        ));
    }
    if let Some(error) = &draft.error {
        fields.push(KeyedView::new(
            "error",
            TextBlock::new()
                .text(error)
                .text_wrapping(TextWrapping::Wrap)
                .foreground(Color::rgb(255, 160, 150)),
        ));
    }
    fields.push(KeyedView::new(
        "actions",
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .children((
                Button::new()
                    .style(ButtonStyle::Accent)
                    .is_enabled(!draft.destination.trim().is_empty())
                    .on_click(context.callback(move |_| Message::QuickConnect(Action::Connect(id))))
                    .content("Connect"),
                Button::new()
                    .on_click(context.callback(move |_| Message::CloseTab(id.to_string())))
                    .content("Cancel"),
            )),
    ));
    ScrollViewer::new().content(
        StackPanel::new()
            .margin(20.0)
            .spacing(14.0)
            .max_width(520.0)
            .horizontal_alignment(HorizontalAlignment::Left)
            .keyed_children(fields),
    )
}
