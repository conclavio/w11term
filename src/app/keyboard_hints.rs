use super::*;
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use w11term::{
    hints::{self, Hint, Target},
    terminal::Snapshot,
};

pub(super) struct Hints {
    pub pane: TabId,
    pub frame: Arc<Snapshot>,
    entries: Vec<Hint>,
    prefix: String,
    selected: Option<usize>,
    backend: HintBackend,
    opening_explorer: Option<PendingExplore>,
    message: Option<String>,
}

enum HintBackend {
    Ssh(Option<String>),
    Local(Option<PathBuf>),
    Wsl {
        executable: PathBuf,
        distribution: String,
        directory: Option<String>,
    },
    Other,
}

enum Reveal {
    Sftp(String, Option<String>),
    Explorer(ExplorerReveal),
}

enum ExplorerReveal {
    Location(explorer::Location),
    Wsl {
        executable: PathBuf,
        distribution: String,
        directory: Option<String>,
        path: String,
    },
}

impl ExplorerReveal {
    fn open(self, cancelled: impl Fn() -> bool) -> anyhow::Result<()> {
        let location = match self {
            Self::Location(location) => location,
            Self::Wsl {
                executable,
                distribution,
                directory,
                path,
            } => {
                let translated = wsl_path::translate(
                    &executable,
                    &distribution,
                    directory.as_deref(),
                    wsl_path::Direction::ToWindows,
                    &path,
                    Instant::now() + Duration::from_secs(8),
                    &cancelled,
                )?;
                explorer::Location::translated_from_wsl(&distribution, &translated).context(
                    "wslpath must return a local drive or a path in the pane’s distribution",
                )?
            }
        };
        anyhow::ensure!(!cancelled(), "The Explorer request was cancelled");
        location.reveal()
    }
}

struct PendingExplore {
    request: Arc<()>,
    task: ComponentTask,
}

impl HintBackend {
    fn resolve(&self, target: &Target) -> Option<Reveal> {
        let path = target.file_path()?;
        match self {
            Self::Ssh(cwd) => {
                reveal_path(path, cwd.as_deref()).map(|(dir, name)| Reveal::Sftp(dir, name))
            }
            Self::Local(cwd) => drive_hint(path, cwd.as_deref()),
            Self::Wsl {
                executable,
                distribution,
                directory,
            } => {
                if let Some(drive) = drive_hint(path, None) {
                    return Some(drive);
                }
                if path.starts_with(r"\\") {
                    let location = explorer::Location::translated_from_wsl(distribution, path)?;
                    return Some(Reveal::Explorer(ExplorerReveal::Location(location)));
                }
                if path.contains('\\') {
                    return drive_hint(path, directory.as_deref().map(Path::new));
                }
                if path.starts_with('~') || path.starts_with("//") || !wsl_path::safe_text(path) {
                    return None;
                }
                let (directory, path) = if path.starts_with('/') {
                    (None, path.to_owned())
                } else {
                    // Prefix relative paths so a filename beginning with a
                    // dash is passed to wslpath as a path, not as an option.
                    (Some(directory.as_ref()?.clone()), format!("./{path}"))
                };
                Some(Reveal::Explorer(ExplorerReveal::Wsl {
                    executable: executable.clone(),
                    distribution: distribution.clone(),
                    directory,
                    path,
                }))
            }
            Self::Other => None,
        }
    }
}

fn drive_hint(path: &str, cwd: Option<&Path>) -> Option<Reveal> {
    let path = explorer::resolve(path, cwd)?;
    Some(Reveal::Explorer(ExplorerReveal::Location(
        explorer::Location::Drive(path),
    )))
}

