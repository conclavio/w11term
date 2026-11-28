use super::*;
use crate::events::DotfileSyncStatus;

mod comparison;
mod hooks;
mod snapshot;
mod tool_path;
use comparison::FragmentBaseline;

const DIRECTORY: &str = ".config/w11term/dotfiles";
const CONFIG_DIRECTORY: &str = ".config";
const PARENTS: [&str; 3] = [CONFIG_DIRECTORY, ".config/w11term", DIRECTORY];
const FISH_DIRECTORY: &str = ".config/fish";
const DISABLED: &str = "disabled\n";
// Startup hooks only load fragments when the first line is a bundle digest.
const AUTOMATIC: &str = "automatic\n";
const MANUAL: &str = "manual\n";
const BASH_LOGIN_FILES: [&str; 3] = [".bash_profile", ".bash_login", ".profile"];
const BEGIN: &str = "# >>> w11term dotfiles >>>";
const END: &str = "# <<< w11term dotfiles <<<";
const MAX_FRAGMENT_BYTES: usize = 16 * 1024;
const MAX_LISTED_VERSIONS: usize = 64;
const FRAGMENT_ROLES: [&str; 2] = ["environment", "prompt"];

pub(crate) async fn sync_login_dotfiles(
    session: &russh::client::Handle<crate::ssh::Handler>,
    settings: &crate::options::DotfileSync,
) -> Result<DotfileSyncStatus> {
    let shell = settings.shell;
    let startup_directory = select_startup_directory(
        shell,
        settings.zdotdir.as_deref(),
        settings.fish_config_dir.as_deref(),
    )?;
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
        return Ok(DotfileSyncStatus::Held);
    }

    let bundle_path = settings.bundle.clone();
    let bundle = tokio::task::spawn_blocking(move || DotfileBundle::load(&bundle_path))
        .await?
        .context("Opening the configured dotfiles bundle")?;
    match &settings.bundle_sha256 {
        Some(version) => bundle
            .verify_version(shell, version)
            .context("The pinned dotfiles bundle changed. Review it in the SSH profile.")?,
        None => bundle.validate(shell)?,
    }
    let plan = engine
        .dotfiles_plan_at(home, &bundle, shell, startup_directory)
        .await?;
    if plan.selector.before.as_deref().is_some_and(selector_held) {
        return Ok(DotfileSyncStatus::Held);
    }
    if let Some(active) = plan.selector.active_version() {
        engine
            .verified_dotfile_version(&plan.home, shell, startup_directory, active)
            .await
            .context("The active bundle or its hooks changed. Review dotfiles manually.")?;
    }
    let mut stage = JobStage::Checking("Syncing dotfiles");
    let changed = engine
        .install_dotfiles(&plan, &bundle, startup_directory, None, &mut stage)
        .await
        .with_context(|| stage.interrupted())?;
    Ok(if changed.is_empty() {
        DotfileSyncStatus::Current
    } else {
        DotfileSyncStatus::Updated
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DotfileShell {
    #[default]
    Bash,
    Zsh,
    Fish,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum DotfileTarget {
    Bundle { sha256: String },
    Disabled,
    Automatic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DotfileVersionState {
    Inactive,
    AwaitingSync,
    Active,
    Paused,
    Disabled,
    Conflict,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DotfileVersions {
    pub home: String,
    pub active_bundle_sha256: Option<String>,
    pub state: DotfileVersionState,
    pub versions: Vec<RetainedDotfileVersion>,
    pub omitted_versions: usize,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RetainedDotfileVersion {
    pub bundle_sha256: String,
    pub modified: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExportedDotfileBundle {
    pub shell: DotfileShell,
    pub home: String,
    pub bundle_sha256: String,
    pub local: PathBuf,
}

impl DotfileShell {
    pub const ALL: [Self; 3] = [Self::Bash, Self::Zsh, Self::Fish];

    pub fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Bash => "Bash",
            Self::Zsh => "Zsh",
            Self::Fish => "Fish",
        }
    }

    pub fn menu_index(self) -> usize {
        self as usize
    }

    fn fragment_extension(self) -> &'static str {
        match self {
            Self::Bash | Self::Zsh => "sh",
            Self::Fish => "fish",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DotfileBundle {
    pub version: u8,
    pub environment: Option<String>,
    pub bash: Option<String>,
    pub zsh: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fish_environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fish: Option<String>,
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
        self.validate_fragments()?;
        ensure!(
            self.selected_fragments(shell)
                .into_iter()
                .any(|item| item.is_some()),
            "The bundle needs an environment or {} fragment",
            shell.name()
        );
        Ok(())
    }

    pub fn content_version(&self, shell: DotfileShell) -> Result<String> {
        self.validate(shell)?;
        let [environment, prompt] = self.fragments(shell);
        Ok(bundle_id(&environment, &prompt))
    }

    pub fn verify_version(&self, shell: DotfileShell, sha256: &str) -> Result<()> {
        ensure!(
            self.content_version(shell)? == sha256,
            "The dotfiles bundle does not reproduce version {sha256}"
        );
        Ok(())
    }

    pub fn validate_for_editing(&self) -> Result<()> {
        self.validate_fragments()?;
        ensure!(
            self.all_fragments().into_iter().any(|item| item.is_some()),
            "Enter at least one dotfile fragment"
        );
        Ok(())
    }

    pub fn save_new(&self, path: &Path) -> Result<()> {
        self.validate_for_editing()?;
        let mut contents = serde_json::to_string_pretty(self)?;
        contents.push('\n');
        crate::transcript::save(path, &contents, false)
    }

    pub fn append_fragment_from_file(path: &Path, existing: &str) -> Result<String> {
        use std::io::Read;
        // Each LF can have a CR prefix, and a UTF-8 file may begin with a BOM.
        const MAX_FILE_BYTES: usize = 2 * MAX_FRAGMENT_BYTES + 3;
        ensure!(
            std::fs::metadata(path)?.is_file(),
            "Choose a regular shell fragment file"
        );
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take((MAX_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= MAX_FILE_BYTES,
            "Shell fragment file is too large"
        );
        let imported = std::str::from_utf8(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
            .context("Shell fragment files must use UTF-8")?
            .replace("\r\n", "\n");
        ensure!(
            !imported.is_empty(),
            "The selected shell fragment file is empty"
        );
        append_fragment(existing, &imported)
    }

    pub fn append_startup_excerpt(
        existing: &str,
        source: &str,
        range: std::ops::Range<usize>,
    ) -> Result<String> {
        let excerpt = source
            .get(range.clone())
            .context("Invalid shell excerpt range")?;
        let markers = format!("{BEGIN}\n{END}\n");
        ensure!(
            !hooks::managed_block(source, &markers)
                .is_some_and(|block| range.start < block.end && block.start < range.end)
                && !excerpt
                    .lines()
                    .any(|line| hooks::candidate_marker(line, [BEGIN, END])),
            "Choose lines outside the w11term-managed startup block"
        );
        append_fragment(existing, excerpt)
    }

    fn validate_fragments(&self) -> Result<()> {
        ensure!(self.version == 1, "Unsupported dotfiles bundle version");
        for fragment in self.all_fragments().into_iter().flatten() {
            validate_fragment(fragment)?;
        }
        Ok(())
    }

    fn all_fragments(&self) -> [Option<&str>; 5] {
        [
            self.environment.as_deref(),
            self.bash.as_deref(),
            self.zsh.as_deref(),
            self.fish_environment.as_deref(),
            self.fish.as_deref(),
        ]
    }

    fn selected_fragments(&self, shell: DotfileShell) -> [Option<&str>; 2] {
        match shell {
            DotfileShell::Bash => [self.environment.as_deref(), self.bash.as_deref()],
            DotfileShell::Zsh => [self.environment.as_deref(), self.zsh.as_deref()],
            DotfileShell::Fish => [self.fish_environment.as_deref(), self.fish.as_deref()],
        }
    }

    fn fragments(&self, shell: DotfileShell) -> [String; 2] {
        self.selected_fragments(shell).map(payload)
    }
}

fn append_fragment(existing: &str, imported: &str) -> Result<String> {
    validate_fragment(imported)?;
    let mut appended = existing.to_owned();
    if !appended.is_empty() && !appended.ends_with('\n') {
        appended.push('\n');
    }
    appended.push_str(imported);
    validate_fragment(&appended)?;
    Ok(appended)
}

fn validate_fragment(fragment: &str) -> Result<()> {
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
    Ok(())
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

pub fn select_startup_directory<'a>(
    shell: DotfileShell,
    zdotdir: Option<&'a str>,
    fish_config_dir: Option<&'a str>,
) -> Result<Option<&'a str>> {
    validate_zdotdir(shell, zdotdir)?;
    if let Some(path) = fish_config_dir {
        ensure!(
            shell == DotfileShell::Fish && path.len() <= 512 && relative_path(path),
            "The Fish configuration directory must be a relative directory under the Fish account's home"
        );
    }
    Ok(zdotdir.or(fish_config_dir))
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
    replaced_hook: Option<String>,
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
            replaced_hook: None,
        }
    }

    fn changed(&self) -> bool {
        self.before != self.after
    }

    fn active_version(&self) -> Option<&str> {
        self.before.as_deref().and_then(selector_version)
    }

    fn selector(path: String, before: Option<String>, desired: String) -> Self {
        match before.as_deref() {
            None if desired == DISABLED => Self::new(path, "unchanged", None, None),
            None => Self::new(path, "create", None, Some(desired)),
            Some(text) if text == desired => Self::new(path, "unchanged", before, Some(desired)),
            Some(text)
                if matches!(text, DISABLED | AUTOMATIC) || selector_version(text).is_some() =>
            {
                Self::new(path, "replace", before, Some(desired))
            }
            Some(_) => Self::new(path, "selector-conflict", before, None),
        }
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
            if self.action == "append-hook"
                && self
                    .before
                    .as_deref()
                    .is_some_and(|text| !text.is_empty() && !text.ends_with('\n'))
            {
                report["add_newline_before_hook"] = json!(true);
            }
        }
        if let Some(replaced) = &self.replaced_hook {
            report["replaced_hook"] = json!(replaced);
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
    let version = match shell {
        DotfileShell::Bash => "BASH_VERSION",
        DotfileShell::Zsh => "ZSH_VERSION",
        DotfileShell::Fish => return include_str!("dotfiles/startup.fish").to_owned(),
    };
    let name = shell.name();
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
    let body = match hook {
        Hook::Interactive => interactive(&format!("{environment}{prompt}")),
        Hook::Login if shell == DotfileShell::Bash => {
            format!("{environment}{}", interactive(&prompt))
        }
        Hook::Login => environment,
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
    let [begin, end] = hooks::hook_markers(&hook);
    let markers: Vec<_> = text
        .lines()
        .filter(|line| hooks::candidate_marker(line, [begin, end]))
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
    } else if markers == [begin, end]
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
    pub(super) async fn dotfile_versions(
        &self,
        shell: DotfileShell,
        cancel: Option<&TransferCancel>,
    ) -> Result<DotfileVersions> {
        let home = self.dotfile_home().await?;
        let mut result = DotfileVersions {
            home,
            active_bundle_sha256: None,
            state: DotfileVersionState::Inactive,
            versions: Vec::new(),
            omitted_versions: 0,
        };
        for relative in PARENTS {
            let path = under_home(&result.home, relative);
            if !self.dotfile_directory_exists(&path).await? {
                return Ok(result);
            }
        }
        let entries = self
            .read_directory(&under_home(&result.home, DIRECTORY), cancel, true)
            .await?;
        if let Some(selector) = self
            .dotfile_text(&selector_path(&result.home, shell))
            .await?
        {
            result.active_bundle_sha256 = selector_version(&selector).map(str::to_owned);
            result.state = match (selector.as_str(), result.active_bundle_sha256.is_some()) {
                (AUTOMATIC, _) => DotfileVersionState::AwaitingSync,
                (DISABLED, _) => DotfileVersionState::Disabled,
                (text, true) if selector_held(text) => DotfileVersionState::Paused,
                (_, true) => DotfileVersionState::Active,
                _ => DotfileVersionState::Conflict,
            };
        }
        let prefix = format!("{}-", shell.name());
        let mut seen = std::collections::HashSet::new();
        for entry in entries {
            if EntryKind::from_permissions(entry.attrs.permissions) == Some(EntryKind::Directory)
                && let Some(id) = entry
                    .filename
                    .strip_prefix(&prefix)
                    .filter(|id| valid_bundle_id(id))
                && seen.insert(id.to_owned())
            {
                result.versions.push(RetainedDotfileVersion {
                    bundle_sha256: id.to_owned(),
                    modified: entry.attrs.mtime,
                });
            }
        }
        let active = result.active_bundle_sha256.as_deref();
        result.versions.sort_unstable_by(|a, b| {
            (active == Some(b.bundle_sha256.as_str()))
                .cmp(&(active == Some(a.bundle_sha256.as_str())))
                .then_with(|| b.modified.cmp(&a.modified))
                .then_with(|| a.bundle_sha256.cmp(&b.bundle_sha256))
        });
        result.omitted_versions = result.versions.len().saturating_sub(MAX_LISTED_VERSIONS);
        result.versions.truncate(MAX_LISTED_VERSIONS);
        Ok(result)
    }

    pub(super) async fn preview_dotfiles(
        &self,
        bundle: DotfileBundle,
        shell: DotfileShell,
        startup_directory: Option<&str>,
    ) -> Result<Value> {
        let plan = self
            .dotfiles_plan(&bundle, shell, startup_directory)
            .await?;
        let (mut report, _) = self.review_dotfile_install(&plan, &bundle).await?;
        report["review"] = json!(self.dotfiles_review(&report)?);
        Ok(report)
    }

    pub(super) async fn apply_dotfiles(
        &self,
        bundle: DotfileBundle,
        shell: DotfileShell,
        startup_directory: Option<&str>,
        review: &str,
        stage: &mut JobStage,
    ) -> Result<Value> {
        let plan = self
            .dotfiles_plan(&bundle, shell, startup_directory)
            .await?;
        let (report, baseline) = self.review_dotfile_install(&plan, &bundle).await?;
        self.check_dotfiles_review(review, &report)?;
        let changed = self
            .install_dotfiles(&plan, &bundle, startup_directory, Some(baseline), stage)
            .await?;
        Ok(
            json!({ "shell": shell, "home": plan.home, "bundle_sha256": plan.id, "changed_files": changed }),
        )
    }

    async fn install_dotfiles<'a>(
        &self,
        plan: &'a Plan,
        bundle: &DotfileBundle,
        startup_directory: Option<&str>,
        baseline: Option<FragmentBaseline>,
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
            self.create_dotfile_directory(path).await?;
        }
        for edit in &plan.files {
            if edit.changed() {
                self.write_dotfile(edit, stage).await?;
                changed.push(edit.path.as_str());
                *stage = JobStage::Checking(INTERRUPTED);
            }
        }
        if plan.selector.changed() {
            let current = self
                .dotfiles_plan(bundle, plan.shell, startup_directory)
                .await?;
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
            if baseline == Some(FragmentBaseline::Verified)
                && let Some(active) = plan.selector.active_version()
            {
                self.verified_dotfile_fragments(&plan.home, plan.shell, active)
                    .await
                    .context("The active dotfile fragments changed during staging. Review them again before activating.")?;
            }
            self.write_dotfile(&plan.selector, stage).await?;
            changed.push(plan.selector.path.as_str());
        }
        Ok(changed)
    }

    pub(super) async fn switch_dotfiles(
        &self,
        target: DotfileTarget,
        shell: DotfileShell,
        startup_directory: Option<&str>,
        review: Option<&str>,
        stage: &mut JobStage,
    ) -> Result<Value> {
        let target = match target {
            DotfileTarget::Bundle { sha256 } => DotfileTarget::Bundle {
                sha256: normalize_sha256(&sha256)?,
            },
            target => target,
        };
        let home = self.dotfile_home().await?;
        for relative in PARENTS {
            let path = under_home(&home, relative);
            ensure!(
                self.dotfile_directory_exists(&path).await?,
                "Managed dotfiles directory is missing: {path}"
            );
        }
        let path = selector_path(&home, shell);
        let before = self.dotfile_text(&path).await?;
        let desired = match &target {
            DotfileTarget::Bundle { sha256 } => format!("{sha256}\n{MANUAL}"),
            DotfileTarget::Disabled => DISABLED.to_owned(),
            DotfileTarget::Automatic => {
                ensure!(
                    before.as_deref().is_some_and(selector_held),
                    "Automatic dotfile sync is not paused or disabled on this account. Refresh dotfile versions."
                );
                before
                    .as_deref()
                    .and_then(selector_version)
                    .map_or_else(|| AUTOMATIC.to_owned(), |id| format!("{id}\n"))
            }
        };
        let id = selector_version(&desired).map(str::to_owned);
        let (files, proposed) = match &id {
            Some(id) => {
                let (files, fragments) = self
                    .verified_dotfile_version(&home, shell, startup_directory, id)
                    .await?;
                (files, Some(fragments))
            }
            None => (Vec::new(), None),
        };
        let selector = Edit::selector(path, before, desired);
        let comparison = self
            .dotfile_comparison(&home, shell, selector.active_version(), proposed)
            .await?;
        let atomic_replace = self.extension(editor::POSIX_RENAME, "1").is_ok();
        let mut report = json!({
            "operation": "switch-dotfiles",
            "shell": shell,
            "home": home,
            "target": target,
            "bundle_sha256": id,
            "active_bundle_sha256": selector.active_version(),
            "selector": selector.report(),
            "files": files,
            "fragment_changes": comparison,
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
        Ok(json!({
            "shell": shell, "home": home, "target": target, "bundle_sha256": id, "changed": changed
        }))
    }

    async fn verified_dotfile_version(
        &self,
        home: &str,
        shell: DotfileShell,
        startup_directory: Option<&str>,
        id: &str,
    ) -> Result<(Vec<Value>, [String; 2])> {
        let fragments = self.verified_dotfile_fragments(home, shell, id).await?;
        let hooks = self.dotfile_hooks(home, shell, startup_directory).await?;
        for hook in &hooks {
            ensure!(
                hook.action == "unchanged",
                "Install or repair the managed startup hook at {} before using this bundle",
                hook.path
            );
        }
        let mut files = Vec::with_capacity(FRAGMENT_ROLES.len() + hooks.len());
        let directory = under_home(home, &version_directory(shell, id));
        for (role, contents) in FRAGMENT_ROLES.into_iter().zip(&fragments) {
            let path = fragment_path(&directory, shell, role);
            files.push(
                Edit::new(
                    path,
                    "unchanged",
                    Some(contents.clone()),
                    Some(contents.clone()),
                )
                .report(),
            );
        }
        files.extend(hooks.iter().map(Edit::report));
        Ok((files, fragments))
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

    async fn create_dotfile_directory(&self, path: &str) -> Result<()> {
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
        Ok(())
    }

    async fn missing_fish_startup_directories(&self, home: &str) -> Result<Vec<String>> {
        let mut directories = Vec::new();
        for relative in [CONFIG_DIRECTORY, FISH_DIRECTORY] {
            let path = under_home(home, relative);
            if !self.dotfile_directory_exists(&path).await? {
                directories.push(path);
            }
        }
        Ok(directories)
    }

    async fn dotfiles_plan(
        &self,
        bundle: &DotfileBundle,
        shell: DotfileShell,
        startup_directory: Option<&str>,
    ) -> Result<Plan> {
        bundle.validate(shell)?;
        let home = self.dotfile_home().await?;
        self.dotfiles_plan_at(home, bundle, shell, startup_directory)
            .await
    }

    async fn dotfiles_plan_at(
        &self,
        home: String,
        bundle: &DotfileBundle,
        shell: DotfileShell,
        startup_directory: Option<&str>,
    ) -> Result<Plan> {
        let [environment, prompt] = bundle.fragments(shell);
        let id = bundle_id(&environment, &prompt);
        let version_directory = version_directory(shell, &id);
        let mut directories_to_create = Vec::new();
        for relative in PARENTS
            .into_iter()
            .chain(
                (shell == DotfileShell::Fish && startup_directory.is_none())
                    .then_some(FISH_DIRECTORY),
            )
            .chain([version_directory.as_str()])
        {
            let path = under_home(&home, relative);
            if !self.dotfile_directory_exists(&path).await? {
                directories_to_create.push(path);
            }
        }
        let mut files = Vec::with_capacity(4);
        let directory = under_home(&home, &version_directory);
        for (role, desired) in [("environment", environment), ("prompt", prompt)] {
            let path = fragment_path(&directory, shell, role);
            let original = self.dotfile_text(&path).await?;
            let (action, after) = match original.as_deref() {
                None => ("create", Some(desired)),
                Some(text) if text == desired => ("unchanged", Some(desired)),
                Some(_) => ("payload-conflict", None),
            };
            files.push(Edit::new(path, action, original, after));
        }
        files.extend(self.dotfile_hooks(&home, shell, startup_directory).await?);
        let path = selector_path(&home, shell);
        let before = self.dotfile_text(&path).await?;
        let selector = Edit::selector(path, before, format!("{id}\n"));
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
        startup_directory: Option<&str>,
    ) -> Result<Vec<Edit>> {
        let files = self.startup_files(home, shell, startup_directory).await?;
        Ok(files
            .into_iter()
            .zip([Hook::Login, Hook::Interactive])
            .map(|((path, original), hook)| hook_change(path, original, hook_text(shell, hook)))
            .collect())
    }

    async fn startup_files(
        &self,
        home: &str,
        shell: DotfileShell,
        startup_directory: Option<&str>,
    ) -> Result<Vec<(String, Option<String>)>> {
        let home_prefix = home.trim_end_matches('/');
        let startup_dir = if let Some(relative) = startup_directory {
            let kind = if shell == DotfileShell::Fish {
                "Fish configuration"
            } else {
                "ZDOTDIR"
            };
            let mut path = home_prefix.to_owned();
            for part in relative.split('/') {
                path.push('/');
                path.push_str(part);
                ensure!(
                    self.dotfile_directory_exists(&path).await?,
                    "{kind} directory is missing: {path}"
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
                for name in BASH_LOGIN_FILES {
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
            DotfileShell::Fish => {
                if startup_directory.is_some() {
                    let path = startup("config.fish");
                    let original = self.dotfile_text(&path).await?;
                    return Ok(vec![(path, original)]);
                }
                let path = startup(".config/fish/config.fish");
                let original = if self
                    .dotfile_directory_exists(&startup(CONFIG_DIRECTORY))
                    .await?
                    && self
                        .dotfile_directory_exists(&startup(FISH_DIRECTORY))
                        .await?
                {
                    self.dotfile_text(&path).await?
                } else {
                    None
                };
                return Ok(vec![(path, original)]);
            }
        };
        let rc = startup(&format!(".{}rc", shell.name()));
        let rc_original = self.dotfile_text(&rc).await?;
        Ok(vec![(login, login_original), (rc, rc_original)])
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
        .filter(|line| valid_bundle_id(line))
}

fn valid_bundle_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
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

fn fragment_path(directory: &str, shell: DotfileShell, role: &str) -> String {
    format!("{directory}/{role}.{}", shell.fragment_extension())
}

pub(super) fn under_home(home: &str, relative: &str) -> String {
    format!("{}/{relative}", home.trim_end_matches('/'))
}

fn remote_parent(path: &str) -> Result<&str> {
    path.rsplit_once('/')
        .map(|(parent, _)| parent)
        .context("Expected an absolute remote dotfile path")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_excerpt_rejects_lines_inside_the_managed_hook_even_without_the_markers() {
        let prefix = "export EDITOR=vi\n";
        let body = "source managed-dotfiles\n";
        let suffix = "alias ll='ls -l'\n";
        let source = format!("{prefix}{BEGIN}\n{body}{END}\n{suffix}");
        let start = prefix.len() + BEGIN.len() + 1;
        assert!(
            DotfileBundle::append_startup_excerpt("", &source, start..start + body.len()).is_err()
        );
        assert_eq!(
            DotfileBundle::append_startup_excerpt(
                "",
                &source,
                source.len() - suffix.len()..source.len()
            )
            .unwrap(),
            suffix
        );
    }

    #[test]
    fn imports_utf8_shell_fragments_on_a_new_line_without_changing_the_source() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(".bashrc");
        let source = "\u{feff}alias café='ls -lh'\r\n# Keep trailing spaces  \r\n";
        std::fs::write(&path, source).unwrap();
        assert_eq!(
            DotfileBundle::append_fragment_from_file(&path, "export EDITOR=vi").unwrap(),
            "export EDITOR=vi\nalias café='ls -lh'\n# Keep trailing spaces  \n"
        );
        assert_eq!(
            DotfileBundle::append_fragment_from_file(&path, "export EDITOR=vi\n").unwrap(),
            "export EDITOR=vi\nalias café='ls -lh'\n# Keep trailing spaces  \n"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
    }

    #[test]
    fn rejects_invalid_or_oversized_imports_and_caps_the_combined_fragment() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(".profile");
        for invalid in [b"".as_slice(), b"a\0b", b"\xff\xfe", b"a\rb"] {
            std::fs::write(&path, invalid).unwrap();
            assert!(DotfileBundle::append_fragment_from_file(&path, "").is_err());
        }
        assert!(DotfileBundle::append_fragment_from_file(directory.path(), "").is_err());

        let source = "\r\n".repeat(MAX_FRAGMENT_BYTES);
        std::fs::write(&path, format!("\u{feff}{source}")).unwrap();
        let imported = DotfileBundle::append_fragment_from_file(&path, "").unwrap();
        assert_eq!(imported, "\n".repeat(MAX_FRAGMENT_BYTES));
        assert!(DotfileBundle::append_fragment_from_file(&path, "# local").is_err());
        std::fs::write(&path, format!("\u{feff}{source}\n")).unwrap();
        assert!(DotfileBundle::append_fragment_from_file(&path, "").is_err());
    }

    #[test]
    fn saves_new_shell_bundles_without_replacing_the_source() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("dotfiles.json");
        let mut bundle = DotfileBundle {
            version: 1,
            environment: None,
            bash: None,
            zsh: Some("PROMPT='ops> '\nexport EDITOR=vi\n".into()),
            fish_environment: None,
            fish: None,
        };
        bundle.save_new(&path).unwrap();
        let original = std::fs::read(&path).unwrap();
        let loaded = DotfileBundle::load(&path).unwrap();
        assert_eq!(loaded.zsh, bundle.zsh);
        loaded.validate(DotfileShell::Zsh).unwrap();
        assert!(loaded.validate(DotfileShell::Bash).is_err());

        bundle.zsh = Some("PROMPT='changed> '\n".into());
        assert!(bundle.save_new(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);

        let invalid = directory.path().join("invalid.json");
        bundle.zsh = Some("x".repeat(MAX_FRAGMENT_BYTES + 1));
        assert!(bundle.save_new(&invalid).is_err());
        assert!(!invalid.exists());
    }
}
