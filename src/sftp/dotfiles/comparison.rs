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
            let path = format!("{directory}/{role}.sh");
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
