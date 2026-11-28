use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::{borrow::Cow, io::Write};
use w11term::{
    command_reference::{self, Key, Page, PageSource, Platform, WINDOWS},
    control::Request,
};

pub(super) const VERB: &str = "command-reference";
pub(super) const HELP: &str =
    "w11term-cli command-reference QUERY [--platform PLATFORM] [--search] [--format json|text]";
pub(super) const OPEN_VERB: &str = "open-command-reference";
pub(super) const OPEN_HELP: &str =
    "w11term-cli [--pid PID] open-command-reference QUERY [--window ID] [--platform PLATFORM]";

pub(super) fn parse_open(mut args: impl Iterator<Item = String>) -> Result<Request> {
    let query = args
        .next()
        .filter(|query| !query.trim().is_empty() && !query.starts_with("--"))
        .context("provide a command or subcommand name")?;
    let mut window_id = None;
    let mut platform = None;
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .with_context(|| format!("{flag} requires a value"))
        };
        match flag.as_str() {
            "--window" => {
                ensure!(window_id.is_none(), "specify --window only once");
                let id: std::num::NonZeroU64 =
                    value()?.parse().context("provide a positive --window ID")?;
                window_id = Some(id.get());
            }
            "--platform" => {
                ensure!(platform.is_none(), "specify --platform only once");
                platform = Some(command_reference::parse_platform(&value()?)?.0.to_owned());
            }
            _ => bail!("unknown option {flag} for {OPEN_VERB}"),
        }
    }
    Ok(Request::OpenCommandReference {
        query,
        window_id,
        platform,
    })
}

struct Options<'a> {
    query: &'a str,
    platform: Platform,
    search: bool,
    text: bool,
}

impl<'a> Options<'a> {
    fn parse(args: &'a [String]) -> Result<Self> {
        let mut args = args.iter().map(String::as_str);
        let query = args
            .next()
            .context("provide a command or task to look up")?
            .trim();
        ensure!(!query.is_empty(), "provide a command or task to look up");
        let mut platform = None;
        let mut format = None;
        let mut search = false;
        while let Some(flag) = args.next() {
            let value = match flag {
                "--search" => {
                    ensure!(!search, "specify --search only once");
                    search = true;
                    continue;
                }
                "--platform" => &mut platform,
                "--format" => &mut format,
                _ => bail!("unknown option {flag} for {VERB}"),
            };
            ensure!(value.is_none(), "specify {flag} only once");
            *value = Some(
                args.next()
                    .with_context(|| format!("{flag} requires a value"))?,
            );
        }
        let platform = platform
            .map(command_reference::parse_platform)
            .transpose()?
            .unwrap_or(WINDOWS);
        let text = match format {
            None | Some("json") => false,
            Some("text") => true,
            Some(_) => bail!("--format requires json or text"),
        };
        Ok(Self {
            query,
            platform,
            search,
            text,
        })
    }
}

#[derive(Serialize)]
struct Report<'a> {
    schema_version: u32,
    query: Cow<'a, str>,
    preferred_platform: &'static str,
    catalog_status: Cow<'a, str>,
    matches: Vec<Manual<'a>>,
}

#[derive(Serialize)]
struct Manual<'a> {
    name: Cow<'a, str>,
    title: Cow<'a, str>,
    platform: &'static str,
    platform_label: &'static str,
    source: &'static str,
    source_label: &'static str,
    description: Cow<'a, str>,
    documentation: Option<Cow<'a, str>>,
    related: Vec<Cow<'a, str>>,
    examples: Vec<Example<'a>>,
}

#[derive(Serialize)]
struct Example<'a> {
    description: Cow<'a, str>,
    command: &'a str,
}

