use super::*;

fn manifest_fragment(mut text: String) -> String {
    // Installation can append one newline to a fragment at the manifest limit.
    if text.len() == MAX_FRAGMENT_BYTES + 1 && text.ends_with('\n') {
        text.pop();
    }
    text
}

impl Engine {
    pub(in crate::sftp) async fn export_dotfile_bundle(
        &self,
        shell: DotfileShell,
        id: &str,
        local: PathBuf,
        cancel: Option<&TransferCancel>,
    ) -> Result<Value> {
        ensure!(
            local.is_absolute() && local.file_name().is_some() && local.to_str().is_some(),
            "Choose an absolute Windows destination for the new dotfiles bundle"
        );
        ensure!(
            !cancel.is_some_and(TransferCancel::is_cancelled),
            "Saving the dotfiles bundle was cancelled"
        );
        let id = normalize_sha256(id)?;
        let home = self.dotfile_home().await?;
        let directory = under_home(&home, &version_directory(shell, &id));
        self.check_entry_parent(&directory).await?;
        let [environment, prompt] = self.verified_dotfile_fragments(&home, shell, &id).await?;
        self.check_entry_parent(&directory).await?;
        let environment = Some(manifest_fragment(environment));
        let prompt = Some(manifest_fragment(prompt));
        let mut bundle = DotfileBundle {
            version: 1,
            environment: None,
            bash: None,
            zsh: None,
            fish_environment: None,
            fish: None,
        };
        match shell {
            DotfileShell::Bash => (bundle.environment, bundle.bash) = (environment, prompt),
            DotfileShell::Zsh => (bundle.environment, bundle.zsh) = (environment, prompt),
            DotfileShell::Fish => (bundle.fish_environment, bundle.fish) = (environment, prompt),
        };
        bundle.verify_version(shell, &id).context(
            "The retained fragments cannot be represented by an exact Windows dotfiles bundle",
        )?;
        let cancel = cancel.cloned();
        let local = tokio::task::spawn_blocking(move || {
            ensure!(
                !cancel.as_ref().is_some_and(TransferCancel::is_cancelled),
                "Saving the dotfiles bundle was cancelled"
            );
            bundle.save_new(&local)?;
            Ok::<_, anyhow::Error>(local)
        })
        .await??;
        Ok(serde_json::to_value(ExportedDotfileBundle {
            shell,
            home,
            bundle_sha256: id,
            local,
        })?)
    }
}
