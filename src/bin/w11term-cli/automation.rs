use anyhow::{Context, Result, bail, ensure};
use w11term::control::Request;
pub const HELP: &str = "w11term-cli [--pid PID] wait-for-text --pane ID [--regex] [--timeout SECONDS] TEXT\nw11term-cli [--pid PID] events --pane ID [--since CURSOR] [--timeout SECONDS]\nw11term-cli [--pid PID] resize --pane ID --cols N --rows N\nw11term-cli [--pid PID] open --profile NAME [--cols N --rows N] [--activate]\nw11term-cli [--pid PID] record --pane ID [--stop | PATH]";
pub fn parse(
    verb: &str,
    args: impl Iterator<Item = String>,
    mut pid: Option<u32>,
    mut pane: Option<u64>,
) -> Result<(Option<u32>, Request)> {
    let mut args = args.peekable();
    let mut cols = None;
    let mut rows = None;
    let mut profile = None;
    let mut activate = false;
    let mut regex = false;
    let mut timeout = None;
    let mut since = None;
    let mut text = None;
    let mut stop = false;
    while let Some(arg) = args.next() {
        let value = |args: &mut dyn Iterator<Item = String>| {
            args.next()
                .with_context(|| format!("{arg} requires a value"))
        };
        match arg.as_str() {
            "--pid" => pid = Some(value(&mut args)?.parse()?),
            "--pane" => pane = Some(value(&mut args)?.parse()?),
            "--cols" if matches!(verb, "open" | "resize") => {
                cols = Some(value(&mut args)?.parse()?)
            }
            "--rows" if matches!(verb, "open" | "resize") => {
                rows = Some(value(&mut args)?.parse()?)
            }
            "--profile" if verb == "open" => profile = Some(value(&mut args)?),
            "--activate" if verb == "open" => activate = true,
            "--regex" if verb == "wait-for-text" => regex = true,
            "--timeout" if matches!(verb, "wait-for-text" | "events") => {
                timeout = Some(value(&mut args)?.parse()?)
            }
            "--since" if verb == "events" => since = Some(value(&mut args)?.parse()?),
            "--stop" if verb == "record" => stop = true,
            "--" if verb == "wait-for-text" => {
                ensure!(text.is_none(), "only one pattern is accepted");
                text = Some(value(&mut args)?);
                ensure!(args.next().is_none(), "unexpected argument after pattern");
            }
            _ if !arg.starts_with("--") && matches!(verb, "wait-for-text" | "record") => {
                ensure!(text.is_none(), "only one pattern/path is accepted");
                text = Some(arg);
            }
            _ => bail!("unsupported {verb} option {arg}"),
        }
    }
    let request = match verb {
        "open" => {
            ensure!(pane.is_none(), "open does not accept --pane");
            Request::Open {
                profile: profile.context("--profile is required")?,
                cols,
                rows,
                activate,
            }
        }
        "resize" => Request::Resize {
            pane: pane.context("--pane is required")?,
            cols: cols.context("--cols is required")?,
            rows: rows.context("--rows is required")?,
        },
        "events" => Request::Events {
            pane: pane.context("--pane is required")?,
            since: since.unwrap_or(0),
            timeout_seconds: timeout.unwrap_or(0),
        },
        "wait-for-text" => Request::WaitForText {
            pane: pane.context("--pane is required")?,
            pattern: text.context("a literal pattern or regular expression is required")?,
            regex,
            timeout_seconds: timeout.unwrap_or(30),
        },
        "record" => {
            ensure!(stop != text.is_some(), "provide PATH or --stop");
            Request::Record {
                pane: pane.context("--pane is required")?,
                path: text.map(std::path::absolute).transpose()?,
            }
        }
        _ => bail!("unknown automation command"),
    };
    Ok((pid, request))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_cross_command_flags_and_missing_values() {
        let p = |v, a: &[&str]| parse(v, a.iter().map(|s| s.to_string()), None, None);
        assert!(matches!(
            p("wait-for-text", &["--pane", "1", "--regex", "fps: [0-9]+"])
                .unwrap()
                .1,
            Request::WaitForText { regex: true, .. }
        ));
        assert!(p("events", &["--pane", "1", "--regex"]).is_err());
        assert!(p("resize", &["--pane", "1", "--cols", "120"]).is_err());
        assert!(p("record", &["--pane", "1", "--stop", "file.vt"]).is_err());
        assert!(p("open", &["--profile", "play", "--pane", "1"]).is_err());
    }
}
