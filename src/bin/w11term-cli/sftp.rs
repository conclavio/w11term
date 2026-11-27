use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use w11term::{
    control::Request,
    sftp::{
        Attributes, DEFAULT_TIMEOUT, DotfileBundle, DotfileShell, DotfileTarget, EntryKind,
        FileSizeRange, FilenameFilter, ModifiedDateRange, Operation, UtilityBundle,
        normalize_sha256, validate_zdotdir,
    },
};

pub const HELP: &str = "w11term-cli [--pid PID] sftp list-transfers [--pane ID]\n\
w11term-cli [--pid PID] sftp cancel-transfer TRANSFER_ID\n\
w11term-cli [--pid PID] sftp wait-transfer TRANSFER_ID [--timeout SECONDS]\n\
w11term-cli [--pid PID] sftp --pane ID [--timeout SECONDS] COMMAND [ARGS]\n\
  info | limits\n\
  get REMOTE LOCAL [--overwrite] [--preserve-mtime] [--max-bytes N] [--sha256 HEX [--resume]]\n\
  put LOCAL REMOTE [--overwrite] [--atomic] [--preserve-times]\n\
  get-dir REMOTE NEW_LOCAL_DIRECTORY [--preserve-mtime]\n\
  put-dir LOCAL NEW_REMOTE_DIRECTORY [--preserve-times]\n\
  ls [PATH] | opendir PATH | readdir --handle ID | close --handle ID\n\
  find ROOT [QUERY] [--regex] [--match-case] [--match-path] [--kind file|directory|symlink]\n\
       [--exclude-dir-regex PATTERN] [--min-size SIZE] [--max-size SIZE]\n\
       [--modified-from YYYY-MM-DD] [--modified-through YYYY-MM-DD]\n\
       [--contains TEXT]\n\
  sha256 PATH [--max-bytes N]\n\
  preview-tool-bundle BUNDLE.json\n\
  apply-tool-bundle BUNDLE.json --review TOKEN\n\
  preview-tool-bundle-add BUNDLE.json\n\
  add-to-tool-bundle BUNDLE.json --review TOKEN\n\
  preview-tool-bundle-remove NAME TOOL\n\
  remove-from-tool-bundle NAME TOOL --review TOKEN\n\
  preview-tool-bundle-rollback NAME SHA256\n\
  rollback-tool-bundle NAME SHA256 --review TOKEN\n\
  preview-tool-bundle-resume NAME SHA256\n\
  resume-tool-bundle-updates NAME SHA256 --review TOKEN\n\
  preview-dotfiles BUNDLE.json --shell bash|zsh [--zdotdir RELATIVE_DIR]\n\
  apply-dotfiles BUNDLE.json --shell bash|zsh --review TOKEN [--zdotdir RELATIVE_DIR]\n\
  list-dotfile-versions --shell bash|zsh\n\
  preview-dotfiles-switch SHA256|disabled|automatic --shell bash|zsh [--zdotdir RELATIVE_DIR]\n\
  switch-dotfiles SHA256|disabled|automatic --shell bash|zsh --review TOKEN [--zdotdir RELATIVE_DIR]\n\
  stat PATH | lstat PATH | realpath PATH | readlink PATH | statvfs PATH | expand-path PATH\n\
  mkdir PATH [--permissions OCTAL] | setstat PATH [ATTRS]\n\
  rename FROM TO | hardlink FROM TO | symlink TARGET LINK [--spec-order]\n\
  remove-file PATH | remove-dir PATH\n\
  open PATH [--flags read,write,create,exclusive,truncate,append] [ATTRS]\n\
  read --handle ID [--offset N] [--length N]\n\
  write --handle ID [--offset N] --base64 DATA\n\
  fstat --handle ID | fsetstat --handle ID [ATTRS] | fsync --handle ID\n\
ATTRS: --size N --uid N --gid N --permissions OCTAL --atime UNIX_SECONDS --mtime UNIX_SECONDS\n\
Find matches filenames by default. --match-path matches slash-separated paths relative to ROOT.\n\
--exclude-dir-regex skips matching relative folder paths and their contents.\n\
Size limits are inclusive and accept whole bytes or binary units (for example 16MiB).\n\
Modification date limits are inclusive UTC calendar dates.\n\
--contains searches UTF-8 file lines. --regex and --match-case apply to both queries.\n\
Underscore spellings (remove_file, remove_dir) are accepted. Use -- before paths starting with --.\n\
Transfers stream through the GUI's existing SSH connection. Folder transfers require a new\n\
destination. Single files can be replaced with --overwrite. Uploads can leave a partial remote\n\
file or folder after interruption. Handles are pane-local\n\
and expire after five idle minutes, timeout, or disconnect. Replies contain result.sftp.";

fn dotfile_shell(value: Option<&str>) -> Result<DotfileShell> {
    match value {
        Some("bash") => Ok(DotfileShell::Bash),
        Some("zsh") => Ok(DotfileShell::Zsh),
        _ => bail!("--shell requires bash or zsh"),
    }
}

