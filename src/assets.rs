//! Version 1 named asset pack, fetched over the existing authenticated SFTP actor.
use crate::{
    sftp::{Attributes, Job, Operation},
    terminal::TransportInput,
};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};

pub const MAX_FILE: usize = 8 * 1024 * 1024;
pub const MAX_PACK: u64 = 64 * 1024 * 1024;
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct File {
    pub name: String,
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub kind: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub pack_version: String,
    pub files: Vec<File>,
}
pub fn name(value: &str) -> bool {
    let ordinary = !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c));
    if !ordinary {
        return false;
    }
    let upper = value.to_ascii_uppercase();
    !matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        && !(upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}
impl Manifest {
    pub fn validate(&self, expected: &str) -> Result<()> {
        ensure!(self.version == 1, "unsupported asset manifest version");
        ensure!(
            name(&self.pack_version) && self.pack_version == expected,
            "asset pack version differs from announcement"
        );
        ensure!(self.files.len() <= 256, "asset manifest exceeds 256 files");
        let mut names = BTreeSet::new();
        let mut paths = BTreeSet::new();
        let mut total = 0u64;
        for file in &self.files {
            ensure!(
                name(&file.name) && names.insert(file.name.clone()),
                "invalid or duplicate asset name"
            );
            ensure!(
                matches!(file.kind.as_str(), "sound" | "sprite" | "font" | "help"),
                "unsupported asset kind"
            );
            ensure!(
                file.path.len() <= 200
                    && file.path.split('/').count() >= 2
                    && file.path.split('/').all(|p| !p.is_empty()
                        && p != "."
                        && p != ".."
                        && p.bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"_- .".contains(&c)))
                    && paths.insert(file.path.clone()),
                "invalid or duplicate asset path"
            );
            ensure!(
                file.kind != "sound" || file.path.ends_with(".wav"),
                "sound assets must be WAV files"
            );
            ensure!(
                file.kind != "sound" || !matches!(file.name.as_str(), "system" | "off"),
                "reserved sound name"
            );
            ensure!(
                file.sha256.len() == 64
                    && file
                        .sha256
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
                "invalid SHA256"
            );
            ensure!(
                file.size > 0 && file.size <= MAX_FILE as u64,
                "asset exceeds 8 MiB limit"
            );
            total += file.size;
        }
        ensure!(total <= MAX_PACK, "asset pack exceeds 64 MiB limit");
        Ok(())
    }
}
#[derive(Clone, Default)]
pub struct Store(Arc<RwLock<(String, BTreeMap<String, PathBuf>)>>);
impl Store {
    pub fn sound(&self, name: &str) -> Option<PathBuf> {
        self.0.read().unwrap().1.get(name).cloned()
    }
    pub fn version(&self) -> String {
        self.0.read().unwrap().0.clone()
    }
}
async fn request(
    input: &mpsc::UnboundedSender<TransportInput>,
    operation: Operation,
) -> Result<serde_json::Value> {
    let (reply, response) = oneshot::channel();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    input
        .send(TransportInput::Sftp(Job {
            operation,
            deadline,
            reply,
        }))
        .context("SSH session closed")?;
    tokio::time::timeout_at(deadline + Duration::from_secs(1), response)
        .await
        .context("asset SFTP timed out")??
        .map_err(anyhow::Error::msg)
}
async fn read(
    input: &mpsc::UnboundedSender<TransportInput>,
    path: String,
    limit: usize,
) -> Result<Vec<u8>> {
    let opened = request(
        input,
        Operation::Open {
            path,
            flags: vec!["read".into()],
            attrs: Attributes::default(),
        },
    )
    .await?;
    let handle = opened["handle"].as_u64().context("missing SFTP handle")?;
    let result = async {
        let mut bytes = Vec::new();
        loop {
            let len = (limit + 1 - bytes.len()).min(16 * 1024) as u32;
            let value = request(
                input,
                Operation::Read {
                    handle,
                    offset: bytes.len() as u64,
                    len,
                },
            )
            .await?;
            if value["eof"].as_bool() == Some(true) {
                break;
            }
            let chunk =
                STANDARD.decode(value["data_base64"].as_str().context("invalid SFTP data")?)?;
            ensure!(!chunk.is_empty(), "SFTP returned an empty non-EOF read");
            bytes.extend_from_slice(&chunk);
            ensure!(
                bytes.len() <= limit,
                "asset exceeds declared size or read limit"
            );
            // At most 1 MiB/s of background asset traffic; PTY rendering stays responsive.
            tokio::time::sleep(Duration::from_millis(16)).await;
        }
        Ok(bytes)
    }
    .await;
    let closed = request(input, Operation::Close { handle }).await;
    let bytes = result?;
    closed?;
    Ok(bytes)
}
pub async fn sync(
    input: mpsc::UnboundedSender<TransportInput>,
    endpoint: String,
    version: String,
    store: Store,
) -> Result<usize> {
    sync_into(
        input,
        endpoint,
        version,
        store,
        crate::keys::data_dir()?.join("assets"),
    )
    .await
}
pub async fn sync_into(
    input: mpsc::UnboundedSender<TransportInput>,
    endpoint: String,
    version: String,
    store: Store,
    cache: PathBuf,
) -> Result<usize> {
    ensure!(name(&version), "invalid asset pack announcement");
    let bytes = read(&input, "/assets/manifest.json".into(), 256 * 1024).await?;
    let manifest: Manifest = serde_json::from_slice(&bytes).context("invalid asset manifest")?;
    manifest.validate(&version)?;
    let dir = cache.join(digest(endpoint.as_bytes()));
    std::fs::create_dir_all(&dir)?;
    let old: BTreeSet<_> = store.0.read().unwrap().1.values().cloned().collect();
    let wanted: BTreeSet<_> = manifest.files.iter().map(|f| f.sha256.as_str()).collect();
    // Prune abandoned staged packs too, so repeatedly failing syncs cannot fill disk.
    for entry in std::fs::read_dir(&dir)?.flatten() {
        let filename = entry.file_name();
        let filename = filename.to_string_lossy();
        if filename.len() == 64
            && filename.bytes().all(|c| c.is_ascii_hexdigit())
            && !wanted.contains(filename.as_ref())
            && !old.contains(&entry.path())
            && entry.file_type()?.is_file()
        {
            std::fs::remove_file(entry.path())?;
        }
    }
    let mut sounds = BTreeMap::new();
    let mut fetched = 0;
    for file in &manifest.files {
        let path = dir.join(&file.sha256);
        let valid = std::fs::metadata(&path).is_ok_and(|m| m.len() == file.size)
            && std::fs::read(&path).is_ok_and(|b| digest(&b) == file.sha256);
        if !valid {
            let bytes = read(&input, format!("/assets/{}", file.path), file.size as usize).await?;
            ensure!(
                bytes.len() as u64 == file.size && digest(&bytes) == file.sha256,
                "asset size/hash mismatch for {}",
                file.name
            );
            if file.kind == "sound" {
                validate_wav(&bytes)?;
            }
            let mut temporary = tempfile::NamedTempFile::new_in(&dir)?;
            temporary.write_all(&bytes)?;
            temporary.as_file().sync_all()?;
            temporary.persist(&path)?;
            fetched += 1;
        }
        if file.kind == "sound" {
            validate_wav(&std::fs::read(&path)?)?;
            sounds.insert(file.name.clone(), path);
        }
    }
    // Publish only a complete, verified generation. A failed download keeps the old pack usable.
    *store.0.write().unwrap() = (version, sounds);
    let keep: BTreeSet<_> = manifest.files.iter().map(|f| f.sha256.as_str()).collect();
    for entry in std::fs::read_dir(&dir)?.flatten() {
        let filename = entry.file_name();
        let filename = filename.to_string_lossy();
        if filename.len() == 64
            && filename.bytes().all(|c| c.is_ascii_hexdigit())
            && !keep.contains(filename.as_ref())
            && entry.file_type()?.is_file()
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(fetched)
}
pub fn validate_wav(bytes: &[u8]) -> Result<()> {
    ensure!(
        bytes.len() >= 44 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
        "sound is not a RIFF/WAVE file"
    );
    let mut offset = 12;
    let mut format = false;
    let mut data = false;
    let mut byte_rate = 0;
    let mut audio_bytes = 0;
    while offset + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let start = offset + 8;
        ensure!(size <= bytes.len() - start, "invalid WAV chunk size");
        if &bytes[offset..offset + 4] == b"fmt " {
            ensure!(size >= 16, "invalid WAV format");
            let value = |i| u16::from_le_bytes([bytes[start + i], bytes[start + i + 1]]);
            let rate = u32::from_le_bytes(bytes[start + 4..start + 8].try_into().unwrap());
            ensure!(
                value(0) == 1
                    && (1..=2).contains(&value(2))
                    && (8000..=96000).contains(&rate)
                    && matches!(value(14), 8 | 16),
                "only mono/stereo PCM WAV, 8/16 bit, 8–96 kHz is supported"
            );
            format = true;
            byte_rate = rate as usize * value(2) as usize * value(14) as usize / 8;
        }
        if &bytes[offset..offset + 4] == b"data" {
            data = size > 0;
            audio_bytes += size;
        }
        offset = start + size + (size & 1);
    }
    ensure!(format && data, "WAV is missing format or audio data");
    ensure!(
        audio_bytes <= byte_rate * 5,
        "sound exceeds five-second duration limit"
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_paths_versions_hashes_and_duplicate_names() {
        let file = File {
            name: "shot".into(),
            path: "sounds/shot.wav".into(),
            sha256: "a".repeat(64),
            size: 44,
            kind: "sound".into(),
        };
        let mut m = Manifest {
            version: 1,
            pack_version: "pack_1".into(),
            files: vec![file.clone()],
        };
        assert!(m.validate("pack_1").is_ok());
        assert!(m.validate("other").is_err());
        for path in [
            "../shot.wav",
            "/sounds/shot.wav",
            "sounds/../shot.wav",
            "sounds\\shot.wav",
            "https://a/shot.wav",
            "sounds/C:shot.wav",
        ] {
            m.files[0].path = path.into();
            assert!(m.validate("pack_1").is_err(), "{path}");
        }
        m.files = vec![file.clone(), file];
        assert!(m.validate("pack_1").is_err());
        assert!(!name("../shot"));
        assert!(!name("file.wav"));
        for device in ["CON", "nul", "Com1", "LPT9"] {
            assert!(
                !name(device),
                "Windows device names must never reach sound file lookup"
            );
        }
    }
}
