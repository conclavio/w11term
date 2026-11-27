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

#[derive(Serialize)]
struct Change {
    path: String,
    action: &'static str,
    before_sha256: Option<String>,
    after_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hook: Option<String>,
}

impl Change {
    fn new(path: String, action: &'static str, before: Option<&str>, after: Option<&str>) -> Self {
        let hash = |text: &str| digest_hex(Sha256::new_with_prefix(text));
        Self {
            path,
            action,
            before_sha256: before.map(hash),
            after_sha256: after.map(hash),
            hook: None,
        }
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
    let source = |role| {
        let flag = format!("_w11term_dotfiles_{name}_{role}");
        let path = format!("$HOME/{DIRECTORY}/{name}-{role}.sh");
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
    format!("{BEGIN}\nif [ -n \"${{{version}-}}\" ]; then\n{body}fi\n{END}\n")
}

fn hook_change(path: String, original: Option<&str>, hook: String) -> Change {
    let text = original.unwrap_or_default();
    let markers: Vec<_> = text
        .lines()
        .filter(|line| matches!(line.trim(), BEGIN | END))
        .take(3)
        .collect();
    let mut change = if markers.is_empty() {
        match original {
            None => Change::new(path, "create", None, Some(&hook)),
            Some(text) => {
                let mut result = text.to_owned();
                if !result.is_empty() && !result.ends_with('\n') {
                    result.push('\n');
                }
                result.push_str(&hook);
                Change::new(path, "append-hook", original, Some(&result))
            }
        }
    } else if markers == [BEGIN, END]
        && text
            .match_indices(&hook)
            .any(|(start, _)| start == 0 || text.as_bytes()[start - 1] == b'\n')
    {
        Change::new(path, "unchanged", original, original)
    } else {
        Change::new(path, "hook-conflict", original, None)
    };
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
        bundle.validate(shell)?;
        validate_zdotdir(shell, zdotdir)?;
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
        for relative in [".config", ".config/w11term", DIRECTORY] {
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
        for (role, contents) in [
            ("environment", bundle.environment.as_deref()),
            ("prompt", bundle.prompt(shell)),
        ] {
            let path = under_home(&format!("{DIRECTORY}/{}-{role}.sh", shell.name()));
            let desired = payload(contents);
            let original = self.dotfile_text(&path).await?;
            let action = match original.as_deref() {
                None => "create",
                Some(text) if text == desired => "unchanged",
                Some(_) => "replace",
            };
            files.push(Change::new(
                path,
                action,
                original.as_deref(),
                Some(&desired),
            ));
        }
        files.push(hook_change(
            login,
            login_original.as_deref(),
            hook_text(shell, Hook::Login),
        ));
        files.push(hook_change(
            rc,
            rc_original.as_deref(),
            hook_text(shell, Hook::Interactive),
        ));
        Ok(json!({
            "shell": shell,
            "home": home,
            "directories_to_create": directories_to_create,
            "files": files,
            "supports_atomic_replace": self.extension(editor::POSIX_RENAME, "1").is_ok(),
        }))
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
