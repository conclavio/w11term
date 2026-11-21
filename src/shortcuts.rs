//! Workspace shortcut definitions shared by the Windows input hook, menus and editor.
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    str::FromStr,
};
use wezterm_term::KeyModifiers as Mods;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Action {
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    SplitRight,
    SplitBelow,
    ZoomPane,
    ClosePane,
    MovePane,
    PanePicker,
    KeyboardHints,
    CopyMode,
    NextTab,
    PreviousTab,
    NewTab,
    CloseTab,
    SelectTab(u8),
    Workspaces,
}
pub struct Binding {
    pub action: Action,
    pub id: &'static str,
    pub label: &'static str,
    default: Option<Chord>,
}
impl Binding {
    const fn new(
        action: Action,
        id: &'static str,
        label: &'static str,
        key: u32,
        mods: Mods,
    ) -> Self {
        Self {
            action,
            id,
            label,
            default: if key == 0 {
                None
            } else {
                Some(Chord { key, mods })
            },
        }
    }
}
const CTRL_ALT: Mods = Mods::CTRL.union(Mods::ALT);
const CTRL_SHIFT: Mods = Mods::CTRL.union(Mods::SHIFT);
const PANE: Mods = CTRL_ALT.union(Mods::SHIFT);
use Action::*;
pub const BINDINGS: &[Binding] = &[
    Binding::new(FocusLeft, "focus_left", "Focus left", 0x25, CTRL_ALT),
    Binding::new(FocusRight, "focus_right", "Focus right", 0x27, CTRL_ALT),
    Binding::new(FocusUp, "focus_up", "Focus above", 0x26, CTRL_ALT),
    Binding::new(FocusDown, "focus_down", "Focus below", 0x28, CTRL_ALT),
    Binding::new(SplitRight, "split_right", "Split right", 0x27, PANE),
    Binding::new(SplitBelow, "split_below", "Split below", 0x28, PANE),
    Binding::new(ZoomPane, "zoom_pane", "Zoom pane", 0x5a, PANE),
    Binding::new(ClosePane, "close_pane", "Close pane", 0x57, PANE),
    Binding::new(MovePane, "move_pane", "Move pane to new tab", 0, Mods::NONE),
    Binding::new(PanePicker, "pane_picker", "Find pane", 0x50, CTRL_SHIFT),
    Binding::new(
        KeyboardHints,
        "keyboard_hints",
        "Keyboard hints",
        0x48,
        CTRL_SHIFT,
    ),
    Binding::new(
        CopyMode,
        "copy_mode",
        "Keyboard copy mode",
        0x58,
        CTRL_SHIFT,
    ),
    Binding::new(Workspaces, "workspaces", "Workspaces", 0, Mods::NONE),
    Binding::new(NextTab, "next_tab", "Next tab", 0x09, Mods::CTRL),
    Binding::new(
        PreviousTab,
        "previous_tab",
        "Previous tab",
        0x09,
        CTRL_SHIFT,
    ),
    Binding::new(NewTab, "new_tab", "New tab", 0x54, CTRL_SHIFT),
    Binding::new(CloseTab, "close_tab", "Close tab", 0x57, CTRL_SHIFT),
    Binding::new(SelectTab(1), "select_tab_1", "Select tab 1", 0x31, CTRL_ALT),
    Binding::new(SelectTab(2), "select_tab_2", "Select tab 2", 0x32, CTRL_ALT),
    Binding::new(SelectTab(3), "select_tab_3", "Select tab 3", 0x33, CTRL_ALT),
    Binding::new(SelectTab(4), "select_tab_4", "Select tab 4", 0x34, CTRL_ALT),
    Binding::new(SelectTab(5), "select_tab_5", "Select tab 5", 0x35, CTRL_ALT),
    Binding::new(SelectTab(6), "select_tab_6", "Select tab 6", 0x36, CTRL_ALT),
    Binding::new(SelectTab(7), "select_tab_7", "Select tab 7", 0x37, CTRL_ALT),
    Binding::new(SelectTab(8), "select_tab_8", "Select tab 8", 0x38, CTRL_ALT),
    Binding::new(SelectTab(9), "select_tab_9", "Select tab 9", 0x39, CTRL_ALT),
];

impl Action {
    pub fn binding(self) -> &'static Binding {
        BINDINGS
            .iter()
            .find(|b| b.action == self)
            .expect("registered shortcut action")
    }
    fn requires_terminal(self) -> bool {
        matches!(
            self,
            FocusLeft
                | FocusRight
                | FocusUp
                | FocusDown
                | SplitRight
                | SplitBelow
                | ZoomPane
                | ClosePane
                | MovePane
                | KeyboardHints
                | CopyMode
        )
    }
}

