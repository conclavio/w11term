use super::{cpu_profile, syscall_capture};
use anyhow::{Context, Result};
use std::{
    io::{self, Write},
    path,
    process::ExitCode,
};
use w11term::{control::Reply, transcript};

type Compare = fn(&[String], &mut dyn Write) -> Result<()>;

pub(super) fn run(args: &[String]) -> Option<ExitCode> {
    let (verb, args) = args.split_first()?;
    let (help, compare): (&str, Compare) = match verb.as_str() {
        cpu_profile::COMPARE_VERB => (cpu_profile::COMPARE_HELP, cpu_profile::compare_files),
        syscall_capture::COMPARE_VERB => (
            syscall_capture::COMPARE_HELP,
            syscall_capture::compare_files,
        ),
        _ => return None,
    };
    if args == ["--help"] || args == ["-h"] {
        println!("{help}");
        return Some(ExitCode::SUCCESS);
    }
    match compare(args, &mut io::stdout().lock()) {
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
        write(stdout).context("Writing the comparison report")?;
    }
    Ok(())
}
