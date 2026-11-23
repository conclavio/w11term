use super::{Alert, TerminalState};
use std::io::Write;
use std::sync::Arc;
pub(super) use wezterm_escape_parser::csi::WindowTitleScope as Scope;

const CAPACITY: usize = 10;

#[derive(Clone)]
pub(super) struct WindowTitle {
    pub(super) raw: Arc<str>,
    pub(super) from_application: bool,
}

impl WindowTitle {
    pub(super) fn set_from_application(&mut self, raw: &str) {
        if &*self.raw != raw {
            self.raw = Arc::from(raw);
        }
        self.from_application = true;
    }
}

impl Default for WindowTitle {
    fn default() -> Self {
        Self {
            raw: Arc::from("wezterm"),
            from_application: false,
        }
    }
}

#[derive(Clone)]
struct SavedIcon {
    label: String,
    follows_window: bool,
}

#[derive(Default)]
struct Titles {
    window: Option<WindowTitle>,
    icon: Option<SavedIcon>,
}

#[derive(Default)]
pub(super) struct TitleStack {
    slots: [Titles; CAPACITY],
    // xterm reports the logical depth even after its ten physical slots wrap.
    depth: u64,
}

fn slot_index(slot: u8) -> Option<usize> {
    let index = usize::from(slot.checked_sub(1)?);
    (index < CAPACITY).then_some(index)
}

impl TitleStack {
    fn save(&mut self, slot: Option<u8>, titles: Titles) {
        let index = match slot {
            Some(slot) => {
                let Some(index) = slot_index(slot) else {
                    return;
                };
                index
            }
            None => {
                let index = (self.depth % CAPACITY as u64) as usize;
                self.depth = self.depth.saturating_add(1);
                index
            }
        };
        self.slots[index] = titles;
    }

    fn restore(&mut self, scope: Scope, slot: Option<u8>) -> Option<Titles> {
        let index = match slot {
            Some(slot) => slot_index(slot)?,
            None => {
                self.depth = self.depth.checked_sub(1)?;
                (self.depth % CAPACITY as u64) as usize
            }
        };
        // xtermPopTitle fills each absent field independently from the previous
        // nine slots, including directly stored slots outside the current depth.
        let mut previous =
            (0..CAPACITY).map(|offset| &self.slots[(index + CAPACITY - offset) % CAPACITY]);
        let window = match scope {
            Scope::Icon => None,
            _ => previous.clone().find_map(|s| s.window.as_ref()).cloned(),
        };
        let icon = match scope {
            Scope::Window => None,
            _ => previous.find_map(|s| s.icon.as_ref()).cloned(),
        };
        if slot.is_none() {
            self.slots[index] = Titles::default();
        }
        Some(Titles { window, icon })
    }
}

impl TerminalState {
    pub(super) fn save_titles(&mut self, scope: Scope, slot: Option<u8>) {
        let titles = Titles {
            window: (scope != Scope::Icon).then(|| self.title.clone()),
            icon: (scope != Scope::Window).then(|| SavedIcon {
                label: self.get_title().to_owned(),
                follows_window: self.icon_title.is_none(),
            }),
        };
        self.saved_titles.save(slot, titles);
    }

    pub(super) fn restore_titles(&mut self, scope: Scope, slot: Option<u8>) {
        let Some(titles) = self.saved_titles.restore(scope, slot) else {
            return;
        };
        let icon_changed = titles
            .icon
            .as_ref()
            .is_some_and(|icon| icon.label != self.get_title());
        if let Some(window) = titles.window {
            let changed = self.title.raw != window.raw;
            self.title = window;
            if changed {
                if let Some(handler) = self.alert_handler.as_mut() {
                    handler.alert(Alert::WindowTitleChanged(self.title.raw.to_string()));
                }
            }
        }
        if let Some(icon) = titles.icon {
            // An inherited icon follows future OSC 2 updates again when both
            // labels are restored. An icon-only restore can need a distinct label.
            self.icon_title = (!icon.follows_window || icon.label != self.title.raw.as_ref())
                .then_some(icon.label);
            if icon_changed {
                if let Some(handler) = self.alert_handler.as_mut() {
                    handler.alert(Alert::IconTitleChanged(self.icon_title.clone()));
                }
            }
        }
    }

    pub(super) fn report_title_stack_position(&mut self) {
        write!(
            self.writer,
            "\x1b[{};{}#S",
            self.saved_titles.depth, CAPACITY
        )
        .ok();
        self.writer.flush().ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(window: Option<&str>, icon: Option<&str>) -> Titles {
        Titles {
            window: window.map(|raw| WindowTitle {
                raw: raw.into(),
                from_application: true,
            }),
            icon: icon.map(|label| SavedIcon {
                label: label.into(),
                follows_window: false,
            }),
        }
    }

    #[test]
    fn partial_slots_search_backwards_and_explicit_empty_strings_stop_the_search() {
        let mut saved = TitleStack::default();
        saved.save(Some(10), titles(Some("window"), Some("icon")));
        saved.save(None, titles(Some(""), None));
        saved.save(None, titles(None, Some("inner icon")));
        let both = saved.restore(Scope::Both, None).unwrap();
        assert_eq!(both.window.as_ref().map(|w| &*w.raw), Some(""));
        assert_eq!(both.icon.unwrap().label, "inner icon");
        let icon = saved.restore(Scope::Icon, None).unwrap();
        assert!(icon.window.is_none());
        assert_eq!(icon.icon.unwrap().label, "icon");
        assert!(saved.restore(Scope::Both, None).is_none());
        for _ in 0..2 {
            let direct = saved.restore(Scope::Both, Some(10)).unwrap();
            assert_eq!(direct.window.as_ref().map(|w| &*w.raw), Some("window"));
            assert_eq!(direct.icon.unwrap().label, "icon");
        }
        assert_eq!(saved.depth, 0);
    }

    #[test]
    fn ten_slots_wrap_but_the_logical_depth_keeps_counting() {
        let mut saved = TitleStack::default();
        for i in 1..=12 {
            saved.save(None, titles(Some(&i.to_string()), None));
        }
        assert_eq!(saved.depth, 12);
        assert_eq!(
            saved
                .restore(Scope::Window, Some(1))
                .unwrap()
                .window
                .as_ref()
                .map(|w| &*w.raw),
            Some("11"),
        );
        for i in (3..=12).rev() {
            assert_eq!(
                saved
                    .restore(Scope::Window, None)
                    .unwrap()
                    .window
                    .map(|w| w.raw.to_string()),
                Some(i.to_string()),
            );
        }
        assert_eq!(saved.depth, 2);
        assert!(saved.restore(Scope::Window, None).unwrap().window.is_none());
        assert!(saved.restore(Scope::Window, None).unwrap().window.is_none());
        assert!(saved.restore(Scope::Window, None).is_none());
    }
}
