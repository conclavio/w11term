use super::*;

const DIRECTORY: &str = ".config/w11term/dotfiles";
const BEGIN: &str = "# >>> w11term dotfiles >>>";
const END: &str = "# <<< w11term dotfiles <<<";
const MAX_FRAGMENT_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DotfileShell {
    Bash,
    Zsh,
}

impl DotfileShell {
    fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
        }
    }

    fn version_variable(self) -> &'static str {
        match self {
            Self::Bash => "BASH_VERSION",
            Self::Zsh => "ZSH_VERSION",
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DotfileBundle {
    pub version: u8,
    pub environment: Option<String>,
    pub bash: Option<String>,
    pub zsh: Option<String>,
}

impl DotfileBundle {
    pub fn validate(&self, shell: DotfileShell) -> Result<()> {
        ensure!(self.version == 1, "Unsupported dotfiles bundle version");
        let prompt = self.prompt(shell);
        ensure!(
            self.environment.is_some() || prompt.is_some(),
            "The bundle needs an environment or {} fragment",
            shell.name()
        );
        for fragment in [&self.environment, &self.bash, &self.zsh]
            .into_iter()
            .flatten()
        {
            ensure!(
                fragment.len() <= MAX_FRAGMENT_BYTES
                    && !fragment.contains('\0')
                    && !fragment.starts_with('\u{feff}'),
                "Dotfile fragments must be at most 16 KiB of UTF-8 without NUL or a BOM"
            );
            ensure!(
                !fragment.replace("\r\n", "\n").contains('\r'),
                "Dotfile fragments must use LF or CRLF line endings"
            );
        }
        Ok(())
    }

    fn prompt(&self, shell: DotfileShell) -> Option<&str> {
        match shell {
            DotfileShell::Bash => self.bash.as_deref(),
            DotfileShell::Zsh => self.zsh.as_deref(),
        }
    }
}

pub fn validate_zdotdir(shell: DotfileShell, zdotdir: Option<&str>) -> Result<()> {
    if let Some(path) = zdotdir {
        ensure!(
            matches!(shell, DotfileShell::Zsh) && path.len() <= 512 && relative_path(path),
            "ZDOTDIR must be a relative directory under the Zsh account's home"
        );
    }
    Ok(())
}

fn relative_path(path: &str) -> bool {
    !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

struct Edit {
    path: String,
    action: &'static str,
    before: Option<String>,
    after: Option<String>,
    hook: Option<String>,
}

impl Edit {
    fn new(
        path: String,
        action: &'static str,
        before: Option<String>,
        after: Option<String>,
    ) -> Self {
        Self {
            path,
            action,
            before,
            after,
            hook: None,
        }
    }

    fn changed(&self) -> bool {
        self.before != self.after
    }

    fn report(&self) -> Value {
        let hash = |text: &str| digest_hex(Sha256::new_with_prefix(text));
        let mut report = json!({
            "path": self.path,
            "action": self.action,
            "before_sha256": self.before.as_deref().map(hash),
            "after_sha256": self.after.as_deref().map(hash),
        });
        if let Some(hook) = &self.hook {
            report["hook"] = json!(hook);
        }
        report
    }
}

struct Plan {
    home: String,
    id: String,
    create: Vec<String>,
    files: Vec<Edit>,
    selector: Edit,
    shell: DotfileShell,
    atomic_replace: bool,
}

impl Plan {
    fn edits(&self) -> impl Iterator<Item = &Edit> {
        self.files.iter().chain(std::iter::once(&self.selector))
    }

    fn report(&self, review_scope: u128) -> Result<Value> {
        let mut report = json!({
            "shell": self.shell,
            "home": self.home,
            "bundle_sha256": self.id,
            "directories_to_create": self.create,
            "files": self.edits().map(Edit::report).collect::<Vec<_>>(),
            "supports_atomic_replace": self.atomic_replace,
        });
        let mut digest = Sha256::new_with_prefix(review_scope.to_le_bytes());
        digest.update(serde_json::to_vec(&report)?);
        report["review"] = json!(digest_hex(digest));
        Ok(report)
    }
}

fn payload(fragment: Option<&str>) -> String {
    let mut text = fragment.unwrap_or_default().replace("\r\n", "\n");
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

#[derive(Clone, Copy)]
enum Hook {
    Login,
    Interactive,
}

fn hook_text(shell: DotfileShell, hook: Hook) -> String {
    let name = shell.name();
    let version = shell.version_variable();
    let selected = format!("_w11term_dotfiles_{name}_version");
    let selector = format!("$HOME/{DIRECTORY}/{name}-current");
    let source = |role| {
        let flag = format!("_w11term_dotfiles_{name}_{role}");
        let path = format!("$HOME/{DIRECTORY}/{name}-${{{selected}}}/{role}.sh");
        format!(
            "if [ \"${{{flag}-}}\" != 1 ] && [ -r \"{path}\" ]; then\n  {flag}=1\n  . \"{path}\"\nfi\n"
        )
    };
    let environment = source("environment");
    let prompt = source("prompt");
    let interactive = |body: &str| format!("case $- in *i*)\n{body};;\nesac\n");
    let body = match (shell, hook) {
        (_, Hook::Interactive) => interactive(&format!("{environment}{prompt}")),
        (DotfileShell::Bash, Hook::Login) => format!("{environment}{}", interactive(&prompt)),
        (DotfileShell::Zsh, Hook::Login) => environment,
    };
    format!(
        "{BEGIN}
if [ -n \"${{{version}-}}\" ]; then
  if [ -z \"${{{selected}-}}\" ] && [ -r \"{selector}\" ]; then
    IFS= read -r {selected} < \"{selector}\" || {selected}=
  fi
  case \"${{{selected}-}}\" in
    ''|*[!0123456789abcdef]*) ;;
    *)
      if [ \"${{#{selected}}}\" -eq 64 ]; then
{body}      fi
      ;;
  esac
fi
{END}\n"
    )
}

fn hook_change(path: String, original: Option<String>, hook: String) -> Edit {
    let text = original.as_deref().unwrap_or_default();
    let markers: Vec<_> = text
        .lines()
        .filter(|line| matches!(line.trim(), BEGIN | END))
        .take(3)
        .collect();
    let (action, after) = if markers.is_empty() {
        match original.as_deref() {
            None => ("create", Some(hook.clone())),
            Some(text) => {
                let mut result = text.to_owned();
                if !result.is_empty() && !result.ends_with('\n') {
                    result.push('\n');
                }
                result.push_str(&hook);
                if result.len() <= TEXT_PREVIEW_MAX_BYTES {
                    ("append-hook", Some(result))
                } else {
                    ("hook-conflict", None)
                }
            }
        }
    } else if markers == [BEGIN, END]
        && text
            .match_indices(&hook)
            .any(|(start, _)| start == 0 || text.as_bytes()[start - 1] == b'\n')
    {
        ("unchanged", original.clone())
    } else {
        ("hook-conflict", None)
    };
    let mut change = Edit::new(path, action, original, after);
    change.hook = Some(hook);
    change
}

impl Engine {
    pub(super) async fn preview_dotfiles(
        &self,
        bundle: DotfileBundle,
        shell: DotfileShell,
        zdotdir: Option<&str>,
    ) -> Result<Value> {
        self.dotfiles_plan(&bundle, shell, zdotdir)
            .await?
            .report(self.dotfiles_review_scope)
    }

    pub(super) async fn apply_dotfiles(
        &self,
        bundle: DotfileBundle,
        shell: DotfileShell,
        zdotdir: Option<&str>,
        review: &str,
        stage: &mut JobStage,
    ) -> Result<Value> {
        let review = normalize_sha256(review).context("Invalid dotfiles review token")?;
        let plan = self.dotfiles_plan(&bundle, shell, zdotdir).await?;
        ensure!(
            plan.report(self.dotfiles_review_scope)?["review"] == review,
            "The dotfiles review is stale or belongs to another SFTP connection. Preview again."
        );
        for edit in plan.edits() {
            ensure!(
                edit.after.is_some(),
                "Resolve the dotfiles conflict at {}",
                edit.path
            );
            ensure!(
                !edit.changed() || edit.before.is_none() || plan.atomic_replace,
                "The server cannot atomically replace {}",
                edit.path
            );
        }
        const INTERRUPTED: &str =
            "Dotfiles installation was interrupted. Preview again to check the active bundle.";
        let mut changed = Vec::new();
        for path in &plan.create {
            self.check_entry_parent(remote_parent(path)?).await?;
            self.raw
                .mkdir(
                    path,
                    FileAttributes {
                        permissions: Some(0o700),
                        ..FileAttributes::empty()
                    },
                )
                .await
                .with_context(|| format!("Creating dotfiles directory {path}"))?;
        }
        for edit in &plan.files {
            if edit.changed() {
                self.write_dotfile(edit, stage).await?;
                changed.push(edit.path.as_str());
                *stage = JobStage::Checking(INTERRUPTED);
            }
        }
        if plan.selector.changed() {
            let current = self.dotfiles_plan(&bundle, shell, zdotdir).await?;
            ensure!(
                current.create.is_empty()
                    && current.files.len() == plan.files.len()
                    && current
                        .files
                        .iter()
                        .zip(&plan.files)
                        .all(|(now, reviewed)| {
                            now.path == reviewed.path
                                && now.before == reviewed.after
                                && !now.changed()
                        })
                    && current.selector.path == plan.selector.path
                    && current.selector.before == plan.selector.before,
                "Remote dotfiles changed during staging. Preview again before activating."
            );
            self.write_dotfile(&plan.selector, stage).await?;
            changed.push(plan.selector.path.as_str());
        }
        Ok(
            json!({ "shell": shell, "home": plan.home, "bundle_sha256": plan.id, "changed_files": changed }),
        )
    }

    async fn write_dotfile(&self, edit: &Edit, stage: &mut JobStage) -> Result<()> {
        self.check_entry_parent(remote_parent(&edit.path)?).await?;
        self.save_text(
            &edit.path,
            edit.before.as_deref(),
            edit.after.as_deref().expect("validated edit"),
            stage,
        )
        .await
        .with_context(|| format!("Installing dotfile {}", edit.path))
    }

    async fn dotfiles_plan(
        &self,
        bundle: &DotfileBundle,
        shell: DotfileShell,
        zdotdir: Option<&str>,
    ) -> Result<Plan> {
        bundle.validate(shell)?;
        validate_zdotdir(shell, zdotdir)?;
        let environment = payload(bundle.environment.as_deref());
        let prompt = payload(bundle.prompt(shell));
        let mut digest = Sha256::new_with_prefix(b"w11term-dotfiles-v1\0");
        digest.update((environment.len() as u64).to_le_bytes());
        digest.update(environment.as_bytes());
        digest.update(prompt.as_bytes());
        let id = digest_hex(digest);
        let version_directory = format!("{DIRECTORY}/{}-{id}", shell.name());
        let homes = self.raw.realpath(".").await?.files;
        let [home] = homes.as_slice() else {
            bail!("SFTP did not resolve the remote home directory to one path");
        };
        let home = home.filename.as_str();
        ensure!(
            home.len() <= 4096
                && (home == "/" || home.strip_prefix('/').is_some_and(relative_path)),
            "SFTP did not return an absolute remote home directory"
        );
        let home_prefix = home.trim_end_matches('/');
        let under_home = |relative: &str| format!("{home_prefix}/{relative}");
        let mut directories_to_create = Vec::new();
        for relative in [".config", ".config/w11term", DIRECTORY, &version_directory] {
            let path = under_home(relative);
            if !self.dotfile_directory_exists(&path).await? {
                directories_to_create.push(path);
            }
        }
        let startup_dir = if let Some(relative) = zdotdir {
            let mut path = home_prefix.to_owned();
            for part in relative.split('/') {
                path.push('/');
                path.push_str(part);
                ensure!(
                    self.dotfile_directory_exists(&path).await?,
                    "ZDOTDIR directory is missing: {path}"
                );
            }
            path
        } else {
            home_prefix.to_owned()
        };
        let startup = |name: &str| format!("{startup_dir}/{name}");
        let (login, login_original) = match shell {
            DotfileShell::Bash => {
                let mut selected = (startup(".profile"), None);
                for name in [".bash_profile", ".bash_login", ".profile"] {
                    let path = startup(name);
                    if let Some(contents) = self.dotfile_text(&path).await? {
                        selected = (path, Some(contents));
                        break;
                    }
                }
                selected
            }
            DotfileShell::Zsh => {
                let path = startup(".zprofile");
                let contents = self.dotfile_text(&path).await?;
                (path, contents)
            }
        };
        let rc = startup(&format!(".{}rc", shell.name()));
        let rc_original = self.dotfile_text(&rc).await?;
        let mut files = Vec::with_capacity(4);
        for (role, desired) in [("environment", environment), ("prompt", prompt)] {
            let path = under_home(&format!("{version_directory}/{role}.sh"));
            let original = self.dotfile_text(&path).await?;
            let (action, after) = match original.as_deref() {
                None => ("create", Some(desired)),
                Some(text) if text == desired => ("unchanged", Some(desired)),
                Some(_) => ("payload-conflict", None),
            };
            files.push(Edit::new(path, action, original, after));
        }
        files.push(hook_change(
            login,
            login_original,
            hook_text(shell, Hook::Login),
        ));
        files.push(hook_change(
            rc,
            rc_original,
            hook_text(shell, Hook::Interactive),
        ));
        let path = under_home(&format!("{DIRECTORY}/{}-current", shell.name()));
        let before = self.dotfile_text(&path).await?;
        let desired = format!("{id}\n");
        let selector = match before.as_deref() {
            None => Edit::new(path, "create", None, Some(desired)),
            Some(text) if text == desired => Edit::new(path, "unchanged", before, Some(desired)),
            Some(text) if valid_selector(text) => Edit::new(path, "replace", before, Some(desired)),
            Some(_) => Edit::new(path, "selector-conflict", before, None),
        };
        Ok(Plan {
            home: home.to_owned(),
            id,
            create: directories_to_create,
            files,
            selector,
            shell,
            atomic_replace: self.extension(editor::POSIX_RENAME, "1").is_ok(),
        })
    }

    async fn dotfile_directory_exists(&self, path: &str) -> Result<bool> {
        match self.raw.lstat(path).await {
            Err(Error::Status(status)) if status.status_code == StatusCode::NoSuchFile => Ok(false),
            Err(error) => Err(error).with_context(|| format!("Checking {path}")),
            Ok(result) => {
                ensure!(
                    EntryKind::from_permissions(result.attrs.permissions)
                        == Some(EntryKind::Directory),
                    "Expected a real directory at {path}"
                );
                Ok(true)
            }
        }
    }

    async fn dotfile_text(&self, path: &str) -> Result<Option<String>> {
        match self.raw.lstat(path).await {
            Err(Error::Status(status)) if status.status_code == StatusCode::NoSuchFile => Ok(None),
            Err(error) => Err(error).with_context(|| format!("Checking {path}")),
            Ok(_) => self
                .read_preview_text(path, None)
                .await
                .map(Some)
                .with_context(|| format!("Reading dotfile {path}")),
        }
    }
}

fn valid_selector(text: &str) -> bool {
    text.strip_suffix('\n').is_some_and(|line| {
        line.len() == 64
            && line
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn remote_parent(path: &str) -> Result<&str> {
    path.rsplit_once('/')
        .map(|(parent, _)| parent)
        .context("Expected an absolute remote dotfile path")
}
