use super::{Action as HostAction, address, *};
use anyhow::{Context, Result, ensure};
use std::{fmt, io::Write};

const LABEL: &str = "Export host list…";

#[derive(Default)]
pub(super) struct State {
    snapshot: Option<Snapshot>,
    revision: u64,
    feedback: String,
    back_to_review: bool,
}

pub(super) struct Snapshot {
    text: Arc<str>,
    count: usize,
    ipv6: bool,
}

pub enum Action {
    Open(BTreeSet<u64>),
    OpenSaved { name: String, members: Vec<String> },
    Review(u64, Change),
}

pub enum Change {
    Copy,
    Save,
    Back,
}

enum EndpointSource<'a> {
    Pane(u64),
    Profile(&'a str),
}

impl fmt::Display for EndpointSource<'_> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pane(pane) => write!(output, "Pane {pane}"),
            Self::Profile(profile) => write!(output, "SSH profile {profile}"),
        }
    }
}

impl Snapshot {
    pub(super) fn profiles<'a>(
        hosts: impl IntoIterator<Item = (&'a str, &'a SshOptions)>,
    ) -> Result<Self> {
        Self::capture(
            hosts
                .into_iter()
                .map(|(name, options)| (EndpointSource::Profile(name), options)),
        )
    }

    fn capture<'a>(
        hosts: impl IntoIterator<Item = (EndpointSource<'a>, &'a SshOptions)>,
    ) -> Result<Self> {
        let mut text = String::new();
        let mut seen = BTreeSet::new();
        let mut ipv6 = false;
        for (source, options) in hosts {
            let line = address::display(&options.user, &options.host, options.port);
            let reason = || format!("{source} has no portable SSH address.");
            let parsed = SshOptions::from_destination(&line).with_context(reason)?;
            // A hosts file must contain literal addresses even if the consumer uses a shell.
            ensure!(
                parsed.user == options.user
                    && !parsed.user.starts_with('-')
                    && word(&parsed.user, b"_.+-@")
                    && word(&parsed.host, b"_.-:%"),
                "{}",
                reason()
            );
            ipv6 |= parsed.host.contains(':');
            if seen.insert(address::Endpoint::from(&parsed)) {
                text.push_str(&line);
                text.push('\n');
            }
        }
        let count = seen.len();
        ensure!(
            count > 0,
            "Choose at least one SSH address to export a host list."
        );
        Ok(Self {
            text: Arc::from(text),
            count,
            ipv6,
        })
    }
}

fn word(value: &str, extra: &[u8]) -> bool {
    value
        .bytes()
        .all(|ch| ch.is_ascii_alphanumeric() || extra.contains(&ch))
}

pub(super) fn button(id: TabId, selected: &[Target], ctx: &ViewContext<TerminalApp>) -> View {
    let selected = selected
        .iter()
        .map(|target| target.pane)
        .collect::<BTreeSet<_>>();
    Button::new()
        .is_enabled(!selected.is_empty())
        .on_click(ctx.callback(move |_| {
            Message::HostGroup(HostAction::HostListExport(
                id,
                Action::Open(selected.clone()),
            ))
        }))
        .content(LABEL)
}

impl State {
    pub(super) fn is_open(&self) -> bool {
        self.snapshot.is_some()
    }

    pub(super) fn open(&mut self, snapshot: Snapshot, back_to_review: bool) {
        self.revision += 1;
        self.snapshot = Some(snapshot);
        self.feedback.clear();
        self.back_to_review = back_to_review;
    }

