use super::*;
use russh::ChannelMsg;

const MANIFEST: &str = ".w11term-manifest.json";
const MANIFEST_LIMIT: text_preview::PreviewLimit = text_preview::PreviewLimit {
    bytes: MAX_MANIFEST,
    name: "Retained tool manifest",
    limit: "the 32 KiB tool manifest limit",
};
const LOCK_READY: &[u8] = b"w11term-tools-locked-v1\n";
const MAX_INSPECTED_BUNDLES: usize = 32;
const MUTATING: &str =
    "Tool bundle activation did not finish. Preview again to check the active version.";
type Ssh = russh::client::Handle<crate::ssh::Handler>;

fn valid_tool_permissions(attrs: &FileAttributes) -> bool {
    attrs
        .permissions
        .is_some_and(|mode| mode & 0o170000 == 0o100000 && mode & 0o100 != 0 && mode & 0o7022 == 0)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredTool {
    pub name: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Release {
    pub name: String,
    pub bin_directory: String,
    pub architecture: UtilityArchitecture,
    pub tools: Vec<StoredTool>,
}

impl Release {
    pub(super) fn id(&self) -> Result<String> {
        let mut digest = Sha256::new_with_prefix(b"w11term-tools-release-v1\0");
        digest.update(serde_json::to_vec(self)?);
        Ok(digest_hex(digest))
    }

    fn validate(
        &self,
        name: &str,
        architecture: Option<UtilityArchitecture>,
        id: &str,
    ) -> Result<()> {
        let valid_tools = (1..=16).contains(&self.tools.len())
            && self
                .tools
                .windows(2)
                .all(|pair| pair[0].name < pair[1].name)
            && self.tools.iter().all(|tool| {
                safe_component(&tool.name)
                    && !tool.name.starts_with(['.', '-'])
                    && (1..=build::MAX_BINARY).contains(&tool.bytes)
                    && normalize_sha256(&tool.sha256).is_ok_and(|hash| tool.sha256 == hash)
            });
        ensure!(
            valid_tools
                && self.tools.iter().map(|tool| tool.bytes).sum::<u64>() <= MAX_LOCAL_BUILD_BYTES
                && self.name == name
                && architecture.is_none_or(|expected| self.architecture == expected)
                && valid_bin_directory(&self.bin_directory)
                && self.id()? == id,
            "Retained tool manifest does not match the reviewed version {id}"
        );
        Ok(())
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct InstalledTool {
    pub name: String,
    pub bundle: String,
    pub path: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct InstalledTools {
    pub tools: Vec<InstalledTool>,
    pub skipped_bundles: usize,
}

#[derive(Debug, Default, Eq, PartialEq, Serialize)]
pub(super) struct Selector {
    pub conflict: bool,
    pub version: Option<String>,
    pub manual: bool,
}

pub(super) struct InstallPlan {
    pub report: Value,
    pub release: Release,
    pub selected: Vec<(StoredTool, PathBuf)>,
    pub selector: Selector,
    pub bin: DirectoryPlan,
    pub managed: DirectoryPlan,
    pub home: String,
    pub store: String,
    pub id: Option<String>,
}

impl Engine {
    pub(in crate::sftp) async fn installed_tools(
        &self,
        cancel: Option<&TransferCancel>,
    ) -> Result<InstalledTools> {
        let home = self.dotfile_home().await?;
        let root = under_home(&home, STORE);
        let directory = self.utility_directories(&home, STORE).await?;
        ensure!(
            directory.blocked_at.is_none(),
            "Managed tool directory is blocked"
        );
        let mut installed = InstalledTools::default();
        if !directory.create.is_empty() {
            return Ok(installed);
        }
        let mut names = BTreeSet::new();
        for entry in self.read_directory(&root, cancel, true).await? {
            let name = entry.filename;
            if safe_component(&name)
                && !name.starts_with('.')
                && EntryKind::from_permissions(entry.attrs.permissions)
                    == Some(EntryKind::Directory)
            {
                names.insert(name);
            }
        }
        installed.skipped_bundles = names.len().saturating_sub(MAX_INSPECTED_BUNDLES);
        for name in names.into_iter().take(MAX_INSPECTED_BUNDLES) {
            ensure!(
                !cancel.is_some_and(TransferCancel::is_cancelled),
                "Installed tools lookup cancelled"
            );
            match self.installed_bundle_tools(&home, &name).await {
                Ok(tools) => installed.tools.extend(tools),
                Err(error) if error.downcast_ref::<Error>().is_some_and(transport_error) => {
                    return Err(error);
                }
                Err(_) => installed.skipped_bundles += 1,
            }
        }
        installed
            .tools
            .sort_unstable_by(|a, b| (&a.name, &a.bundle).cmp(&(&b.name, &b.bundle)));
        Ok(installed)
    }

    async fn installed_bundle_tools(&self, home: &str, name: &str) -> Result<Vec<InstalledTool>> {
        let relative = format!("{STORE}/{name}");
        let store = under_home(home, &relative);
        let managed = self
            .utility_directories(home, &format!("{relative}/versions"))
            .await?;
        let selector = self.utility_selector_state(&store, &managed).await?;
        ensure!(!selector.conflict, "Invalid active tool bundle selector");
        let Some(id) = &selector.version else {
            return Ok(Vec::new());
        };
        let release = self
            .utility_release_manifest(&store, name, None, id)
            .await?;
        let bin = self
            .utility_directories(home, &release.bin_directory)
            .await?;
        let launchers = self.launcher_actions(home, &store, &release, &bin).await?;
        let mut tools = Vec::new();
        for (tool, (path, action)) in release.tools.into_iter().zip(launchers) {
            if action != "managed" {
                continue;
            }
            let executable = format!("{store}/versions/{id}/{}", tool.name);
            if self.utility_lstat(&executable).await?.is_some_and(|attrs| {
                valid_tool_permissions(&attrs) && attrs.size == Some(tool.bytes)
            }) {
                tools.push(InstalledTool {
                    name: tool.name,
                    bundle: name.to_owned(),
                    path,
                });
            }
        }
        self.check_selector(&store, &selector).await?;
        Ok(tools)
    }

    pub(super) fn utility_review(&self, report: &Value) -> Result<String> {
        let mut digest = Sha256::new_with_prefix(self.dotfiles_review_scope.to_le_bytes());
        digest.update(b"w11term-tools-review-v1\0");
        digest.update(serde_json::to_vec(report)?);
        Ok(digest_hex(digest))
    }

    fn check_utility_review(&self, report: &Value, review: &str) -> Result<()> {
        ensure!(
            self.utility_review(report)? == normalize_sha256(review)?,
            "The tool bundle review is stale or belongs to another SFTP connection. Preview again."
        );
        Ok(())
    }

    pub(super) async fn utility_selector_state(
        &self,
        store: &str,
        managed: &DirectoryPlan,
    ) -> Result<Selector> {
        if managed.blocked_at.is_some() {
            return Ok(Selector {
                conflict: true,
                ..Default::default()
            });
        }
        let versions = format!("{store}/versions");
        if managed.create.iter().any(|p| p != &versions) {
            return Ok(Selector::default());
        }
        let path = format!("{store}/current");
        let Some(attrs) = self.utility_lstat(&path).await? else {
            return Ok(Selector::default());
        };
        if EntryKind::from_permissions(attrs.permissions) == Some(EntryKind::Symlink) {
            let target = self.read_link_target(&path).await?;
            let (target, manual) = target
                .strip_prefix("./")
                .map_or((target.as_str(), false), |path| (path, true));
            if let Some(id) = target.strip_prefix("versions/")
                && normalize_sha256(id).is_ok_and(|hash| hash == id)
            {
                return Ok(Selector {
                    conflict: false,
                    version: Some(id.to_owned()),
                    manual,
                });
            }
        }
        Ok(Selector {
            conflict: true,
            ..Default::default()
        })
    }

    pub(in crate::sftp) async fn apply_utility_bundle(
        &self,
        session: &Ssh,
        bundle: UtilityBundle,
        review: &str,
        stage: &mut JobStage,
    ) -> Result<Value> {
        let plan = self.utility_plan(session, bundle).await?;
        self.check_utility_review(&plan.report, review)?;
        self.activate_utility_plan(session, &plan, stage).await
    }

    pub(super) async fn activate_utility_plan(
        &self,
        session: &Ssh,
        plan: &InstallPlan,
        stage: &mut JobStage,
    ) -> Result<Value> {
        ensure!(
            plan.report["ready"] == true,
            "Resolve the tool bundle conflicts before installing"
        );
        let id = plan
            .id
            .as_deref()
            .context("Select compatible builds before installing")?;
        *stage = JobStage::Checking(MUTATING);
        self.create_utility_directories(&plan.managed).await?;
        let store = &plan.store;
        self.check_entry_parent(store).await?;
        with_lock(session, store, async {
            self.check_selector(store, &plan.selector).await?;
            self.check_launchers(&plan.home, store, &plan.release).await?;
            let version_path = format!("{store}/versions/{id}");
            let (uploaded, copied_tools) = if self.utility_lstat(&version_path).await?.is_some() {
                self.verify_utility_release(store, &plan.release.name, plan.release.architecture, id).await?;
                (false, Vec::new())
            } else {
                let copied = self.stage_utility_release(plan, id).await?;
                (copied.len() < plan.selected.len(), copied)
            };
            let launchers = self.create_launchers(&plan.home, store, &plan.release, &plan.bin).await?;
            let changed = self.switch_utility_selector(store, id, false, &plan.selector).await?;
            Ok(json!({
                "bundle": plan.release.name, "bundle_sha256": id, "previous_bundle_sha256": plan.selector.version,
                "changed": changed, "uploaded": uploaded, "copied_tools": copied_tools,
                "launchers_created": launchers, "version_directory": version_path,
            }))
        }).await
    }

    pub(in crate::sftp) async fn rollback_utility_bundle(
        &self,
        session: &Ssh,
        name: String,
        id: String,
        review: Option<&str>,
        stage: &mut JobStage,
    ) -> Result<Value> {
        ensure!(
            safe_component(&name) && !name.starts_with('.'),
            "Invalid tool bundle name"
        );
        let id = normalize_sha256(&id)?;
        let (host, home) = tokio::try_join!(probe(session), self.dotfile_home())?;
        ensure!(
            home == host.home,
            "SSH probe and SFTP disagree on the account's home"
        );
        let relative = format!("{STORE}/{name}");
        let store = under_home(&home, &relative);
        let plan = self
            .utility_directories(&home, &format!("{relative}/versions"))
            .await?;
        ensure!(
            plan.create.is_empty() && plan.blocked_at.is_none(),
            "Managed tool bundle directory was not found"
        );
        let selector = self.utility_selector_state(&store, &plan).await?;
        let release = self
            .verify_utility_release(&store, &name, host.architecture, &id)
            .await?;
        let bin = self
            .utility_directories(&home, &release.bin_directory)
            .await?;
        let launchers = self.launcher_actions(&home, &store, &release, &bin).await?;
        let atomic = self.extension(editor::POSIX_RENAME, "1").is_ok();
        let ready = !selector.conflict
            && bin.blocked_at.is_none()
            && launchers.iter().all(|(_, action)| *action != "conflict")
            && (atomic
                || (selector.manual && selector.version.as_ref() == Some(&id))
                || selector.version.is_none());
        let ready = ready
            && self
                .utility_bin_matches_active(&store, &selector, &release)
                .await?;
        let mut report = json!({
            "operation": "rollback-tool-bundle", "home": home, "bundle": name, "bundle_sha256": id,
            "current": selector, "retained": release, "bin_directory_plan": bin, "launchers": launchers,
            "ready": ready, "supports_atomic_replace": atomic,
        });
        let Some(review) = review else {
            if ready {
                report["review"] = json!(self.utility_review(&report)?);
            }
            return Ok(report);
        };
        self.check_utility_review(&report, review)?;
        ensure!(ready, "Resolve the tool bundle conflicts before rollback");
        *stage = JobStage::Checking(MUTATING);
        self.check_entry_parent(&store).await?;
        with_lock(session, &store, async {
            self.check_selector(&store, &selector).await?;
            self.verify_utility_release(&store, &name, host.architecture, &id)
                .await?;
            self.create_launchers(&home, &store, &release, &bin).await?;
            let changed = self
                .switch_utility_selector(&store, &id, true, &selector)
                .await?;
            Ok(json!({
                "bundle": name, "bundle_sha256": id, "previous_bundle_sha256": selector.version,
                "changed": changed, "uploaded": false,
            }))
        })
        .await
    }

    async fn stage_utility_release(&self, plan: &InstallPlan, id: &str) -> Result<Vec<String>> {
        let versions = format!("{}/versions", plan.store);
        let staging = format!("{versions}/.w11term-stage-{:032x}", rand::random::<u128>());
        let destination = format!("{versions}/{id}");
        let prior = if let Some(active) = &plan.selector.version {
            let release = self
                .utility_release_manifest(
                    &plan.store,
                    &plan.release.name,
                    Some(plan.release.architecture),
                    active,
                )
                .await?;
            Some((format!("{versions}/{active}"), release.tools))
        } else {
            None
        };
        self.check_entry_parent(&versions).await?;
        self.raw.mkdir(&staging, private_mode(0o700)).await?;
        let mut staged = Vec::new();
        let result: Result<Vec<String>> = async {
            let mut copied = Vec::new();
            for (tool, path) in &plan.selected {
                let staged_path = format!("{staging}/{}", tool.name);
                if let Some((directory, prior)) = &prior
                    && prior.iter().any(|before| {
                        before.name == tool.name
                            && before.sha256 == tool.sha256
                            && before.bytes == tool.bytes
                    })
                    && self
                        .copy_unchanged_tool(directory, &staging, &staged_path, tool, &mut staged)
                        .await?
                {
                    copied.push(tool.name.clone());
                    continue;
                }
                let source = path.clone();
                let pin = tool.sha256.clone();
                let mut remaining = tool.bytes;
                let contents = tokio::task::spawn_blocking(move || {
                    build::read_pinned(&source, &pin, &mut remaining)
                })
                .await??;
                self.write_new_tool_file(&staged_path, &contents, 0o700, &mut staged)
                    .await?;
                self.verify_tool_file(&staged_path, tool).await?;
            }
            let manifest = serde_json::to_vec(&plan.release)?;
            let manifest_path = format!("{staging}/{MANIFEST}");
            self.write_new_tool_file(&manifest_path, &manifest, 0o600, &mut staged)
                .await?;
            ensure!(
                self.read_preview_bytes(&manifest_path, None, &MANIFEST_LIMIT)
                    .await?
                    == manifest,
                "The staged tool manifest did not match the reviewed release"
            );
            self.check_entry_parent(&staging).await?;
            self.raw.rename(&staging, &destination).await?;
            Ok(copied)
        }
        .await;
        if let Err(error) = &result
            && !error.downcast_ref::<Error>().is_some_and(transport_error)
        {
            // Remove only entries created by this operation, under its random staging directory.
            if self.check_entry_parent(&staging).await.is_ok() {
                for path in staged.iter().rev() {
                    let _ = self.raw.remove(path).await;
                }
                let _ = self.raw.rmdir(&staging).await;
            }
        }
        result
    }

    async fn copy_unchanged_tool(
        &self,
        prior: &str,
        staging: &str,
        destination: &str,
        tool: &StoredTool,
        staged: &mut Vec<String>,
    ) -> Result<bool> {
        let source = format!("{prior}/{}", tool.name);
        match self.verify_tool_file(&source, tool).await {
            Ok(()) => (),
            Err(err) if err.downcast_ref::<Error>().is_some_and(transport_error) => {
                return Err(err);
            }
            // A damaged old version must not prevent staging from the pinned Windows file.
            Err(_) => return Ok(false),
        }
        self.copy_entry(
            &source,
            CopyDestination::Folder {
                directory: staging.to_owned(),
                is_canonical: true,
            },
            &mut JobStage::Checking(MUTATING),
        )
        .await?;
        staged.push(destination.to_owned());
        self.raw.setstat(destination, private_mode(0o700)).await?;
        self.verify_tool_file(destination, tool).await?;
        Ok(true)
    }

    async fn write_new_tool_file(
        &self,
        path: &str,
        contents: &[u8],
        mode: u32,
        staged: &mut Vec<String>,
    ) -> Result<()> {
        let (parent, _) = path
            .rsplit_once('/')
            .context("Expected an absolute tool path")?;
        self.check_entry_parent(parent).await?;
        let handle = self
            .raw
            .open(
                path,
                OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
                private_mode(0o600),
            )
            .await?
            .handle;
        staged.push(path.to_owned());
        let prepared = async {
            let mut offset = 0;
            for bytes in contents.chunks(self.write_size) {
                self.raw.write(&handle, offset, bytes.to_vec()).await?;
                offset += bytes.len() as u64;
            }
            self.raw.fsetstat(&handle, private_mode(mode)).await?;
            if self.extension(extensions::FSYNC, "1").is_ok() {
                self.raw.fsync(&handle).await?;
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        let closed = self.raw.close(handle).await;
        prepared?;
        closed?;
        Ok(())
    }

    async fn verify_utility_release(
        &self,
        store: &str,
        name: &str,
        architecture: UtilityArchitecture,
        id: &str,
    ) -> Result<Release> {
        let release = self
            .utility_release_manifest(store, name, Some(architecture), id)
            .await?;
        let directory = format!("{store}/versions/{id}");
        for tool in &release.tools {
            self.verify_tool_file(&format!("{directory}/{}", tool.name), tool)
                .await?;
        }
        Ok(release)
    }

    async fn utility_release_manifest(
        &self,
        store: &str,
        name: &str,
        architecture: Option<UtilityArchitecture>,
        id: &str,
    ) -> Result<Release> {
        let directory = format!("{store}/versions/{id}");
        self.check_entry_parent(&directory)
            .await
            .context("Retained tool bundle is missing or moved")?;
        let path = format!("{directory}/{MANIFEST}");
        let manifest = self
            .read_preview_bytes(&path, None, &MANIFEST_LIMIT)
            .await
            .context("Reading the retained tool manifest")?;
        let release = serde_json::from_slice::<Release>(&manifest)?;
        release.validate(name, architecture, id)?;
        Ok(release)
    }

    pub(super) async fn utility_bin_matches_active(
        &self,
        store: &str,
        selector: &Selector,
        release: &Release,
    ) -> Result<bool> {
        let Some(active) = &selector.version else {
            return Ok(true);
        };
        let manifest = self
            .utility_release_manifest(store, &release.name, Some(release.architecture), active)
            .await?;
        Ok(manifest.bin_directory == release.bin_directory)
    }

    async fn verify_tool_file(&self, path: &str, tool: &StoredTool) -> Result<()> {
        let attrs = self.raw.lstat(path).await?.attrs;
        ensure!(
            valid_tool_permissions(&attrs),
            "Managed executable permissions changed at {path}"
        );
        let actual = self
            .remote_sha256(path, Some(tool.bytes), None, true)
            .await?;
        ensure!(
            actual["sha256"] == tool.sha256 && actual["bytes"] == tool.bytes,
            "Managed executable contents changed at {path}"
        );
        Ok(())
    }

    async fn create_utility_directories(&self, plan: &DirectoryPlan) -> Result<()> {
        ensure!(plan.blocked_at.is_none(), "Managed directory is blocked");
        for path in &plan.create {
            let (parent, _) = path
                .rsplit_once('/')
                .context("Expected an absolute directory")?;
            self.check_entry_parent(parent).await?;
            if let Err(error) = self.raw.mkdir(path, private_mode(0o700)).await {
                let attrs = self.utility_lstat(path).await?;
                if !attrs.is_some_and(|attrs| {
                    EntryKind::from_permissions(attrs.permissions) == Some(EntryKind::Directory)
                }) {
                    return Err(error)
                        .with_context(|| format!("Creating managed directory {path}"));
                }
            }
        }
        Ok(())
    }

    async fn launcher_actions(
        &self,
        home: &str,
        store: &str,
        release: &Release,
        plan: &DirectoryPlan,
    ) -> Result<Vec<(String, &'static str)>> {
        let bin = under_home(home, &release.bin_directory);
        let mut actions = Vec::new();
        for tool in &release.tools {
            let path = format!("{bin}/{}", tool.name);
            let target = format!("{store}/current/{}", tool.name);
            let status = self.utility_launcher(&path, &target, plan).await?;
            actions.push((path, status));
        }
        Ok(actions)
    }

    async fn check_launchers(&self, home: &str, store: &str, release: &Release) -> Result<()> {
        let plan = self
            .utility_directories(home, &release.bin_directory)
            .await?;
        for (path, action) in self.launcher_actions(home, store, release, &plan).await? {
            ensure!(
                action != "conflict",
                "Existing command at {path} was preserved"
            );
        }
        Ok(())
    }

    async fn create_launchers(
        &self,
        home: &str,
        store: &str,
        release: &Release,
        plan: &DirectoryPlan,
    ) -> Result<usize> {
        self.create_utility_directories(plan).await?;
        let bin = under_home(home, &release.bin_directory);
        let now = self
            .utility_directories(home, &release.bin_directory)
            .await?;
        ensure!(
            now.create.is_empty() && now.blocked_at.is_none(),
            "The tool bin directory changed"
        );
        self.check_entry_parent(&bin).await?;
        let mut created = 0;
        for tool in &release.tools {
            let path = format!("{bin}/{}", tool.name);
            let target = format!("{store}/current/{}", tool.name);
            match self.utility_launcher(&path, &target, &now).await? {
                "managed" => (),
                "create" => {
                    // OpenSSH v3 SYMLINK creates a new entry and fails if the name already exists.
                    self.raw
                        .symlink(&target, &path)
                        .await
                        .with_context(|| format!("Creating launcher {path}"))?;
                    created += 1;
                }
                _ => bail!("Existing command at {path} was preserved"),
            }
        }
        Ok(created)
    }

    async fn check_selector(&self, store: &str, expected: &Selector) -> Result<()> {
        self.check_entry_parent(store).await?;
        let current = self
            .utility_selector_state(store, &DirectoryPlan::default())
            .await?;
        ensure!(
            &current == expected && !current.conflict,
            "The active tool bundle changed. Preview again."
        );
        Ok(())
    }

    async fn switch_utility_selector(
        &self,
        store: &str,
        id: &str,
        manual: bool,
        before: &Selector,
    ) -> Result<bool> {
        self.check_selector(store, before).await?;
        if before.version.as_deref() == Some(id) && before.manual == manual {
            return Ok(false);
        }
        let prefix = if manual { "./" } else { "" };
        let target = format!("{prefix}versions/{id}");
        let path = format!("{store}/current");
        if before.version.is_none() {
            self.raw.symlink(&target, &path).await?;
        } else {
            self.extension(editor::POSIX_RENAME, "1")?;
            let temporary = format!("{store}/.w11term-current-{:032x}", rand::random::<u128>());
            self.raw.symlink(&target, &temporary).await?;
            let result = async {
                self.check_selector(store, before).await?;
                self.replace_staged_file(&temporary, &path).await
            }
            .await;
            if let Err(error) = &result
                && !error.downcast_ref::<Error>().is_some_and(transport_error)
            {
                let _ = self.raw.remove(&temporary).await;
            }
            result?;
        }
        Ok(true)
    }
}

fn private_mode(mode: u32) -> FileAttributes {
    FileAttributes {
        permissions: Some(mode),
        ..FileAttributes::empty()
    }
}

fn quote_sh(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(super) async fn with_lock<T>(
    session: &Ssh,
    directory: &str,
    work: impl Future<Output = Result<T>>,
) -> Result<T> {
    // Lock a directory descriptor to avoid stale lock files. The remote process
    // holds it until SSH closes stdin, including when the owning channel dies.
    let command = format!(
        r#"/bin/sh -c 'exec 9<"$1" || exit 2; command -p flock -w 3 9 || exit 3; printf "%s\n" w11term-tools-locked-v1; command -p cat >/dev/null' w11term {}"#,
        quote_sh(directory)
    );
    let mut lock = PendingChannel(Some(session.channel_open_session().await?));
    let channel = lock.0.as_mut().expect("opened channel");
    channel.exec(true, command).await?;
    tokio::time::timeout(Duration::from_secs(8), async {
        let mut output = Vec::new();
        let mut stderr = 0;
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Data {data}) => {
                    ensure!(output.len() + data.len() <= MAX_PROBE, "Too much output from the tool bundle lock");
                    output.extend_from_slice(&data);
                    if output.ends_with(LOCK_READY) { return Ok::<_, anyhow::Error>(()); }
                }
                Some(ChannelMsg::ExtendedData {data, ..}) => {
                    stderr += data.len();
                    ensure!(stderr <= MAX_PROBE, "Too much output from the tool bundle lock");
                }
                Some(ChannelMsg::Success) => (),
                _ => bail!("Could not lock the managed tool directory. Check flock or retry after the other operation."),
            }
        }
    }).await.context("Timed out acquiring the managed tool bundle lock")??;
    let lost = async {
        while matches!(
            channel.wait().await,
            Some(ChannelMsg::Success | ChannelMsg::WindowAdjusted { .. })
        ) {}
    };
    let result = tokio::select! {
        biased;
        _ = lost => Err(anyhow::anyhow!("Lost the managed tool bundle lock. Preview again.")),
        result = work => result,
    };
    let _ = channel.eof().await;
    let _ = channel.close().await;
    result
}
