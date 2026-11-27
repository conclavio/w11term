use super::*;
use std::{
    borrow::Cow,
    path::PathBuf,
    time::{Duration, Instant},
};
use w11term::{
    options::PlayerOptions, sftp::FilenameMatch, ssh::ConnectionShare, workspace::SshStartup,
};

pub(super) const OPEN_EXPLORER: &str = "Open current folder in File Explorer";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Directory {
    Ssh(String),
    Local(PathBuf),
    Wsl(String),
}

impl Directory {
    pub(super) fn reported(source: &Source, report: &str) -> Option<Self> {
        match source {
            Source::Ssh(ssh) => {
                file_drawer::shell_directory(Some(report), &ssh.host).map(Self::Ssh)
            }
            Source::Local(shell) => {
                let computer = std::env::var("COMPUTERNAME").ok();
                match shell.wsl_distribution() {
                    Some(distribution) => {
                        wsl_directory(report, distribution, computer.as_deref()).map(Self::Wsl)
                    }
                    None => local_directory(report, computer.as_deref()).map(Self::Local),
                }
            }
            _ => None,
        }
    }

    pub(super) fn display(&self) -> Cow<'_, str> {
        match self {
            Self::Ssh(path) | Self::Wsl(path) => Cow::Borrowed(path),
            Self::Local(path) => path.to_string_lossy(),
        }
    }

    pub(super) fn explorer_location(&self, source: &Source) -> Option<explorer::Location> {
        let Source::Local(shell) = source else {
            return None;
        };
        match self {
            Self::Local(path) => Some(explorer::Location::Drive(path.clone())),
            Self::Wsl(path) if path.starts_with('/') => {
                explorer::Location::wsl(shell.wsl_distribution()?, path)
            }
            Self::Wsl(path) => Some(explorer::Location::Drive(PathBuf::from(path))),
            Self::Ssh(_) => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Origin {
    pane: u64,
    generation: u64,
    directory: Directory,
    drawer: Option<DrawerOrigin>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DrawerOrigin {
    Listing(u64),
    Search(u64, FilenameMatch),
}

pub(super) struct PendingCopy {
    request: Arc<()>,
    origin: Origin,
    clipboard_sequence: u32,
    task: ComponentTask,
}

impl Origin {
    pub(super) fn listing(pane: u64, generation: u64, directory: &str, revision: u64) -> Self {
        Self {
            pane,
            generation,
            directory: Directory::Ssh(directory.into()),
            drawer: Some(DrawerOrigin::Listing(revision)),
        }
    }

    pub(super) fn search(
        pane: u64,
        generation: u64,
        serial: u64,
        hit: &FilenameMatch,
    ) -> Option<Self> {
        let directory = w11term::profiles::bookmark_directory(&hit.parent)?;
        Some(Self {
            pane,
            generation,
            directory: Directory::Ssh(directory.into()),
            drawer: Some(DrawerOrigin::Search(serial, hit.clone())),
        })
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

pub(super) fn explorer_message(target: Option<&Origin>) -> Message {
    target
        .cloned()
        .map_or(Message::ControlIdle, Message::OpenExplorer)
}

pub(super) fn copy_message(target: Option<&Origin>) -> Message {
    target
        .cloned()
        .map_or(Message::ControlIdle, Message::CopyCurrentDirectory)
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
        dotfiles: pane.dotfiles.clone(),
        tool_bundle: pane.tool_bundle.clone(),
        group_prompt: pane.group_prompt.clone(),
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

pub(super) fn reported_wsl_directory(report: Option<&str>, distribution: &str) -> Option<String> {
    wsl_directory(
        report?,
        distribution,
        std::env::var("COMPUTERNAME").ok().as_deref(),
    )
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
                drawer: None,
            },
            connection,
        ))
    }

    fn local_directory_split(&self) -> Option<Origin> {
        let Source::Local(_) = &self.source else {
            return None;
        };
        let frame = self.session.frames.load();
        if !self.can_report_split_directory(frame.connection_generation) {
            return None;
        }
        let report = frame.reported_directory.as_deref()?;
        let directory = Directory::reported(&self.source, report)?;
        Some(Origin {
            pane: self.session.pane_id,
            generation: frame.connection_generation,
            directory,
            drawer: None,
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

    fn explorer_folder(&self, origin: &Origin) -> Option<explorer::Location> {
        let Page::Terminal(tab) = &self.tabs.active()?.page else {
            return None;
        };
        if tab.local_directory_split().as_ref()? != origin {
            return None;
        }
        origin.directory.explorer_location(&tab.source)
    }

    pub(super) fn can_open_explorer(&self, origin: Option<&Origin>) -> bool {
        origin
            .and_then(|origin| self.explorer_folder(origin))
            .is_some()
    }

    pub(super) fn open_explorer(&mut self, origin: Origin, ctx: &ComponentContext<Self>) {
        let Some(folder) = self.explorer_folder(&origin) else {
            return;
        };
        explorer::open_location(folder, ctx);
    }

    pub(super) fn cancel_directory_copy(&mut self) {
        if let Some(copy) = self.pending_directory_copy.take() {
            copy.task.cancel();
        }
    }

    pub(super) fn copy_current_directory(&mut self, origin: Origin, ctx: &ComponentContext<Self>) {
        if self.directory_split_origin().as_ref() != Some(&origin) {
            return;
        }
        self.cancel_directory_copy();
        let result = match &origin.directory {
            Directory::Ssh(path) => w11term::clipboard::write(path),
            Directory::Local(path) => w11term::clipboard::write(&path.to_string_lossy()),
            Directory::Wsl(path) if path.starts_with('/') => w11term::clipboard::write(path),
            Directory::Wsl(path) => {
                let Some(Page::Terminal(live)) = self.tabs.active().map(|tab| &tab.page) else {
                    return;
                };
                let Source::Local(shell) = &live.source else {
                    return;
                };
                let Some(distribution) = shell.wsl_distribution().map(str::to_owned) else {
                    return;
                };
                let executable = shell.executable.clone();
                let path = path.clone();
                let request = Arc::new(());
                let result_request = request.clone();
                let clipboard_sequence = w11term::clipboard::sequence();
                let task = ctx.spawn_background_with_rejection(
                    move |cancel| {
                        let result = wsl_path::translate(
                            &executable,
                            &distribution,
                            None,
                            wsl_path::Direction::ToLinux,
                            &path,
                            Instant::now() + Duration::from_secs(8),
                            || cancel.is_cancelled(),
                        );
                        Message::DirectoryCopyReady(
                            result_request,
                            result.map_err(|e| format!("{e:#}")),
                        )
                    },
                    Message::DirectoryCopyReady(
                        request.clone(),
                        Err("The WSL translation could not start.".into()),
                    ),
                );
                self.pending_directory_copy = Some(PendingCopy {
                    request,
                    origin,
                    clipboard_sequence,
                    task,
                });
                return;
            }
        };
        self.directory_copy_result(result);
    }

    pub(super) fn finish_directory_copy(
        &mut self,
        request: Arc<()>,
        result: Result<String, String>,
    ) {
        if !self
            .pending_directory_copy
            .as_ref()
            .is_some_and(|copy| Arc::ptr_eq(&copy.request, &request))
        {
            return;
        }
        let copy = self.pending_directory_copy.take().unwrap();
        if self.directory_split_origin().as_ref() != Some(&copy.origin) {
            return;
        }
        self.directory_copy_result(result.map_err(anyhow::Error::msg).and_then(|path| {
            w11term::clipboard::write_if_sequence(&path, copy.clipboard_sequence)
        }));
    }

    fn directory_copy_result(&mut self, result: anyhow::Result<()>) {
        if let Err(error) = result {
            self.window_error = Some(format!("Couldn’t copy the current directory. {error:#}"));
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
            Destination::Split(axis) => {
                let _ = self.insert_split(id, child, axis, ctx);
            }
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
                self.appearance.read().unwrap().pane_preferences(),
            )
            .with_profile(tab.profile_name.clone());
            return Some(child);
        }
        let resolved = if let Some(origin) = &target.drawer {
            if !self.drawer_shell_eligible(tab, target.generation) {
                return None;
            }
            let pane = tab.session.pane_id;
            let current = match origin {
                DrawerOrigin::Listing(_) => tab.files.shell_origin(pane),
                DrawerOrigin::Search(serial, hit) => {
                    tab.files.search_shell_origin(pane, *serial, hit)
                }
            };
            current.zip(tab.session.ssh_connection_at(target.generation))
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
            self.appearance.read().unwrap().pane_preferences(),
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
    fn bash_directory_reports_preserve_encoded_paths_and_clear_an_invalid_location() {
        let (writer, _) = tokio::sync::mpsc::unbounded_channel();
        let mut term = w11term::terminal::create(Default::default(), writer);
        term.advance_bytes(
            b"\x1b]7;file:///home/tree%20%23100%25%20%5Bpilot%5D%20%27%20%E9%9B%AA\x07",
        );
        let report = term.reported_current_dir().map(AsRef::as_ref);
        assert_eq!(
            file_drawer::shell_directory(report, "node.example").as_deref(),
            Some("/home/tree #100% [pilot] ' 雪")
        );
        term.advance_bytes(b"\x1b]7;file://\x07");
        let report = term.reported_current_dir().map(AsRef::as_ref);
        assert!(file_drawer::shell_directory(report, "node.example").is_none());
    }

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
    fn search_split_keeps_the_containing_directory_and_exact_match() {
        let hit = FilenameMatch {
            parent: "/Pilot's Work/deeper".into(),
            name: "child-witness.txt".into(),
            kind: None,
            mtime: None,
            size: None,
            content_match: None,
        };
        let origin = Origin::search(14, 3, 8, &hit).unwrap();
        assert_eq!(origin.pane, 14);
        assert_eq!(origin.generation, 3);
        assert_eq!(origin.directory, Directory::Ssh(hit.parent.clone()));
        assert_eq!(origin.drawer, Some(DrawerOrigin::Search(8, hit.clone())));
        assert_ne!(Some(origin), Origin::search(14, 4, 8, &hit));
        let invalid = FilenameMatch {
            parent: "/reports/../private".into(),
            ..hit
        };
        assert!(Origin::search(14, 3, 8, &invalid).is_none());
    }

    #[test]
    fn queued_directory_actions_keep_the_parent_and_the_directory_that_was_offered() {
        let origin = Origin {
            pane: 14,
            generation: 3,
            directory: Directory::Ssh("/srv/client v2".into()),
            drawer: None,
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
