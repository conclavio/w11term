//! Console companion: stdout always contains exactly one JSON reply (including errors).
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{io::Read, path::PathBuf};
use w11term::control::{self, Reply, Request};
#[path = "w11term-cli/automation.rs"]
mod automation;
#[path = "w11term-cli/sftp.rs"]
mod sftp_args;

const HELP: &str = "w11term-cli [--pid PID] list\nw11term-cli [--pid PID] send-text --pane ID --no-paste [TEXT | --base64 BASE64 | --file PATH | --stdin]\nw11term-cli [--pid PID] get-text --pane ID\nw11term-cli [--pid PID] screenshot --pane ID PATH\nw11term-cli instances\n\nTEXT is literal UTF-8 (no escape interpretation). Use --base64/--file/--stdin for arbitrary bytes.\nWithout --pid, exactly one live instance is required. No command starts a GUI or changes focus.";

fn parse(args: impl IntoIterator<Item = String>) -> Result<(Option<u32>, Request)> {
    let mut args = args.into_iter();
    let mut pid = None;
    let mut verb = None;
    let mut pane = None;
    let mut no_paste = false;
    let mut payload = None;
    let mut path = None;
    while let Some(arg) = args.next() {
        let value = |args: &mut dyn Iterator<Item = String>| -> Result<String> {
            args.next()
                .with_context(|| format!("{arg} requires a value"))
        };
        match arg.as_str() {
            "wait-for-text" | "events" | "open" | "resize" | "record" if verb.is_none() => {
                ensure!(
                    payload.is_none() && path.is_none() && !no_paste,
                    "invalid options before automation command"
                );
                return automation::parse(&arg, args, pid, pane);
            }
            "sftp" if verb.is_none() => {
                ensure!(
                    payload.is_none() && path.is_none() && !no_paste,
                    "sftp accepts only --pid and --pane before the command"
                );
                return sftp_args::parse(args, pid, pane);
            }
            "--pid" => pid = Some(value(&mut args)?.parse()?),
            "--pane" => pane = Some(value(&mut args)?.parse()?),
            "--no-paste" => no_paste = true,
            "--base64" | "--file" | "--stdin" => {
                ensure!(payload.is_none(), "choose one input source");
                let bytes = match arg.as_str() {
                    "--base64" => STANDARD
                        .decode(value(&mut args)?)
                        .context("invalid base64")?,
                    "--file" => read_limited(std::fs::File::open(value(&mut args)?)?)?,
                    _ => read_limited(std::io::stdin().lock())?,
                };
                payload = Some(bytes);
            }
            "--" => {
                ensure!(payload.is_none(), "choose one input source");
                payload = Some(
                    args.next()
                        .context("-- requires literal text")?
                        .into_bytes(),
                );
                ensure!(
                    args.next().is_none(),
                    "only one literal text argument is accepted"
                );
            }
            _ if arg.starts_with("--") => {
                bail!("unknown option {arg}; use -- before literal text beginning with --")
            }
            _ if verb.is_none() => verb = Some(arg),
            _ if verb.as_deref() == Some("send-text") => {
                ensure!(payload.is_none(), "choose one input source");
                payload = Some(arg.into_bytes());
            }
            _ if verb.as_deref() == Some("screenshot") => {
                ensure!(path.is_none(), "only one screenshot path is accepted");
                path = Some(PathBuf::from(arg));
            }
            _ => bail!("unknown argument {arg}"),
        }
    }
    let verb = verb.context(HELP)?;
    let request = match verb.as_str() {
        "list" => {
            ensure!(
                pane.is_none() && payload.is_none() && path.is_none() && !no_paste,
                "list takes no pane/input options"
            );
            Request::List
        }
        "get-text" => {
            ensure!(
                payload.is_none() && path.is_none() && !no_paste,
                "get-text takes only --pane"
            );
            Request::GetText {
                pane: pane.context("--pane is required")?,
            }
        }
        "send-text" => {
            ensure!(no_paste, "--no-paste is required (raw SSH input)");
            let bytes = payload.context("provide TEXT, --base64, --file, or --stdin")?;
            ensure!(bytes.len() <= control::MAX_BYTES, "input exceeds 1 MiB");
            Request::SendText {
                pane: pane.context("--pane is required")?,
                data_base64: STANDARD.encode(bytes),
                no_paste,
            }
        }
        "screenshot" => {
            ensure!(
                payload.is_none() && !no_paste,
                "screenshot takes only --pane and PATH"
            );
            Request::Screenshot {
                pane: pane.context("--pane is required")?,
                path: std::path::absolute(path.context("screenshot path is required")?)?,
            }
        }
        _ => bail!("unknown command {verb}\n{HELP}"),
    };
    Ok((pid, request))
}
fn read_limited(reader: impl Read) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((control::MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= control::MAX_BYTES, "input exceeds 1 MiB");
    Ok(bytes)
}
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--help"] || args == ["-h"] {
        println!("{HELP}\n\n{}\n\n{}", sftp_args::HELP, automation::HELP);
        return;
    }
    let result = if args == ["instances"] {
        control::discover().await.map(|instances| {
            Reply::from_result(serde_json::to_value(instances).map_err(Into::into))
        })
    } else {
        match parse(args) {
            Ok((pid, request)) => control::call(pid, &request).await,
            Err(e) => Err(e),
        }
    };
    let reply = result.unwrap_or_else(|e| Reply::from_result(Err(e)));
    println!("{}", serde_json::to_string(&reply).unwrap());
    if !reply.ok {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_bytes_and_required_options() {
        let p = |a: &[&str]| parse(a.iter().map(|s| s.to_string()));
        let (_, Request::SendText { data_base64, .. }) = p(&[
            "send-text",
            "--pane",
            "3",
            "--no-paste",
            "--base64",
            "AAcbW0P/",
        ])
        .unwrap() else {
            panic!()
        };
        assert_eq!(STANDARD.decode(data_base64).unwrap(), b"\0\x07\x1b[C\xff");
        assert!(p(&["send-text", "--pane", "3", "hello"]).is_err());
        assert!(p(&["get-text"]).is_err());
        assert!(p(&["list", "--file", "missing"]).is_err());
    }
}
