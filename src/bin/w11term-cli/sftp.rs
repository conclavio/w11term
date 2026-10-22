use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use w11term::{
    control::Request,
    sftp::{Attributes, DEFAULT_TIMEOUT, Operation},
};

pub const HELP: &str = "w11term-cli [--pid PID] sftp --pane ID [--timeout SECONDS] COMMAND [ARGS]\n\
  info | limits\n\
  get REMOTE LOCAL [--overwrite] | put LOCAL REMOTE [--overwrite]\n\
  ls [PATH] | opendir PATH | readdir --handle ID | close --handle ID\n\
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
Transfers stream through the GUI's existing SSH connection. Destinations are exclusive unless\n\
--overwrite is supplied. Upload failures can leave partial remote files. Handles are pane-local\n\
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
        "get" | "put" => {
            let first = path()?;
            let second = path()?;
            let (remote, local) = if command == "get" {
                (first, second)
            } else {
                (second, first)
            };
            fields.insert("remote".into(), json!(remote));
            fields.insert("local".into(), json!(std::path::absolute(local)?));
            fields.insert("overwrite".into(), json!(overwrite));
            overwrite = false;
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
    fn transfers_handles_attributes_and_validation() {
        let (
            _,
            Request::Sftp {
                operation:
                    Operation::Put {
                        local,
                        remote,
                        overwrite,
                    },
                ..
            },
        ) = p(&["put", "--pane", "2", "a b", "/tmp/a b", "--overwrite"]).unwrap()
        else {
            panic!()
        };
        assert!(local.is_absolute());
        assert_eq!(remote, "/tmp/a b");
        assert!(overwrite);
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