// Only SFTP on this pane interprets the path. Its server resolves "." and "..",
// preserving symlink semantics. A relative path needs an absolute OSC 7 base.
fn reveal_path(path: &str, cwd: Option<&str>) -> Option<(String, Option<String>)> {
    if path.contains(['\\', ':'])
        || path.starts_with("//")
        || path.starts_with('~')
        || path.chars().any(char::is_control)
    {
        return None;
    }
    let path = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("{}/{}", cwd?.trim_end_matches('/'), path)
    };
    let (dir, name) = path.rsplit_once('/')?;
    if matches!(name, "" | "." | "..") {
        Some((path, None))
    } else {
        Some((
            if dir.is_empty() {
                "/".into()
            } else {
                dir.into()
            },
            Some(name.into()),
        ))
    }
}

impl Hints {
    pub fn visible_labels(&self) -> impl Iterator<Item = &Hint> {
        self.entries
            .iter()
            .filter(|h| h.label.starts_with(&self.prefix))
    }

    fn selected(&self) -> Option<&Target> {
        self.selected.map(|i| &self.entries[i].target)
    }

    fn reveal_instruction(&self, target: &Target) -> &'static str {
        match self.backend.resolve(target) {
            Some(Reveal::Sftp(..)) => " · R reveal in SFTP",
            Some(Reveal::Explorer(_)) => " · R reveal in File Explorer",
            None => "",
        }
    }

    pub fn instructions(&self) -> String {
        if let Some(message) = &self.message {
            return message.clone();
        }
        match self.selected() {
            Some(Target::Url(uri)) => format!("Enter open · C copy · Esc cancel   {uri}"),
            Some(target @ Target::Path(path)) => {
                format!(
                    "Enter copy{} · Esc cancel   {path}",
                    self.reveal_instruction(target)
                )
            }
            Some(target @ Target::Pattern { name, text }) => {
                let action = if target.pattern_url().is_some() {
                    " · O open URL"
                } else {
                    self.reveal_instruction(target)
                };
                format!("{name} · Enter copy{action} · Esc cancel   {text}")
            }
            None if self.entries.is_empty() => "No matching hints in this pane · Esc close".into(),
            None => format!("Type a hint label · Esc cancel   {}", self.prefix),
        }
    }
}

impl TerminalApp {
    pub(super) fn open_hints(&mut self) {
        self.close_copy_mode();
        let Some(id) = self.tabs.active_id() else {
            self.keyboard.set_hints(false);
            return;
        };
        let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.get(id)
        else {
            self.keyboard.set_hints(false);
            return;
        };
        if tab.find.open
            || tab.session.frames.load().prompt.is_some()
            || tab.browsing_sessions()
            || self.picker.is_some()
            || self.switcher.is_some()
            || tab.device_busy
        {
            self.keyboard.set_hints(false);
            return;
        }
        let frame = tab.session.frames.load();
        let entries = {
            let settings = self.appearance.read().unwrap();
            hints::viewport_with_patterns(
                &frame,
                settings.interaction.detect_urls,
                &settings.hint_patterns,
            )
        };
        let report = frame.reported_directory.as_deref();
        let backend = match &tab.source {
            Source::Ssh(ssh) => HintBackend::Ssh(file_drawer::shell_directory(report, &ssh.host)),
            Source::Local(shell) if let Some(distribution) = shell.wsl_distribution() => {
                HintBackend::Wsl {
                    executable: shell.executable.clone(),
                    distribution: distribution.into(),
                    directory: directory_split::reported_wsl_directory(report, distribution),
                }
            }
            Source::Local(_) => {
                HintBackend::Local(directory_split::reported_local_directory(report))
            }
            _ => HintBackend::Other,
        };
        *self.hints.borrow_mut() = Some(Hints {
            pane: id,
            frame,
            entries,
            prefix: String::new(),
            selected: None,
            backend,
            opening_explorer: None,
            message: None,
        });
        if let Some(inspector) = &mut self.inspection {
            inspector.clear_pointer();
        }
        self.keyboard.set_hints(true);
        tab.surface.invalidator.invalidate();
    }

