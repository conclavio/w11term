use super::*;
use w11term::options::{JumpHost, MAX_JUMPS, validate_host_key_alias};

const TITLE: &str = "SSH jump hosts";

#[derive(Clone, Default)]
pub(super) struct Editor {
    expanded: bool,
    edit: u64,
    pending: Option<Box<Draft>>,
}

#[derive(Clone, Copy)]
pub enum Field {
    Host,
    User,
    Port,
    Identity,
    Certificate,
    AgentFingerprint,
    AgentPipe,
    Fingerprint,
    Alias,
    Timeout,
    Interval,
    Unanswered,
}

pub enum Edit {
    Expand(bool),
    Add,
    Row(usize, JumpHost, RowAction),
    Text(Field, String),
    Auth(usize),
    Apply,
    Cancel,
}

#[derive(Clone, Copy)]
pub enum RowAction {
    Edit,
    Earlier,
    Later,
    Remove,
}

impl RowAction {
    fn label(self) -> &'static str {
        match self {
            Self::Edit => "Edit jump host",
            Self::Earlier => "Move earlier",
            Self::Later => "Move later",
            Self::Remove => "Remove jump host",
        }
    }
}

#[derive(Clone)]
struct Draft {
    index: Option<usize>,
    host: String,
    user: String,
    port: String,
    authentication: usize,
    identity: String,
    certificate: String,
    agent_fingerprint: String,
    agent_pipe: String,
    fingerprint: String,
    alias: String,
    timeout: String,
    interval: String,
    unanswered: String,
    error: Option<String>,
}

impl Draft {
    fn new(index: Option<usize>, existing: Option<&JumpHost>) -> Self {
        let keepalive = existing.map_or_else(SshKeepalive::default, |hop| hop.keepalive);
        let (authentication, identity, certificate, agent_fingerprint) = existing.map_or_else(
            || (0, String::new(), String::new(), String::new()),
            |hop| ssh_auth_fields(&hop.authentication),
        );
        Self {
            index,
            host: existing.map(|hop| hop.host.clone()).unwrap_or_default(),
            user: existing.map(|hop| hop.user.clone()).unwrap_or_default(),
            port: existing.map_or(22, |hop| hop.port).to_string(),
            authentication,
            identity,
            certificate,
            agent_fingerprint,
            agent_pipe: existing
                .and_then(|hop| hop.agent_pipe.as_ref())
                .map(AgentPipe::as_str)
                .unwrap_or_default()
                .into(),
            fingerprint: existing
                .and_then(|hop| hop.host_key.clone())
                .unwrap_or_default(),
            alias: existing
                .and_then(|hop| hop.host_key_alias.clone())
                .unwrap_or_default(),
            timeout: existing
                .and_then(|hop| hop.connect_timeout_seconds)
                .map(|value| value.to_string())
                .unwrap_or_default(),
            interval: keepalive.interval_seconds.to_string(),
            unanswered: keepalive.max_unanswered.to_string(),
            error: None,
        }
    }

    fn jump(&self) -> anyhow::Result<JumpHost> {
        let (host, user, port) = parse_ssh_address(&self.host, &self.user, &self.port)?;
        let alias = self.alias.trim();
        if !alias.is_empty() {
            validate_host_key_alias(alias)?;
        }
        let fingerprint = self.fingerprint.trim();
        let authentication = ssh_authentication(
            self.authentication,
            &self.identity,
            &self.certificate,
            &self.agent_fingerprint,
        )?;
        Ok(JumpHost {
            host,
            user,
            port,
            agent_pipe: ssh_agent_pipe(&authentication, &self.agent_pipe)?,
            authentication,
            host_key: (!fingerprint.is_empty()).then(|| fingerprint.to_owned()),
            host_key_alias: (!alias.is_empty()).then(|| alias.to_ascii_lowercase()),
            connect_timeout_seconds: parse_ssh_connect_timeout(&self.timeout)?,
            keepalive: parse_ssh_keepalive(&self.interval, &self.unanswered)?,
        })
    }

