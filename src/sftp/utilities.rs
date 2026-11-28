use super::dotfiles::under_home;
use super::*;
use std::{collections::BTreeSet, io::Read};

mod build;
mod downloads;
mod install;
mod local;
mod login;
use build::{LibcVersion, Verified};
pub use downloads::{
    ToolBundlePreset, UtilityDownload, builtin_tool_bundle, builtin_tool_names,
    export_builtin_tool_bundle, fetch_tool_bundle, fetch_tool_bundle_cancellable,
    prepare_builtin_tool_bundle,
};
pub(super) use install::with_lock;
use install::{InstallPlan, Release, StoredTool};
pub use install::{InstalledTool, InstalledTools, RetainedToolVersion, ToolBundleVersions};
pub use local::{LocalUtilityBundle, LocalUtilityExecutable};
pub(crate) use login::sync_login_tools;

const DEFAULT_BIN: &str = ".local/bin";
const STORE: &str = ".local/share/w11term/tools";
const MAX_MANIFEST: u64 = 32 * 1024;
const MAX_LOCAL_BUILD_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PROBE: usize = 16 * 1024;
const PROBE_PREFIX: &str = "w11term-tools-v1";
const PROBE: &str = r#"/bin/sh -c 'printf "%s\n" w11term-tools-v1; command -p uname -s && command -p uname -m || exit 1; printf "%s\n" "$HOME" "$PATH" "$(command -p getconf GNU_LIBC_VERSION 2>/dev/null || :)"'"#;

