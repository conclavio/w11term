use super::{
    command_reference, cpu_profile, dotfile_profiles, dotfiles, packet_capture, syscall_capture,
    triage,
};
use anyhow::{Context, Result};
use std::{
    io::{self, Write},
    path,
    process::ExitCode,
};
use w11term::{control::Reply, transcript};

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
