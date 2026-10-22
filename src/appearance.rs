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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub font_family: String,
    pub ligatures: bool,
    pub font_size: f32,
    pub interaction: crate::interaction::Interaction,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}
impl Default for Appearance {
    fn default() -> Self {
        Self {
            font_family: "Cascadia Mono".into(),
            ligatures: true,
            font_size: 16.0,
            interaction: crate::interaction::Interaction::default(),
            extra: BTreeMap::new(),
        }
    }
}
impl Appearance {
    pub fn path() -> Result<PathBuf> {
        if let Some(path) = std::env::var_os("W11TERM_SETTINGS_PATH") {
            return std::path::absolute(path).context("Resolving settings path");
        }
        Ok(
            PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is unavailable")?)
                .join("w11term")
                .join("settings.json"),
        )
    }
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("Reading appearance settings"),
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
        std::fs::write(&path, r#"{"font_family":"Consolas","other_setting":42}"#).unwrap();
        let mut appearance = Appearance::load(&path).unwrap();
        assert!(appearance.ligatures);
        appearance.font_family = "Fira Code".into();
        appearance.ligatures = false;
        appearance.save(&path).unwrap();
        let saved = Appearance::load(&path).unwrap();
        assert_eq!(saved.font_family, "Fira Code");
        assert!(!saved.ligatures);
        assert_eq!(saved.extra["other_setting"], 42);
    }
}