#[derive(Clone, Copy, PartialEq, Eq)]
enum InstallMode {
    Replace,
    Add,
    Sync,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UtilityArchitecture {
    X86_64,
    Aarch64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UtilityBundle {
    pub version: u8,
    pub name: String,
    #[serde(default = "default_bin")]
    pub bin_directory: String,
    pub tools: Vec<UtilityTool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UtilityTool {
    pub name: String,
    pub builds: Vec<UtilityBuild>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UtilityBuild {
    pub architecture: UtilityArchitecture,
    #[serde(default, skip_serializing_if = "empty_path")]
    pub path: PathBuf,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<UtilityDownload>,
}

fn empty_path(path: &Path) -> bool {
    path.as_os_str().is_empty()
}

fn default_bin() -> String {
    DEFAULT_BIN.into()
}

fn safe_component(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !matches!(name, "." | "..")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
}

pub(super) fn valid_bin_directory(directory: &str) -> bool {
    directory.len() <= 192
        && directory.split('/').all(safe_component)
        && directory.split('/').count() <= 8
        && directory != STORE
        && !directory.starts_with(&format!("{STORE}/"))
}

pub(super) fn managed_tool_store(home: &str, bundle: &str) -> Result<String> {
    ensure!(
        safe_component(bundle) && !bundle.starts_with('.'),
        "Invalid tool bundle name"
    );
    Ok(under_home(home, &format!("{STORE}/{bundle}")))
}

fn valid_command_name(name: &str) -> bool {
    safe_component(name) && !name.starts_with(['.', '-'])
}

impl UtilityBundle {
    pub fn load(manifest: &Path) -> Result<Self> {
        Self::load_manifest(manifest).map(|(bundle, _)| bundle)
    }

    /// Load the manifest and hash the exact bytes used to build it.
    pub fn load_with_manifest_sha256(manifest: &Path) -> Result<(Self, String)> {
        let (bundle, bytes) = Self::load_manifest(manifest)?;
        Ok((bundle, digest_hex(Sha256::new_with_prefix(bytes))))
    }

    fn load_manifest(manifest: &Path) -> Result<(Self, Vec<u8>)> {
        let manifest = std::path::absolute(manifest)?;
        let mut bytes = Vec::new();
        let file = std::fs::File::open(&manifest)?;
        ensure!(
            file.metadata()?.is_file(),
            "Choose a regular tool bundle manifest"
        );
        file.take(MAX_MANIFEST + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_MANIFEST,
            "Tool bundle manifest exceeds 32 KiB"
        );
        let mut bundle: Self =
            serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
                .context("Reading the tool bundle JSON")?;
        let directory = manifest
            .parent()
            .context("Tool bundle file has no parent directory")?;
        for build in bundle.tools.iter_mut().flat_map(|tool| &mut tool.builds) {
            if !empty_path(&build.path) {
                build.path = directory.join(&build.path);
            }
        }
        bundle.validate()?;
        Ok((bundle, bytes))
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(self.version == 1, "Unsupported tool bundle version");
        ensure!(
            safe_component(&self.name) && !self.name.starts_with('.'),
            "Invalid tool bundle name"
        );
        ensure!(
            valid_bin_directory(&self.bin_directory),
            "Tool bin_directory must be a relative directory under the remote home"
        );
        ensure!((1..=16).contains(&self.tools.len()), "Choose 1 to 16 tools");
        let mut names = BTreeSet::new();
        for tool in &self.tools {
            ensure!(valid_command_name(&tool.name), "Invalid command name");
            ensure!(names.insert(&tool.name), "Duplicate tool {}", tool.name);
            ensure!(
                (1..=4).contains(&tool.builds.len()),
                "Choose 1 to 4 builds for {}",
                tool.name
            );
            for build in &tool.builds {
                match &build.download {
                    Some(download) => {
                        ensure!(
                            empty_path(&build.path),
                            "Choose a path or a download for {}",
                            tool.name
                        );
                        download
                            .validate()
                            .with_context(|| format!("Invalid download for {}", tool.name))?;
                    }
                    None => ensure!(
                        build.path.is_absolute(),
                        "Tool executable paths in control requests must be absolute Windows paths"
                    ),
                }
                normalize_sha256(&build.sha256)
                    .with_context(|| format!("Invalid SHA-256 for {}", tool.name))?;
                ensure!(
                    build.release.as_ref().is_none_or(|release| {
                        release.len() <= 1024
                            && release.starts_with("https://")
                            && crate::interaction::safe_url(release)
                    }),
                    "Use a valid HTTPS release URL for {}",
                    tool.name
                );
            }
        }
        Ok(())
    }
}

struct Host {
    architecture: UtilityArchitecture,
    home: String,
    glibc: Option<LibcVersion>,
    search_path: Vec<String>,
    has_relative_path: bool,
}

impl Host {
    fn parse(response: &[u8]) -> Result<Self> {
        let response =
            std::str::from_utf8(response).context("Remote tool probe returned invalid UTF-8")?;
        let mut lines = response.lines().skip_while(|line| *line != PROBE_PREFIX);
        ensure!(
            lines.next().is_some(),
            "Remote command did not return a tool probe report"
        );
        let fields: Vec<_> = lines.collect();
        let [os, arch, home, path, libc] = fields.as_slice() else {
            bail!("Remote tool probe returned an incomplete report");
        };
        ensure!(*os == "Linux", "Managed utilities require a Linux SSH host");
        let architecture = match *arch {
            "x86_64" => UtilityArchitecture::X86_64,
            "aarch64" => UtilityArchitecture::Aarch64,
            _ => bail!("Supported Linux architectures are x86_64 and aarch64"),
        };
        validate_dotfiles_home(home).context("Remote shell reported an invalid home directory")?;
        let search_path: Vec<_> = path.split(':').collect();
        ensure!(
            search_path.len() <= 64 && path.len() <= 8192,
            "Remote PATH is too large to inspect"
        );
        let valid_components: Vec<_> = search_path
            .iter()
            .filter(|path| validate_dotfiles_home(path).is_ok())
            .map(|path| (*path).to_owned())
            .collect();
        Ok(Self {
            architecture,
            home: (*home).into(),
            glibc: libc.strip_prefix("glibc ").and_then(LibcVersion::parse),
            has_relative_path: search_path.len() != valid_components.len(),
            search_path: valid_components,
        })
    }
}

async fn probe(session: &russh::client::Handle<crate::ssh::Handler>) -> Result<Host> {
    let (bytes, _) = tool_exec_report(session, PROBE, Duration::from_secs(8))
        .await
        .context("Running the remote Linux compatibility probe")?;
    Host::parse(&bytes)
}

async fn tool_exec_report(
    session: &russh::client::Handle<crate::ssh::Handler>,
    command: &str,
    timeout: Duration,
) -> Result<(Vec<u8>, usize)> {
    use russh::ChannelMsg;

    tokio::time::timeout(timeout, async {
        let mut pending = PendingChannel(Some(session.channel_open_session().await?));
        let channel = pending.0.as_mut().expect("opened channel");
        channel.exec(true, command).await?;
        let mut bytes = Vec::new();
        let mut stderr = 0;
        let mut exited = None;
        let mut eof = false;
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Data { data }) => {
                    ensure!(
                        bytes.len() + data.len() <= MAX_PROBE,
                        "Remote tool command output exceeded 16 KiB"
                    );
                    bytes.extend_from_slice(&data);
                }
                Some(ChannelMsg::ExtendedData { data, .. }) => {
                    stderr += data.len();
                    ensure!(
                        stderr <= MAX_PROBE,
                        "Remote tool command error output exceeded 16 KiB"
                    );
                }
                Some(ChannelMsg::Failure) => bail!("Server refused the SSH tool command"),
                Some(ChannelMsg::ExitStatus { exit_status }) => exited = Some(exit_status),
                Some(ChannelMsg::Eof) => eof = true,
                Some(ChannelMsg::Close) | None => break,
                _ => {}
            }
            if eof && exited.is_some() {
                break;
            }
        }
        ensure!(
            exited == Some(0),
            "Remote tool command did not exit successfully"
        );
        Ok((bytes, stderr))
    })
    .await
    .context("Remote tool command timed out")?
}

#[derive(Default, Serialize)]
struct DirectoryPlan {
    create: Vec<String>,
    blocked_at: Option<String>,
}

#[derive(Serialize)]
struct BuildReport {
    architecture: UtilityArchitecture,
    path: Option<PathBuf>,
    release: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    download: Option<UtilityDownload>,
    sha256: String,
    verified: Option<Verified>,
    status: &'static str,
    reason: Option<String>,
}

struct Candidate {
    build: UtilityBuild,
    checked: Option<Result<(Verified, PathBuf)>>,
}

async fn inspect_builds(
    tools: Vec<UtilityTool>,
    architecture: UtilityArchitecture,
) -> Result<Vec<(String, Vec<Candidate>)>> {
    tokio::task::spawn_blocking(move || {
        let mut remaining = MAX_LOCAL_BUILD_BYTES;
        tools
            .into_iter()
            .map(|tool| {
                let candidates = tool
                    .builds
                    .into_iter()
                    .map(|build| {
                        let checked = (build.architecture == architecture)
                            .then(|| Verified::read(&build, &mut remaining));
                        Candidate { build, checked }
                    })
                    .collect();
                (tool.name, candidates)
            })
            .collect()
    })
    .await
    .context("Validating tool bundles on Windows")
}

fn is_executable(attrs: &FileAttributes) -> bool {
    attrs.permissions.is_some_and(|mode| {
        mode & 0o111 != 0 && EntryKind::from_permissions(Some(mode)) == Some(EntryKind::File)
    })
}

impl Engine {
    pub(super) async fn preview_utility_bundle(
        &self,
        session: &russh::client::Handle<crate::ssh::Handler>,
        bundle: UtilityBundle,
    ) -> Result<Value> {
        let plan = self
            .utility_plan(session, bundle, InstallMode::Replace)
            .await?;
        self.review_utility_plan(plan)
    }

