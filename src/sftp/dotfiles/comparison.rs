use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum FragmentBaseline {
    Inactive,
    Verified,
    Unavailable,
}

#[derive(Serialize)]
pub(super) struct FragmentComparison {
    baseline: FragmentBaseline,
    fragments: Vec<FragmentChange>,
}

#[derive(Serialize)]
struct FragmentChange {
    role: &'static str,
    before: Option<String>,
    after: Option<String>,
}

impl DotfileBundle {
    pub fn compare_local(
        &self,
        after: &Self,
        selected_shell: Option<DotfileShell>,
        include_text: bool,
    ) -> Result<Value> {
        self.validate_for_editing()
            .context("Validating the before dotfiles bundle")?;
        after
            .validate_for_editing()
            .context("Validating the after dotfiles bundle")?;
        let mut shells = Vec::new();
        let mut changed = false;
        for shell in DotfileShell::ALL {
            if selected_shell.is_some_and(|selected| selected != shell) {
                continue;
            }
            let before = self.has_fragments_for(shell).then(|| self.fragments(shell));
            let after = after
                .has_fragments_for(shell)
                .then(|| after.fragments(shell));
            if before.is_none() && after.is_none() {
                continue;
            }
            let digest = |fragments: &Option<[String; 2]>| {
                fragments
                    .as_ref()
                    .map(|[environment, prompt]| bundle_id(environment, prompt))
            };
            let before_digest = digest(&before);
            let after_digest = digest(&after);
            let change = match (&before_digest, &after_digest) {
                (None, _) => "added",
                (_, None) => "removed",
                (Some(before), Some(after)) if before == after => "unchanged",
                _ => "changed",
            };
            changed |= before_digest != after_digest;
            let fragments: Vec<_> = FRAGMENT_ROLES
                .into_iter()
                .enumerate()
                .filter_map(|(index, role)| {
                    let before = before.as_ref().map(|fragments| fragments[index].as_str());
                    let after = after.as_ref().map(|fragments| fragments[index].as_str());
                    if before.unwrap_or_default() == after.unwrap_or_default() {
                        return None;
                    }
                    Some(if include_text {
                        json!({"role": role, "before": before, "after": after})
                    } else {
                        json!({"role": role})
                    })
                })
                .collect();
            shells.push(json!({
                "shell": shell,
                "change": change,
                "before_bundle_sha256": before_digest,
                "after_bundle_sha256": after_digest,
                "changed_fragments": fragments,
            }));
        }
        if let Some(shell) = selected_shell {
            ensure!(
                !shells.is_empty(),
                "Neither bundle has fragments for {}",
                shell.label()
            );
        }
        Ok(json!({"changed": changed, "shells": shells}))
    }
}

impl Engine {
    pub(super) async fn verified_dotfile_fragments(
        &self,
        home: &str,
        shell: DotfileShell,
        id: &str,
    ) -> Result<[String; 2]> {
        let directory = under_home(home, &version_directory(shell, id));
        ensure!(
            self.dotfile_directory_exists(&directory).await?,
            "Retained dotfiles bundle not found: {id}"
        );
        let read = |role| {
            let path = fragment_path(&directory, shell, role);
            async move {
                let contents = self
                    .dotfile_text(&path)
                    .await?
                    .with_context(|| format!("Retained dotfile is missing: {path}"))?;
                ensure!(
                    contents.len() <= MAX_FRAGMENT_BYTES + 1,
                    "Retained dotfile exceeds its size limit: {path}"
                );
                Ok::<_, anyhow::Error>(contents)
            }
        };
        let [environment, prompt] = FRAGMENT_ROLES;
        let (environment, prompt) = tokio::try_join!(read(environment), read(prompt))?;
        ensure!(
            bundle_id(&environment, &prompt) == id,
            "Retained dotfiles do not match bundle {id}"
        );
        Ok([environment, prompt])
    }

    pub(super) async fn dotfile_comparison(
        &self,
        home: &str,
        shell: DotfileShell,
        active: Option<&str>,
        proposed: Option<[String; 2]>,
    ) -> Result<FragmentComparison> {
        let (baseline, before) = if let Some(id) = active {
            match self.verified_dotfile_fragments(home, shell, id).await {
                Ok(fragments) => (FragmentBaseline::Verified, fragments.map(Some)),
                Err(error) if error.downcast_ref::<Error>().is_some_and(transport_error) => {
                    return Err(error);
                }
                Err(_) => (FragmentBaseline::Unavailable, [None, None]),
            }
        } else {
            (FragmentBaseline::Inactive, [None, None])
        };
        let after = proposed.map_or([None, None], |fragments| fragments.map(Some));
        Ok(FragmentComparison {
            baseline,
            fragments: FRAGMENT_ROLES
                .into_iter()
                .zip(before)
                .zip(after)
                .map(|((role, before), after)| FragmentChange {
                    role,
                    before,
                    after,
                })
                .collect(),
        })
    }

    pub(super) async fn review_dotfile_install(
        &self,
        plan: &Plan,
        bundle: &DotfileBundle,
    ) -> Result<(Value, FragmentBaseline)> {
        let comparison = self
            .dotfile_comparison(
                &plan.home,
                plan.shell,
                plan.selector.active_version(),
                Some(bundle.fragments(plan.shell)),
            )
            .await?;
        let baseline = comparison.baseline;
        let mut report = plan.report();
        report["fragment_changes"] = json!(comparison);
        Ok((report, baseline))
    }
}
