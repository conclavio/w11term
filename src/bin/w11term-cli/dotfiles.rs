use super::{offline, sftp_args::dotfile_shell};
use anyhow::{Context, Result, bail, ensure};
use std::{io::Write, path::Path};
use w11term::{
    sftp::{DotfileBundle, DotfileShell},
    shell_setup::{Mode, Setup},
};

pub(super) const VERB: &str = "save-dotfiles-bundle";
pub(super) const HELP: &str = concat!(
    "w11term-cli save-dotfiles-bundle --shell bash|zsh|fish --setup directory|full ",
    "[--from-bundle PATH] [--output-file PATH]"
);

pub(super) fn save(args: &[String], output: &mut dyn Write) -> Result<()> {
    let mut shell = None;
    let mut mode = None;
    let mut source = None;
    let mut destination = None;
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        let target = match flag.as_str() {
            "--shell" => &mut shell,
            "--setup" => &mut mode,
            "--from-bundle" => &mut source,
            "--output-file" => &mut destination,
            _ => bail!("unsupported dotfiles option {flag}"),
        };
        let value = args
            .next()
            .with_context(|| format!("{flag} requires a value"))?;
        ensure!(
            target.replace(value.as_str()).is_none(),
            "specify {flag} only once"
        );
    }
    let shell = dotfile_shell(shell)?;
    let mode = match mode {
        Some("directory") => Mode::Directory,
        Some("full") => Mode::Full,
        _ => bail!("--setup requires directory or full"),
    };
    let mut bundle = match source {
        Some(path) => DotfileBundle::load(Path::new(path))?,
        None => DotfileBundle {
            version: 1,
            environment: None,
            bash: None,
            zsh: None,
            fish_environment: None,
            fish: None,
        },
    };
    let fragment = match shell {
        DotfileShell::Bash => &mut bundle.bash,
        DotfileShell::Zsh => &mut bundle.zsh,
        DotfileShell::Fish => &mut bundle.fish,
    };
    let existing = fragment
        .as_deref()
        .unwrap_or_default()
        .replace("\r\n", "\n");
    *fragment = Some(Setup::for_shell(shell, mode).insert(&existing)?);
    bundle.validate_for_editing()?;
    offline::write_report(output, destination, |writer| {
        serde_json::to_writer_pretty(&mut *writer, &bundle)?;
        writer.write_all(b"\n")
    })
}
