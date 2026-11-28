use super::hooks::{candidate_marker, hook_markers, managed_block, repair_hook};
use super::*;
use crate::options::{ToolPath, ToolSync};
use crate::sftp::{
    InstalledToolPath,
    utilities::{managed_tool_store, valid_bin_directory, with_lock},
};

struct PathPlan {
    create: Vec<String>,
    edits: Vec<Edit>,
    atomic: bool,
}

impl PathPlan {
    fn check(&self) -> Result<()> {
        for edit in &self.edits {
            ensure!(
                edit.action == "unchanged" || edit.after.is_some(),
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

fn path_hook(shell: DotfileShell, bin: &str) -> String {
    let id = digest_hex(Sha256::new_with_prefix(bin.as_bytes()));
    if shell == DotfileShell::Fish {
        return include_str!("tool-path.fish")
            .replace("@ID@", &id)
            .replace("@BIN@", bin);
    }
    format!(
        "# >>> w11term tool PATH {id} >>>\n\
         case \":${{PATH-}}:\" in\n\
         *\":$HOME/{bin}:\"*) ;;\n\
         *) PATH=\"${{PATH:+$PATH:}}$HOME/{bin}\"; export PATH ;;\n\
         esac\n\
         # <<< w11term tool PATH {id} <<<\n"
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PathEdit {
    Sync,
    Repair,
    Remove,
}

fn plan_edit(path: String, original: Option<String>, hook: String, mode: PathEdit) -> Edit {
    if mode == PathEdit::Remove {
        return remove_path_hook(path, original, &hook);
    }
    let edit = hook_change(path, original, hook);
    if mode == PathEdit::Repair {
        repair_hook(edit)
    } else {
        edit
    }
}

fn remove_path_hook(path: String, original: Option<String>, hook: &str) -> Edit {
    let Some(before) = original.as_deref() else {
        return Edit::new(path, "unchanged", None, None);
    };
    let markers = hook_markers(hook);
    if !before.lines().any(|line| candidate_marker(line, markers)) {
        return Edit::new(path, "unchanged", original.clone(), original);
    }
    let Some(range) = managed_block(before, hook) else {
        return Edit::new(path, "hook-conflict", original, None);
    };
    let block = before[range.clone()].to_owned();
    if block.replace("\r\n", "\n") != hook {
        return Edit::new(path, "hook-conflict", original, None);
    }
    let mut after = before.to_owned();
    after.replace_range(range, "");
    let mut edit = Edit::new(path, "remove-hook", original, Some(after));
    edit.replaced_hook = Some(block);
    edit
}

impl Engine {
    async fn plan_tool_path(
        &self,
        home: &str,
        bin: &str,
        settings: &ToolPath,
        mode: PathEdit,
    ) -> Result<PathPlan> {
        validate_zdotdir(settings.shell, settings.zdotdir.as_deref())?;
        ensure!(
            !home.contains(':'),
            "The account home contains a PATH separator"
        );
        ensure!(
            valid_bin_directory(bin),
            "Invalid approved tool bin directory"
        );
        let hook = path_hook(settings.shell, bin);
        let create = if settings.shell == DotfileShell::Fish && mode != PathEdit::Remove {
            self.missing_fish_startup_directories(home).await?
        } else {
            Vec::new()
        };
        let files = if mode == PathEdit::Remove && settings.shell == DotfileShell::Bash {
            // A higher-priority login file may have appeared since setup.
            let mut files = Vec::with_capacity(4);
            for name in BASH_LOGIN_FILES.into_iter().chain([".bashrc"]) {
                let path = under_home(home, name);
                let original = self.dotfile_text(&path).await?;
                files.push((path, original));
            }
            files
        } else {
            self.startup_files(home, settings.shell, settings.zdotdir.as_deref())
                .await?
        };
        let edits = files
            .into_iter()
            .map(|(path, original)| plan_edit(path, original, hook.clone(), mode))
            .collect();
        Ok(PathPlan {
            create,
            edits,
            atomic: self.extension(editor::POSIX_RENAME, "1").is_ok(),
        })
    }

    async fn apply_tool_path(&self, plan: &PathPlan, stage: &mut JobStage) -> Result<bool> {
        plan.check()?;
        for path in &plan.create {
            self.create_dotfile_directory(path).await?;
        }
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
        let plan = self
            .plan_tool_path(home, bin, settings, PathEdit::Sync)
            .await?;
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
        mut installed: InstalledToolPath,
        review: Option<&str>,
        stage: &mut JobStage,
    ) -> Result<Value> {
        let home = self.dotfile_home().await?;
        installed.bundle_sha256 = normalize_sha256(&installed.bundle_sha256)?;
        let plan = async {
            let bin = self
                .installed_tool_bin_directory(
                    &home,
                    &installed.bundle,
                    &installed.bundle_sha256,
                    !installed.remove,
                )
                .await?;
            self.review_path_change(
                &home,
                &bin,
                &installed.path,
                Some(&installed),
                review,
                stage,
            )
            .await
        };
        if review.is_some() {
            let store = managed_tool_store(&home, &installed.bundle)?;
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
        installed: Option<&InstalledToolPath>,
        review: Option<&str>,
        stage: &mut JobStage,
    ) -> Result<Value> {
        let remove = installed.is_some_and(|target| target.remove);
        let mode = if remove {
            PathEdit::Remove
        } else {
            PathEdit::Repair
        };
        let plan = self.plan_tool_path(home, bin, path, mode).await?;
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
        if path.shell == DotfileShell::Fish {
            report["directories_to_create"] = json!(plan.create);
        }
        if let Some(target) = installed {
            report["bundle"] = json!(target.bundle);
            report["bundle_sha256"] = json!(target.bundle_sha256);
            if remove {
                report["remove"] = json!(true);
            }
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
