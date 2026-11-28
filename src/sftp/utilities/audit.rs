use super::*;
use std::io::ErrorKind;

#[derive(Clone, Serialize)]
pub struct ToolArchitectureAudit {
    pub architecture: UtilityArchitecture,
    pub locally_available: bool,
    pub commands: Vec<ToolCommandAudit>,
}

#[derive(Clone, Serialize)]
pub struct ToolCommandAudit {
    pub name: String,
    pub locally_available: bool,
    pub builds: Vec<ToolExecutableAudit>,
}

#[derive(Clone, Serialize)]
pub struct ToolExecutableAudit {
    pub source: ToolExecutableSource,
    pub status: ToolExecutableStatus,
    pub pinned_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interpreter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum_glibc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolExecutableSource {
    LocalFile,
    DownloadCache,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolExecutableStatus {
    Verified,
    Missing,
    Error,
}

impl UtilityBundle {
    /// Use the local files and cache through the same verifier as SSH installation.
    pub(crate) fn audit_executables(
        &self,
        selected: Option<UtilityArchitecture>,
    ) -> Vec<ToolArchitectureAudit> {
        [UtilityArchitecture::X86_64, UtilityArchitecture::Aarch64]
            .into_iter()
            .filter(|architecture| match selected {
                Some(selected) => *architecture == selected,
                None => self
                    .tools
                    .iter()
                    .flat_map(|tool| &tool.builds)
                    .any(|build| build.architecture == *architecture),
            })
            .map(|architecture| {
                let mut remaining = MAX_LOCAL_BUILD_BYTES;
                let commands: Vec<_> = self
                    .tools
                    .iter()
                    .map(|tool| {
                        let builds: Vec<_> = tool
                            .builds
                            .iter()
                            .filter(|build| build.architecture == architecture)
                            .map(|build| audit_build(build, &mut remaining))
                            .collect();
                        ToolCommandAudit {
                            name: tool.name.clone(),
                            locally_available: builds
                                .iter()
                                .any(|build| build.status == ToolExecutableStatus::Verified),
                            builds,
                        }
                    })
                    .collect();
                ToolArchitectureAudit {
                    architecture,
                    locally_available: commands.iter().all(|command| command.locally_available),
                    commands,
                }
            })
            .collect()
    }
}

fn audit_build(build: &UtilityBuild, remaining: &mut u64) -> ToolExecutableAudit {
    let cached = build.download.is_some();
    let source = if cached {
        ToolExecutableSource::DownloadCache
    } else {
        ToolExecutableSource::LocalFile
    };
    match Verified::read(build, remaining) {
        Ok((verified, path)) => ToolExecutableAudit {
            source,
            status: ToolExecutableStatus::Verified,
            pinned_sha256: verified.sha256,
            path: Some(path),
            bytes: Some(verified.bytes),
            interpreter: verified.interpreter,
            minimum_glibc: verified.glibc_minimum.map(|version| version.to_string()),
            detail: None,
        },
        Err(error) => ToolExecutableAudit {
            source,
            status: if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == ErrorKind::NotFound)
            {
                ToolExecutableStatus::Missing
            } else {
                ToolExecutableStatus::Error
            },
            pinned_sha256: build.sha256.trim().to_ascii_lowercase(),
            path: (!cached).then(|| build.path.clone()),
            bytes: None,
            interpreter: None,
            minimum_glibc: None,
            detail: Some(format!("{error:#}")),
        },
    }
}
