use super::*;
use std::io::Write;

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

    fn into_build(self) -> UtilityBuild {
        UtilityBuild {
            architecture: self.architecture,
            path: self.path,
            sha256: self.sha256,
            release: None,
            download: None,
        }
    }
}

#[derive(Clone, Default)]
pub struct LocalUtilityBundle {
    tools: Vec<UtilityTool>,
}

impl LocalUtilityBundle {
    pub fn from_tools(tools: Vec<UtilityTool>) -> Self {
        Self { tools }
    }

    pub fn add(&mut self, name: &str, executable: LocalUtilityExecutable) -> Result<()> {
        ensure!(valid_command_name(name), "Invalid command name");
        if let Some(tool) = self.tools.iter_mut().find(|tool| tool.name == name) {
            ensure!(
                tool.builds.len() < 4,
                "A command can contain up to 4 builds"
            );
            ensure!(
                !tool.builds.iter().any(|build| {
                    build.architecture == executable.architecture
                        && build.sha256.eq_ignore_ascii_case(&executable.sha256)
                }),
                "This executable is already pinned for this command"
            );
            tool.builds.push(executable.into_build());
        } else {
            ensure!(
                self.tools.len() < 16,
                "A bundle can contain up to 16 commands"
            );
            self.tools.push(UtilityTool {
                name: name.into(),
                builds: vec![executable.into_build()],
            });
        }
        Ok(())
    }

    pub fn replace(
        &mut self,
        name: &str,
        index: usize,
        executable: LocalUtilityExecutable,
    ) -> Result<()> {
        let build = self
            .tools
            .iter_mut()
            .find(|tool| tool.name == name)
            .and_then(|tool| tool.builds.get_mut(index))
            .context("The selected build is no longer available")?;
        ensure!(
            build.architecture == executable.architecture,
            "Choose a replacement executable with the same architecture"
        );
        *build = executable.into_build();
        Ok(())
    }

    pub fn remove(&mut self, name: &str, index: usize) {
        if let Some(tool) = self.tools.iter_mut().find(|tool| tool.name == name)
            && index < tool.builds.len()
        {
            tool.builds.remove(index);
        }
        self.tools.retain(|tool| !tool.builds.is_empty());
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn tools(&self) -> &[UtilityTool] {
        &self.tools
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
            tools: self.tools.clone(),
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