    fn review_utility_plan(&self, plan: InstallPlan) -> Result<Value> {
        let mut report = plan.report;
        if report["ready"] == true {
            report["review"] = json!(self.utility_review(&report)?);
        }
        Ok(report)
    }

    async fn utility_plan(
        &self,
        session: &russh::client::Handle<crate::ssh::Handler>,
        mut bundle: UtilityBundle,
        mode: InstallMode,
    ) -> Result<InstallPlan> {
        bundle.validate()?;
        let (host, home) = tokio::try_join!(probe(session), self.dotfile_home())?;
        ensure!(
            home == host.home,
            "SSH probe and SFTP disagree on the account's home"
        );
        let store = format!("{STORE}/{}", bundle.name);
        let versions = format!("{store}/versions");
        let managed = self.utility_directories(&home, &versions).await?;
        let store_path = under_home(&home, &store);
        let selector = self.utility_selector_state(&store_path, &managed).await?;
        let mut retained_tools = Vec::new();
        if mode == InstallMode::Add
            && let Some(id) = &selector.version
        {
            let active = self
                .verify_utility_release(session, &store_path, &bundle.name, host.architecture, id)
                .await
                .context("Checking the tools that will stay installed")?;
            bundle.bin_directory = active.bin_directory;
            bundle
                .tools
                .retain(|tool| !active.tools.iter().any(|old| old.name == tool.name));
            retained_tools = active.tools;
            ensure!(
                bundle.tools.len() + retained_tools.len() <= 16,
                "The extended bundle would exceed its 16-tool limit"
            );
        }
        let bin = self
            .utility_directories(&home, &bundle.bin_directory)
            .await?;
        let bin_path = under_home(&home, &bundle.bin_directory);
        let path_position = host.search_path.iter().position(|entry| *entry == bin_path);
        let earlier = &host.search_path[..path_position.unwrap_or(host.search_path.len())];
        let mut reports = Vec::new();
        let mut selected = Vec::new();
        let mut blocked = bin.blocked_at.is_some() || managed.blocked_at.is_some();
        let mut tools = inspect_builds(bundle.tools, host.architecture).await?;
        tools.extend(
            retained_tools
                .iter()
                .map(|tool| (tool.name.clone(), Vec::new())),
        );
        tools.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        for (name, candidates) in tools {
            let destination = format!("{bin_path}/{name}");
            let expected_link = format!("{store_path}/current/{name}");
            let (launcher, shadowed_by, builds) = tokio::try_join!(
                self.utility_launcher(&destination, &expected_link, &bin),
                self.utility_shadow(earlier, &name),
                self.utility_build_reports(candidates, &host),
            )?;
            let choice = builds.iter().position(|build| build.status == "compatible");
            let retained = retained_tools.iter().find(|tool| tool.name == name);
            if let Some(tool) = retained {
                selected.push((tool.clone(), None));
            } else if let Some(choice) = choice {
                let choice = &builds[choice];
                selected.push((
                    StoredTool {
                        name: name.clone(),
                        sha256: choice.sha256.clone(),
                        bytes: choice
                            .verified
                            .as_ref()
                            .expect("compatible build verified")
                            .bytes,
                    },
                    Some(
                        choice
                            .path
                            .clone()
                            .expect("verified executable has a local path"),
                    ),
                ));
            }
            blocked |= launcher == "conflict" || (choice.is_none() && retained.is_none());
            let mut report = json!({
                "name": name, "destination": destination, "launcher": launcher,
                "launcher_target": expected_link, "earlier_executable": shadowed_by,
                "selected_build": choice, "builds": builds,
            });
            if let Some(tool) = retained {
                report["retained"] = json!(tool);
            }
            reports.push(report);
        }
        let release = Release {
            name: bundle.name,
            bin_directory: bundle.bin_directory,
            architecture: host.architecture,
            tools: selected.iter().map(|(tool, _)| tool.clone()).collect(),
        };
        let bundle_sha256 = (selected.len() == reports.len())
            .then(|| release.id())
            .transpose()?;
        ensure!(
            release.tools.iter().map(|tool| tool.bytes).sum::<u64>() <= MAX_LOCAL_BUILD_BYTES,
            "The extended bundle would exceed its 256 MiB executable limit"
        );
        let atomic_replace = self.extension(editor::POSIX_RENAME, "1").is_ok();
        let version_path = bundle_sha256
            .as_ref()
            .map(|sha| format!("{store_path}/versions/{sha}"));
        let path = match path_position {
            Some(_) if host.has_relative_path => "present-precedence-partial",
            Some(_) => "present",
            None => "missing",
        };
        let bin_matches_active = mode == InstallMode::Add
            || self
                .utility_bin_matches_active(&store_path, &selector, &release)
                .await?;
        blocked |= selector.conflict || !bin_matches_active;
        let repairs = match bundle_sha256.as_deref() {
            Some(id) if mode == InstallMode::Replace && !blocked => {
                self.review_tool_repairs(session, &store_path, &release, id)
                    .await?
            }
            _ => Vec::new(),
        };
        let keeping_active = mode == InstallMode::Replace
            && selector.version.is_some()
            && selector.version == bundle_sha256;
        let manual = (mode == InstallMode::Add
            && (selector.manual || selector.version != bundle_sha256))
            || (keeping_active && selector.manual);
        blocked |= !atomic_replace
            && (repairs.iter().any(|repair| repair.replaces_file())
                || selector.version.as_ref().is_some_and(|current| {
                    selector.manual != manual || Some(current) != bundle_sha256.as_ref()
                }));
        let mut report = json!({
            "bundle": release.name, "bundle_sha256": bundle_sha256,
            "host": {"os": "Linux", "architecture": host.architecture, "home": home, "glibc": host.glibc.map(|v| v.to_string())},
            "bin_directory": bin_path, "bin_matches_active": bin_matches_active, "path_status": path, "path_source": "ssh-exec",
            "managed_directory": store_path, "version_directory": version_path, "current": selector,
            "bin_directory_plan": bin, "managed_directory_plan": managed,
            "supports_atomic_replace": atomic_replace, "ready": !blocked, "tools": reports,
        });
        if mode == InstallMode::Add {
            report["mode"] = json!("add");
            report["automatic_sync_paused"] = json!(manual);
        } else if mode == InstallMode::Replace {
            report["repairs"] = json!(repairs);
            report["automatic_sync_paused"] = json!(manual);
        }
        Ok(InstallPlan {
            report,
            release,
            selected,
            selector,
            bin,
            managed,
            home,
            store: store_path,
            id: bundle_sha256,
            manual,
            configure_launchers: true,
            repairs,
        })
    }

