use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, TryLockError},
    io::{ErrorKind, Read},
    path::PathBuf,
    thread,
    time::Duration,
};

const MAX_ENTRIES: usize = 12;
const MAX_PATHS: usize = 256;
const MAX_ENTRY_BYTES: usize = 512 * 1024;
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
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
    entries: Vec<Artifact>,
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
        ensure!(
            catalog.version == 1
                && catalog.entries.len() <= MAX_ENTRIES
                && catalog.entries.iter().all(Artifact::valid),
            "Recent diagnostics file has invalid entries",
        );
        for artifact in &catalog.entries {
            ensure!(
                serde_json::to_vec_pretty(artifact)?.len() <= MAX_ENTRY_BYTES,
                "Recent diagnostics file has an oversized entry",
            );
        }
        Ok(catalog)
    }

    pub fn list(&self) -> Result<Vec<Artifact>> {
        Ok(self.load()?.entries)
    }

    pub fn remember(&self, artifact: &Artifact) -> Result<Vec<Artifact>> {
        ensure!(
            artifact.valid(),
            "Recent diagnostics requires absolute artifact paths"
        );
        ensure!(
            serde_json::to_vec_pretty(artifact)?.len() <= MAX_ENTRY_BYTES,
            "The artifact has too many path bytes to remember",
        );
        self.update(|entries| {
            entries.retain(|entry| entry != artifact);
            entries.insert(0, artifact.clone());
            entries.truncate(MAX_ENTRIES);
        })
    }

    pub fn forget(&self, artifact: &Artifact) -> Result<Vec<Artifact>> {
        self.update(|entries| entries.retain(|entry| entry != artifact))
    }

    fn update(&self, change: impl Fn(&mut Vec<Artifact>)) -> Result<Vec<Artifact>> {
        // Two open windows can finish captures together. Retry contention
        // briefly, and always read the catalog under the write lock.
        for attempt in 0..=20 {
            let result = crate::stored_json::update(&self.path, LABEL, || {
                let mut catalog = self.load()?;
                change(&mut catalog.entries);
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
        assert_eq!(
            open().list().unwrap().as_slice(),
            std::slice::from_ref(&artifact)
        );
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
        assert!(artifacts.iter().all(|artifact| entries.contains(artifact)));
    }
}
