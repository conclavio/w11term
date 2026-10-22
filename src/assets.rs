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
pub struct Store(Arc<RwLock<Published>>);
#[derive(Default)]
struct Published {
    version: String,
    sounds: BTreeMap<String, PathBuf>,
    // A shared OS lock advertises this generation to other panes/processes.
    // The lease disappears only after its last Store owner releases it.
    _lease: Option<tempfile::NamedTempFile>,
}
async fn disk<T: Send + 'static>(
    lock: Arc<std::fs::File>,
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(move || {
        let _lock = lock;
        work()
    })
    .await?
}
fn prune(dir: &std::path::Path, wanted: &BTreeSet<String>) -> Result<()> {
    let mut keep = wanted.clone();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with("lease-") {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(entry.path())?;
            match file.try_lock() {
                Ok(()) => {
                    drop(file);
                    let _ = std::fs::remove_file(entry.path());
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    keep.extend(serde_json::from_reader::<_, BTreeSet<String>>(file)?);
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.len() == 64
            && name.bytes().all(|c| c.is_ascii_hexdigit())
            && !keep.contains(&name)
            && entry.file_type()?.is_file()
        {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}
impl Store {
    pub fn sound(&self, name: &str) -> Option<PathBuf> {
        self.0.read().unwrap().sounds.get(name).cloned()
    }
    pub fn version(&self) -> String {
        self.0.read().unwrap().version.clone()
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
struct RemoteFile {
    input: mpsc::UnboundedSender<TransportInput>,
    handle: Option<u64>,
}
impl Drop for RemoteFile {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let (reply, _) = oneshot::channel();
            let _ = self.input.send(TransportInput::Sftp(Job {
                operation: Operation::Close { handle },
                deadline: tokio::time::Instant::now() + Duration::from_secs(15),
                reply,
            }));
        }
    }
}
async fn read(
    input: &mpsc::UnboundedSender<TransportInput>,
    path: String,
    limit: usize,
) -> Result<Vec<u8>> {
    let info = request(input, Operation::Info).await?;
    let chunk_size = info["read_chunk"]
        .as_u64()
        .context("missing negotiated read size")?
        .min(16 * 1024) as usize;
    ensure!(chunk_size > 0, "unusable negotiated read size");
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
    let mut guard = RemoteFile {
        input: input.clone(),
        handle: Some(handle),
    };
    let result = async {
        let mut bytes = Vec::new();
        loop {
            let len = (limit + 1 - bytes.len()).min(chunk_size) as u32;
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
    guard.handle = None;
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
    let dir = cache.join(digest(endpoint.as_bytes()));
    let lock_dir = dir.clone();
    let lock = Arc::new(
        tokio::task::spawn_blocking(move || -> Result<std::fs::File> {
            std::fs::create_dir_all(&lock_dir)?;
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(lock_dir.join("sync.lock"))?;
            file.lock()?;
            Ok(file)
        })
        .await??,
    );
    let bytes = read(&input, "/assets/manifest.json".into(), 256 * 1024).await?;
    let manifest: Manifest = serde_json::from_slice(&bytes).context("invalid asset manifest")?;
    manifest.validate(&version)?;
    let wanted: BTreeSet<String> = manifest.files.iter().map(|f| f.sha256.clone()).collect();
    let cleanup_dir = dir.clone();
    let wanted_before = wanted.clone();
    disk(lock.clone(), move || prune(&cleanup_dir, &wanted_before)).await?;
    let mut sounds = BTreeMap::new();
    let mut fetched = 0;
    for file in &manifest.files {
        let path = dir.join(&file.sha256);
        let check_path = path.clone();
        let check = file.clone();
        let valid = disk(lock.clone(), move || {
            let valid = std::fs::metadata(&check_path).is_ok_and(|m| m.len() == check.size)
                && std::fs::read(&check_path).is_ok_and(|b| {
                    digest(&b) == check.sha256
                        && (check.kind != "sound" || validate_wav(&b).is_ok())
                });
            Ok(valid)
        })
        .await?;
        if !valid {
            let bytes = read(&input, format!("/assets/{}", file.path), file.size as usize).await?;
            let check = file.clone();
            let target = path.clone();
            let parent = dir.clone();
            disk(lock.clone(), move || {
                ensure!(
                    bytes.len() as u64 == check.size && digest(&bytes) == check.sha256,
                    "asset size/hash mismatch for {}",
                    check.name
                );
                if check.kind == "sound" {
                    validate_wav(&bytes)?;
                }
                let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
                temporary.write_all(&bytes)?;
                temporary.as_file().sync_all()?;
                temporary.persist(target)?;
                Ok(())
            })
            .await?;
            fetched += 1;
        }
        if file.kind == "sound" {
            sounds.insert(file.name.clone(), path);
        }
    }
    let publish_dir = dir.clone();
    let lease = disk(lock.clone(), move || {
        let mut lease = tempfile::Builder::new()
            .prefix("lease-")
            .tempfile_in(&publish_dir)?;
        serde_json::to_writer(&mut lease, &wanted)?;
        lease.flush()?;
        lease.as_file().lock_shared()?;
        Ok(lease)
    })
    .await?;
    // Every asset kind remains leased until a complete replacement is published.
    *store.0.write().unwrap() = Published {
        version,
        sounds,
        _lease: Some(lease),
    };
    disk(lock, move || prune(&dir, &BTreeSet::new())).await?;
    Ok(fetched)
}
pub fn validate_wav(bytes: &[u8]) -> Result<()> {
    crate::wav::validate(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelling_an_asset_read_enqueues_close() {
        let (input, mut jobs) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move { read(&input, "/assets/test".into(), 100).await });
        let Some(TransportInput::Sftp(job)) = jobs.recv().await else {
            panic!()
        };
        assert!(matches!(job.operation, Operation::Info));
        job.reply
            .send(Ok(serde_json::json!({"read_chunk":512})))
            .unwrap();
        let Some(TransportInput::Sftp(job)) = jobs.recv().await else {
            panic!()
        };
        job.reply.send(Ok(serde_json::json!({"handle":7}))).unwrap();
        let Some(TransportInput::Sftp(job)) = jobs.recv().await else {
            panic!()
        };
        assert!(matches!(
            job.operation,
            Operation::Read {
                handle: 7,
                len: 101,
                ..
            }
        ));
        task.abort();
        let _ = task.await;
        let Some(TransportInput::Sftp(close)) = jobs.recv().await else {
            panic!()
        };
        assert!(matches!(close.operation, Operation::Close { handle: 7 }));
    }
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