    pub(super) fn close_hints(&mut self) {
        if let Some(hints) = self.hints.borrow_mut().take() {
            if let Some(pending) = hints.opening_explorer {
                pending.task.cancel();
            }
            self.keyboard.set_hints(false);
            if let Some(AppTab {
                page: Page::Terminal(tab),
                ..
            }) = self.tabs.get(hints.pane)
            {
                tab.surface.invalidator.invalidate();
            }
        }
    }

    pub(super) fn hint_key(&mut self, vk: u32, context: &ComponentContext<Self>) {
        if vk == 0x1b {
            self.close_hints();
            return;
        }
        let mut state = self.hints.borrow_mut();
        let Some(h) = &mut *state else { return };
        let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.get(h.pane)
        else {
            return;
        };
        if self.tabs.active_id() != Some(h.pane)
            || tab.session.frames.load().connection_generation != h.frame.connection_generation
        {
            drop(state);
            self.close_hints();
            return;
        }
        if h.opening_explorer.is_some() {
            return;
        }
        h.message = None;
        if vk == 0x08 {
            h.selected = None;
            h.prefix.pop();
            tab.surface.invalidator.invalidate();
            return;
        }
        let Some(target) = h.selected() else {
            if (0x41..=0x5a).contains(&vk) {
                h.prefix.push((vk as u8 as char).to_ascii_lowercase());
                if h.entries.iter().any(|e| e.label.starts_with(&h.prefix)) {
                    h.selected = h.entries.iter().position(|e| e.label == h.prefix);
                } else {
                    h.prefix.pop();
                }
                tab.surface.invalidator.invalidate();
            }
            return;
        };
        let mut reveal = None;
        match (vk, target) {
            (0x0d, Target::Url(uri)) => Self::open_link(uri),
            (0x4f, Target::Pattern { .. }) => {
                let Some(uri) = target.pattern_url() else {
                    return;
                };
                Self::open_link(&uri);
            }
            (0x0d | 0x43, _) => {
                if let Err(error) = w11term::clipboard::write(target.text()) {
                    h.message = Some(format!("Copy failed: {error} · Esc close"));
                    return;
                }
            }
            (0x52, _) => match h.backend.resolve(target) {
                Some(Reveal::Explorer(explorer)) => {
                    let request = Arc::new(());
                    h.message = Some("Opening File Explorer… · Esc close".into());
                    let rejected = Message::HintRevealed(
                        request.clone(),
                        Err("File Explorer is unavailable".into()),
                    );
                    let result_request = request.clone();
                    let task = context.spawn_background_with_rejection(
                        move |cancel| {
                            Message::HintRevealed(
                                result_request,
                                explorer
                                    .open(|| cancel.is_cancelled())
                                    .map_err(|e| format!("{e:#}")),
                            )
                        },
                        rejected,
                    );
                    h.opening_explorer = Some(PendingExplore { request, task });
                    return;
                }
                Some(Reveal::Sftp(dir, name)) => {
                    if tab.files.busy() {
                        h.message = Some("File drawer busy · R retry · Esc cancel".into());
                        return;
                    }
                    reveal = Some(FileAction::Reveal(
                        tab.session.pane_id,
                        h.frame.connection_generation,
                        dir,
                        name,
                    ));
                }
                None => return,
            },
            _ => return,
        }
        drop(state);
        self.close_hints();
        if let Some(action) = reveal {
            self.file_action(action, context);
        }
    }

