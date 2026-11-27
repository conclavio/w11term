use super::*;
use std::{collections::BTreeMap, io::Write};

#[derive(Clone, Debug)]
pub struct LocalUtilityExecutable {
    pub architecture: UtilityArchitecture,
    pub path: PathBuf,
    pub sha256: String,
}

impl LocalUtilityExecutable {
    pub fn inspect(path: &Path) -> Result<Self> {
        let path = std::path::absolute(path)?;
        let mut remaining = build::MAX_BINARY;
        let bytes = build::read_executable(&path, &mut remaining)?;
        let sha256 = digest_hex(Sha256::new_with_prefix(&bytes));
        let (_, architecture) = Verified::inspect_elf(&bytes, sha256.clone(), None)?;
        Ok(Self {
            architecture,
            path,
            sha256,
        })
    }
}

#[derive(Clone, Default)]
pub struct LocalUtilityBundle {
    tools: BTreeMap<String, Vec<LocalUtilityExecutable>>,
}

impl LocalUtilityBundle {
    pub fn add(&mut self, name: &str, executable: LocalUtilityExecutable) -> Result<()> {
        ensure!(valid_command_name(name), "Invalid command name");
        ensure!(
            self.tools.contains_key(name) || self.tools.len() < 16,
            "A bundle can contain up to 16 commands"
        );
        let builds = self.tools.entry(name.to_owned()).or_default();
        ensure!(
            !builds
                .iter()
                .any(|build| build.architecture == executable.architecture),
            "This command already has a build for that architecture"
        );
        builds.push(executable);
        Ok(())
    }

    pub fn remove(&mut self, name: &str, architecture: UtilityArchitecture) {
        if let Some(builds) = self.tools.get_mut(name) {
            builds.retain(|build| build.architecture != architecture);
            if builds.is_empty() {
                self.tools.remove(name);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn executables(&self) -> impl Iterator<Item = (&str, &LocalUtilityExecutable)> {
        self.tools
            .iter()
            .flat_map(|(name, builds)| builds.iter().map(move |build| (name.as_str(), build)))
    }

    pub fn save(&self, destination: &Path, name: &str, bin_directory: &str) -> Result<()> {
        let destination = std::path::absolute(destination)?;
        let parent = destination
            .parent()
            .context("Choose a bundle manifest file")?;
        let mut bundle = UtilityBundle {
            version: 1,
            name: name.into(),
            bin_directory: bin_directory.into(),
            tools: self
                .tools
                .iter()
                .map(|(name, builds)| UtilityTool {
                    name: name.clone(),
                    builds: builds
                        .iter()
                        .map(|build| UtilityBuild {
                            architecture: build.architecture,
                            path: build.path.clone(),
                            sha256: build.sha256.clone(),
                            release: None,
                            download: None,
                        })
                        .collect(),
                })
                .collect(),
        };
        bundle.validate()?;
        for build in bundle.tools.iter_mut().flat_map(|tool| &mut tool.builds) {
            if let Ok(relative) = build.path.strip_prefix(parent) {
                build.path = relative.into();
            }
        }
        let mut bytes = serde_json::to_vec_pretty(&bundle)?;
        bytes.push(b'\n');
        ensure!(
            bytes.len() as u64 <= MAX_MANIFEST,
            "Tool bundle manifest exceeds 32 KiB"
        );
        let mut staged = tempfile::NamedTempFile::new_in(parent)?;
        staged.write_all(&bytes)?;
        staged.as_file().sync_all()?;
        staged
            .persist_noclobber(&destination)
            .map_err(|error| error.error)
            .context("Saving a new tool bundle manifest")?;
        Ok(())
    }
}
