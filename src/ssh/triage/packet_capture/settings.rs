use super::PacketCaptureOptions;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{fs::File, io::Read, path::Path};

const SETTINGS_VERSION: u8 = 1;
const MAX_SETTINGS_BYTES: u64 = 16 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    schema_version: u8,
    options: PacketCaptureOptions,
}

impl PacketCaptureOptions {
    pub fn load_settings(path: &Path) -> Result<Self> {
        let file = File::open(path).context("Opening packet capture settings")?;
        ensure!(
            file.metadata()?.is_file(),
            "Choose a packet capture settings file"
        );
        let mut bytes = Vec::new();
        file.take(MAX_SETTINGS_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("Reading packet capture settings")?;
        ensure!(
            bytes.len() as u64 <= MAX_SETTINGS_BYTES,
            "Packet capture settings exceed 16 KiB"
        );
        let settings: Settings =
            serde_json::from_slice(&bytes).context("Reading packet capture settings JSON")?;
        ensure!(
            settings.schema_version == SETTINGS_VERSION,
            "Unsupported packet capture settings version {}",
            settings.schema_version
        );
        settings.options.validate()
    }

    pub fn settings_json(self) -> Result<String> {
        let settings = Settings {
            schema_version: SETTINGS_VERSION,
            options: self.validate()?,
        };
        let mut json = serde_json::to_string_pretty(&settings)?;
        json.push('\n');
        Ok(json)
    }

    pub fn save_settings(self, path: &Path) -> Result<()> {
        crate::transcript::save(path, &self.settings_json()?, false)
            .context("Saving new packet capture settings")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::fs;

    #[test]
    fn validates_version_size_and_capture_options_before_loading() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut saved = json!({
            "schema_version": 1,
            "options": {
                "interface": "lo", "filter": "udp and port 4321", "snap_length": 128,
                "seconds": 10, "max_mib": 2, "packet_limit": null,
                "scratch_directory": "/tmp", "require_tmpfs": false
            }
        });
        let load = |document: &Value| {
            fs::write(&path, serde_json::to_vec(document).unwrap()).unwrap();
            PacketCaptureOptions::load_settings(&path)
        };
        assert_eq!(load(&saved).unwrap().snap_length, 128);
        saved["options"]["seconds"] = json!(61);
        assert!(
            load(&saved)
                .unwrap_err()
                .to_string()
                .contains("1..=60 seconds")
        );
        saved["options"]["seconds"] = json!(10);
        saved["schema_version"] = json!(2);
        assert!(load(&saved).unwrap_err().to_string().contains("version 2"));
        saved["schema_version"] = json!(1);
        saved["review_id"] = json!("previous review");
        assert!(load(&saved).is_err());
        fs::write(&path, vec![b' '; MAX_SETTINGS_BYTES as usize + 1]).unwrap();
        assert!(
            PacketCaptureOptions::load_settings(&path)
                .unwrap_err()
                .to_string()
                .contains("16 KiB")
        );
    }
}