    pub(super) fn hint_revealed(&mut self, request: Arc<()>, result: Result<(), String>) {
        let mut state = self.hints.borrow_mut();
        let Some(h) = state.as_mut() else { return };
        if !h
            .opening_explorer
            .as_ref()
            .is_some_and(|pending| Arc::ptr_eq(&pending.request, &request))
        {
            return;
        }
        h.opening_explorer = None;
        if let Err(error) = result {
            h.message = Some(format!("Reveal failed: {error} · R retry · Esc cancel"));
            return;
        }
        drop(state);
        self.close_hints();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl_hint_paths_use_the_captured_distribution_and_directory() {
        let backend = HintBackend::Wsl {
            executable: r"C:\Windows\System32\wsl.exe".into(),
            distribution: "QA Linux".into(),
            directory: Some("/home/dev/build tree".into()),
        };
        let path = |text: &str| Target::Path(text.into());
        assert!(matches!(
            backend.resolve(&path("../src/main.rs")),
            Some(Reveal::Explorer(ExplorerReveal::Wsl {
                executable, distribution, directory: Some(directory), path,
            })) if executable == Path::new(r"C:\Windows\System32\wsl.exe")
                && distribution == "QA Linux" && directory == "/home/dev/build tree"
                && path == "./../src/main.rs"
        ));
        assert!(matches!(
            backend.resolve(&path("/home/dev/main.rs")),
            Some(Reveal::Explorer(ExplorerReveal::Wsl { directory: None, path, .. }))
                if path == "/home/dev/main.rs"
        ));
        assert!(matches!(
            backend.resolve(&path("-a/file.txt")),
            Some(Reveal::Explorer(ExplorerReveal::Wsl { path, .. })) if path == "./-a/file.txt"
        ));
        assert!(matches!(
            backend.resolve(&path(r"\\wsl$\QA Linux\home\dev\main.rs")),
            Some(Reveal::Explorer(ExplorerReveal::Location(explorer::Location::Wsl(path))))
                if path == Path::new(r"\\wsl.localhost\QA Linux\home\dev\main.rs")
        ));
        for invalid in [
            "//host/share",
            r"\\host\share",
            "~/file.txt",
            "/home/file\nnext",
        ] {
            assert!(backend.resolve(&path(invalid)).is_none(), "{invalid:?}");
        }
        let unreported = HintBackend::Wsl {
            executable: "wsl.exe".into(),
            distribution: "QA Linux".into(),
            directory: None,
        };
        assert!(unreported.resolve(&path("src/main.rs")).is_none());
        assert!(unreported.resolve(&path("/etc/profile")).is_some());
    }
    #[test]
    fn path_actions_follow_the_pane_that_produced_the_text() {
        let local = HintBackend::Local(Some(PathBuf::from(r"C:\build")));
        let remote = HintBackend::Ssh(Some("/srv/build".into()));
        let path = Target::Pattern {
            name: "Source".into(),
            text: "src/main.rs".into(),
        };
        assert!(
            matches!(local.resolve(&path), Some(Reveal::Explorer(ExplorerReveal::Location(explorer::Location::Drive(p)))) if p == Path::new(r"C:\build\src\main.rs"))
        );
        assert!(
            matches!(remote.resolve(&path), Some(Reveal::Sftp(dir, Some(name))) if dir == "/srv/build/src" && name == "main.rs")
        );
        assert!(HintBackend::Other.resolve(&path).is_none());
        assert!(HintBackend::Local(None).resolve(&path).is_none());
        assert!(
            local
                .resolve(&Target::Path("/etc/profile".into()))
                .is_none()
        );
        assert!(
            remote
                .resolve(&Target::Path(r"C:\work\code.rs".into()))
                .is_none()
        );
        assert!(
            local
                .resolve(&Target::Url("https://example.org/a.rs".into()))
                .is_none()
        );
    }

    #[test]
    fn ssh_reveal_resolves_unix_paths_against_the_reported_directory() {
        assert_eq!(
            reveal_path("src/main.rs", Some("/home/a b")),
            Some(("/home/a b/src".into(), Some("main.rs".into())))
        );
        assert_eq!(
            reveal_path("/error.log", None),
            Some(("/".into(), Some("error.log".into())))
        );
        assert_eq!(
            reveal_path("../src/", Some("/build")),
            Some(("/build/../src/".into(), None))
        );
        for s in [
            "rel.txt",
            "~user/file",
            "//server/share",
            "C:\\foo.txt",
            "sftp://elsewhere/path",
        ] {
            assert!(reveal_path(s, None).is_none());
        }
    }
}
