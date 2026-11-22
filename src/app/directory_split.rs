use super::*;
use w11term::{options::PlayerOptions, ssh::ConnectionShare, workspace::SshStartup};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Origin {
    pane: u64,
    generation: u64,
    directory: String,
    listing_revision: Option<u64>,
}

impl Origin {
    pub(super) fn listing(pane: u64, generation: u64, directory: &str, revision: u64) -> Self {
        Self {
            pane,
            generation,
            directory: directory.into(),
            listing_revision: Some(revision),
        }
    }
}

pub(super) fn axis(shortcut: Shortcut) -> Option<Axis> {
    match shortcut {
        Shortcut::SplitRightInCurrentDirectory => Some(Axis::Columns),
        Shortcut::SplitBelowInCurrentDirectory => Some(Axis::Rows),
        _ => None,
    }
}

pub(super) fn message(axis: Axis, target: Option<&Origin>) -> Message {
    target.cloned().map_or(Message::ControlIdle, |origin| {
        Message::SplitInDirectory(axis, origin)
    })
}

fn child_options(connection: &SshOptions, pane: &SshOptions, player: PlayerOptions) -> SshOptions {
    // A shared transport can serve PTYs opened with different TERM and history settings.
    SshOptions {
        terminal_type: pane.terminal_type.clone(),
        scrollback_lines: pane.scrollback_lines,
        player,
        ..connection.clone()
    }
}

impl LiveTab {
    fn directory_split(&self) -> Option<(Origin, ConnectionShare)> {
        if !self.session.is_direct_ssh_shell() {
            return None;
        }
        let frame = self.session.frames.load();
        let report = frame.reported_directory.as_deref()?;
        let generation = frame.connection_generation;
        let connection = self.session.ssh_connection_at(generation)?;
        let directory = file_drawer::shell_directory(Some(report), &connection.options().host)?;
        Some((
            Origin {
                pane: self.session.pane_id,
                generation,
                directory,
                listing_revision: None,
            },
            connection,
        ))
    }
}

impl TerminalApp {
    pub(super) fn directory_split_origin(&self) -> Option<Origin> {
        let Page::Terminal(tab) = &self.tabs.active()?.page else {
            return None;
        };
        tab.directory_split().map(|(origin, _)| origin)
    }

    pub(super) fn split_in_directory(
        &mut self,
        axis: Axis,
        target: Origin,
        ctx: &ComponentContext<Self>,
    ) {
        let Some(id) = self.tabs.active_id() else {
            return;
        };
        let Some(AppTab {
            label,
            page: Page::Terminal(tab),
        }) = self.tabs.get(id)
        else {
            return;
        };
        let resolved = if target.listing_revision.is_some() {
            if !self.drawer_shell_eligible(tab, target.generation) {
                return;
            }
            tab.files
                .shell_origin(tab.session.pane_id)
                .zip(tab.session.ssh_connection_at(target.generation))
        } else {
            tab.directory_split()
        };
        let Some((current, connection)) = resolved.filter(|(current, _)| *current == target) else {
            return;
        };
        let Source::Ssh(pane) = &tab.source else {
            return;
        };
        let options = child_options(
            connection.options(),
            pane,
            tab.session.player.read().unwrap().clone(),
        );
        let Ok(startup) = SshStartup::new(&options, current.directory, String::new()) else {
            return;
        };
        // A profile may have been edited to point elsewhere since this login.
        // Keep its name for workspace restore only if it still identifies this connection.
        let profile = tab
            .profile_name
            .as_ref()
            .filter(|name| {
                matches!(self.catalog.source(name), Some(Source::Ssh(ssh))
                if !ssh.durable && connection.matches(&ssh, Some(&tab.session.prompts)))
            })
            .cloned();
        let session = Session::start_ssh_with_startup(
            options.clone(),
            TerminalSize::default(),
            self.appearance.read().unwrap().interaction.osc52,
            startup.clone(),
            Some(connection),
        );
        let source = Source::Ssh(options);
        let label = if tab.profile_name.is_some() && profile.is_none() {
            source_label(&source)
        } else {
            label.clone()
        };
        let mut child = live_tab_for_session(label, source, session).with_profile(profile);
        if let Page::Terminal(live) = &mut child.page {
            live.startup = Some(Box::new(startup));
        }
        self.insert_split(id, child, axis, ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_inherits_the_originating_ptys_settings_on_a_shared_transport() {
        let connection: SshOptions = serde_json::from_value(serde_json::json!({
            "host":"example.test", "user":"login", "auth":"identity", "identity":"active-key"
        }))
        .unwrap();
        let pane: SshOptions = serde_json::from_value(serde_json::json!({
            "host":"example.test", "user":"login", "auth":"password",
            "terminal_type":"xterm-sixel", "scrollback_lines":18
        }))
        .unwrap();
        let player = PlayerOptions {
            mute: true,
            ..Default::default()
        };
        let child = child_options(&connection, &pane, player.clone());
        assert_eq!(child.authentication, connection.authentication);
        assert_eq!(child.terminal_type, pane.terminal_type);
        assert_eq!(child.scrollback_lines, pane.scrollback_lines);
        assert_eq!(child.player, player);
    }

    #[test]
    fn queued_directory_actions_keep_the_parent_and_the_directory_that_was_offered() {
        let origin = Origin {
            pane: 14,
            generation: 3,
            directory: "/srv/client v2".into(),
            listing_revision: None,
        };
        for (label, expected) in [
            ("Split right in current directory    F7", Axis::Columns),
            ("Split below in current directory", Axis::Rows),
        ] {
            let Message::Shortcut(key) = tab_menu_message(label.into()) else {
                panic!("unrecognized directory action")
            };
            let direction = axis(key).unwrap();
            assert_eq!(direction, expected);
            assert!(matches!(message(direction, None), Message::ControlIdle));
            assert!(
                matches!(message(direction, Some(&origin)), Message::SplitInDirectory(a, target) if a == expected && target == origin)
            );
        }
    }
}
