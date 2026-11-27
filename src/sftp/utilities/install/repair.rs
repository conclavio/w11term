use super::*;

#[derive(Serialize)]
pub(in crate::sftp::utilities) struct RepairTool {
    pin: StoredTool,
    before: ObservedTool,
    reason: &'static str,
}

#[derive(Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum ObservedTool {
    Missing,
    File {
        sha256: String,
        bytes: u64,
        permissions: u32,
        uid: Option<u32>,
        gid: Option<u32>,
    },
}

impl RepairTool {
    pub(in crate::sftp::utilities) fn replaces_file(&self) -> bool {
        matches!(self.before, ObservedTool::File { .. })
    }
}

impl Engine {
    async fn observe_repair_tool(&self, path: &str) -> Result<ObservedTool> {
        self.check_entry_parent(
            path.rsplit_once('/')
                .context("Expected a tool file path")?
                .0,
        )
        .await?;
        let Some(attrs) = self.utility_lstat(path).await? else {
            return Ok(ObservedTool::Missing);
        };
        ensure!(
            EntryKind::from_permissions(attrs.permissions) == Some(EntryKind::File),
            "Tool repair needs a regular file or a missing file at {path}"
        );
        let contents = self
            .remote_sha256(path, Some(build::MAX_BINARY), None, true)
            .await?;
        let after = self.raw.lstat(path).await?.attrs;
        ensure!(
            attrs.permissions == after.permissions
                && attrs.uid == after.uid
                && attrs.gid == after.gid
                && attrs.size == after.size
                && attrs.mtime == after.mtime,
            "The executable changed during repair review at {path}"
        );
        Ok(ObservedTool::File {
            sha256: contents["sha256"]
                .as_str()
                .context("Expected an executable digest")?
                .to_owned(),
            bytes: contents["bytes"]
                .as_u64()
                .context("Expected an executable size")?,
            permissions: attrs.permissions.expect("confirmed regular file"),
            uid: attrs.uid,
            gid: attrs.gid,
        })
    }

    pub(in crate::sftp::utilities) async fn review_tool_repairs(
        &self,
        session: &Ssh,
        store: &str,
        release: &Release,
        id: &str,
    ) -> Result<Vec<RepairTool>> {
        let directory = format!("{store}/versions/{id}");
        if self.utility_lstat(&directory).await?.is_none() {
            return Ok(Vec::new());
        }
        let retained = self
            .utility_release_manifest(store, &release.name, Some(release.architecture), id)
            .await?;
        match self
            .verify_release_over_ssh(session, &directory, &retained)
            .await
        {
            Ok(true) => return Ok(Vec::new()),
            Err(error) if error.downcast_ref::<Error>().is_some_and(transport_error) => {
                return Err(error);
            }
            _ => {}
        }
        let mut repairs = Vec::new();
        for pin in retained.tools {
            let before = self
                .observe_repair_tool(&format!("{directory}/{}", pin.name))
                .await?;
            let reason = match &before {
                ObservedTool::Missing => "missing",
                ObservedTool::File { bytes, sha256, .. }
                    if *bytes != pin.bytes || *sha256 != pin.sha256 =>
                {
                    "contents-changed"
                }
                ObservedTool::File { permissions, .. } if !valid_tool_mode(*permissions) => {
                    "permissions-changed"
                }
                ObservedTool::File { .. } => continue,
            };
            repairs.push(RepairTool {
                pin,
                before,
                reason,
            });
        }
        Ok(repairs)
    }

    pub(super) async fn repair_utility_release(
        &self,
        plan: &InstallPlan,
        id: &str,
    ) -> Result<Vec<String>> {
        let versions = format!("{}/versions", plan.store);
        let directory = format!("{versions}/{id}");
        let staging = format!("{versions}/.w11term-repair-{:032x}", rand::random::<u128>());
        self.check_entry_parent(&versions).await?;
        self.raw.mkdir(&staging, private_mode(0o700)).await?;
        let mut staged = Vec::new();
        let work = async {
            for repair in &plan.repairs {
                let pin = &repair.pin;
                let source = plan.selected.iter()
                    .find(|(selected, _)| selected == pin)
                    .and_then(|(_, path)| path.clone())
                    .with_context(|| format!("Select the pinned Windows build for {}", pin.name))?;
                let hash = pin.sha256.clone();
                let mut remaining = pin.bytes;
                let contents = tokio::task::spawn_blocking(move || {
                    build::read_pinned(&source, &hash, &mut remaining)
                })
                .await??;
                let path = format!("{staging}/{}", pin.name);
                self.write_new_tool_file(&path, &contents, 0o700, &mut staged)
                    .await?;
                self.verify_tool_file(&path, pin).await?;
            }
            self.utility_release_manifest(
                &plan.store, &plan.release.name, Some(plan.release.architecture), id,
            ).await?;
            let mut repaired = Vec::new();
            for repair in &plan.repairs {
                self.check_selector(&plan.store, &plan.selector).await?;
                self.check_entry_parent(&staging).await?;
                let name = &repair.pin.name;
                let destination = format!("{directory}/{name}");
                ensure!(
                    self.observe_repair_tool(&destination).await? == repair.before,
                    "The executable changed after its repair review at {destination}. Review the bundle again."
                );
                let source = format!("{staging}/{name}");
                if repair.replaces_file() {
                    self.replace_staged_file(&source, &destination).await?;
                } else {
                    self.raw.rename(&source, &destination).await?;
                }
                staged.retain(|path| path != &source);
                repaired.push(name.clone());
            }
            Ok::<_, anyhow::Error>(repaired)
        }.await;
        if !work
            .as_ref()
            .err()
            .is_some_and(|error| error.downcast_ref::<Error>().is_some_and(transport_error))
            && self.check_entry_parent(&staging).await.is_ok()
        {
            for path in &staged {
                let _ = self.raw.remove(path).await;
            }
            let _ = self.raw.rmdir(&staging).await;
        }
        work
    }
}