impl<'a> Manual<'a> {
    fn new(key: &'a Key, page: &'a Page) -> Self {
        Self {
            name: printable(&key.name),
            title: printable(&page.title),
            platform: key.platform.0,
            platform_label: key.platform.1,
            source: match page.source {
                PageSource::Tldr => "tldr",
                PageSource::BuiltIn => "built-in",
                PageSource::Custom => "private",
            },
            source_label: page.source.label(),
            description: printable(&page.description),
            documentation: page.link.as_deref().map(printable),
            related: page.related.iter().map(|label| printable(label)).collect(),
            examples: page
                .examples
                .iter()
                .map(|example| Example {
                    description: printable(&example.description),
                    command: &example.command,
                })
                .collect(),
        }
    }
}

// Private files and updated catalogues can contain terminal control characters.
// Preserve line breaks and tabs in help text while keeping output printable.
fn printable(text: &str) -> Cow<'_, str> {
    let allowed = |c: char| !c.is_control() || matches!(c, '\n' | '\t');
    if text.chars().all(allowed) {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(text.chars().filter(|&c| allowed(c)).collect())
    }
}

pub(super) fn lookup(args: &[String], out: &mut dyn Write) -> Result<()> {
    let options = Options::parse(args)?;
    let library = command_reference::load()?;
    let exact;
    let matches = if options.search {
        library.catalog.search(options.query, options.platform)
    } else {
        exact = library
            .catalog
            .exact(options.query, options.platform)
            .with_context(|| {
                format!(
                    "No offline manual for {:?} on {}",
                    options.query, options.platform.1
                )
            })?;
        let page = library
            .catalog
            .page(&exact)
            .context("The offline manual is unavailable")?;
        vec![(&exact, page)]
    };
    let report = Report {
        schema_version: 1,
        query: printable(options.query),
        preferred_platform: options.platform.0,
        catalog_status: printable(&library.status),
        matches: matches
            .into_iter()
            .map(|(key, page)| Manual::new(key, page))
            .collect(),
    };
    if options.text {
        write_text(&report, options.search, out)?;
    } else {
        serde_json::to_writer(&mut *out, &report)?;
        writeln!(out)?;
    }
    Ok(())
}

fn write_text(report: &Report<'_>, search: bool, out: &mut dyn Write) -> Result<()> {
    if report.matches.is_empty() {
        writeln!(out, "No matching command manuals")?;
        return Ok(());
    }
    for page in &report.matches {
        writeln!(
            out,
            "{} [{}] · {}",
            page.title, page.platform_label, page.source_label
        )?;
        if search {
            if let Some(summary) = page.description.lines().next() {
                writeln!(out, "  {summary}")?;
            }
            continue;
        }
        if !page.description.is_empty() {
            writeln!(out, "\n{}", page.description)?;
        }
        for example in &page.examples {
            writeln!(out, "\n{}\n  {}", example.description, example.command)?;
        }
        if !page.related.is_empty() {
            writeln!(out, "\nSee also: {}", page.related.join(", "))?;
        }
        if let Some(url) = &page.documentation {
            writeln!(out, "Documentation: {url}")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    #[test]
    fn open_command_reference_builds_a_window_scoped_control_request() {
        let invocation = super::super::parse_invocation(
            [
                "--pid",
                "23",
                OPEN_VERB,
                "git checkout",
                "--window",
                "7",
                "--platform",
                "Linux",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(invocation.pid, Some(23));
        assert_eq!(
            to_value(invocation.request).unwrap(),
            json!({
                "verb": OPEN_VERB, "query": "git checkout", "window_id": 7, "platform": "linux",
            })
        );
        assert_eq!(
            to_value(parse_open(["git".to_owned()].into_iter()).unwrap()).unwrap(),
            json!({"verb": OPEN_VERB, "query": "git"})
        );
    }

    #[test]
    fn open_command_reference_rejects_invalid_selectors() {
        for args in [
            &[][..],
            &["--window", "7"],
            &["git", "--window", "0"],
            &["git", "--window", "2", "--window", "3"],
            &["git", "--platform", "unsupported"],
            &["git", "--platform", "common", "--platform", "linux"],
            &["git", "--search"],
        ] {
            assert!(
                parse_open(args.iter().copied().map(str::to_owned)).is_err(),
                "{args:?}"
            );
        }
    }
}
