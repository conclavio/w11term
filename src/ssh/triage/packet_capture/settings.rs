use super::PacketCaptureOptions;
use crate::ssh::triage::capture_settings;
use anyhow::{Context, Result};
use std::path::Path;

const MAX_SETTINGS_KIB: u64 = 16;

impl PacketCaptureOptions {
    pub fn load_settings(path: &Path) -> Result<Self> {
        capture_settings::load::<Self>(path, "packet capture", MAX_SETTINGS_KIB)?.validate()
    }

    pub fn settings_json(self) -> Result<String> {
        capture_settings::json(self.validate()?)
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
        fs::write(&path, vec![b' '; MAX_SETTINGS_KIB as usize * 1024 + 1]).unwrap();
        assert!(
            PacketCaptureOptions::load_settings(&path)
                .unwrap_err()
                .to_string()
                .contains("16 KiB")
        );
    }
}
