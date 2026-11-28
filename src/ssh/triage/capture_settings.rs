use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{fs::File, io::Read, path::Path};

const SETTINGS_VERSION: u8 = 1;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Settings<T> {
    schema_version: u8,
    options: T,
}

pub(super) fn load<T: DeserializeOwned>(path: &Path, kind: &str, max_kib: u64) -> Result<T> {
    let file = File::open(path).with_context(|| format!("Opening {kind} settings"))?;
    ensure!(file.metadata()?.is_file(), "Choose a {kind} settings file");
    let max_bytes = max_kib * 1024;
    let mut bytes = Vec::new();
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("Reading {kind} settings"))?;
    ensure!(
        bytes.len() as u64 <= max_bytes,
        "The {kind} settings exceed {max_kib} KiB"
    );
    let settings: Settings<T> =
        serde_json::from_slice(&bytes).with_context(|| format!("Reading {kind} settings JSON"))?;
    ensure!(
        settings.schema_version == SETTINGS_VERSION,
        "Unsupported {kind} settings version {}",
        settings.schema_version
    );
    Ok(settings.options)
}

pub(super) fn json<T: Serialize>(options: T) -> Result<String> {
    let mut json = serde_json::to_string_pretty(&Settings {
        schema_version: SETTINGS_VERSION,
        options,
    })?;
    json.push('\n');
    Ok(json)
}
