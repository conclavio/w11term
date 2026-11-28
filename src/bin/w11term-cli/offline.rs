use super::{
    command_reference, cpu_profile, dotfile_profiles, dotfiles, packet_capture, syscall_capture,
    tool_bundles, triage,
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    io::{self, Write},
    path::{self, Path, PathBuf},
    process::ExitCode,
};
use w11term::{control::Reply, options::default_profiles_path, profiles::ProfileStore, transcript};

type OfflineCommand = fn(&[String], &mut dyn Write) -> Result<()>;

pub(super) fn run(args: &[String]) -> Option<ExitCode> {
    let (verb, args) = args.split_first()?;
    let (help, command): (&str, OfflineCommand) = match verb.as_str() {
        command_reference::VERB => (command_reference::HELP, command_reference::lookup),
        dotfiles::VERB => (dotfiles::HELP, dotfiles::save),
        dotfiles::INSPECT_VERB => (dotfiles::INSPECT_HELP, dotfiles::inspect),
        dotfiles::COMPARE_VERB => (dotfiles::COMPARE_HELP, dotfiles::compare),
        dotfile_profiles::VERB => (dotfile_profiles::HELP, dotfile_profiles::audit),
        dotfile_profiles::ROLLOUT_VERB => {
            (dotfile_profiles::ROLLOUT_HELP, dotfile_profiles::rollout)
        }
        dotfile_profiles::ROLLBACK_VERB => {
            (dotfile_profiles::ROLLBACK_HELP, dotfile_profiles::rollback)
        }
        tool_bundles::AUDIT_VERB => (tool_bundles::AUDIT_HELP, tool_bundles::audit),
        tool_bundles::ROLLOUT_VERB => (tool_bundles::ROLLOUT_HELP, tool_bundles::rollout),
        tool_bundles::ROLLBACK_VERB => (tool_bundles::ROLLBACK_HELP, tool_bundles::rollback),
        cpu_profile::COMPARE_VERB => (cpu_profile::COMPARE_HELP, cpu_profile::compare_files),
        syscall_capture::INSPECT_VERB => (
            syscall_capture::INSPECT_HELP,
            syscall_capture::inspect_files,
        ),
        syscall_capture::COMPARE_VERB => (
            syscall_capture::COMPARE_HELP,
            syscall_capture::compare_files,
        ),
        triage::COMPARE_VERB => (triage::COMPARE_HELP, triage::compare_files),
        packet_capture::INSPECT_VERB => {
            (packet_capture::INSPECT_HELP, packet_capture::inspect_file)
        }
        packet_capture::COMPARE_VERB => {
            (packet_capture::COMPARE_HELP, packet_capture::compare_files)
        }
        packet_capture::CLIP_VERB => (packet_capture::CLIP_HELP, packet_capture::clip_file),
        packet_capture::VERIFY_REPORT_VERB => (
            packet_capture::VERIFY_REPORT_HELP,
            packet_capture::verify_report,
        ),
        packet_capture::SAVE_SETTINGS_VERB => (
            packet_capture::SAVE_SETTINGS_HELP,
            packet_capture::save_settings,
        ),
        _ => return None,
    };
    if args == ["--help"] || args == ["-h"] {
        println!("{help}");
        return Some(ExitCode::SUCCESS);
    }
    match command(args, &mut io::stdout().lock()) {
        Ok(()) => Some(ExitCode::SUCCESS),
        Err(error) => {
            let reply = Reply::from_result(Err(error));
            eprintln!("{}", serde_json::to_string(&reply).unwrap());
            Some(ExitCode::FAILURE)
        }
    }
}

pub(super) fn profile_store(catalog: Option<&str>) -> Result<ProfileStore> {
    let catalog = match catalog {
        Some(path) => path::absolute(path)?,
        None => default_profiles_path()?,
    };
    ensure!(
        catalog
            .metadata()
            .with_context(|| format!("Opening profile catalog {}", catalog.display()))?
            .is_file(),
        "The profile catalog must be a regular file"
    );
    ProfileStore::new(catalog)
}

pub(super) struct ProfileRollout<'a> {
    pub store: ProfileStore,
    pub profiles: Vec<String>,
    pub candidate: PathBuf,
    pub review: Option<&'a str>,
}

pub(super) fn profile_rollout<'a>(
    args: &'a [String],
    missing_candidate: &'static str,
) -> Result<ProfileRollout<'a>> {
    let mut catalog = None;
    let mut candidate = None;
    let mut review = None;
    let mut profiles = Vec::new();
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .with_context(|| format!("{flag} requires a value"))?;
        let target = match flag.as_str() {
            "--profiles" => &mut catalog,
            "--candidate" => &mut candidate,
            "--review" => &mut review,
            "--profile" => {
                profiles.push(value.clone());
                continue;
            }
            _ => bail!("unsupported profile rollout option {flag}"),
        };
        ensure!(
            target.replace(value.as_str()).is_none(),
            "specify {flag} only once"
        );
    }
    let candidate = path::absolute(candidate.context(missing_candidate)?)?;
    Ok(ProfileRollout {
        store: profile_store(catalog)?,
        profiles,
        candidate,
        review,
    })
}

pub(super) fn rollback_args(args: &[String]) -> Result<(&Path, Option<&str>)> {
    match args {
        [path] => Ok((Path::new(path), None)),
        [path, flag, token] if flag == "--review" => Ok((Path::new(path), Some(token.as_str()))),
        _ => bail!("Provide the saved rollout reply and optionally --review TOKEN"),
    }
}

pub(super) fn write_report(
    stdout: &mut dyn Write,
    output_file: Option<&str>,
    write: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> Result<()> {
    if let Some(path) = output_file {
        let output_file = path::absolute(path)?;
        transcript::save_with(&output_file, false, |file| write(file))?;
        let reply = Reply::from_result(Ok(serde_json::json!({"path": output_file})));
        serde_json::to_writer(&mut *stdout, &reply)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    } else {
        write(stdout).context("Writing the offline report")?;
    }
    Ok(())
}
