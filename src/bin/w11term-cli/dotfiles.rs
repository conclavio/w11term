use super::{offline, sftp_args::dotfile_shell};
use anyhow::{Context, Result, bail, ensure};
use std::{io::Write, path::Path};
use w11term::{
    sftp::{DotfileBundle, DotfileShell},
    shell_setup::{Mode, Setup},
};

pub(super) const VERB: &str = "save-dotfiles-bundle";
pub(super) const HELP: &str = concat!(
    "w11term-cli save-dotfiles-bundle [--shell bash|zsh|fish --setup directory|full]\n",
    "    [--environment-file PATH] [--bash-file PATH] [--zsh-file PATH]\n",
    "    [--fish-environment-file PATH] [--fish-file PATH] [--from-bundle PATH] [--output-file PATH]\n",
    "Provide a shell setup, one or more fragment files, or both. Fragment file options can be repeated."
);

pub(super) fn save(args: &[String], output: &mut dyn Write) -> Result<()> {
    let mut shell = None;
    let mut mode = None;
    let mut source = None;
    let mut destination = None;
    let mut environment_files = Vec::new();
    let mut bash_files = Vec::new();
    let mut zsh_files = Vec::new();
    let mut fish_environment_files = Vec::new();
    let mut fish_files = Vec::new();
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .with_context(|| format!("{flag} requires a value"))?
            .as_str();
        let import = match flag.as_str() {
            "--environment-file" => Some(&mut environment_files),
            "--bash-file" => Some(&mut bash_files),
            "--zsh-file" => Some(&mut zsh_files),
            "--fish-environment-file" => Some(&mut fish_environment_files),
            "--fish-file" => Some(&mut fish_files),
            _ => None,
        };
        if let Some(files) = import {
            files.push(value);
            continue;
        }
        let target = match flag.as_str() {
            "--shell" => &mut shell,
            "--setup" => &mut mode,
            "--from-bundle" => &mut source,
            "--output-file" => &mut destination,
            _ => bail!("unsupported dotfiles option {flag}"),
        };
        ensure!(target.replace(value).is_none(), "specify {flag} only once");
    }
    let setup = match (shell, mode) {
        (None, None) => None,
        (Some(shell), Some(mode)) => {
            let shell = dotfile_shell(Some(shell))?;
            let mode = match mode {
                "directory" => Mode::Directory,
                "full" => Mode::Full,
                _ => bail!("--setup requires directory or full"),
            };
            Some(Setup::for_shell(shell, mode))
        }
        _ => bail!("Use --shell and --setup together"),
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
    let imports = [
        (&mut bundle.environment, environment_files),
        (&mut bundle.bash, bash_files),
        (&mut bundle.zsh, zsh_files),
        (&mut bundle.fish_environment, fish_environment_files),
        (&mut bundle.fish, fish_files),
    ];
    ensure!(
        setup.is_some() || imports.iter().any(|(_, files)| !files.is_empty()),
        "Provide a shell setup or a fragment file"
    );
    for (fragment, files) in imports {
        if files.is_empty() {
            continue;
        }
        let mut text = fragment
            .as_deref()
            .unwrap_or_default()
            .replace("\r\n", "\n");
        for file in files {
            text = DotfileBundle::append_fragment_from_file(Path::new(file), &text)
                .with_context(|| format!("Importing shell fragment {file}"))?;
        }
        *fragment = Some(text);
    }
    if let Some(setup) = setup {
        let fragment = match setup.shell() {
            DotfileShell::Bash => &mut bundle.bash,
            DotfileShell::Zsh => &mut bundle.zsh,
            DotfileShell::Fish => &mut bundle.fish,
        };
        let existing = fragment
            .as_deref()
            .unwrap_or_default()
            .replace("\r\n", "\n");
        *fragment = Some(setup.insert(&existing)?);
    }
    bundle.validate_for_editing()?;
    offline::write_report(output, destination, |writer| {
        serde_json::to_writer_pretty(&mut *writer, &bundle)?;
        writer.write_all(b"\n")
    })
}