const KEY_NAMES: &[(u32, &str)] = &[
    (0x08, "Backspace"),
    (0x09, "Tab"),
    (0x0d, "Enter"),
    (0x1b, "Escape"),
    (0x20, "Space"),
    (0x21, "PageUp"),
    (0x22, "PageDown"),
    (0x23, "End"),
    (0x24, "Home"),
    (0x25, "Left"),
    (0x26, "Up"),
    (0x27, "Right"),
    (0x28, "Down"),
    (0x2d, "Insert"),
    (0x2e, "Delete"),
    (0xba, "Semicolon"),
    (0xbb, "Equal"),
    (0xbc, "Comma"),
    (0xbd, "Minus"),
    (0xbe, "Period"),
    (0xbf, "Slash"),
    (0xc0, "Backtick"),
    (0xdb, "LeftBracket"),
    (0xdc, "Backslash"),
    (0xdd, "RightBracket"),
    (0xde, "Quote"),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Chord {
    pub key: u32,
    pub mods: Mods,
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (modifier, name) in [
            (Mods::CTRL, "Ctrl"),
            (Mods::ALT, "Alt"),
            (Mods::SHIFT, "Shift"),
        ] {
            if self.mods.contains(modifier) {
                write!(f, "{name}+")?;
            }
        }
        match self.key {
            0x41..=0x5a | 0x30..=0x39 => write!(f, "{}", char::from(self.key as u8)),
            0x70..=0x87 => write!(f, "F{}", self.key - 0x6f),
            key => f.write_str(
                KEY_NAMES
                    .iter()
                    .find(|(k, _)| *k == key)
                    .ok_or(fmt::Error)?
                    .1,
            ),
        }
    }
}
impl From<Chord> for String {
    fn from(chord: Chord) -> Self {
        chord.to_string()
    }
}
impl TryFrom<String> for Chord {
    type Error = anyhow::Error;
    fn try_from(s: String) -> Result<Self> {
        s.parse()
    }
}
impl FromStr for Chord {
    type Err = anyhow::Error;
    fn from_str(text: &str) -> Result<Self> {
        let mut parts = text.split('+').map(str::trim);
        let key_name = parts.next_back().unwrap_or_default().to_ascii_uppercase();
        let mut mods = Mods::NONE;
        for part in parts {
            let modifier = match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => Mods::CTRL,
                "alt" => Mods::ALT,
                "shift" => Mods::SHIFT,
                _ => bail!("Unknown modifier '{part}'. Use Ctrl, Alt or Shift."),
            };
            ensure!(!mods.contains(modifier), "Modifier '{part}' appears twice.");
            mods |= modifier;
        }
        let key = match key_name.as_bytes() {
            [key] if key.is_ascii_alphanumeric() => u32::from(*key),
            _ if let Some(number) = key_name
                .strip_prefix('F')
                .and_then(|n| n.parse::<u32>().ok())
                .filter(|n| (1..=24).contains(n)) =>
            {
                0x6f + number
            }
            _ => KEY_NAMES
                .iter()
                .find(|(_, name)| name.eq_ignore_ascii_case(&key_name))
                .map(|(vk, _)| *vk)
                .ok_or_else(|| {
                    anyhow::anyhow!("Choose a letter, digit, F1–F24 or a named key such as Right.")
                })?,
        };
        ensure!(
            mods.intersects(Mods::CTRL | Mods::ALT) || (0x70..=0x87).contains(&key),
            "Use Ctrl or Alt with this key so ordinary typing and navigation stay available."
        );
        let chord = Self { key, mods };
        ensure!(
            !chord.reserved(),
            "{chord} is already used by Windows or a terminal editing command."
        );
        Ok(chord)
    }
}
impl Chord {
    // These commands are handled outside the workspace action map.
    fn reserved(self) -> bool {
        let Self { key, mods } = self;
        let ctrl = mods.contains(Mods::CTRL);
        let alt = mods.contains(Mods::ALT);
        let shift = mods.contains(Mods::SHIFT);
        matches!(key, 0x72 | 0x74 | 0x7a)
            || (alt && matches!(key, 0x73 | 0x20 | 0x09 | 0x1b))
            || (ctrl && alt && key == 0x2e)
            || (ctrl && key == 0x1b)
            || (ctrl && shift && key == 0x46)
            || (ctrl && key == 0x56)
            || (ctrl && !alt && matches!(key, 0x43 | 0x2d | 0xbb | 0xbd | 0x30))
            || (shift && matches!(key, 0x2d | 0x21 | 0x22))
            || (mods == CTRL_SHIFT && matches!(key, 0x26 | 0x28 | 0x23 | 0x24))
    }
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Shortcuts(BTreeMap<String, Option<Chord>>);

impl Shortcuts {
    pub fn get(&self, binding: &Binding) -> Option<Chord> {
        if let Some(assigned) = self.0.get(binding.id) {
            return *assigned;
        }
        // A later release can add a default that the user already assigned.
        binding.default.filter(|chord| {
            !BINDINGS
                .iter()
                .any(|other| self.0.get(other.id) == Some(&Some(*chord)))
        })
    }

    pub fn set(&mut self, action: Action, chord: Option<Chord>) -> Result<()> {
        let binding = action.binding();
        if let Some(chord) = chord
            && let Some(other) = BINDINGS
                .iter()
                .find(|b| b.action != action && self.get(b) == Some(chord))
        {
            bail!(
                "{chord} is already assigned to {}. Clear or change that shortcut first.",
                other.label
            );
        }
        if binding.default == chord {
            self.0.remove(binding.id);
        } else {
            self.0.insert(binding.id.into(), chord);
        }
        Ok(())
    }