    fn update(&mut self, field: Field, value: String) {
        *match field {
            Field::Host => &mut self.host,
            Field::User => &mut self.user,
            Field::Port => &mut self.port,
            Field::Identity => &mut self.identity,
            Field::Certificate => &mut self.certificate,
            Field::AgentFingerprint => &mut self.agent_fingerprint,
            Field::AgentPipe => &mut self.agent_pipe,
            Field::Fingerprint => &mut self.fingerprint,
            Field::Alias => &mut self.alias,
            Field::Timeout => &mut self.timeout,
            Field::Interval => &mut self.interval,
            Field::Unanswered => &mut self.unanswered,
        } = value;
        self.error = None;
    }

    fn view(
        &self,
        id: TabId,
        edit: u64,
        can_browse: bool,
        context: &ViewContext<TerminalApp>,
    ) -> View {
        let text_box =
            |field, label: &str, value: &str, placeholder: &str| {
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new().text(label),
                    TextBox::new()
                        .automation_name(label)
                        .text(value)
                        .placeholder_text(placeholder)
                        .on_text_changed(context.callback(move |value| {
                            Message::ProfileJump(id, Edit::Text(field, value))
                        })),
                ))
            };
        let action = |label, edit: fn() -> Edit| {
            Button::new()
                .on_click(context.callback(move |_| Message::ProfileJump(id, edit())))
                .content(label)
        };
        let mut fields = vec![
            TextBlock::new()
                .text(self.index.map_or_else(
                    || "Add a jump host".into(),
                    |index| format!("Edit jump host {}", index + 1),
                ))
                .font_weight(windows_reactor::FontWeight::SEMI_BOLD)
                .into(),
            text_box(
                Field::Host,
                "Jump host address",
                &self.host,
                "bastion.example.com",
            ),
            text_box(Field::User, "Jump host user", &self.user, "SSH user"),
            text_box(Field::Port, "Jump host port", &self.port, "22"),
            StackPanel::new().spacing(4.0).children((
                TextBlock::new().text("Jump host authentication"),
                ComboBox::new()
                    .automation_name("Jump host authentication")
                    .items_source(SSH_AUTH_LABELS)
                    .selected_index(self.authentication)
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .on_selection_changed(context.callback(move |value: Option<usize>| {
                        value.map_or(Message::ControlIdle, |index| {
                            Message::ProfileJump(id, Edit::Auth(index))
                        })
                    })),
            )),
        ];
        use identity_picker::{Kind, Target};
        if uses_agent_auth(self.authentication) {
            fields.push(agent_key_picker::pipe_field(
                "Jump host SSH agent pipe (optional)",
                &self.agent_pipe,
                context.callback(move |value| {
                    Message::ProfileJump(id, Edit::Text(Field::AgentPipe, value))
                }),
            ));
        }
        if self.authentication == PRIVATE_KEY_AUTH {
            fields.push(identity_picker::field(
                Target::Jump {
                    tab: id,
                    edit,
                    kind: Kind::PrivateKey,
                },
                &self.identity,
                can_browse,
                context.callback(move |value| {
                    Message::ProfileJump(id, Edit::Text(Field::Identity, value))
                }),
                context,
            ));
        }
        if let Some(kind) = Kind::certificate(self.authentication) {
            fields.push(identity_picker::field(
                Target::Jump {
                    tab: id,
                    edit,
                    kind,
                },
                &self.certificate,
                can_browse,
                context.callback(move |value| {
                    Message::ProfileJump(id, Edit::Text(Field::Certificate, value))
                }),
                context,
            ));
        }
        if self.authentication == SPECIFIC_AGENT_KEY_AUTH {
            fields.push(agent_key_picker::field(
                "Jump host agent key fingerprint",
                &self.agent_fingerprint,
                &self.agent_pipe,
                context.callback(move |value| {
                    Message::ProfileJump(id, Edit::Text(Field::AgentFingerprint, value))
                }),
            ));
        } else if self.authentication == AGENT_CERTIFICATE_AUTH {
            fields.push(text_box(
                Field::AgentFingerprint,
                "Jump host agent key fingerprint (optional)",
                &self.agent_fingerprint,
                "Use the certificate's key when blank",
            ));
        }
        fields.extend([
            text_box(
                Field::Fingerprint,
                "Jump host fingerprint",
                &self.fingerprint,
                "Optional SHA256 pin",
            ),
            text_box(
                Field::Alias,
                "Jump host key alias",
                &self.alias,
                "Optional known_hosts name",
            ),
            text_box(
                Field::Timeout,
                "Jump host connection timeout (seconds)",
                &self.timeout,
                "Default when blank",
            ),
            text_box(
                Field::Interval,
                "Jump host keepalive interval (seconds)",
                &self.interval,
                "0 disables keepalives",
            ),
            text_box(
                Field::Unanswered,
                "Jump host unanswered keepalives allowed",
                &self.unanswered,
                "1 or more",
            ),
            TextBlock::new()
                .text(self.error.as_deref().unwrap_or_default())
                .foreground(Color::rgb(255, 160, 150))
                .text_wrapping(TextWrapping::Wrap)
                .into(),
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    action("Apply jump host", || Edit::Apply),
                    action("Cancel jump host edit", || Edit::Cancel),
                )),
        ]);
        StackPanel::new().spacing(8.0).keyed_children(
            fields
                .into_iter()
                .enumerate()
                .map(|(index, view)| KeyedView::new(index.to_string(), view)),
        )
    }
}

