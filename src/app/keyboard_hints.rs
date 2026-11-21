use super::*;
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
    ssh: bool,
    directory: Option<String>,
    message: Option<String>,
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

    fn reveal(&self) -> Option<(String, Option<String>)> {
        match self.selected()? {
            Target::Path(path) if self.ssh => reveal_path(path, self.directory.as_deref()),
            _ => None,
        }
    }

    pub fn instructions(&self) -> String {
        if let Some(message) = &self.message {
            return message.clone();
        }
        match self.selected() {
            Some(Target::Url(uri)) => format!("Enter open · C copy · Esc cancel   {uri}"),
            Some(Target::Path(path)) if self.reveal().is_some() => {
                format!("Enter copy · R reveal in SFTP · Esc cancel   {path}")
            }
            Some(Target::Path(path)) => format!("Enter copy · Esc cancel   {path}"),
            None if self.entries.is_empty() => {
                "No URL or path hints in this pane · Esc close".into()
            }
            None => format!("Type a hint label · Esc cancel   {}", self.prefix),
        }
    }
}

impl TerminalApp {
    pub(super) fn open_hints(&mut self) {
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
        let detect = self.appearance.read().unwrap().interaction.detect_urls;
        let entries = hints::viewport(&frame, detect);
        let (ssh, directory) = match &tab.source {
            Source::Ssh(ssh) => (
                true,
                file_drawer::shell_directory(frame.reported_directory.as_deref(), &ssh.host),
            ),
            _ => (false, None),
        };
        *self.hints.borrow_mut() = Some(Hints {
            pane: id,
            frame,
            entries,
            prefix: String::new(),
            selected: None,
            ssh,
            directory,
            message: None,
        });
        self.keyboard.set_hints(true);
        tab.surface.invalidator.invalidate();
    }

    pub(super) fn close_hints(&mut self) {
        if let Some(hints) = self.hints.borrow_mut().take() {
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
            (0x0d | 0x43, _) => {
                if let Err(error) = w11term::clipboard::write(target.text()) {
                    h.message = Some(format!("Copy failed: {error} · Esc close"));
                    return;
                }
            }
            (0x52, Target::Path(_)) => {
                let Some((dir, name)) = h.reveal() else {
                    return;
                };
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
            _ => return,
        }
        drop(state);
        self.close_hints();
        if let Some(action) = reveal {
            self.file_action(action, context);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