    pub fn restore(&mut self, action: Action) -> Result<()> {
        self.set(action, action.binding().default)
    }

    pub fn keymap(&self) -> Result<KeyMap> {
        let mut keys = HashMap::new();
        for binding in BINDINGS {
            if let Some(chord) = self.get(binding)
                && let Some(other) = keys.insert(chord, binding.action)
            {
                bail!(
                    "{chord} is assigned to both {} and {}.",
                    binding.label,
                    other.binding().label
                );
            }
        }
        Ok(KeyMap(keys))
    }
}
pub struct KeyMap(HashMap<Chord, Action>);
impl KeyMap {
    pub fn resolve(&self, key: u32, mods: Mods, terminal: bool, right_alt: bool) -> Option<Action> {
        if right_alt {
            return None;
        }
        self.0
            .get(&Chord { key, mods })
            .copied()
            .filter(|action| terminal || !action.requires_terminal())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_roundtrip_and_must_not_steal_altgr_or_editor_input() {
        let defaults = Shortcuts::default();
        let keys = defaults.keymap().unwrap();
        for binding in BINDINGS {
            if let Some(chord) = defaults.get(binding) {
                assert_eq!(chord.to_string().parse::<Chord>().unwrap(), chord);
                assert_eq!(
                    keys.resolve(chord.key, chord.mods, true, false),
                    Some(binding.action)
                );
                assert_eq!(keys.resolve(chord.key, chord.mods, true, true), None);
                assert_eq!(
                    keys.resolve(chord.key, chord.mods | Mods::SUPER, true, false),
                    None
                );
                assert_eq!(
                    keys.resolve(chord.key, chord.mods, false, false).is_none(),
                    binding.action.requires_terminal()
                );
            }
        }
    }

    #[test]
    fn edit_clear_restore_and_conflicts_survive_serialization() {
        let mut shortcuts = Shortcuts::default();
        let f8 = "f8".parse().unwrap();
        shortcuts.set(FocusRight, Some(f8)).unwrap();
        assert!(shortcuts.set(FocusLeft, Some(f8)).is_err());
        shortcuts
            .set(FocusLeft, Some("ctrl + ALT + right".parse().unwrap()))
            .unwrap();
        assert!(shortcuts.restore(FocusRight).is_err());
        shortcuts.set(FocusLeft, None).unwrap();
        let json = serde_json::to_string(&shortcuts).unwrap();
        assert_eq!(json, r#"{"focus_left":null,"focus_right":"F8"}"#);
        let mut saved: Shortcuts = serde_json::from_str(&json).unwrap();
        saved.restore(FocusRight).unwrap();
        assert_eq!(
            saved.keymap().unwrap().resolve(0x25, CTRL_ALT, true, false),
            None
        );
        assert_eq!(
            saved.keymap().unwrap().resolve(0x27, CTRL_ALT, true, false),
            Some(FocusRight)
        );
    }
    #[test]
    fn an_existing_override_wins_over_a_new_default() {
        let existing: Shortcuts =
            serde_json::from_str(r#"{"focus_right":"Ctrl+Shift+P"}"#).unwrap();
        assert_eq!(existing.get(PanePicker.binding()), None);
        assert_eq!(
            existing
                .keymap()
                .unwrap()
                .resolve(0x50, CTRL_SHIFT, true, false),
            Some(FocusRight)
        );
    }
    #[test]
    fn editing_the_hint_binding_releases_its_old_chord() {
        let mut shortcuts = Shortcuts::default();
        let f8 = "F8".parse::<Chord>().unwrap();
        shortcuts.set(KeyboardHints, Some(f8)).unwrap();
        let map = shortcuts.keymap().unwrap();
        assert_eq!(map.resolve(0x48, CTRL_SHIFT, true, false), None);
        assert_eq!(
            map.resolve(f8.key, f8.mods, true, false),
            Some(KeyboardHints)
        );
        assert_eq!(map.resolve(f8.key, f8.mods, false, false), None);
        shortcuts.set(KeyboardHints, None).unwrap();
        assert_eq!(
            shortcuts
                .keymap()
                .unwrap()
                .resolve(f8.key, f8.mods, true, false),
            None
        );
    }
    #[test]
    fn invalid_chords_and_duplicate_saved_bindings_are_reported() {
        for invalid in [
            "t",
            "Shift+A",
            "Ctrl+Ctrl+Right",
            "Ctrl+",
            "Ctrl+V",
            "F11",
            "Alt+F4",
            "Win+K",
            "Ctrl+Shift+F",
            "Alt+Tab",
            "F30",
        ] {
            assert!(invalid.parse::<Chord>().is_err(), "{invalid}");
        }
        let shortcuts: Shortcuts =
            serde_json::from_str(r#"{"focus_right":"F8","focus_left":"F8"}"#).unwrap();
        assert!(shortcuts.keymap().is_err());
    }
}