impl Editor {
    pub(super) fn editing(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn identity_fields(
        &mut self,
        edit: u64,
        kind: identity_picker::Kind,
    ) -> Option<(&mut String, &mut Option<String>)> {
        let draft = self.pending.as_mut()?;
        let field = kind.field(&mut draft.identity, &mut draft.certificate);
        (self.edit == edit && kind.available(draft.authentication))
            .then_some((field, &mut draft.error))
    }

    pub(super) fn update(&mut self, hops: &mut Vec<JumpHost>, edit: Edit) -> bool {
        match edit {
            Edit::Expand(expanded) => self.expanded = expanded,
            Edit::Add if !self.editing() && hops.len() < MAX_JUMPS => {
                self.edit += 1;
                self.pending = Some(Box::new(Draft::new(None, None)));
            }
            Edit::Row(index, expected, action)
                if !self.editing() && hops.get(index) == Some(&expected) =>
            {
                match action {
                    RowAction::Edit => {
                        self.edit += 1;
                        self.pending = Some(Box::new(Draft::new(Some(index), Some(&expected))));
                    }
                    RowAction::Earlier if index > 0 => {
                        hops.swap(index, index - 1);
                        return true;
                    }
                    RowAction::Later if index + 1 < hops.len() => {
                        hops.swap(index, index + 1);
                        return true;
                    }
                    RowAction::Remove => {
                        hops.remove(index);
                        return true;
                    }
                    _ => {}
                }
            }
            Edit::Text(field, value) => {
                if let Some(draft) = &mut self.pending {
                    draft.update(field, value);
                }
            }
            Edit::Auth(index) => {
                if let Some(draft) = &mut self.pending
                    && index < SSH_AUTH_LABELS.len()
                    && index != draft.authentication
                {
                    self.edit += 1;
                    draft.authentication = index;
                    draft.error = None;
                }
            }
            Edit::Apply => return self.apply(hops),
            Edit::Cancel => self.pending = None,
            _ => {}
        }
        false
    }

    fn apply(&mut self, hops: &mut Vec<JumpHost>) -> bool {
        let Some(draft) = &mut self.pending else {
            return false;
        };
        let hop = match draft.jump() {
            Ok(hop) => hop,
            Err(error) => {
                draft.error = Some(format!("{error:#}"));
                return false;
            }
        };
        let index = self.pending.take().and_then(|draft| draft.index);
        if let Some(index) = index {
            let changed = hops[index] != hop;
            hops[index] = hop;
            return changed;
        }
        hops.push(hop);
        true
    }

    pub(super) fn view(
        &self,
        id: TabId,
        hops: &[JumpHost],
        can_browse: bool,
        context: &ViewContext<TerminalApp>,
    ) -> View {
        let rows = hops
            .iter()
            .enumerate()
            .map(|(index, hop)| {
                let endpoint = config_import::endpoint(
                    &hop.host,
                    hop.port,
                    &hop.user,
                    &hop.authentication,
                    hop.host_key_alias.as_deref(),
                    hop.agent_pipe.as_ref(),
                );
                let name = format!("Jump host {}", index + 1);
                KeyedView::new(
                    index.to_string(),
                    Grid::new()
                        .columns([GridLength::STAR, GridLength::Auto])
                        .column_spacing(8.0)
                        .children((
                            TextBlock::new()
                                .text(format!("{name} · {endpoint}"))
                                .vertical_alignment(VerticalAlignment::Center)
                                .text_wrapping(TextWrapping::Wrap),
                            View::component::<JumpActions>(ActionsInput {
                                sender: context.sender(),
                                row: ActionRow {
                                    id,
                                    index,
                                    hop: hop.clone(),
                                    enabled: !self.editing(),
                                    can_move_later: index + 1 < hops.len(),
                                },
                            }),
                        )),
                )
            })
            .collect::<Vec<_>>();
        let actions = if let Some(draft) = &self.pending {
            draft.view(id, self.edit, can_browse, context)
        } else if hops.len() < MAX_JUMPS {
            Button::new()
                .horizontal_alignment(HorizontalAlignment::Left)
                .on_click(context.callback(move |_| Message::ProfileJump(id, Edit::Add)))
                .content("Add jump host")
        } else {
            View::empty()
        };
        let suffix = if self.editing() {
            " · edit pending"
        } else {
            ""
        };
        Expander::new()
            .automation_name(TITLE)
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .is_expanded(self.expanded)
            .on_is_expanded_changed(
                context.callback(move |expanded| Message::ProfileJump(id, Edit::Expand(expanded))),
            )
            .slots([
                SlotView::new(
                    ExpanderSlot::Header,
                    TextBlock::new().text(format!("{TITLE} ({}{suffix})", hops.len())),
                ),
                SlotView::new(
                    ExpanderSlot::Content,
                    StackPanel::new().spacing(8.0).children((
                        TextBlock::new()
                            .text("SSH connects through these hosts in order before reaching the destination.")
                            .text_wrapping(TextWrapping::Wrap),
                        StackPanel::new().spacing(4.0).keyed_children(rows),
                        actions,
                    )),
                ),
            ])
    }
}

struct JumpActions(LocalSender<Message>);

#[derive(Clone)]
struct ActionsInput {
    sender: LocalSender<Message>,
    row: ActionRow,
}

impl PartialEq for ActionsInput {
    fn eq(&self, other: &Self) -> bool {
        self.row == other.row
    }
}

#[derive(Clone, PartialEq)]
struct ActionRow {
    id: TabId,
    index: usize,
    hop: JumpHost,
    enabled: bool,
    can_move_later: bool,
}

// Reactor replaces the native menu when its view is republished. Keep a menu
// open while unrelated terminal updates redraw the window.
impl Component for JumpActions {
    type Input = ActionsInput;
    type Message = ();