    async fn utility_directories(&self, home: &str, relative: &str) -> Result<DirectoryPlan> {
        let mut plan = DirectoryPlan::default();
        let mut path = home.trim_end_matches('/').to_owned();
        for component in relative.split('/') {
            path.push('/');
            path.push_str(component);
            if plan.create.is_empty() {
                match self.utility_lstat(&path).await? {
                    Some(attrs)
                        if EntryKind::from_permissions(attrs.permissions)
                            == Some(EntryKind::Directory) =>
                    {
                        continue;
                    }
                    Some(_) => {
                        plan.blocked_at = Some(path);
                        break;
                    }
                    None => {}
                }
            }
            plan.create.push(path.clone());
        }
        Ok(plan)
    }

    async fn utility_lstat(&self, path: &str) -> Result<Option<FileAttributes>> {
        match self.raw.lstat(path).await {
            Ok(result) => Ok(Some(result.attrs)),
            Err(Error::Status(status)) if status.status_code == StatusCode::NoSuchFile => Ok(None),
            Err(error) => Err(error).with_context(|| format!("Inspecting {path}")),
        }
    }

    async fn utility_launcher(
        &self,
        path: &str,
        target: &str,
        plan: &DirectoryPlan,
    ) -> Result<&'static str> {
        if plan.blocked_at.is_some() {
            return Ok("conflict");
        }
        if !plan.create.is_empty() {
            return Ok("create");
        }
        let Some(attrs) = self.utility_lstat(path).await? else {
            return Ok("create");
        };
        if EntryKind::from_permissions(attrs.permissions) == Some(EntryKind::Symlink) {
            let link = self.raw.readlink(path).await?.files;
            if matches!(link.as_slice(), [entry] if entry.filename == target) {
                return Ok("managed");
            }
        }
        Ok("conflict")
    }