pub fn parse(
    args: impl Iterator<Item = String>,
    mut pid: Option<u32>,
    mut pane: Option<u64>,
) -> Result<(Option<u32>, Request)> {
    let mut args = args.peekable();
    let mut command = None;
    let mut positional = Vec::new();
    let mut opts = BTreeMap::new();
    let mut overwrite = false;
    let mut atomic = false;
    let mut resume = false;
    let mut preserve_mtime = false;
    let mut preserve_times = false;
    let mut spec_order = false;
    while let Some(arg) = args.next() {
        let value = |args: &mut std::iter::Peekable<_>| {
            args.next()
                .with_context(|| format!("{arg} requires a value"))
        };
        match arg.as_str() {
            "--pid" => pid = Some(value(&mut args)?.parse()?),
            "--pane" => pane = Some(value(&mut args)?.parse()?),
            "--overwrite" => {
                ensure!(!overwrite, "duplicate --overwrite");
                overwrite = true;
            }
            "--atomic" => {
                ensure!(!atomic, "duplicate --atomic");
                atomic = true;
            }
            "--resume" => {
                ensure!(!resume, "duplicate --resume");
                resume = true;
            }
            "--preserve-mtime" => {
                ensure!(!preserve_mtime, "duplicate --preserve-mtime");
                preserve_mtime = true;
            }
            "--preserve-times" => {
                ensure!(!preserve_times, "duplicate --preserve-times");
                preserve_times = true;
            }
            "--spec-order" => {
                ensure!(!spec_order, "duplicate --spec-order");
                spec_order = true;
            }
            "--regex" | "--match-case" | "--match-path" => {
                ensure!(
                    opts.insert(arg.clone(), String::new()).is_none(),
                    "duplicate option {arg}"
                );
            }
            "--" => {
                positional.extend(args);
                break;
            }
            _ if arg.starts_with("--") => {
                let v = value(&mut args)?;
                ensure!(
                    opts.insert(arg.clone(), v).is_none(),
                    "duplicate option {arg}"
                );
            }
            _ if command.is_none() => command = Some(arg.replace('_', "-")),
            _ => positional.push(arg),
        }
    }
    let command = command.context(HELP)?;
    if matches!(
        command.as_str(),
        "list-transfers" | "cancel-transfer" | "wait-transfer"
    ) {
        let wait_timeout = if command == "wait-transfer" {
            let timeout = opts
                .remove("--timeout")
                .map(|value| value.parse::<u64>())
                .transpose()?
                .unwrap_or(30);
            ensure!(
                (1..=3600).contains(&timeout),
                "timeout must be 1..=3600 seconds"
            );
            Some(timeout)
        } else {
            None
        };
        ensure!(
            opts.is_empty()
                && !overwrite
                && !atomic
                && !resume
                && !spec_order
                && !preserve_mtime
                && !preserve_times,
            "Unexpected options for {command}"
        );
        let request = if command == "list-transfers" {
            ensure!(
                positional.is_empty(),
                "list-transfers takes only an optional --pane"
            );
            ensure!(pane != Some(0), "--pane must be positive");
            Request::ListSftpTransfers { pane }
        } else {
            ensure!(
                pane.is_none(),
                "The transfer ID already identifies its pane"
            );
            let [id] = positional.as_slice() else {
                bail!("{command} requires one transfer ID from list-transfers");
            };
            let transfer_id = id.parse().context("Invalid SFTP transfer ID")?;
            match wait_timeout {
                Some(timeout_seconds) => Request::WaitSftpTransfer {
                    transfer_id,
                    timeout_seconds,
                },
                None => Request::CancelSftpTransfer { transfer_id },
            }
        };
        return Ok((pid, request));
    }
    ensure!(!atomic || command == "put", "--atomic requires put");
    ensure!(
        !preserve_mtime || matches!(command.as_str(), "get" | "get-dir"),
        "--preserve-mtime requires get or get-dir"
    );
    ensure!(
        !preserve_times || matches!(command.as_str(), "put" | "put-dir"),
        "--preserve-times requires put or put-dir"
    );
    ensure!(
        !resume || (command == "get" && opts.contains_key("--sha256")),
        "--resume requires get with --sha256"
    );
    let timeout = opts
        .remove("--timeout")
        .map(|s| s.parse::<u64>())
        .transpose()?
        .unwrap_or(DEFAULT_TIMEOUT);
    ensure!(
        (1..=3600).contains(&timeout),
        "timeout must be 1..=3600 seconds"
    );
    let mut fields = Map::new();
    fields.insert("op".into(), json!(command));
    let mut paths = positional.into_iter();
    let mut path = || paths.next().context("missing path argument");
    match command.as_str() {
        "get" | "get-dir" | "put" | "put-dir" => {
            let first = path()?;
            let second = path()?;
            let (remote, local) = if matches!(command.as_str(), "get" | "get-dir") {
                (first, second)
            } else {
                (second, first)
            };
            fields.insert("remote".into(), json!(remote));
            fields.insert("local".into(), json!(std::path::absolute(local)?));
            if preserve_mtime {
                fields.insert("preserve_mtime".into(), json!(true));
            }
            if preserve_times {
                fields.insert("preserve_times".into(), json!(true));
            }
            if matches!(command.as_str(), "get" | "put") {
                fields.insert("overwrite".into(), json!(overwrite));
                overwrite = false;
            }
            if command == "put" && atomic {
                fields.insert("atomic".into(), json!(true));
            }
            if command == "get"
                && let Some(limit) = opts.remove("--max-bytes")
            {
                fields.insert("max_bytes".into(), json!(limit.parse::<u64>()?));
            }
            if command == "get"
                && let Some(sha256) = opts.remove("--sha256")
            {
                fields.insert("expected_sha256".into(), json!(normalize_sha256(&sha256)?));
                fields.insert("resume".into(), json!(resume));
            }
        }
        "rename" | "hardlink" => {
            fields.insert("from".into(), json!(path()?));
            fields.insert("to".into(), json!(path()?));
        }
        "symlink" => {
            fields.insert("target".into(), json!(path()?));
            fields.insert("path".into(), json!(path()?));
            fields.insert("spec_order".into(), json!(spec_order));
            spec_order = false;
        }
        "ls" => {
            fields.insert(
                "path".into(),
                json!(paths.next().unwrap_or_else(|| ".".into())),
            );
        }
        "find" => {
            fields.insert("path".into(), json!(path()?));
            let query = paths.next().unwrap_or_default();
            let kind = opts
                .remove("--kind")
                .map(|name| serde_json::from_value::<EntryKind>(json!(name)))
                .transpose()
                .context("--kind must be file, directory, or symlink")?;
            let regex = opts.remove("--regex").is_some();
            let match_case = opts.remove("--match-case").is_some();
            let match_path = opts.remove("--match-path").is_some();
            let file_size = FileSizeRange::parse(
                &opts.remove("--min-size").unwrap_or_default(),
                &opts.remove("--max-size").unwrap_or_default(),
            )?;
            file_size.validate(kind)?;
            let filter = FilenameFilter {
                regex,
                match_case,
                match_path,
                kind,
                exclude_dir_regex: opts.remove("--exclude-dir-regex").unwrap_or_default(),
                file_size,
                modified_dates: ModifiedDateRange {
                    from: opts.remove("--modified-from").unwrap_or_default(),
                    through: opts.remove("--modified-through").unwrap_or_default(),
                },
                content_query: opts.remove("--contains").unwrap_or_default(),
            };
            filter.modified_dates.validate()?;
            ensure!(
                filter.valid_query(&query),
                "Enter a search pattern up to 256 bytes or add a search filter"
            );
            fields.insert("query".into(), json!(query));
            fields.insert("regex".into(), json!(regex));
            fields.insert("match_case".into(), json!(match_case));
            if match_path {
                fields.insert("match_path".into(), json!(true));
            }
            if let Some(kind) = kind {
                fields.insert("kind".into(), json!(kind));
            }
            if !filter.exclude_dir_regex.is_empty() {
                fields.insert("exclude_dir_regex".into(), json!(filter.exclude_dir_regex));
            }
            if !file_size.is_unrestricted() {
                fields.insert("file_size".into(), json!(file_size));
            }
            if !filter.modified_dates.is_unrestricted() {
                fields.insert("modified_dates".into(), json!(filter.modified_dates));
            }
            if !filter.content_query.is_empty() {
                fields.insert("content_query".into(), json!(filter.content_query));
            }
        }
        "sha256" => {
            fields.insert("path".into(), json!(path()?));
            if let Some(limit) = opts.remove("--max-bytes") {
                fields.insert("max_bytes".into(), json!(limit.parse::<u64>()?));
            }
        }
        "preview-tool-bundle"
        | "apply-tool-bundle"
        | "preview-tool-bundle-add"
        | "add-to-tool-bundle" => {
            if matches!(command.as_str(), "apply-tool-bundle" | "add-to-tool-bundle") {
                let token = opts
                    .remove("--review")
                    .context("Preview the tool bundle and supply --review")?;
                fields.insert("review".into(), json!(normalize_sha256(&token)?));
            }
            if command == "preview-tool-bundle-add" {
                fields.insert("op".into(), json!("add-to-tool-bundle"));
            }
            let manifest = path()?;
            let bundle = UtilityBundle::load(std::path::Path::new(&manifest))
                .with_context(|| format!("Opening the tool bundle {manifest}"))?;
            fields.insert("bundle".into(), json!(bundle));
        }
        "preview-tool-bundle-remove" | "remove-from-tool-bundle" => {
            if command == "remove-from-tool-bundle" {
                let token = opts
                    .remove("--review")
                    .context("Preview the tool removal and supply --review")?;
                fields.insert("review".into(), json!(normalize_sha256(&token)?));
            }
            fields.insert("op".into(), json!("remove-from-tool-bundle"));
            fields.insert("name".into(), json!(path()?));
            fields.insert("tool".into(), json!(path()?));
        }
        "preview-tool-bundle-rollback"
        | "rollback-tool-bundle"
        | "preview-tool-bundle-resume"
        | "resume-tool-bundle-updates" => {
            if matches!(
                command.as_str(),
                "rollback-tool-bundle" | "resume-tool-bundle-updates"
            ) {
                let token = opts
                    .remove("--review")
                    .context("Preview the tool bundle change and supply --review")?;
                fields.insert("review".into(), json!(normalize_sha256(&token)?));
            }
            let op = match command.as_str() {
                "preview-tool-bundle-resume" | "resume-tool-bundle-updates" => {
                    "resume-tool-bundle-updates"
                }
                _ => "rollback-tool-bundle",
            };
            fields.insert("op".into(), json!(op));
            fields.insert("name".into(), json!(path()?));
            fields.insert("bundle_sha256".into(), json!(normalize_sha256(&path()?)?));
        }
        "list-dotfile-versions" => {
            fields.insert(
                "shell".into(),
                json!(dotfile_shell(opts.remove("--shell").as_deref())?),
            );
        }
        "preview-dotfiles" | "apply-dotfiles" | "preview-dotfiles-switch" | "switch-dotfiles" => {
            let shell = dotfile_shell(opts.remove("--shell").as_deref())?;
            let zdotdir = opts.remove("--zdotdir");
            validate_zdotdir(shell, zdotdir.as_deref())?;
            if matches!(command.as_str(), "apply-dotfiles" | "switch-dotfiles") {
                let review = opts
                    .remove("--review")
                    .context("--review is required. Preview the bundle first.")?;
                fields.insert("review".into(), json!(normalize_sha256(&review)?));
            }
            fields.insert("shell".into(), json!(shell));
            fields.insert("zdotdir".into(), json!(zdotdir));
            if matches!(
                command.as_str(),
                "preview-dotfiles-switch" | "switch-dotfiles"
            ) {
                let value = path()?;
                let target = match value.as_str() {
                    "disabled" => DotfileTarget::Disabled,
                    "automatic" => DotfileTarget::Automatic,
                    _ => DotfileTarget::Bundle {
                        sha256: normalize_sha256(&value)?,
                    },
                };
                fields.insert("op".into(), json!("switch-dotfiles"));
                fields.insert("target".into(), json!(target));
            } else {
                let manifest = path()?;
                let bundle = DotfileBundle::load(std::path::Path::new(&manifest))
                    .with_context(|| format!("Opening dotfiles bundle {manifest}"))?;
                bundle.validate(shell)?;
                fields.insert("bundle".into(), json!(bundle));
            }
        }
        "opendir" | "stat" | "lstat" | "setstat" | "mkdir" | "remove-file" | "remove-dir"
        | "realpath" | "readlink" | "statvfs" | "expand-path" | "open" => {
            fields.insert("path".into(), json!(path()?));
        }
        "readdir" | "close" | "read" | "write" | "fstat" | "fsetstat" | "fsync" => {
            fields.insert(
                "handle".into(),
                json!(
                    opts.remove("--handle")
                        .context("--handle is required")?
                        .parse::<u64>()?
                ),
            );
        }
        "info" | "limits" => {}
        _ => bail!("unknown SFTP command {command}\n{HELP}"),
    }
    ensure!(paths.next().is_none(), "too many path arguments");
    ensure!(!spec_order, "--spec-order is only supported for symlink");
    ensure!(!overwrite, "--overwrite is only supported for get/put");
    if command == "read" || command == "write" {
        let offset = opts
            .remove("--offset")
            .map(|v| v.parse::<u64>())
            .transpose()?
            .unwrap_or(0);
        fields.insert("offset".into(), json!(offset));
        if command == "read" {
            let len = opts
                .remove("--length")
                .map(|v| v.parse::<u32>())
                .transpose()?
                .unwrap_or(w11term::sftp::CHUNK as u32);
            ensure!(
                len > 0 && len as usize <= w11term::sftp::CHUNK,
                "length must be 1..=32768"
            );
            fields.insert("len".into(), json!(len));
        } else {
            fields.insert(
                "data_base64".into(),
                json!(opts.remove("--base64").context("--base64 is required")?),
            );
        }
    }
    if matches!(command.as_str(), "open" | "mkdir" | "setstat" | "fsetstat") {
        let mut attrs = Map::new();
        for name in ["size", "uid", "gid", "atime", "mtime", "permissions"] {
            if let Some(value) = opts.remove(&format!("--{name}")) {
                let n = if name == "permissions" {
                    u64::from_str_radix(value.strip_prefix("0o").unwrap_or(&value), 8)?
                } else {
                    value.parse()?
                };
                attrs.insert(name.into(), json!(n));
            }
        }
        let attrs: Attributes = serde_json::from_value(Value::Object(attrs))?;
        ensure!(
            attrs.uid.is_some() == attrs.gid.is_some(),
            "uid and gid must be supplied together"
        );
        ensure!(
            attrs.atime.is_some() == attrs.mtime.is_some(),
            "atime and mtime must be supplied together"
        );
        fields.insert("attrs".into(), serde_json::to_value(attrs)?);
    }
    if command == "open" {
        let flags: Vec<String> = opts
            .remove("--flags")
            .unwrap_or_else(|| "read".into())
            .split(',')
            .map(str::to_owned)
            .collect();
        w11term::sftp::open_flags(&flags)?;
        fields.insert("flags".into(), json!(flags));
    }
    ensure!(
        opts.is_empty(),
        "unexpected SFTP options: {}",
        opts.keys().cloned().collect::<Vec<_>>().join(", ")
    );
    let operation: Operation = serde_json::from_value(Value::Object(fields))?;
    Ok((
        pid,
        Request::Sftp {
            pane: pane.context("--pane is required")?,
            timeout_seconds: timeout,
            operation,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn p(s: &[&str]) -> Result<(Option<u32>, Request)> {
        parse(s.iter().map(|s| s.to_string()), None, None)
    }

    #[test]
    fn preview_tool_bundle_resolves_builds_beside_the_manifest() {
        let local = tempfile::tempdir().unwrap();
        let manifest = local.path().join("Fleet tools.json");
        std::fs::write(
            &manifest,
            json!({
                "version": 1, "name": "fleet",
                "tools": [{"name": "rg", "builds": [{
                    "architecture": "x86_64", "path": "bins/rg", "sha256": "a".repeat(64)
                }]}]
            })
            .to_string(),
        )
        .unwrap();
        let (_, request) = p(&[
            "--pane",
            "7",
            "preview-tool-bundle",
            manifest.to_str().unwrap(),
        ])
        .unwrap();
        let Request::Sftp {
            pane: 7,
            operation: Operation::PreviewToolBundle { bundle },
            ..
        } = request
        else {
            panic!("expected the tool bundle preview request");
        };
        assert_eq!(bundle.bin_directory, ".local/bin");
        assert_eq!(bundle.tools[0].builds[0].path, local.path().join("bins/rg"));

        let path = manifest.to_str().unwrap();
        let (_, adding) = p(&["--pane", "7", "preview-tool-bundle-add", path]).unwrap();
        assert!(matches!(
            adding,
            Request::Sftp {
                pane: 7,
                operation: Operation::AddToToolBundle { review: None, .. },
                ..
            }
        ));
        let token = "b".repeat(64);
        assert!(p(&["--pane", "7", "add-to-tool-bundle", path]).is_err());
        let (_, adding) = p(&[
            "--pane",
            "7",
            "add-to-tool-bundle",
            path,
            "--review",
            &token,
        ])
        .unwrap();
        assert!(matches!(
            adding,
            Request::Sftp {
                pane: 7,
                operation: Operation::AddToToolBundle { review: Some(review), .. },
                ..
            } if review == token
        ));
    }

    #[test]
    fn tool_install_and_rollback_require_an_explicit_review() {
        let version = "a".repeat(64);
        let token = "b".repeat(64);
        assert!(p(&["--pane", "9", "apply-tool-bundle", "fleet.json"]).is_err());
        assert!(p(&["--pane", "9", "rollback-tool-bundle", "fleet", &version]).is_err());
        let (_, preview) = p(&[
            "--pane",
            "9",
            "preview-tool-bundle-rollback",
            "fleet",
            &version,
        ])
        .unwrap();
        assert!(matches!(preview, Request::Sftp {
            pane: 9, operation: Operation::RollbackToolBundle { review: None, bundle_sha256, .. }, ..
        } if bundle_sha256 == version));
        let (_, request) = p(&[
            "--pane",
            "9",
            "rollback-tool-bundle",
            "fleet",
            &version,
            "--review",
            &token,
        ])
        .unwrap();
        assert!(matches!(request, Request::Sftp {
            pane: 9, operation: Operation::RollbackToolBundle { review: Some(review), bundle_sha256, .. }, ..
        } if review == token && bundle_sha256 == version));
    }

    #[test]
    fn removing_a_tool_requires_a_review_for_one_bundle_and_command() {
        let (_, preview) =
            p(&["--pane", "9", "preview-tool-bundle-remove", "fleet", "rg"]).unwrap();
        assert!(matches!(
            preview,
            Request::Sftp {
                pane: 9,
                operation: Operation::RemoveFromToolBundle { name, tool, review: None },
                ..
            } if name == "fleet" && tool == "rg"
        ));
        assert!(p(&["--pane", "9", "remove-from-tool-bundle", "fleet", "rg"]).is_err());
        let token = "b".repeat(64);
        let (_, apply) = p(&[
            "--pane",
            "9",
            "remove-from-tool-bundle",
            "fleet",
            "rg",
            "--review",
            &token,
        ])
        .unwrap();
        assert!(matches!(
            apply,
            Request::Sftp {
                pane: 9,
                operation: Operation::RemoveFromToolBundle { name, tool, review: Some(review) },
                ..
            } if name == "fleet" && tool == "rg" && review == token
        ));
    }

    #[test]
    fn resuming_tool_updates_requires_a_review_for_one_release() {
        let version = "a".repeat(64);
        let token = "b".repeat(64);
        assert!(
            p(&[
                "--pane",
                "9",
                "resume-tool-bundle-updates",
                "fleet",
                &version
            ])
            .is_err()
        );
        let (_, preview) = p(&[
            "--pane",
            "9",
            "preview-tool-bundle-resume",
            "fleet",
            &version,
        ])
        .unwrap();
        assert!(matches!(preview, Request::Sftp {
            pane: 9, operation: Operation::ResumeToolBundleUpdates { name, bundle_sha256, review: None }, ..
        } if name == "fleet" && bundle_sha256 == version));
        let (_, apply) = p(&[
            "--pane",
            "9",
            "resume-tool-bundle-updates",
            "fleet",
            &version,
            "--review",
            &token,
        ])
        .unwrap();
        assert!(matches!(apply, Request::Sftp {
            pane: 9, operation: Operation::ResumeToolBundleUpdates { bundle_sha256, review: Some(review), .. }, ..
        } if bundle_sha256 == version && review == token));
    }

    #[test]
    fn previews_a_versioned_windows_dotfiles_bundle_on_one_ssh_pane() {
        let local = tempfile::tempdir().unwrap();
        let manifest = local.path().join("Fleet bundle.json");
        std::fs::write(
            &manifest,
            "\u{feff}{\"version\":1,\"zsh\":\"PROMPT='fleet> '\"}",
        )
        .unwrap();
        let path = manifest.to_str().unwrap();
        let (_, request) = p(&[
            "--pane",
            "2",
            "preview-dotfiles",
            path,
            "--shell",
            "zsh",
            "--zdotdir",
            "etc/zsh",
        ])
        .unwrap();
        assert!(matches!(
            request,
            Request::Sftp {
                pane: 2,
                operation: Operation::PreviewDotfiles { bundle, shell: DotfileShell::Zsh, zdotdir },
                ..
            } if bundle.zsh.as_deref() == Some("PROMPT='fleet> '") && zdotdir.as_deref() == Some("etc/zsh")
        ));
        assert!(p(&["--pane", "2", "preview-dotfiles", path, "--shell", "bash"]).is_err());
        assert!(
            p(&[
                "--pane",
                "2",
                "preview-dotfiles",
                path,
                "--shell",
                "zsh",
                "--zdotdir",
                "/etc/zsh"
            ])
            .is_err()
        );
        let token = "a".repeat(64);
        let (_, apply) = p(&[
            "--pane",
            "2",
            "apply-dotfiles",
            path,
            "--shell",
            "zsh",
            "--review",
            &token,
            "--zdotdir",
            "etc/zsh",
        ])
        .unwrap();
        assert!(matches!(
            apply,
            Request::Sftp {
                pane: 2,
                operation: Operation::ApplyDotfiles { shell: DotfileShell::Zsh, review, .. },
                ..
            } if review == token
        ));
        assert!(p(&["--pane", "2", "apply-dotfiles", path, "--shell", "zsh"]).is_err());
    }

    #[test]
    fn lists_retained_dotfiles_by_shell() {
        let (_, list) = p(&["--pane", "2", "list-dotfile-versions", "--shell", "zsh"]).unwrap();
        assert!(matches!(
            list,
            Request::Sftp {
                pane: 2,
                operation: Operation::ListDotfileVersions {
                    shell: DotfileShell::Zsh
                },
                ..
            }
        ));
        assert!(p(&["--pane", "2", "list-dotfile-versions"]).is_err());
        assert!(p(&["--pane", "2", "list-dotfile-versions", "--shell", "fish"]).is_err());
    }

    #[test]
    fn switches_dotfiles_only_with_a_reviewed_target() {
        let id = "a".repeat(64);
        let token = "b".repeat(64);
        let (_, preview) = p(&[
            "--pane",
            "2",
            "preview-dotfiles-switch",
            &id,
            "--shell",
            "bash",
        ])
        .unwrap();
        assert!(matches!(
            preview,
            Request::Sftp {
                pane: 2,
                operation: Operation::SwitchDotfiles {
                    target: DotfileTarget::Bundle { sha256 }, review: None, ..
                },
                ..
            } if sha256 == id
        ));
        let (_, disable) = p(&[
            "--pane",
            "2",
            "switch-dotfiles",
            "disabled",
            "--shell",
            "zsh",
            "--review",
            &token,
        ])
        .unwrap();
        assert!(matches!(
            disable,
            Request::Sftp {
                pane: 2,
                operation: Operation::SwitchDotfiles {
                    target: DotfileTarget::Disabled, shell: DotfileShell::Zsh, review: Some(review), ..
                },
                ..
            } if review == token
        ));
        let (_, resume) = p(&[
            "--pane",
            "2",
            "preview-dotfiles-switch",
            "automatic",
            "--shell",
            "bash",
        ])
        .unwrap();
        assert!(matches!(
            resume,
            Request::Sftp {
                pane: 2,
                operation: Operation::SwitchDotfiles {
                    target: DotfileTarget::Automatic,
                    shell: DotfileShell::Bash,
                    review: None,
                    ..
                },
                ..
            }
        ));
        assert!(
            p(&[
                "--pane",
                "2",
                "switch-dotfiles",
                "disabled",
                "--shell",
                "bash"
            ])
            .is_err()
        );
        assert!(
            p(&[
                "--pane",
                "2",
                "preview-dotfiles-switch",
                "../bundle",
                "--shell",
                "bash"
            ])
            .is_err()
        );
        assert!(
            p(&[
                "--pane",
                "2",
                "preview-dotfiles-switch",
                &id,
                "--shell",
                "bash",
                "--review",
                &token
            ])
            .is_err()
        );
    }

    #[test]
    fn transfer_commands_accept_only_a_pane_filter_or_an_exact_transfer_id() {
        let (pid, list) = p(&["list-transfers", "--pid", "42", "--pane", "9"]).unwrap();
        assert_eq!(pid, Some(42));
        assert!(matches!(list, Request::ListSftpTransfers { pane: Some(9) }));
        assert!(matches!(
            p(&["list-transfers"]).unwrap().1,
            Request::ListSftpTransfers { pane: None }
        ));
        let (_, cancel) = p(&["cancel-transfer", "9:2:11"]).unwrap();
        assert_eq!(
            serde_json::to_value(cancel).unwrap(),
            json!({"verb":"cancel-sftp-transfer", "transfer_id":"9:2:11"})
        );
        let (_, wait) = p(&["wait-transfer", "9:2:11", "--timeout", "42"]).unwrap();
        assert_eq!(
            serde_json::to_value(wait).unwrap(),
            json!({"verb":"wait-sftp-transfer", "transfer_id":"9:2:11", "timeout_seconds":42})
        );
        assert!(matches!(
            p(&["wait-transfer", "9:2:11"]).unwrap().1,
            Request::WaitSftpTransfer {
                timeout_seconds: 30,
                ..
            }
        ));
        for invalid in [
            &["list-transfers", "extra"][..],
            &["list-transfers", "--pane", "0"],
            &["list-transfers", "--timeout", "5"],
            &["cancel-transfer", "9:2"],
            &["cancel-transfer", "9:2:0"],
            &["cancel-transfer", "9:2:11:3"],
            &["cancel-transfer", "9:2:+1"],
            &["cancel-transfer", "--pane", "9", "9:2:11"],
            &["wait-transfer", "9:2:11", "--timeout", "0"],
            &["wait-transfer", "9:2:11", "--timeout", "3601"],
            &["wait-transfer", "9:2:11", "extra"],
        ] {
            assert!(p(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn folder_upload_resolves_the_local_source_and_requires_a_new_remote_name() {
        for command in ["put-dir", "put_dir"] {
            let (
                Some(42),
                Request::Sftp {
                    pane: 7,
                    timeout_seconds: 90,
                    operation:
                        Operation::PutDir {
                            local,
                            remote,
                            preserve_times: false,
                        },
                },
            ) = p(&[
                "--pid",
                "42",
                "--pane",
                "7",
                "--timeout",
                "90",
                command,
                "--",
                "--pack 雪",
                "new packs/v1",
            ])
            .unwrap()
            else {
                panic!("expected a directory upload")
            };
            assert_eq!(local, std::path::absolute("--pack 雪").unwrap());
            assert_eq!(remote, "new packs/v1");
        }
        for args in [
            &["put-dir", "--pane", "7", "pack"][..],
            &["put-dir", "--pane", "7", "pack", "remote", "extra"],
            &["put-dir", "--pane", "7", "pack", "remote", "--overwrite"],
            &[
                "put-dir",
                "--pane",
                "7",
                "pack",
                "remote",
                "--max-bytes",
                "8",
            ],
        ] {
            assert!(p(args).is_err(), "{args:?}");
        }
    }
    #[test]
    fn find_preserves_the_remote_root_and_literal_fragment() {
        let (
            _,
            Request::Sftp {
                operation:
                    Operation::Find {
                        path,
                        query,
                        regex: false,
                        match_case: false,
                        match_path: false,
                        kind: None,
                        ..
                    },
                ..
            },
        ) = p(&["find", "--pane", "2", "/logs old", "[Ångström]"]).unwrap()
        else {
            panic!("expected filename search")
        };
        assert_eq!(path, "/logs old");
        assert_eq!(query, "[Ångström]");
        assert!(p(&["find", "--pane", "2", "/logs", " "]).is_err());
        assert!(p(&["find", "--pane", "2", "/logs"]).is_err());
    }

    #[test]
    fn filename_search_takes_boolean_regex_and_match_case_flags() {
        let (_, request) = p(&[
            "find",
            "--pane",
            "2",
            "--regex",
            "/logs",
            "^APP$",
            "--match-case",
        ])
        .unwrap();
        assert!(matches!(
            request,
            Request::Sftp {
                operation: Operation::Find {
                    regex: true,
                    match_case: true,
                    ..
                },
                ..
            }
        ));
        assert!(p(&["find", "--pane", "2", "--regex", "--regex", "/logs", "app"]).is_err());
        assert!(p(&["ls", "--pane", "2", "--match-case", "/logs"]).is_err());
        assert!(p(&["wait-transfer", "--regex", "9"]).is_err());
        let (_, request) = p(&["find", "--pane", "2", "--", "/logs", "--regex"]).unwrap();
        assert!(matches!(request, Request::Sftp {
            operation: Operation::Find { query, regex: false, .. }, ..
        } if query == "--regex"));
    }

    #[test]
    fn find_match_path_is_a_boolean_flag_for_search_only() {
        let (_, request) = p(&[
            "find",
            "--pane",
            "2",
            "/logs",
            "^archive/.*",
            "--match-path",
            "--regex",
        ])
        .unwrap();
        assert!(matches!(request, Request::Sftp {
            operation: Operation::Find { query, match_path: true, regex: true, .. }, ..
        } if query == "^archive/.*"));
        assert!(p(&["ls", "--pane", "2", "/logs", "--match-path"]).is_err());
        assert!(
            p(&[
                "find",
                "--pane",
                "2",
                "/logs",
                "x",
                "--match-path",
                "--match-path"
            ])
            .is_err()
        );
        let (_, request) = p(&["find", "--pane", "2", "--", "/logs", "--match-path"]).unwrap();
        assert!(matches!(request, Request::Sftp {
            operation: Operation::Find { query, match_path: false, .. }, ..
        } if query == "--match-path"));
    }

    #[test]
    fn find_excludes_folders_with_or_without_a_positive_query() {
        let pattern = r"(^|/)(\.git|node_modules)$";
        for query in [None, Some("report")] {
            let mut args = vec![
                "find",
                "--pane",
                "2",
                "/repo",
                "--exclude-dir-regex",
                pattern,
            ];
            args.extend(query);
            let (_, request) = p(&args).unwrap();
            assert!(matches!(request, Request::Sftp {
                operation: Operation::Find { query: actual, exclude_dir_regex, .. }, ..
            } if actual == query.unwrap_or_default() && exclude_dir_regex == pattern));
        }
        assert!(p(&["find", "--pane", "2", "/repo", "--exclude-dir-regex"]).is_err());
        assert!(p(&["ls", "--pane", "2", "/repo", "--exclude-dir-regex", pattern]).is_err());
    }

    #[test]
    fn find_accepts_inclusive_file_size_limits_and_rejects_invalid_ranges() {
        let (_, request) = p(&[
            "find",
            "--pane",
            "2",
            "/repo",
            "--min-size",
            "1 KiB",
            "--max-size",
            "16MiB",
        ])
        .unwrap();
        assert!(matches!(request, Request::Sftp {
            operation: Operation::Find {
                query,
                file_size: FileSizeRange { minimum: Some(1024), maximum: Some(16_777_216) },
                ..
            }, ..
        } if query.is_empty()));
        let (_, request) = p(&["find", "--pane", "2", "/repo", "--max-size", "0"]).unwrap();
        assert!(matches!(
            request,
            Request::Sftp {
                operation: Operation::Find {
                    file_size: FileSizeRange {
                        minimum: None,
                        maximum: Some(0)
                    },
                    ..
                },
                ..
            }
        ));
        for args in [
            &[
                "find",
                "--pane",
                "2",
                "/repo",
                "--min-size",
                "2 GiB",
                "--max-size",
                "1 GiB",
            ][..],
            &["find", "--pane", "2", "/repo", "--min-size", "999 EiB"],
            &["find", "--pane", "2", "/repo", "--max-size", "-1"],
            &[
                "find",
                "--pane",
                "2",
                "/repo",
                "--max-size",
                "4 KiB",
                "--kind",
                "directory",
            ],
            &["ls", "--pane", "2", "/repo", "--min-size", "4 KiB"],
        ] {
            assert!(p(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn find_accepts_utc_date_bounds_and_rejects_invalid_calendar_dates() {
        let (_, request) = p(&[
            "find",
            "--pane",
            "2",
            "/repo",
            "--modified-from",
            "2024-02-29",
            "--modified-through",
            "2024-03-01",
        ])
        .unwrap();
        assert!(matches!(request, Request::Sftp {
            operation: Operation::Find { query, modified_dates, .. }, ..
        } if query.is_empty() && modified_dates.from == "2024-02-29" && modified_dates.through == "2024-03-01"));
        let (_, request) = p(&[
            "find",
            "--pane",
            "2",
            "/repo",
            "--modified-through",
            "1969-12-31",
        ])
        .unwrap();
        assert!(matches!(request, Request::Sftp {
            operation: Operation::Find { modified_dates, .. }, ..
        } if modified_dates.from.is_empty() && modified_dates.through == "1969-12-31"));
        for args in [
            &[
                "find",
                "--pane",
                "2",
                "/repo",
                "--modified-through",
                "2025-02-29",
            ][..],
            &[
                "find",
                "--pane",
                "2",
                "/repo",
                "--modified-from",
                "2025-2-01",
            ],
            &[
                "find",
                "--pane",
                "2",
                "/repo",
                "--modified-from",
                "2025-03-01",
                "--modified-through",
                "2025-02-01",
            ],
            &[
                "ls",
                "--pane",
                "2",
                "/repo",
                "--modified-from",
                "2025-02-01",
            ],
        ] {
            assert!(p(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn find_accepts_a_text_query_alone_or_with_a_filename_pattern() {
        let (_, request) =
            p(&["find", "--pane", "2", "/repo", "--contains", "request [id]"]).unwrap();
        assert!(matches!(request, Request::Sftp {
            operation: Operation::Find { query, content_query, regex: false, .. }, ..
        } if query.is_empty() && content_query == "request [id]"));
        let (_, request) = p(&[
            "find",
            "--pane",
            "2",
            "/repo",
            "\\.log$",
            "--contains",
            "^Error",
            "--regex",
            "--match-case",
        ])
        .unwrap();
        assert!(matches!(request, Request::Sftp {
            operation: Operation::Find { query, content_query, regex: true, match_case: true, .. }, ..
        } if query == "\\.log$" && content_query == "^Error"));
        assert!(p(&["find", "--pane", "2", "/repo", "--contains"]).is_err());
        assert!(p(&["ls", "--pane", "2", "/repo", "--contains", "Error"]).is_err());
    }

    #[test]
    fn filename_search_accepts_an_exact_result_type_with_or_without_a_pattern() {
        let (_, request) = p(&["find", "--pane", "2", "/logs", "--kind", "symlink"]).unwrap();
        assert!(matches!(request, Request::Sftp {
            operation: Operation::Find { query, kind: Some(EntryKind::Symlink), .. }, ..
        } if query.is_empty()));
        let (_, request) = p(&[
            "find", "--pane", "2", "/logs", "^ERROR", "--regex", "--kind", "file",
        ])
        .unwrap();
        assert!(matches!(
            request,
            Request::Sftp {
                operation: Operation::Find {
                    regex: true,
                    kind: Some(EntryKind::File),
                    ..
                },
                ..
            }
        ));
        for args in [
            &["find", "--pane", "2", "/logs", "--kind", "device"][..],
            &["find", "--pane", "2", "/logs", "--regex"],
            &["ls", "--pane", "2", "/logs", "--kind", "directory"],
        ] {
            assert!(p(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn expected_sha256_is_only_accepted_for_single_file_downloads() {
        let digest = "AB".repeat(32);
        let (
            _,
            Request::Sftp {
                operation:
                    Operation::Get {
                        expected_sha256,
                        resume,
                        ..
                    },
                ..
            },
        ) = p(&[
            "get", "--pane", "2", "/remote", "local", "--sha256", &digest, "--resume",
        ])
        .unwrap()
        else {
            panic!("expected a single file download")
        };
        assert_eq!(expected_sha256, Some(digest.to_ascii_lowercase()));
        assert!(resume);
        assert!(p(&["get", "--pane", "2", "/remote", "local", "--resume"]).is_err());
        assert!(p(&["put", "--pane", "2", "local", "/remote", "--resume"]).is_err());
        assert!(p(&["get", "--pane", "2", "/remote", "local", "--sha256", "abcd"]).is_err());
        assert!(
            p(&[
                "get-dir", "--pane", "2", "/remote", "local", "--sha256", &digest
            ])
            .is_err()
        );
    }

    #[test]
    fn preserve_mtime_is_available_only_for_downloads() {
        let operation = |command| {
            p(&[
                command,
                "--pane",
                "2",
                "/remote",
                "local",
                "--preserve-mtime",
            ])
            .unwrap()
            .1
        };
        assert!(matches!(
            operation("get"),
            Request::Sftp {
                operation: Operation::Get {
                    preserve_mtime: true,
                    ..
                },
                ..
            }
        ));
        assert!(matches!(
            operation("get-dir"),
            Request::Sftp {
                operation: Operation::GetDir {
                    preserve_mtime: true,
                    ..
                },
                ..
            }
        ));
        assert!(p(&["put", "--pane", "2", "local", "/remote", "--preserve-mtime"]).is_err());
    }

    #[test]
    fn preserve_times_is_available_only_for_uploads() {
        let operation = |command| {
            p(&[
                command,
                "--pane",
                "2",
                "local",
                "/remote",
                "--preserve-times",
            ])
            .unwrap()
            .1
        };
        assert!(matches!(
            operation("put"),
            Request::Sftp {
                operation: Operation::Put {
                    preserve_times: true,
                    ..
                },
                ..
            }
        ));
        assert!(matches!(
            operation("put-dir"),
            Request::Sftp {
                operation: Operation::PutDir {
                    preserve_times: true,
                    ..
                },
                ..
            }
        ));
        assert!(p(&["get", "--pane", "2", "/remote", "local", "--preserve-times"]).is_err());
    }

    #[test]
    fn transfers_handles_attributes_and_validation() {
        let (
            _,
            Request::Sftp {
                operation:
                    Operation::Put {
                        local,
                        remote,
                        overwrite,
                        atomic,
                        ..
                    },
                ..
            },
        ) = p(&[
            "put",
            "--pane",
            "2",
            "a b",
            "/tmp/a b",
            "--overwrite",
            "--atomic",
        ])
        .unwrap()
        else {
            panic!()
        };
        assert!(local.is_absolute());
        assert_eq!(remote, "/tmp/a b");
        assert!(overwrite);
        assert!(atomic);
        assert!(p(&["put-dir", "--pane", "2", "local", "/remote", "--atomic"]).is_err());
        let (
            _,
            Request::Sftp {
                pane,
                operation:
                    Operation::GetDir {
                        local,
                        remote,
                        preserve_mtime: false,
                    },
                ..
            },
        ) = p(&["get_dir", "--pane", "7", "/var/log", "saved logs"]).unwrap()
        else {
            panic!("expected a directory download");
        };
        assert_eq!(pane, 7);
        assert_eq!(remote, "/var/log");
        assert_eq!(local, std::path::absolute("saved logs").unwrap());
        assert!(p(&["get-dir", "--pane", "7", "/var/log", ".", "--overwrite"]).is_err());
        assert!(matches!(
            p(&["remove_file", "--pane", "2", "/tmp/a"]).unwrap().1,
            Request::Sftp {
                operation: Operation::RemoveFile { .. },
                ..
            }
        ));
        assert!(p(&["stat", "--pane", "2", "/tmp", "--overwrite"]).is_err());
        assert!(p(&["read", "--pane", "2", "--handle", "7", "--length", "32769"]).is_err());
        assert!(p(&["setstat", "--pane", "2", "/tmp", "--uid", "1"]).is_err());
        assert!(p(&["open", "--pane", "2", "/tmp", "--flags", "write,exclusive"]).is_err());
        let (
            _,
            Request::Sftp {
                operation: Operation::Mkdir { attrs, .. },
                ..
            },
        ) = p(&["mkdir", "--pane", "2", "/tmp/a", "--permissions", "0750"]).unwrap()
        else {
            panic!()
        };
        assert_eq!(attrs.permissions, Some(0o750));
    }
}