    fn create(input: &ActionsInput, _: &ComponentContext<Self>) -> Self {
        Self(input.sender.clone())
    }

    fn view(&self, input: &ActionsInput, _: &mut ViewContext<Self>) -> View {
        let row = input.row.clone();
        let actions = [
            ("edit", RowAction::Edit, true),
            ("earlier", RowAction::Earlier, row.index > 0),
            ("later", RowAction::Later, row.can_move_later),
            ("remove", RowAction::Remove, true),
        ];
        Button::new()
            .grid_column(1)
            .automation_name(format!("Jump host {} actions", row.index + 1))
            .is_enabled(row.enabled)
            .content("Actions")
            .menu(Menu::new(
                actions
                    .map(|(key, action, enabled)| command_menu_item(key, action.label(), enabled)),
                self.0.callback(move |label: String| {
                    actions
                        .iter()
                        .find(|(_, action, enabled)| *enabled && action.label() == label)
                        .map_or(Message::ControlIdle, |(_, action, _)| {
                            Message::ProfileJump(
                                row.id,
                                Edit::Row(row.index, row.hop.clone(), *action),
                            )
                        })
                }),
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> JumpHost {
        JumpHost {
            host: "2001:db8::9".into(),
            user: "admin".into(),
            port: 2222,
            authentication: Authentication::identity("keys/bastion", None),
            agent_pipe: None,
            host_key: Some("SHA256:fixture".into()),
            host_key_alias: Some("bastion.internal".into()),
            connect_timeout_seconds: std::num::NonZeroU32::new(15),
            keepalive: SshKeepalive {
                interval_seconds: 0,
                max_unanswered: std::num::NonZeroU32::new(6).unwrap(),
            },
        }
    }

    #[test]
    fn editing_preserves_existing_authentication_trust_and_timeouts() {
        let hop = sample();
        let mut draft = Draft::new(Some(0), Some(&hop));
        assert_eq!(draft.jump().unwrap(), hop);
        draft.host = " [2001:db8::9] ".into();
        draft.alias = " BASTION.INTERNAL ".into();
        assert_eq!(draft.jump().unwrap(), hop);
        draft.alias = "*.example.com".into();
        assert!(draft.jump().is_err());
        draft.alias.clear();
        draft.identity.clear();
        assert!(draft.jump().is_err());
    }

    #[test]
    fn identity_picker_is_scoped_to_one_edit_and_authentication_choice() {
        let mut editor = Editor::default();
        let mut hops = vec![sample(), sample()];
        let first = hops[0].clone();
        editor.update(&mut hops, Edit::Row(0, first, RowAction::Edit));
        let cancelled_edit = editor.edit;
        assert!(
            editor
                .identity_fields(cancelled_edit, identity_picker::Kind::PrivateKey)
                .is_some()
        );
        editor.update(&mut hops, Edit::Cancel);
        assert!(
            editor
                .identity_fields(cancelled_edit, identity_picker::Kind::PrivateKey)
                .is_none()
        );

        let second = hops[1].clone();
        editor.update(&mut hops, Edit::Row(1, second, RowAction::Edit));
        let former_auth = editor.edit;
        assert!(
            editor
                .identity_fields(cancelled_edit, identity_picker::Kind::PrivateKey)
                .is_none()
        );
        editor.update(&mut hops, Edit::Auth(3));
        assert!(
            editor
                .identity_fields(editor.edit, identity_picker::Kind::PrivateKey)
                .is_none()
        );
        editor.update(&mut hops, Edit::Auth(PRIVATE_KEY_AUTH));
        assert!(
            editor
                .identity_fields(former_auth, identity_picker::Kind::PrivateKey)
                .is_none()
        );
        let (path, error) = editor
            .identity_fields(editor.edit, identity_picker::Kind::PrivateKey)
            .unwrap();
        assert_eq!(path, "keys/bastion");
        *path = "other key".into();
        assert!(error.is_none());
        assert!(editor.update(&mut hops, Edit::Apply));
        assert_eq!(hops[0], sample());
        assert_eq!(
            hops[1].authentication,
            Authentication::identity("other key", None)
        );
    }

    #[test]
    fn route_edits_validate_before_staging_and_respect_the_hop_limit() {
        let mut editor = Editor::default();
        let mut hops = vec![sample(); MAX_JUMPS];
        editor.update(&mut hops, Edit::Add);
        assert!(!editor.editing());
        let first = hops[0].clone();
        assert!(editor.update(&mut hops, Edit::Row(0, first, RowAction::Remove)));
        editor.update(&mut hops, Edit::Add);
        assert!(editor.editing());
        assert!(!editor.update(&mut hops, Edit::Apply));
        assert_eq!(hops.len(), MAX_JUMPS - 1);
        assert!(editor.pending.as_ref().unwrap().error.is_some());

        editor.update(
            &mut hops,
            Edit::Text(Field::Host, "bastion.example.com".into()),
        );
        editor.update(&mut hops, Edit::Text(Field::User, "operator".into()));
        assert!(editor.update(&mut hops, Edit::Apply));
        let index = MAX_JUMPS - 1;
        let added = hops[index].clone();
        assert!(editor.update(
            &mut hops,
            Edit::Row(index, added.clone(), RowAction::Earlier)
        ));
        assert!(!editor.update(
            &mut hops,
            Edit::Row(index, added.clone(), RowAction::Remove)
        ));
        assert_eq!(hops[index - 1], added);
        editor.update(
            &mut hops,
            Edit::Row(index - 1, added.clone(), RowAction::Edit),
        );
        editor.update(
            &mut hops,
            Edit::Text(Field::Host, "discard.example.com".into()),
        );
        editor.update(&mut hops, Edit::Cancel);
        assert_eq!(hops[index - 1], added);
    }
}
