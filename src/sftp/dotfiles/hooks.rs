use super::*;
use std::ops::Range;

pub(super) fn hook_markers(hook: &str) -> [&str; 2] {
    [
        hook.lines().next().expect("generated opening marker"),
        hook.lines().last().expect("generated closing marker"),
    ]
}

pub(super) fn candidate_marker(line: &str, markers: [&str; 2]) -> bool {
    let line = line.trim();
    markers.into_iter().any(|marker| {
        let prefix = marker.trim_end_matches([' ', '<', '>']);
        line.strip_prefix(prefix)
            .is_some_and(|tail| tail.is_empty() || tail.starts_with([' ', '\t', '<', '>']))
    })
}

pub(super) fn managed_block(before: &str, hook: &str) -> Option<Range<usize>> {
    let [begin, end] = hook_markers(hook);
    let mut offset = 0;
    let mut markers = before.split_inclusive('\n').filter_map(|line| {
        let start = offset;
        offset += line.len();
        candidate_marker(line, [begin, end]).then_some((start, line))
    });
    let (start, first) = markers.next()?;
    let (finish, last) = markers.next()?;
    if first.trim_end_matches(['\r', '\n']) != begin
        || last.trim_end_matches(['\r', '\n']) != end
        || markers.next().is_some()
    {
        return None;
    }
    let body = &before[start + first.len()..finish];
    if body.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("# >>> w11term ") || line.starts_with("# <<< w11term ")
    }) {
        return None;
    }
    Some(start..finish + last.len())
}

pub(super) fn repair_hook(mut edit: Edit) -> Edit {
    if edit.after.is_none()
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
    pub(in crate::sftp) async fn repair_dotfile_hooks(
        &self,
        shell: DotfileShell,
        zdotdir: Option<&str>,
        review: Option<&str>,
        stage: &mut JobStage,
    ) -> Result<Value> {
        validate_zdotdir(shell, zdotdir)?;
        let home = self.dotfile_home().await?;
        let mut directories = Vec::new();
        if shell == DotfileShell::Fish {
            for relative in [CONFIG_DIRECTORY, FISH_DIRECTORY] {
                let path = under_home(&home, relative);
                if !self.dotfile_directory_exists(&path).await? {
                    directories.push(path);
                }
            }
        }
        let edits = self
            .dotfile_hooks(&home, shell, zdotdir)
            .await?
            .into_iter()
            .map(repair_hook)
            .collect::<Vec<_>>();
        let atomic = self.extension(editor::POSIX_RENAME, "1").is_ok();
        let mut report = json!({
            "operation": "repair-dotfile-hooks",
            "home": home,
            "shell": shell,
            "zdotdir": zdotdir,
            "files": edits.iter().map(Edit::report).collect::<Vec<_>>(),
            "supports_atomic_replace": atomic,
            "ready": edits.iter().all(|edit| edit.after.is_some()
                && (atomic || edit.before.is_none() || !edit.changed())),
        });
        if shell == DotfileShell::Fish {
            report["directories_to_create"] = json!(directories);
        }
        let Some(review) = review else {
            report["review"] = json!(self.dotfiles_review(&report)?);
            return Ok(report);
        };
        self.check_dotfiles_review(review, &report)?;
        for edit in &edits {
            ensure!(
                edit.after.is_some(),
                "Resolve the startup hook conflict at {}",
                edit.path
            );
            ensure!(
                !edit.changed() || edit.before.is_none() || atomic,
                "The server cannot atomically replace {}",
                edit.path
            );
        }
        let mut changed = Vec::new();
        for path in &directories {
            self.create_dotfile_directory(path).await?;
        }
        for edit in edits.iter().filter(|edit| edit.changed()) {
            let current = self.startup_files(&home, shell, zdotdir).await?;
            ensure!(
                current.len() == edits.len()
                    && current
                        .iter()
                        .zip(&edits)
                        .all(|((path, _), edit)| *path == edit.path),
                "The startup files changed. Review the hooks again."
            );
            self.write_dotfile(edit, stage).await?;
            changed.push(&edit.path);
            *stage = JobStage::Checking(
                "Review the startup hooks again to check any remaining changes.",
            );
        }
        Ok(json!({ "shell": shell, "home": home, "changed_files": changed }))
    }
}
