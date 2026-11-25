use super::*;

impl Engine {
    pub(super) async fn read_link_target(&self, path: &str) -> Result<String> {
        let mut names = self.raw.readlink(path).await?.files;
        ensure!(
            matches!(names.as_slice(), [name] if !name.filename.is_empty()),
            "The server returned no single link target"
        );
        Ok(names.remove(0).filename)
    }

    pub(super) async fn open_link_target(
        &self,
        path: &str,
        expected_target: &str,
        cancel: Option<&TransferCancel>,
    ) -> Result<Value> {
        self.checked_source_folder(path).await?;
        let link = self.raw.lstat(path).await?.attrs;
        ensure!(
            EntryKind::from_permissions(link.permissions) == Some(EntryKind::Symlink)
                && self.read_link_target(path).await? == expected_target,
            "The symbolic link changed. Reopen Properties to review its target"
        );
        let resolved = self
            .canonical_remote_path(path)
            .await
            .context("Could not resolve the link target")?;
        ensure!(
            resolved != path,
            "The server did not resolve the symbolic link"
        );
        let target = self
            .raw
            .lstat(&resolved)
            .await
            .context("Could not read the resolved link target")?
            .attrs;
        let (directory, selected) = match EntryKind::from_permissions(target.permissions) {
            Some(EntryKind::Directory) => (resolved.as_str(), None),
            Some(EntryKind::File) => {
                let (parent, name) = resolved.rsplit_once('/').context("Invalid link target")?;
                ensure!(!name.is_empty(), "Invalid link target filename");
                (if parent.is_empty() { "/" } else { parent }, Some(name))
            }
            _ => bail!("The link target is not a regular file or directory"),
        };
        let entries = self
            .read_directory(directory, cancel)
            .await
            .context("Could not list the link target's folder")?;
        Ok(json!({"directory":directory,"selected":selected,"entries":entries}))
    }
}
