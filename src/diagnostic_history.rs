use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, TryLockError},
    io::{ErrorKind, Read},
    path::PathBuf,
    thread,
    time::Duration,
};

const MAX_RECENT: usize = 12;
const MAX_PINNED: usize = 12;
const MAX_PATHS: usize = 256;
const MAX_ENTRY_BYTES: usize = 512 * 1024;
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const LABEL: &str = "recent diagnostics files";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    TriageReport,
    CpuProfile,
    PacketCapture,
    SyscallTrace,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub kind: Kind,
    pub paths: Vec<PathBuf>,
}

impl Artifact {
    fn valid(&self) -> bool {
        let count = self.paths.len();
        count > 0
            && count
                <= if self.kind == Kind::SyscallTrace {
                    MAX_PATHS
                } else {
                    1
                }
            && self.paths.iter().all(|path| path.is_absolute())
    }
}

#[derive(Serialize, Deserialize)]
struct Catalog {
    version: u8,
    entries: Vec<Entry>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    #[serde(flatten)]
    pub artifact: Artifact,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
}

fn pinned_count(entries: &[Entry]) -> usize {
    entries.iter().take_while(|entry| entry.pinned).count()
}

pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn user() -> Result<Self> {
        Ok(Self {
            path: crate::keys::data_dir()?.join("recent-diagnostics.json"),
        })
    }

    fn load(&self) -> Result<Catalog> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(Catalog {
                    version: 1,
                    entries: Vec::new(),
                });
            }
            Err(error) => return Err(error).context("Opening recent diagnostics files"),
        };
        let mut bytes = Vec::new();
        file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_FILE_BYTES,
            "Recent diagnostics file is too large"
        );
        let catalog: Catalog =
            serde_json::from_slice(&bytes).context("Reading recent diagnostics files")?;
        let pinned = pinned_count(&catalog.entries);
        let recent = &catalog.entries[pinned..];
        ensure!(
            catalog.version == 1
                && pinned <= MAX_PINNED
                && recent.len() <= MAX_RECENT
                && recent.iter().all(|entry| !entry.pinned)
                && catalog.entries.iter().all(|entry| entry.artifact.valid()),
            "Recent diagnostics file has invalid entries",
        );
        for entry in &catalog.entries {
            ensure!(
                serde_json::to_vec_pretty(&entry.artifact)?.len() <= MAX_ENTRY_BYTES,
                "Recent diagnostics file has an oversized entry",
            );
        }
        Ok(catalog)
    }

    pub fn list(&self) -> Result<Vec<Entry>> {
        Ok(self.load()?.entries)
    }

    pub fn remember(&self, artifact: &Artifact) -> Result<Vec<Entry>> {
        ensure!(
            artifact.valid(),
            "Recent diagnostics requires absolute artifact paths"
        );
        ensure!(
            serde_json::to_vec_pretty(artifact)?.len() <= MAX_ENTRY_BYTES,
            "The artifact has too many path bytes to remember",
        );
        self.update(|entries| {
            if let Some(index) = entries.iter().position(|entry| entry.artifact == *artifact) {
                if entries[index].pinned {
                    return Ok(());
                }
                entries.remove(index);
            }
            let pinned = pinned_count(entries);
            entries.insert(
                pinned,
                Entry {
                    artifact: artifact.clone(),
                    pinned: false,
                },
            );
            entries.truncate(pinned + MAX_RECENT);
            Ok(())
        })
    }

    pub fn set_pinned(&self, artifact: &Artifact, pin: bool) -> Result<Vec<Entry>> {
        self.update(|entries| {
            let index = entries
                .iter()
                .position(|entry| entry.artifact == *artifact)
                .context("The diagnostic artifact is no longer in the list")?;
            if entries[index].pinned == pin {
                return Ok(());
            }
            ensure!(
                !pin || pinned_count(entries) < MAX_PINNED,
                "Unpin a diagnostic artifact before pinning another",
            );
            let mut entry = entries.remove(index);
            entry.pinned = pin;
            let index = if pin { 0 } else { pinned_count(entries) };
            entries.insert(index, entry);
            entries.truncate(pinned_count(entries) + MAX_RECENT);
            Ok(())
        })
    }

    pub fn forget(&self, artifact: &Artifact) -> Result<Vec<Entry>> {
        self.update(|entries| {
            entries.retain(|entry| entry.artifact != *artifact);
            Ok(())
        })
    }

    fn update(&self, change: impl Fn(&mut Vec<Entry>) -> Result<()>) -> Result<Vec<Entry>> {
        // Two open windows can finish captures together. Retry contention
        // briefly, and always read the catalog under the write lock.
        for attempt in 0..=20 {
            let result = crate::stored_json::update(&self.path, LABEL, || {
                let mut catalog = self.load()?;
                change(&mut catalog.entries)?;
                Ok(catalog)
            });
            match result {
                Err(error)
                    if attempt < 20
                        && matches!(
                            error.downcast_ref::<TryLockError>(),
                            Some(TryLockError::WouldBlock)
                        ) =>
                {
                    thread::sleep(Duration::from_millis(25));
                }
                result => return result.map(|catalog| catalog.entries),
            }
        }
        unreachable!("the final attempt returns its result")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_whole_trace_sets_and_forgets_only_history() {
        let dir = tempfile::tempdir().unwrap();
        let open = || Store {
            path: dir.path().join("history.json"),
        };
        let first = dir.path().join("run.22");
        let second = dir.path().join("run.21");
        std::fs::write(&first, b"trace").unwrap();
        let artifact = Artifact {
            kind: Kind::SyscallTrace,
            paths: vec![first.clone(), second],
        };
        assert!(open().list().unwrap().is_empty());
        open().remember(&artifact).unwrap();
        open().remember(&artifact).unwrap();
        let entries = open().list().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].artifact, artifact);
        open().forget(&artifact).unwrap();
        assert!(open().list().unwrap().is_empty());
        assert_eq!(std::fs::read(first).unwrap(), b"trace");
    }

    #[test]
    fn simultaneous_windows_keep_entries_from_both_writers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        let artifacts = ["worker.folded", "after.folded"].map(|name| Artifact {
            kind: Kind::CpuProfile,
            paths: vec![dir.path().join(name)],
        });
        let barrier = std::sync::Barrier::new(2);
        thread::scope(|scope| {
            let handles = artifacts.each_ref().map(|artifact| {
                let store = Store { path: path.clone() };
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    store.remember(artifact).unwrap();
                })
            });
            for handle in handles {
                handle.join().unwrap();
            }
        });
        let entries = (Store { path }).list().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(
            artifacts
                .iter()
                .all(|artifact| entries.iter().any(|entry| entry.artifact == *artifact))
        );
    }

    #[test]
    fn legacy_entries_can_be_pinned_and_survive_recent_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store {
            path: dir.path().join("history.json"),
        };
        let baseline = Artifact {
            kind: Kind::CpuProfile,
            paths: vec![dir.path().join("baseline.folded")],
        };
        std::fs::write(
            &store.path,
            serde_json::to_vec(&serde_json::json!({"version":1,"entries":[&baseline]})).unwrap(),
        )
        .unwrap();
        assert!(!store.list().unwrap()[0].pinned);
        store.set_pinned(&baseline, true).unwrap();
        for index in 0..=MAX_RECENT {
            store
                .remember(&Artifact {
                    kind: Kind::CpuProfile,
                    paths: vec![dir.path().join(format!("{index}.folded"))],
                })
                .unwrap();
        }
        let entries = store.remember(&baseline).unwrap();
        assert_eq!(entries.len(), MAX_RECENT + 1);
        assert_eq!(entries[0].artifact, baseline);
        assert!(entries[0].pinned);
        assert_eq!(pinned_count(&entries), 1);
        assert!(entries[1..].iter().all(|entry| !entry.pinned));
        assert!(
            !entries
                .iter()
                .any(|entry| entry.artifact.paths[0].ends_with("0.folded"))
        );

        let entries = store.set_pinned(&baseline, false).unwrap();
        assert_eq!(entries.len(), MAX_RECENT);
        assert_eq!(entries[0].artifact, baseline);
        assert_eq!(pinned_count(&entries), 0);
    }
}
