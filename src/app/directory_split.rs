use super::*;
use std::path::PathBuf;
use w11term::{options::PlayerOptions, ssh::ConnectionShare, workspace::SshStartup};

#[derive(Clone, Debug, Eq, PartialEq)]
enum Directory {
    Ssh(String),
    Local(PathBuf),
    Wsl(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Origin {
    pane: u64,
    generation: u64,
    directory: Directory,
    listing_revision: Option<u64>,
}

impl Origin {
    pub(super) fn listing(pane: u64, generation: u64, directory: &str, revision: u64) -> Self {
        Self {
            pane,
            generation,
            directory: Directory::Ssh(directory.into()),
            listing_revision: Some(revision),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Destination {
    Tab,
    Split(Axis),
}

pub(super) fn destination(shortcut: Shortcut) -> Option<Destination> {
    match shortcut {
        Shortcut::NewTabInCurrentDirectory => Some(Destination::Tab),
        Shortcut::SplitRightInCurrentDirectory => Some(Destination::Split(Axis::Columns)),
        Shortcut::SplitBelowInCurrentDirectory => Some(Destination::Split(Axis::Rows)),
        _ => None,
    }
}

pub(super) fn message(destination: Destination, target: Option<&Origin>) -> Message {
    target.cloned().map_or(Message::ControlIdle, |origin| {
        Message::OpenInDirectory(destination, origin)
    })
}

fn child_options(
    connection: &SshOptions,
    pane: &SshOptions,
    color_scheme: ColorScheme,
    player: PlayerOptions,
) -> SshOptions {
    // PTYs sharing a transport keep their own terminal and presentation settings.
    SshOptions {
        terminal_type: pane.terminal_type.clone(),
        scrollback_lines: pane.scrollback_lines,
        color_scheme,
        local_forwards: pane.local_forwards.clone(),
        player,
        ..connection.clone()
    }
}

fn local_directory(report: &str, computer: Option<&str>) -> Option<PathBuf> {
    // Reuse the SSH file-URI decoder for strict percent escapes and authority
    // matching. Build only a Windows drive-letter path, never a UNC/device path.
    let authority = file_authority(report)?;
    let host = if authority.is_empty() || authority.eq_ignore_ascii_case("localhost") {
        "localhost"
    } else {
        computer.filter(|name| !name.is_empty())?
    };
    let path = file_drawer::shell_directory(Some(report), host)?;
    let (drive, tail) = path.strip_prefix('/')?.split_once('/')?;
    if !matches!(drive.as_bytes(), [letter, b':'] if letter.is_ascii_alphabetic()) {
        return None;
    }
    let mut cwd = PathBuf::from(format!("{drive}\\"));
    if !tail.is_empty() {
        for name in tail.strip_suffix('/').unwrap_or(tail).split('/') {
            w11term::sftp::validate_download_name(name).ok()?;
            cwd.push(name);
        }
    }
    Some(cwd)
}

pub(super) fn reported_local_directory(report: Option<&str>) -> Option<PathBuf> {
    local_directory(report?, std::env::var("COMPUTERNAME").ok().as_deref())
}

fn file_authority(report: &str) -> Option<&str> {
    Some(report.split_once("://")?.1.split_once('/')?.0)
}

fn wsl_directory(report: &str, distribution: &str, computer: Option<&str>) -> Option<String> {
    let authority = file_authority(report)?;
    if authority.eq_ignore_ascii_case("wsl.localhost") || authority.eq_ignore_ascii_case("wsl$") {
        let path = file_drawer::shell_directory(Some(report), authority)?;
        let path = path.strip_prefix('/')?;
        let (reported_distribution, suffix) = path.split_once('/').unwrap_or((path, ""));
        if reported_distribution.eq_ignore_ascii_case(distribution) && !suffix.starts_with('/') {
            return Some(format!("/{suffix}"));
        }
        return None;
    }
    if let Some(windows) = local_directory(report, computer) {
        // WSL accepts an absolute Windows drive path as its --cd argument.
        return windows.into_os_string().into_string().ok();
    }
    let host = if authority.eq_ignore_ascii_case("localhost") {
        "localhost"
    } else {
        computer.unwrap_or_default()
    };
    let path = file_drawer::shell_directory(Some(report), host)?;
    // A malformed Windows drive report cannot fall back to a Linux path.
    let first = path.strip_prefix('/')?.split('/').next()?;
    if matches!(first.as_bytes(), [drive, b':', ..] if drive.is_ascii_alphabetic()) {
        return None;
    }
    Some(path)
}

impl LiveTab {
    fn ssh_directory_split(&self) -> Option<(Origin, ConnectionShare)> {
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
                directory: Directory::Ssh(directory),
                listing_revision: None,
            },
            connection,
        ))
    }

    fn local_directory_split(&self) -> Option<Origin> {
        let Source::Local(shell) = &self.source else {
            return None;
        };
        let frame = self.session.frames.load();
        if !self.can_report_split_directory(frame.connection_generation) {
            return None;
        }
        let report = frame.reported_directory.as_deref()?;
        let computer = std::env::var("COMPUTERNAME").ok();
        let directory = match shell.wsl_distribution() {
            Some(distribution) => {
                Directory::Wsl(wsl_directory(report, distribution, computer.as_deref())?)
            }
            None => Directory::Local(local_directory(report, computer.as_deref())?),
        };
        Some(Origin {
            pane: self.session.pane_id,
            generation: frame.connection_generation,
            directory,
            listing_revision: None,
        })
    }

    pub(super) fn can_report_split_directory(&self, generation: u64) -> bool {
        match &self.source {
            Source::Local(shell) => {
                (LocalOptions::supports_powershell_integration(&shell.executable, &shell.args)
                    || shell.wsl_distribution().is_some())
                    && self.session.events.shell_generation() == Some(generation)
            }
            _ => self.session.ssh_connection_at(generation).is_some(),
        }
    }
}

impl TerminalApp {
    pub(super) fn directory_split_origin(&self) -> Option<Origin> {
        let Page::Terminal(tab) = &self.tabs.active()?.page else {
            return None;
        };
        match tab.source {
            Source::Local(_) => tab.local_directory_split(),
            _ => tab.ssh_directory_split().map(|(origin, _)| origin),
        }
    }

    pub(super) fn open_in_directory(
        &mut self,
        destination: Destination,
        target: Origin,
        ctx: &ComponentContext<Self>,
    ) {
        let Some(id) = self.tabs.active_id() else {
            return;
        };
        let Some(child) = self.directory_child(id, target) else {
            return;
        };
        match destination {
            Destination::Tab => self.insert_tab(child, ctx),
            Destination::Split(axis) => self.insert_split(id, child, axis, ctx),
        }
    }

    fn directory_child(&self, id: TabId, target: Origin) -> Option<AppTab> {
        let AppTab {
            label,
            page: Page::Terminal(tab),
        } = self.tabs.get(id)?
        else {
            return None;
        };
        if let Source::Local(shell) = &tab.source {
            if tab.local_directory_split().as_ref() != Some(&target) {
                return None;
            }
            let mut shell = match target.directory {
                Directory::Local(cwd) => LocalOptions {
                    cwd: Some(cwd),
                    ..shell.clone()
                },
                Directory::Wsl(cwd) => shell.wsl_in_directory(cwd)?,
                Directory::Ssh(_) => return None,
            };
            shell.color_scheme = tab.color_scheme;
            // Saved local panes carry their full launch settings as well as
            // their profile name. Keep the new directory out of the catalog.
            let child = live_tab(
                label.clone(),
                Source::Local(shell),
                self.appearance
                    .read()
                    .unwrap()
                    .interaction
                    .remote_clipboard_policy(),
            )
            .with_profile(tab.profile_name.clone());
            return Some(child);
        }
        let resolved = if target.listing_revision.is_some() {
            if !self.drawer_shell_eligible(tab, target.generation) {
                return None;
            }
            tab.files
                .shell_origin(tab.session.pane_id)
                .zip(tab.session.ssh_connection_at(target.generation))
        } else {
            tab.ssh_directory_split()
        };
        let (current, connection) = resolved.filter(|(current, _)| *current == target)?;
        let Source::Ssh(pane) = &tab.source else {
            return None;
        };
        let options = child_options(
            connection.options(),
            pane,
            tab.color_scheme,
            tab.session.player.read().unwrap().clone(),
        );
        let Directory::Ssh(directory) = current.directory else {
            return None;
        };
        let startup = SshStartup::new(&options, directory, String::new()).ok()?;
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
            self.appearance
                .read()
                .unwrap()
                .interaction
                .remote_clipboard_policy(),
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
        Some(child)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_file_reports_decode_drive_paths_without_filesystem_probes() {
        for (uri, expected) in [
            ("file:///C:/", r"C:\"),
            (
                "file://localhost/C:/work%20dir/100%25%20%23%20%E6%96%87/",
                r"C:\work dir\100% # 文",
            ),
            ("FiLe://DEsK/z:/checkout", r"z:\checkout"),
            ("file:///Z:/literal%252F", r"Z:\literal%2F"),
        ] {
            assert_eq!(
                local_directory(uri, Some("desk")),
                Some(expected.into()),
                "{uri}"
            );
        }
        for invalid in [
            "",
            "file:///C:",
            "file:///C:relative",
            "file://alien/C:/dir",
            "file://host@desk/C:/dir",
            "file://desk:80/C:/dir",
            "file://./C:/dir",
            "file:////server/share",
            "file:////?/C:/a",
            "file:///C:/../a",
            "file:///C:/a//b",
            "file:///C:/a/%2e%2e/b",
            "file:///C:/a%5Cb",
            "file:///C:/a%2Fb",
            "file:///C:/dir?query",
            "file:///C:/dir#fragment",
            "file:///C:/%zz",
            "file:///C:/a:%24DATA",
            "file:///C:/NUL.txt",
            "file:///C:/foo%00bar",
            "file:///C:/%fe",
            "file:///C:/ends.",
            "file:///Env:/path",
        ] {
            assert!(
                local_directory(invalid, Some("desk")).is_none(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn wsl_reports_resolve_only_local_paths_or_the_selected_distribution() {
        for (uri, expected) in [
            ("file:///", "/"),
            (
                "file://desk/home/dev/100%25%20%23%20%E9%9B%AA",
                "/home/dev/100% # 雪",
            ),
            ("file://localhost/mnt/c/work%20tree", "/mnt/c/work tree"),
            ("file://wsl.localhost/qa%20LINUX/home/dev/", "/home/dev/"),
            ("file://wsl$/QA%20Linux", "/"),
            ("file:///C:/work%20tree", r"C:\work tree"),
        ] {
            assert_eq!(
                wsl_directory(uri, "QA Linux", Some("desk")).as_deref(),
                Some(expected),
                "{uri}"
            );
        }
        for uri in [
            "file://host.example/home/dev",
            "file://wsl.localhost/Ubuntu/home/dev",
            "file://wsl.localhost/QA%20Linux//another/share",
            "file://wsl.localhost/QA%20Linux/home/%2e%2e/another",
            "file://wsl.localhost/QA%20Linux%2Fother/home",
            "file:////server/share",
            "file:///C:/bad%2Fchild",
            "file:///C:/NUL.txt",
        ] {
            assert!(
                wsl_directory(uri, "QA Linux", Some("desk")).is_none(),
                "{uri}"
            );
        }
    }

    #[test]
    fn child_inherits_the_originating_ptys_settings_on_a_shared_transport() {
        let connection: SshOptions = serde_json::from_value(serde_json::json!({
            "host":"example.test", "user":"login", "auth":"identity", "identity":"active-key"
        }))
        .unwrap();
        let pane: SshOptions = serde_json::from_value(serde_json::json!({
            "host":"example.test", "user":"login", "auth":"password",
            "terminal_type":"xterm-sixel", "scrollback_lines":18, "color_scheme":"one-half-dark",
            "local_forwards":[{"local_port":8080, "remote_port":8000}]
        }))
        .unwrap();
        let player = PlayerOptions {
            mute: true,
            ..Default::default()
        };
        let child = child_options(
            &connection,
            &pane,
            ColorScheme::OneHalfLight,
            player.clone(),
        );
        assert_eq!(child.authentication, connection.authentication);
        assert_eq!(child.terminal_type, pane.terminal_type);
        assert_eq!(child.scrollback_lines, pane.scrollback_lines);
        assert_eq!(child.color_scheme, ColorScheme::OneHalfLight);
        assert_eq!(child.local_forwards, pane.local_forwards);
        assert_eq!(child.player, player);
    }

    #[test]
    fn queued_directory_actions_keep_the_parent_and_the_directory_that_was_offered() {
        let origin = Origin {
            pane: 14,
            generation: 3,
            directory: Directory::Ssh("/srv/client v2".into()),
            listing_revision: None,
        };
        for (label, expected) in [
            ("New tab in current directory    F9", Destination::Tab),
            (
                "Split right in current directory    F7",
                Destination::Split(Axis::Columns),
            ),
            (
                "Split below in current directory",
                Destination::Split(Axis::Rows),
            ),
        ] {
            let Message::Shortcut(key) = tab_menu_message(label.into()) else {
                panic!("unrecognized directory action")
            };
            let direction = destination(key).unwrap();
            assert_eq!(direction, expected);
            assert!(matches!(message(direction, None), Message::ControlIdle));
            assert!(
                matches!(message(direction, Some(&origin)), Message::OpenInDirectory(a, target) if a == expected && target == origin)
            );
        }
    }
}