    async fn utility_shadow(&self, earlier: &[String], name: &str) -> Result<Option<String>> {
        for directory in earlier {
            let path = under_home(directory, name);
            match self.raw.stat(&path).await {
                Ok(response) if is_executable(&response.attrs) => return Ok(Some(path)),
                Err(Error::Status(status))
                    if matches!(
                        status.status_code,
                        StatusCode::NoSuchFile | StatusCode::PermissionDenied
                    ) => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("Inspecting PATH entry {path}"));
                }
                _ => {}
            }
        }
        Ok(None)
    }

    async fn utility_build_reports(
        &self,
        candidates: Vec<Candidate>,
        host: &Host,
    ) -> Result<Vec<BuildReport>> {
        let mut reports = Vec::with_capacity(candidates.len());
        for Candidate { build, checked } in candidates {
            let mut path = (!empty_path(&build.path)).then_some(build.path);
            let (status, reason, verified) = match checked {
                None => ("other-architecture", None, None),
                Some(Err(error)) => ("invalid", Some(format!("{error:#}")), None),
                Some(Ok((verified, resolved))) => {
                    path = Some(resolved);
                    let reason = self.utility_runtime_conflict(&verified, host).await?;
                    (
                        if reason.is_none() {
                            "compatible"
                        } else {
                            "runtime-unverified"
                        },
                        reason,
                        Some(verified),
                    )
                }
            };
            reports.push(BuildReport {
                architecture: build.architecture,
                path,
                release: build.release,
                download: build.download,
                sha256: normalize_sha256(&build.sha256)?,
                verified,
                status,
                reason,
            });
        }
        Ok(reports)
    }

    async fn utility_runtime_conflict(
        &self,
        binary: &Verified,
        host: &Host,
    ) -> Result<Option<String>> {
        let Some(interpreter) = binary.interpreter.as_deref() else {
            return Ok(None);
        };
        let reason = match self.raw.stat(interpreter).await {
            Ok(result) if is_executable(&result.attrs) => {
                if let Some(required) = binary.glibc_minimum
                    && host.glibc.is_none_or(|installed| installed < required)
                {
                    format!(
                        "The build requires glibc {required}; the SSH probe did not find a sufficient version"
                    )
                } else {
                    "Shared library resolution is unverified. Supply a self-contained build.".into()
                }
            }
            Ok(_) => format!("ELF interpreter is not an executable file at {interpreter}"),
            Err(Error::Status(status))
                if matches!(
                    status.status_code,
                    StatusCode::NoSuchFile | StatusCode::PermissionDenied
                ) =>
            {
                format!("ELF interpreter is unavailable at {interpreter}")
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Checking ELF interpreter {interpreter}"));
            }
        };
        Ok(Some(reason))
    }
}
