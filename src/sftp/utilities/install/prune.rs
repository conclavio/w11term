use super::*;

struct PruneContents {
    files: Vec<StoredTool>,
    incomplete: bool,
}

impl Engine {
    async fn inspect_prune_contents(
        &self,
        directory: &str,
        name: &str,
        id: &str,
    ) -> Result<PruneContents> {
        let mut entries = BTreeSet::new();
        for entry in self.read_directory(directory, None, true).await? {
            if matches!(entry.filename.as_str(), "." | "..") {
                continue;
            }
            ensure!(
                EntryKind::from_permissions(entry.attrs.permissions) == Some(EntryKind::File),
                "Unexpected entry type in the retained release at {:?}",
                entry.filename
            );
            ensure!(
                entries.insert(entry.filename),
                "The retained release listing contains duplicate entries"
            );
        }
        if entries.is_empty() {
            // A disconnect after the manifest unlink can leave an empty version directory.
            return Ok(PruneContents {
                files: Vec::new(),
                incomplete: true,
            });
        }
        let manifest_path = format!("{directory}/{MANIFEST}");
        ensure!(
            entries.remove(MANIFEST),
            "The non-empty retained release is missing its manifest"
        );
        let manifest = self
            .read_preview_bytes(&manifest_path, None, &MANIFEST_LIMIT)
            .await
            .context("Reading the retained tool manifest")?;
        let release: Release = serde_json::from_slice(&manifest)?;
        release.validate(name, None, id)?;
        ensure!(
            entries
                .iter()
                .all(|entry| release.tools.iter().any(|tool| tool.name == *entry)),
            "The retained release contains files outside its manifest"
        );
        let mut files = Vec::with_capacity(entries.len() + 1);
        let incomplete = entries.len() != release.tools.len();
        for tool in release.tools {
            if entries.contains(&tool.name) {
                let path = format!("{directory}/{}", tool.name);
                self.verify_tool_file(&path, &tool).await?;
                files.push(tool);
            }
        }
        // Keep the manifest until the last executable is gone so partial deletions can be reviewed again.
        files.push(StoredTool {
            name: MANIFEST.into(),
            sha256: digest_hex(Sha256::new_with_prefix(&manifest)),
            bytes: manifest.len() as u64,
        });
        Ok(PruneContents { files, incomplete })
    }

    pub(in crate::sftp) async fn prune_tool_bundle_release(
        &self,
        session: &Ssh,
        name: &str,
        id: &str,
        review: Option<&str>,
        stage: &mut JobStage,
    ) -> Result<Value> {
        let id = normalize_sha256(id)?;
        let home = self.dotfile_home().await?;
        let store = managed_tool_store(&home, name)?;
        let versions = format!("{STORE}/{name}/versions");
        let directory = format!("{store}/versions/{id}");
        let work = async {
            let managed = self.utility_directories(&home, &versions).await?;
            ensure!(
                managed.create.is_empty() && managed.blocked_at.is_none(),
                "Managed tool bundle directory was not found"
            );
            let selector = self.utility_selector_state(&store, &managed).await?;
            ensure!(
                !selector.conflict,
                "The active tool bundle selector is invalid"
            );
            ensure!(
                selector.version.as_deref() != Some(id.as_str()),
                "The active tool release cannot be deleted"
            );
            let path = self
                .utility_directories(&home, &format!("{versions}/{id}"))
                .await?;
            ensure!(
                path.create.is_empty() && path.blocked_at.is_none(),
                "The retained tool release is missing or moved"
            );
            let contents = self.inspect_prune_contents(&directory, name, &id).await?;
            self.check_selector(&store, &selector).await?;
            let bytes = contents.files.iter().map(|file| file.bytes).sum::<u64>();
            let mut report = json!({
                "operation": "prune-tool-bundle-release",
                "bundle": name, "bundle_sha256": id, "home": home, "version_directory": directory,
                "current": selector, "files": contents.files, "remaining_bytes": bytes,
                "incomplete": contents.incomplete,
            });
            let Some(review) = review else {
                report["review"] = json!(self.utility_review(&report)?);
                return Ok(report);
            };
            self.check_utility_review(&report, review)?;
            *stage = JobStage::Checking(
                "Deletion did not finish. Refresh bundle versions and review any remaining files.",
            );
            let delete = async {
                for file in &contents.files {
                    self.check_selector(&store, &selector).await?;
                    self.check_entry_parent(&directory).await?;
                    let path = format!("{directory}/{}", file.name);
                    let attrs = self.raw.lstat(&path).await?.attrs;
                    ensure!(
                        EntryKind::from_permissions(attrs.permissions) == Some(EntryKind::File)
                            && attrs.size == Some(file.bytes),
                        "A reviewed file changed at {path}"
                    );
                    self.raw.remove(&path).await?;
                }
                self.check_selector(&store, &selector).await?;
                self.check_entry_parent(&directory).await?;
                self.raw.rmdir(&directory).await?;
                Ok::<_, anyhow::Error>(())
            }
            .await;
            delete.with_context(|| stage.interrupted())?;
            Ok(json!({
                "bundle": name, "bundle_sha256": id, "removed_bytes": bytes,
            }))
        };
        if review.is_some() {
            self.check_entry_parent(&store).await?;
            with_lock(session, &store, work).await
        } else {
            work.await
        }
    }
}
