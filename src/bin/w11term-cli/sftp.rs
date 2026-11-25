use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use w11term::{
    control::Request,
    sftp::{Attributes, DEFAULT_TIMEOUT, Operation, normalize_sha256},
};

pub const HELP: &str = "w11term-cli [--pid PID] sftp list-transfers [--pane ID]\n\
w11term-cli [--pid PID] sftp cancel-transfer TRANSFER_ID\n\
w11term-cli [--pid PID] sftp wait-transfer TRANSFER_ID [--timeout SECONDS]\n\
w11term-cli [--pid PID] sftp --pane ID [--timeout SECONDS] COMMAND [ARGS]\n\
  info | limits\n\
  get REMOTE LOCAL [--overwrite] [--preserve-mtime] [--max-bytes N] [--sha256 HEX [--resume]]\n\
  put LOCAL REMOTE [--overwrite] [--atomic]\n\
  get-dir REMOTE NEW_LOCAL_DIRECTORY [--preserve-mtime] | put-dir LOCAL NEW_REMOTE_DIRECTORY\n\
  ls [PATH] | opendir PATH | readdir --handle ID | close --handle ID\n\
  find ROOT QUERY\n\
  sha256 PATH [--max-bytes N]\n\
  stat PATH | lstat PATH | realpath PATH | readlink PATH | statvfs PATH | expand-path PATH\n\
  mkdir PATH [--permissions OCTAL] | setstat PATH [ATTRS]\n\
  rename FROM TO | hardlink FROM TO | symlink TARGET LINK [--spec-order]\n\
  remove-file PATH | remove-dir PATH\n\
  open PATH [--flags read,write,create,exclusive,truncate,append] [ATTRS]\n\
  read --handle ID [--offset N] [--length N]\n\
  write --handle ID [--offset N] --base64 DATA\n\
  fstat --handle ID | fsetstat --handle ID [ATTRS] | fsync --handle ID\n\
ATTRS: --size N --uid N --gid N --permissions OCTAL --atime UNIX_SECONDS --mtime UNIX_SECONDS\n\
Underscore spellings (remove_file, remove_dir) are accepted. Use -- before paths starting with --.\n\
Transfers stream through the GUI's existing SSH connection. Folder transfers require a new\n\
destination. Single files can be replaced with --overwrite. Uploads can leave a partial remote\n\
file or folder after interruption. Handles are pane-local\n\
and expire after five idle minutes, timeout, or disconnect. Replies contain result.sftp.";

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
            "--spec-order" => {
                ensure!(!spec_order, "duplicate --spec-order");
                spec_order = true;
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
            opts.is_empty() && !overwrite && !atomic && !resume && !spec_order && !preserve_mtime,
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
            let query = paths.next().context("missing filename fragment")?;
            ensure!(
                w11term::sftp::valid_filename_query(&query),
                "Enter a filename fragment up to 256 bytes"
            );
            fields.insert("query".into(), json!(query));
        }
        "sha256" => {
            fields.insert("path".into(), json!(path()?));
            if let Some(limit) = opts.remove("--max-bytes") {
                fields.insert("max_bytes".into(), json!(limit.parse::<u64>()?));
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
                    operation: Operation::PutDir { local, remote },
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
                operation: Operation::Find { path, query },
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