    pub(super) fn view(
        &self,
        id: TabId,
        export_busy: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> Option<View> {
        let snapshot = self.snapshot.as_ref()?;
        let revision = self.revision;
        let action = move |change| {
            Message::HostGroup(HostAction::HostListExport(
                id,
                Action::Review(revision, change),
            ))
        };
        let note = if snapshot.ipv6 {
            TextBlock::new()
                .text("w11term can import the IPv6 entries. Polysh host files support hostnames and IPv4 addresses.")
                .text_wrapping(TextWrapping::Wrap)
                .into()
        } else {
            View::empty()
        };
        let feedback = if self.feedback.is_empty() {
            View::empty()
        } else {
            TextBlock::new()
                .text(&self.feedback)
                .text_wrapping(TextWrapping::Wrap)
                .into()
        };
        Some(
            Grid::new()
                .margin(16.0)
                .row_spacing(8.0)
                .rows([GridLength::Auto, GridLength::STAR, GridLength::Auto])
                .children((
                    TextBlock::new()
                        .font_size(24.0)
                        .text("Export SSH host list"),
                    ScrollViewer::new().grid_row(1).content(
                        TextBlock::new()
                            .automation_name("SSH host list preview")
                            .is_text_selection_enabled(true)
                            .text(snapshot.text.as_ref()),
                    ),
                    StackPanel::new().grid_row(2).spacing(6.0).children((
                        note,
                        feedback,
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(8.0)
                            .children((
                                Button::new()
                                    .on_click(ctx.callback(move |_| action(Change::Copy)))
                                    .content("Copy host list"),
                                Button::new()
                                    .is_enabled(!export_busy)
                                    .on_click(ctx.callback(move |_| action(Change::Save)))
                                    .content("Save host list…"),
                                Button::new()
                                    .on_click(ctx.callback(move |_| action(Change::Back)))
                                    .content(if self.back_to_review {
                                        "Back to launch review"
                                    } else {
                                        "Back to SSH group"
                                    }),
                            )),
                    )),
                )),
        )
    }
}

impl TerminalApp {
    pub(super) fn handle_host_list_export(
        &mut self,
        id: TabId,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        match action {
            Action::Open(selected) if self.host_group_command_target() == Some(id) => {
                let snapshot = self.capture_host_list(id, &selected);
                if let Some(page) = self.host_group_mut(id) {
                    match snapshot {
                        Ok(snapshot) => page.host_list_export.open(snapshot, false),
                        Err(error) => page.saved.feedback = format!("{error:#}"),
                    }
                }
            }
            Action::OpenSaved { name, members } if self.host_group_command_target() == Some(id) => {
                let snapshot = self.capture_saved_host_list(id, &name, &members);
                if snapshot.is_err() {
                    self.refresh_profiles();
                }
                if let Some(page) = self.host_group_mut(id) {
                    match snapshot {
                        Ok(snapshot) => page.host_list_export.open(snapshot, false),
                        Err(error) => page.saved.feedback = format!("{error:#}"),
                    }
                }
            }
            Action::Review(revision, change) if self.tabs.active_id() == Some(id) => {
                let export_busy = self.export.pending.is_some();
                let Some(page) = self.host_group_mut(id) else {
                    return;
                };
                let export = &mut page.host_list_export;
                let Some(snapshot) = export
                    .snapshot
                    .as_ref()
                    .filter(|_| export.revision == revision)
                else {
                    return;
                };
                match change {
                    Change::Copy => {
                        export.feedback = match w11term::clipboard::write(&snapshot.text) {
                            Ok(()) if snapshot.count == 1 => "Copied 1 SSH address".into(),
                            Ok(()) => format!("Copied {} SSH addresses", snapshot.count),
                            Err(error) => format!("Could not copy the host list. {error}"),
                        };
                    }
                    Change::Save if !export_busy => {
                        let text = Arc::clone(&snapshot.text);
                        self.begin_text_export(
                            text_export::Subject::SshHostList,
                            move |file| file.write_all(text.as_bytes()),
                            ctx,
                        );
                    }
                    Change::Save => {}
                    Change::Back => export.snapshot = None,
                }
            }
            _ => {}
        }
    }

    fn capture_host_list(&self, id: TabId, selected: &BTreeSet<u64>) -> Result<Snapshot> {
        let Some(Page::HostGroup(page)) = self.tabs.get(id).map(|tab| &tab.page) else {
            anyhow::bail!("The SSH group tab has closed.");
        };
        ensure!(
            page.selected_shown(&self.host_group_targets()) == *selected,
            "The shown selection changed. Try exporting again."
        );
        Snapshot::capture(self.tabs.entries().iter().filter_map(|entry| {
            let Page::Terminal(live) = &entry.value.page else {
                return None;
            };
            let Source::Ssh(options) = &live.source else {
                return None;
            };
            let pane = live.session.pane_id;
            selected
                .contains(&pane)
                .then_some((EndpointSource::Pane(pane), options))
        }))
    }

    fn capture_saved_host_list(
        &self,
        id: TabId,
        name: &str,
        members: &[String],
    ) -> Result<Snapshot> {
        let Some(Page::HostGroup(page)) = self.tabs.get(id).map(|tab| &tab.page) else {
            anyhow::bail!("The SSH group tab has closed.");
        };
        ensure!(
            page.saved.name(&self.catalog) == Some(name),
            "Select the saved group again before exporting."
        );
        let catalog = self.load_expected_host_group(name, members)?;
        let options = catalog.resolve_ssh_host_group_members(members)?;
        Snapshot::profiles(members.iter().map(String::as_str).zip(options))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_keeps_roster_order_users_ports_and_bracketed_ipv6() {
        let host = |line| SshOptions::from_destination(line).unwrap();
        let first = host("ops@Web.EXAMPLE:2222");
        let duplicate = host("ops@web.example:2222");
        let other_user = host("ops+pilot@web.example:2222");
        let other_port = host("ops@web.example:22");
        let v6 = host("u@realm@[fe80::1%12]:2345");
        let snapshot = Snapshot::capture([
            (EndpointSource::Pane(1), &first),
            (EndpointSource::Pane(2), &duplicate),
            (EndpointSource::Pane(3), &other_user),
            (EndpointSource::Pane(4), &v6),
            (EndpointSource::Pane(5), &other_port),
        ])
        .unwrap();
        assert_eq!(snapshot.count, 4);
        assert!(snapshot.ipv6);
        assert_eq!(
            &*snapshot.text,
            "ops@Web.EXAMPLE:2222\nops+pilot@web.example:2222\nu@realm@[fe80::1%12]:2345\nops@web.example:22\n"
        );
    }

    #[test]
    fn list_rejects_an_unrepresentable_endpoint_as_a_whole() {
        let ordinary = SshOptions::from_destination("ops@web:22").unwrap();
        let mut invalid = ordinary.clone();
        for user in ["-flag", "ops\nother", "ops;other", "ops:secret", "ops*"] {
            invalid.user = user.into();
            assert!(
                Snapshot::capture([
                    (EndpointSource::Pane(1), &ordinary),
                    (EndpointSource::Pane(2), &invalid)
                ])
                .is_err(),
                "{user:?}"
            );
        }
        assert!(Snapshot::capture([]).is_err());
    }

    #[test]
    fn a_saved_address_error_names_the_profile() {
        let mut unsafe_profile = SshOptions::from_destination("ops@web:22").unwrap();
        unsafe_profile.user = "ops;other".into();
        let error = Snapshot::profiles([("Web", &unsafe_profile)])
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("SSH profile Web has no portable SSH address."));
    }
}
