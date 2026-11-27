use super::*;

const DIRECTORY: &str = ".config/w11term/dotfiles";
const PARENTS: [&str; 3] = [".config", ".config/w11term", DIRECTORY];
const DISABLED: &str = "disabled\n";
const MANUAL: &str = "manual\n";
const BEGIN: &str = "# >>> w11term dotfiles >>>";
const END: &str = "# <<< w11term dotfiles <<<";
const MAX_FRAGMENT_BYTES: usize = 16 * 1024;

pub(crate) async fn sync_login_dotfiles(
    session: &russh::client::Handle<crate::ssh::Handler>,
    settings: &crate::options::DotfileSync,
) -> Result<&'static str> {
    let shell = settings.shell;
    let zdotdir = settings.zdotdir.as_deref();
    validate_zdotdir(shell, zdotdir)?;
    let engine = Engine::connect(session).await?;
    let home = engine.dotfile_home().await?;
    ensure!(
        home == settings.home,
        "SFTP home differs from the approved dotfiles home"
    );
    let mut managed_exists = true;
    for relative in PARENTS {
        if !engine
            .dotfile_directory_exists(&under_home(&home, relative))
            .await?
        {
            managed_exists = false;
            break;
        }
    }
    if managed_exists
        && engine
            .dotfile_text(&selector_path(&home, shell))
            .await?
            .as_deref()
            .is_some_and(selector_held)
    {
        return Ok("held");
    }

    let bundle_path = settings.bundle.clone();
    let bundle = tokio::task::spawn_blocking(move || DotfileBundle::load(&bundle_path))
        .await?
        .context("Opening the configured dotfiles bundle")?;
    bundle.validate(shell)?;
    let plan = engine
        .dotfiles_plan_at(home, &bundle, shell, zdotdir)
        .await?;
    if plan.selector.before.as_deref().is_some_and(selector_held) {
        return Ok("held");
    }
    if let Some(active) = plan.selector.active_version() {
        engine
            .verified_dotfile_version(&plan.home, shell, zdotdir, active)
            .await
            .context("The active bundle or its hooks changed. Review dotfiles manually.")?;
    }
    let mut stage = JobStage::Checking("Syncing dotfiles");
    let changed = engine
        .install_dotfiles(&plan, &bundle, zdotdir, &mut stage)
        .await
        .with_context(|| stage.interrupted())?;
    Ok(if changed.is_empty() {
        "current"
    } else {
        "updated"
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DotfileShell {
    Bash,
    Zsh,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum DotfileTarget {
    Bundle { sha256: String },
    Disabled,
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
    pub fn load(path: &Path) -> Result<Self> {
        use std::io::Read;
        const MAX_BYTES: usize = 1024 * 1024;
        ensure!(
            !path.as_os_str().is_empty(),
            "Choose a dotfiles bundle file"
        );
        ensure!(
            std::fs::metadata(path)?.is_file(),
            "Choose a regular dotfiles bundle file"
        );
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take((MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= MAX_BYTES, "Dotfiles bundle exceeds 1 MiB");
        serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
            .context("Reading the dotfiles bundle JSON")
    }

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

pub fn validate_dotfiles_home(home: &str) -> Result<()> {
    ensure!(
        home.len() <= 4096 && (home == "/" || home.strip_prefix('/').is_some_and(relative_path)),
        "Enter an absolute Linux home directory"
    );
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

    fn active_version(&self) -> Option<&str> {
        self.before.as_deref().and_then(selector_version)
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

    fn report(&self) -> Value {
        json!({
            "shell": self.shell,
            "home": self.home,
            "bundle_sha256": self.id,
            "active_bundle_sha256": self.selector.active_version(),
            "directories_to_create": self.create,
            "files": self.edits().map(Edit::report).collect::<Vec<_>>(),
            "supports_atomic_replace": self.atomic_replace,
        })
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
        let mut report = self.dotfiles_plan(&bundle, shell, zdotdir).await?.report();
        report["review"] = json!(self.dotfiles_review(&report)?);
        Ok(report)
    }

    pub(super) async fn apply_dotfiles(
        &self,
        bundle: DotfileBundle,
        shell: DotfileShell,
        zdotdir: Option<&str>,
        review: &str,
        stage: &mut JobStage,
    ) -> Result<Value> {
        let plan = self.dotfiles_plan(&bundle, shell, zdotdir).await?;
        self.check_dotfiles_review(review, &plan.report())?;
        let changed = self
            .install_dotfiles(&plan, &bundle, zdotdir, stage)
            .await?;
        Ok(
            json!({ "shell": shell, "home": plan.home, "bundle_sha256": plan.id, "changed_files": changed }),
        )
    }

    async fn install_dotfiles<'a>(
        &self,
        plan: &'a Plan,
        bundle: &DotfileBundle,
        zdotdir: Option<&str>,
        stage: &mut JobStage,
    ) -> Result<Vec<&'a str>> {
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
            let current = self.dotfiles_plan(bundle, plan.shell, zdotdir).await?;
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
        Ok(changed)
    }

    pub(super) async fn switch_dotfiles(
        &self,
        target: DotfileTarget,
        shell: DotfileShell,
        zdotdir: Option<&str>,
        review: Option<&str>,
        stage: &mut JobStage,
    ) -> Result<Value> {
        validate_zdotdir(shell, zdotdir)?;
        let id = match target {
            DotfileTarget::Bundle { sha256 } => Some(normalize_sha256(&sha256)?),
            DotfileTarget::Disabled => None,
        };
        let home = self.dotfile_home().await?;
        for relative in PARENTS {
            let path = under_home(&home, relative);
            ensure!(
                self.dotfile_directory_exists(&path).await?,
                "Managed dotfiles directory is missing: {path}"
            );
        }
        let files = match &id {
            Some(id) => {
                self.verified_dotfile_version(&home, shell, zdotdir, id)
                    .await?
            }
            None => Vec::new(),
        };
        let desired = id
            .as_ref()
            .map_or_else(|| DISABLED.to_owned(), |id| format!("{id}\n{MANUAL}"));
        let selector = self.dotfile_selector(&home, shell, desired).await?;
        let atomic_replace = self.extension(editor::POSIX_RENAME, "1").is_ok();
        let mut report = json!({
            "operation": "switch-dotfiles",
            "shell": shell,
            "home": home,
            "bundle_sha256": id,
            "active_bundle_sha256": selector.active_version(),
            "selector": selector.report(),
            "files": files,
            "supports_atomic_replace": atomic_replace,
        });
        let Some(review) = review else {
            report["review"] = json!(self.dotfiles_review(&report)?);
            return Ok(report);
        };
        self.check_dotfiles_review(review, &report)?;
        ensure!(
            selector.action != "selector-conflict",
            "Resolve the dotfiles conflict at {}",
            selector.path
        );
        let changed = selector.changed();
        if changed {
            ensure!(
                selector.before.is_none() || atomic_replace,
                "The server cannot atomically replace {}",
                selector.path
            );
            self.write_dotfile(&selector, stage).await?;
        }
        Ok(json!({"shell": shell, "home": home, "bundle_sha256": id, "changed": changed}))
    }

    async fn verified_dotfile_version(
        &self,
        home: &str,
        shell: DotfileShell,
        zdotdir: Option<&str>,
        id: &str,
    ) -> Result<Vec<Value>> {
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
                Ok::<_, anyhow::Error>((path, contents))
            }
        };
        let (environment_path, environment) = read("environment").await?;
        let (prompt_path, prompt) = read("prompt").await?;
        ensure!(
            bundle_id(&environment, &prompt) == id,
            "Retained dotfiles do not match bundle {id}"
        );
        let hooks = self.dotfile_hooks(home, shell, zdotdir).await?;
        for hook in &hooks {
            ensure!(
                hook.action == "unchanged",
                "Install or repair the managed startup hook at {} before restoring a bundle",
                hook.path
            );
        }
        let mut files = Vec::with_capacity(4);
        for (path, contents) in [(environment_path, environment), (prompt_path, prompt)] {
            files.push(
                Edit::new(path, "unchanged", Some(contents.clone()), Some(contents)).report(),
            );
        }
        files.extend(hooks.iter().map(Edit::report));
        Ok(files)
    }

    fn dotfiles_review(&self, report: &Value) -> Result<String> {
        let mut digest = Sha256::new_with_prefix(self.dotfiles_review_scope.to_le_bytes());
        digest.update(serde_json::to_vec(report)?);
        Ok(digest_hex(digest))
    }

    fn check_dotfiles_review(&self, token: &str, report: &Value) -> Result<()> {
        let token = normalize_sha256(token).context("Invalid dotfiles review token")?;
        ensure!(
            self.dotfiles_review(report)? == token,
            "The dotfiles review is stale or belongs to another SFTP connection. Preview again."
        );
        Ok(())
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
        let home = self.dotfile_home().await?;
        self.dotfiles_plan_at(home, bundle, shell, zdotdir).await
    }

    async fn dotfiles_plan_at(
        &self,
        home: String,
        bundle: &DotfileBundle,
        shell: DotfileShell,
        zdotdir: Option<&str>,
    ) -> Result<Plan> {
        let environment = payload(bundle.environment.as_deref());
        let prompt = payload(bundle.prompt(shell));
        let id = bundle_id(&environment, &prompt);
        let version_directory = version_directory(shell, &id);
        let mut directories_to_create = Vec::new();
        for relative in PARENTS.into_iter().chain([version_directory.as_str()]) {
            let path = under_home(&home, relative);
            if !self.dotfile_directory_exists(&path).await? {
                directories_to_create.push(path);
            }
        }
        let mut files = Vec::with_capacity(4);
        for (role, desired) in [("environment", environment), ("prompt", prompt)] {
            let path = under_home(&home, &format!("{version_directory}/{role}.sh"));
            let original = self.dotfile_text(&path).await?;
            let (action, after) = match original.as_deref() {
                None => ("create", Some(desired)),
                Some(text) if text == desired => ("unchanged", Some(desired)),
                Some(_) => ("payload-conflict", None),
            };
            files.push(Edit::new(path, action, original, after));
        }
        files.extend(self.dotfile_hooks(&home, shell, zdotdir).await?);
        let selector = self
            .dotfile_selector(&home, shell, format!("{id}\n"))
            .await?;
        Ok(Plan {
            home,
            id,
            create: directories_to_create,
            files,
            selector,
            shell,
            atomic_replace: self.extension(editor::POSIX_RENAME, "1").is_ok(),
        })
    }

    pub(super) async fn dotfile_home(&self) -> Result<String> {
        let homes = self.raw.realpath(".").await?.files;
        let [home] = homes.as_slice() else {
            bail!("SFTP did not resolve the remote home directory to one path");
        };
        let home = home.filename.as_str();
        validate_dotfiles_home(home).context("SFTP returned an invalid home directory")?;
        Ok(home.to_owned())
    }

    async fn dotfile_hooks(
        &self,
        home: &str,
        shell: DotfileShell,
        zdotdir: Option<&str>,
    ) -> Result<[Edit; 2]> {
        let home_prefix = home.trim_end_matches('/');
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
        Ok([
            hook_change(login, login_original, hook_text(shell, Hook::Login)),
            hook_change(rc, rc_original, hook_text(shell, Hook::Interactive)),
        ])
    }

    async fn dotfile_selector(
        &self,
        home: &str,
        shell: DotfileShell,
        desired: String,
    ) -> Result<Edit> {
        let path = selector_path(home, shell);
        let before = self.dotfile_text(&path).await?;
        Ok(match before.as_deref() {
            None if desired == DISABLED => Edit::new(path, "unchanged", None, None),
            None => Edit::new(path, "create", None, Some(desired)),
            Some(text) if text == desired => Edit::new(path, "unchanged", before, Some(desired)),
            Some(text) if text == DISABLED || selector_version(text).is_some() => {
                Edit::new(path, "replace", before, Some(desired))
            }
            Some(_) => Edit::new(path, "selector-conflict", before, None),
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

fn selector_version(text: &str) -> Option<&str> {
    text.strip_suffix(MANUAL)
        .unwrap_or(text)
        .strip_suffix('\n')
        .filter(|line| {
            line.len() == 64
                && line
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

fn selector_held(text: &str) -> bool {
    text == DISABLED || (text.ends_with(MANUAL) && selector_version(text).is_some())
}

fn selector_path(home: &str, shell: DotfileShell) -> String {
    under_home(home, &format!("{DIRECTORY}/{}-current", shell.name()))
}

fn bundle_id(environment: &str, prompt: &str) -> String {
    let mut digest = Sha256::new_with_prefix(b"w11term-dotfiles-v1\0");
    digest.update((environment.len() as u64).to_le_bytes());
    digest.update(environment.as_bytes());
    digest.update(prompt.as_bytes());
    digest_hex(digest)
}

fn version_directory(shell: DotfileShell, id: &str) -> String {
    format!("{DIRECTORY}/{}-{id}", shell.name())
}

pub(super) fn under_home(home: &str, relative: &str) -> String {
    format!("{}/{relative}", home.trim_end_matches('/'))
}

fn remote_parent(path: &str) -> Result<&str> {
    path.rsplit_once('/')
        .map(|(parent, _)| parent)
        .context("Expected an absolute remote dotfile path")
}
