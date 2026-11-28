//! Curated terminal font choices and per-user appearance preferences.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};
use windows::{
    Win32::Graphics::DirectWrite::{
        DWRITE_FACTORY_TYPE_SHARED, DWriteCreateFactory, IDWriteFactory,
    },
    core::{BOOL, w},
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefaultCursorStyle {
    #[default]
    SteadyBlock,
    SteadyBar,
    SteadyUnderline,
    BlinkingBlock,
    BlinkingBar,
    BlinkingUnderline,
}

impl DefaultCursorStyle {
    pub const CHOICES: [(Self, &'static str); 6] = [
        (Self::SteadyBlock, "Steady block"),
        (Self::SteadyBar, "Steady bar"),
        (Self::SteadyUnderline, "Steady underline"),
        (Self::BlinkingBlock, "Blinking block"),
        (Self::BlinkingBar, "Blinking bar"),
        (Self::BlinkingUnderline, "Blinking underline"),
    ];

    pub fn shape(self) -> termwiz::surface::CursorShape {
        use termwiz::surface::CursorShape;
        match self {
            Self::SteadyBlock => CursorShape::Default,
            Self::SteadyBar => CursorShape::SteadyBar,
            Self::SteadyUnderline => CursorShape::SteadyUnderline,
            Self::BlinkingBlock => CursorShape::BlinkingBlock,
            Self::BlinkingBar => CursorShape::BlinkingBar,
            Self::BlinkingUnderline => CursorShape::BlinkingUnderline,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorGuide {
    #[default]
    Off,
    On,
    FollowApplications,
}

impl CursorGuide {
    pub const CHOICES: [(Self, &'static str); 3] = [
        (Self::Off, "Off"),
        (Self::On, "On"),
        (Self::FollowApplications, "Follow applications"),
    ];

    pub fn shown(self, application_requested: bool) -> bool {
        match self {
            Self::Off => false,
            Self::On => true,
            Self::FollowApplications => application_requested,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub font_family: String,
    pub ligatures: bool,
    pub font_size: f32,
    pub follow_windows_text_size: bool,
    pub default_cursor_style: DefaultCursorStyle,
    pub cursor_guide: CursorGuide,
    #[serde(skip, default = "default_text_scale")]
    windows_text_scale: f32,
    pub interaction: crate::interaction::Interaction,
    pub shortcuts: crate::shortcuts::Shortcuts,
    #[serde(skip_serializing_if = "crate::hints::Patterns::is_empty")]
    pub hint_patterns: crate::hints::Patterns,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}
impl Default for Appearance {
    fn default() -> Self {
        Self {
            font_family: "Cascadia Mono".into(),
            ligatures: true,
            font_size: 16.0,
            follow_windows_text_size: true,
            default_cursor_style: DefaultCursorStyle::default(),
            cursor_guide: CursorGuide::default(),
            windows_text_scale: default_text_scale(),
            interaction: crate::interaction::Interaction::default(),
            shortcuts: Default::default(),
            hint_patterns: Default::default(),
            extra: BTreeMap::new(),
        }
    }
}
impl Appearance {
    pub fn pane_preferences(&self) -> crate::session::PanePreferences {
        crate::session::PanePreferences {
            clipboard_policy: self.interaction.remote_clipboard_policy(),
            default_cursor_shape: self.default_cursor_style.shape(),
        }
    }

    pub fn set_windows_text_scale(&mut self, factor: f64) {
        self.windows_text_scale = if (1.0..=2.25).contains(&factor) {
            factor as f32
        } else {
            default_text_scale()
        };
    }

    pub fn terminal_font_size(&self) -> f32 {
        self.terminal_font_size_for(None)
    }

    pub fn terminal_font_size_for(&self, base: Option<f32>) -> f32 {
        let size = base.unwrap_or(self.font_size);
        if self.follow_windows_text_size {
            size * self.windows_text_scale
        } else {
            size
        }
    }

    pub fn path() -> Result<PathBuf> {
        if let Some(path) = std::env::var_os("W11TERM_SETTINGS_PATH")
            && crate::keys::environment_overrides_allowed()?
        {
            return std::path::absolute(path).context("Resolving settings path");
        }
        Ok(crate::keys::data_dir()?.join("settings.json"))
    }
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let settings: Self =
                    serde_json::from_slice(&bytes).context("Reading appearance settings")?;
                settings
                    .shortcuts
                    .keymap()
                    .context("Checking keyboard shortcuts")?;
                Ok(settings)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .context("Settings path has no parent directory")?;
        std::fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut file, self)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(path).context("Saving appearance settings")?;
        Ok(())
    }
}

fn default_text_scale() -> f32 {
    1.0
}

fn family_order(name: &str) -> Option<usize> {
    let normalized: String = name
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .flat_map(char::to_lowercase)
        .collect();
    match normalized.as_str() {
        "cascadiacode" => Some(0),
        "cascadiamono" => Some(1),
        "consolas" => Some(2),
        "jetbrainsmono" => Some(3),
        name if name.starts_with("monaspace") => Some(4),
        "firacode" => Some(5),
        _ => None,
    }
}

pub fn installed_fonts() -> Result<Vec<String>> {
    // SAFETY: DirectWrite owns its font collection; all indices and output buffers are
    // obtained from that collection and remain on this calling thread.
    unsafe {
        let factory: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let mut collection = None;
        factory.GetSystemFontCollection(&mut collection, true)?;
        let collection = collection.context("DirectWrite returned no font collection")?;
        let mut fonts = Vec::new();
        for index in 0..collection.GetFontFamilyCount() {
            let names = collection.GetFontFamily(index)?.GetFamilyNames()?;
            let mut name_index = 0;
            let mut found = BOOL::default();
            names.FindLocaleName(w!("en-us"), &mut name_index, &mut found)?;
            if !found.as_bool() {
                name_index = 0;
            }
            let mut name = vec![0u16; names.GetStringLength(name_index)? as usize + 1];
            names.GetString(name_index, &mut name)?;
            let name = String::from_utf16(&name[..name.len() - 1])?;
            if family_order(&name).is_some() {
                fonts.push(name);
            }
        }
        fonts.sort_by_key(|name| (family_order(name), name.to_lowercase()));
        fonts.dedup();
        Ok(fonts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_text_size_combines_with_zoom_and_only_saves_the_preference() {
        let mut appearance: Appearance = serde_json::from_str(r#"{"font_size":20.0}"#).unwrap();
        assert!(appearance.follow_windows_text_size);
        assert_eq!(appearance.terminal_font_size(), 20.0);
        appearance.set_windows_text_scale(1.5);
        assert_eq!(appearance.terminal_font_size(), 30.0);
        appearance.font_size = 16.0;
        assert_eq!(appearance.terminal_font_size(), 24.0);
        appearance.follow_windows_text_size = false;
        assert_eq!(appearance.terminal_font_size(), 16.0);
        let saved = serde_json::to_value(&appearance).unwrap();
        assert!(!saved["follow_windows_text_size"].as_bool().unwrap());
        assert!(saved.get("windows_text_scale").is_none());
        let mut restored: Appearance = serde_json::from_value(saved).unwrap();
        restored.follow_windows_text_size = true;
        assert_eq!(restored.terminal_font_size(), 16.0);
        restored.set_windows_text_scale(2.25);
        assert_eq!(restored.terminal_font_size(), 36.0);
        restored.set_windows_text_scale(f64::NAN);
        assert_eq!(restored.terminal_font_size(), 16.0);
    }

    #[test]
    fn curated_families_include_aliases_and_monaspace_variants_only() {
        for name in [
            "Cascadia Code",
            "Cascadia Mono",
            "Consolas",
            "JetBrains Mono",
            "JetBrainsMono",
            "Fira Code",
            "FiraCode",
            "Monaspace Neon",
            "Monaspace Argon Var",
            "MonaspaceKrypton NF",
        ] {
            assert!(family_order(name).is_some(), "{name}");
        }
        for name in [
            "Arial",
            "Courier New",
            "Lucida Console",
            "JetBrains Mono Nerd Font",
            "Cascadia Code PL",
        ] {
            assert!(family_order(name).is_none(), "{name}");
        }
    }
    #[test]
    fn saving_preferences_roundtrips_and_preserves_future_settings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"font_family":"Consolas","interaction":{"copyOnSelect":false},"other_setting":42}"#,
        )
        .unwrap();
        let mut appearance = Appearance::load(&path).unwrap();
        assert!(appearance.ligatures);
        assert!(!appearance.interaction.application_tab_titles);
        assert!(appearance.interaction.failed_command_marks);
        assert!(appearance.interaction.sticky_command_context);
        assert!(!appearance.interaction.visual_bell);
        assert!(!appearance.interaction.copy_on_select);
        appearance.font_family = "Fira Code".into();
        appearance.ligatures = false;
        appearance.interaction.application_tab_titles = true;
        appearance.interaction.failed_command_marks = false;
        appearance.interaction.sticky_command_context = false;
        appearance.interaction.visual_bell = true;
        appearance.save(&path).unwrap();
        let saved = Appearance::load(&path).unwrap();
        assert_eq!(saved.font_family, "Fira Code");
        assert!(!saved.ligatures);
        assert!(saved.interaction.application_tab_titles);
        assert!(!saved.interaction.failed_command_marks);
        assert!(!saved.interaction.sticky_command_context);
        assert!(saved.interaction.visual_bell);
        assert!(!saved.interaction.copy_on_select);
        assert_eq!(saved.extra["other_setting"], 42);
    }
}
