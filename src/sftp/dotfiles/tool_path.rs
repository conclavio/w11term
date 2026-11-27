use super::*;
use crate::options::{ToolPath, ToolSync};
use crate::sftp::utilities::{managed_tool_store, valid_bin_directory, with_lock};
use std::ops::Range;

struct PathPlan {
    edits: [Edit; 2],
    atomic: bool,
}

impl PathPlan {
    fn check(&self) -> Result<()> {
        for edit in &self.edits {
            ensure!(
                edit.after.is_some(),
                "Resolve the managed PATH conflict at {}",
                edit.path
            );
            ensure!(
                !edit.changed() || edit.before.is_none() || self.atomic,
                "The server cannot atomically replace {}",
                edit.path
            );
        }
        Ok(())
    }

    fn changed(&self) -> impl Iterator<Item = &Edit> {
        self.edits.iter().filter(|edit| edit.changed())
    }
}

fn path_hook(bin: &str) -> String {
    let id = digest_hex(Sha256::new_with_prefix(bin.as_bytes()));
    format!(
        "# >>> w11term tool PATH {id} >>>\n\
         case \":${{PATH-}}:\" in\n\
         *\":$HOME/{bin}:\"*) ;;\n\
         *) PATH=\"${{PATH:+$PATH:}}$HOME/{bin}\"; export PATH ;;\n\
         esac\n\
         # <<< w11term tool PATH {id} <<<\n"
    )
}

fn managed_block(before: &str, hook: &str) -> Option<Range<usize>> {
    let begin = hook.lines().next()?;
    let end = hook.lines().last()?;
    let mut offset = 0;
    let mut markers = before.split_inclusive('\n').filter_map(|line| {
        let start = offset;
        offset += line.len();
        [begin, end].contains(&line.trim()).then_some((start, line))
    });
    let (start, first) = markers.next()?;
    let (finish, last) = markers.next()?;
    (first.trim_end_matches(['\r', '\n']) == begin
        && last.trim_end_matches(['\r', '\n']) == end
        && markers.next().is_none())
    .then_some(start..finish + last.len())
}

fn plan_edit(path: String, original: Option<String>, hook: String, repair: bool) -> Edit {
    let mut edit = hook_change(path, original, hook);
    if repair
        && edit.after.is_none()
        && let (Some(before), Some(hook)) = (&edit.before, &edit.hook)
        && let Some(range) = managed_block(before, hook)
        && before.len() - range.len() + hook.len() <= TEXT_PREVIEW_MAX_BYTES
    {
        let mut after = before.clone();
        let replaced = before[range.clone()].to_owned();
        after.replace_range(range, hook);
        edit.action = "replace-hook";
        edit.after = Some(after);
        edit.replaced_hook = Some(replaced);
    }
    edit
}

impl Engine {
    async fn plan_tool_path(
        &self,
        home: &str,
        bin: &str,
        settings: &ToolPath,
        repair: bool,
    ) -> Result<PathPlan> {
        validate_zdotdir(settings.shell, settings.zdotdir.as_deref())?;
        ensure!(
            valid_bin_directory(bin),
            "Invalid approved tool bin directory"
        );
        let hook = path_hook(bin);
        let edits = self
            .startup_files(home, settings.shell, settings.zdotdir.as_deref())
            .await?
            .map(|(path, original)| plan_edit(path, original, hook.clone(), repair));
        Ok(PathPlan {
            edits,
            atomic: self.extension(editor::POSIX_RENAME, "1").is_ok(),
        })
    }

    async fn apply_tool_path(&self, plan: &PathPlan, stage: &mut JobStage) -> Result<bool> {
        plan.check()?;
        let mut changed = false;
        for edit in plan.changed() {
            self.write_dotfile(edit, stage).await?;
            changed = true;
        }
        Ok(changed)
    }

    pub(in crate::sftp) async fn sync_tool_path(
        &self,
        home: &str,
        bin: &str,
        settings: &ToolPath,
    ) -> Result<bool> {
        let plan = self.plan_tool_path(home, bin, settings, false).await?;
        let mut stage = JobStage::Checking("Updating managed tool PATH");
        self.apply_tool_path(&plan, &mut stage).await
    }

    pub(in crate::sftp) async fn review_tool_path(
        &self,
        session: &russh::client::Handle<crate::ssh::Handler>,
        settings: &ToolSync,
        review: Option<&str>,
        stage: &mut JobStage,
    ) -> Result<Value> {
        let path = settings
            .add_to_path
            .as_ref()
            .context("This profile has no managed tool PATH approval")?;
        let home = self.dotfile_home().await?;
        ensure!(
            home == settings.home,
            "SFTP home differs from the approved tool bundle home"
        );
        let plan =
            self.review_path_change(&home, &settings.bin_directory, path, None, review, stage);
        if review.is_some() {
            with_lock(session, &home, plan).await
        } else {
            plan.await
        }
    }

    pub(in crate::sftp) async fn review_installed_tool_path(
        &self,
        session: &russh::client::Handle<crate::ssh::Handler>,
        bundle: &str,
        bundle_sha256: &str,
        path: &ToolPath,
        review: Option<&str>,
        stage: &mut JobStage,
    ) -> Result<Value> {
        let home = self.dotfile_home().await?;
        let id = normalize_sha256(bundle_sha256)?;
        let plan = async {
            let bin = self
                .installed_tool_bin_directory(&home, bundle, &id)
                .await?;
            self.review_path_change(&home, &bin, path, Some((bundle, &id)), review, stage)
                .await
        };
        if review.is_some() {
            let store = managed_tool_store(&home, bundle)?;
            self.check_entry_parent(&store).await?;
            with_lock(session, &store, with_lock(session, &home, plan)).await
        } else {
            plan.await
        }
    }

    async fn review_path_change(
        &self,
        home: &str,
        bin: &str,
        path: &ToolPath,
        installed: Option<(&str, &str)>,
        review: Option<&str>,
        stage: &mut JobStage,
    ) -> Result<Value> {
        ensure!(
            !home.contains(':'),
            "The account home contains a PATH separator"
        );
        let plan = self.plan_tool_path(home, bin, path, true).await?;
        let block = plan.check().err().map(|error| error.to_string());
        let ready = block.is_none() && plan.changed().next().is_some();
        let mut report = json!({
            "operation": "tool-path",
            "home": home,
            "bin_directory": bin,
            "shell": path.shell,
            "files": plan.edits.iter().map(Edit::report).collect::<Vec<_>>(),
            "blocked": block,
            "ready": ready,
        });
        if let Some((bundle, id)) = installed {
            report["bundle"] = json!(bundle);
            report["bundle_sha256"] = json!(id);
        }
        if let Some(review) = review {
            ensure!(
                self.dotfiles_review(&report)? == normalize_sha256(review)?,
                "The tool PATH review is stale or belongs to another SFTP connection. Review again."
            );
            plan.check()?;
            ensure!(ready, "The tool PATH is already current");
            let changed = self.apply_tool_path(&plan, stage).await?;
            Ok(json!({ "changed": changed }))
        } else {
            if ready {
                report["review"] = json!(self.dotfiles_review(&report)?);
            }
            Ok(report)
        }
    }
}
