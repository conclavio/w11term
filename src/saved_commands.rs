use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const MAX_COMMANDS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Entry {
    pub name: String,
    pub text: String,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(try_from = "Vec<Entry>")]
pub struct Commands(Vec<Entry>);

fn single_line(value: &str) -> bool {
    !value
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
}

pub fn valid_command_text(text: &str) -> bool {
    text.len() <= 4096 && !text.trim().is_empty() && single_line(text)
}

impl TryFrom<Vec<Entry>> for Commands {
    type Error = anyhow::Error;

    fn try_from(entries: Vec<Entry>) -> Result<Self> {
        let mut commands = Self::default();
        for entry in entries {
            commands.change(None, Some(entry))?;
        }
        Ok(commands)
    }
}

impl Commands {
    pub fn entries(&self) -> &[Entry] {
        &self.0
    }

    pub fn path() -> Result<PathBuf> {
        Ok(crate::keys::data_dir()?.join("saved-commands.json"))
    }

    pub fn load(path: &Path) -> Result<Self> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("Reading saved commands"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).context("Opening saved commands"),
        }
    }

    fn change(&mut self, original: Option<&Entry>, new: Option<Entry>) -> Result<()> {
        let index = original
            .map(|entry| {
                self.0.iter().position(|current| current == entry)
                    .context("This command changed in another window. Reopen the editor to see the latest version.")
            })
            .transpose()?;
        let Some(mut entry) = new else {
            let index = index.context("Choose a command to remove.")?;
            self.0.remove(index);
            return Ok(());
        };
        ensure!(
            single_line(&entry.name)
                && !entry.name.trim().is_empty()
                && entry.name.trim().chars().count() <= 80,
            "Enter a single-line command name of 1–80 characters."
        );
        entry.name = entry.name.trim().to_owned();
        ensure!(
            valid_command_text(&entry.text),
            "Enter one command line of up to 4,096 UTF-8 bytes, without tabs or control characters."
        );
        let name = entry.name.to_lowercase();
        ensure!(
            !self
                .0
                .iter()
                .enumerate()
                .any(|(i, other)| { Some(i) != index && other.name.to_lowercase() == name }),
            "A command named {} already exists.",
            entry.name
        );
        if let Some(index) = index {
            self.0[index] = entry;
        } else {
            ensure!(
                self.0.len() < MAX_COMMANDS,
                "Up to {MAX_COMMANDS} commands can be saved."
            );
            self.0.push(entry);
        }
        Ok(())
    }

    /// Read and update under the same lock so edits from different windows compose.
    pub fn update(path: &Path, original: Option<&Entry>, new: Option<Entry>) -> Result<Self> {
        crate::stored_json::update(path, "commands", || {
            let mut commands = Self::load(path)?;
            commands.change(original, new)?;
            Ok(commands)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, text: &str) -> Entry {
        Entry {
            name: name.into(),
            text: text.into(),
        }
    }

    #[test]
    fn merges_independent_updates_and_rejects_a_stale_edit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("commands.json");
        let first = entry(" Fleet ", "  Get-Date ");
        let saved = Commands::update(&path, None, Some(first)).unwrap();
        let original = &saved.entries()[0];
        assert_eq!(original, &entry("Fleet", "  Get-Date "));
        let second = entry("Disk", "df -h");
        Commands::update(&path, None, Some(second.clone())).unwrap();
        let replacement = entry("Status", "uptime");
        Commands::update(&path, Some(original), Some(replacement.clone())).unwrap();
        assert!(Commands::update(&path, Some(original), None).is_err());
        assert!(Commands::update(&path, None, Some(entry("status", "echo duplicate"))).is_err());
        assert_eq!(
            Commands::load(&path).unwrap().entries(),
            &[replacement.clone(), second.clone()]
        );
        assert_eq!(
            Commands::update(&path, Some(&second), None)
                .unwrap()
                .entries(),
            &[replacement]
        );
    }

    #[test]
    fn rejects_controls_and_line_separators_on_edit_and_load() {
        for text in [
            "x\r",
            "x\n",
            "x\t",
            "x\x1b[201~",
            "x\u{85}",
            "x\u{2028}",
            "x\u{2029}",
        ] {
            assert!(
                Commands::default()
                    .change(None, Some(entry("Name", text)))
                    .is_err()
            );
            let json = serde_json::to_string(&[entry("Name", text)]).unwrap();
            assert!(serde_json::from_str::<Commands>(&json).is_err());
        }
    }
}
