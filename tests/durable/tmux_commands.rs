//! Exercise the production commands and parsers on Linux without the WinUI build.
#[path = "../../src/durable/tmux.rs"]
mod tmux;
use std::{env, os::unix::process::CommandExt, process::Command};

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let sh = |command: &str| {
        let mut shell = Command::new("sh");
        shell.args(["-c", command]);
        shell
    };
    if args == ["list"] {
        let output = sh(tmux::LIST).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        for entry in tmux::parse_listing(&output.stdout).unwrap() {
            println!(
                "{} {} {} {} {}",
                entry.target.name(),
                entry.target.generation(),
                entry.windows,
                entry.clients,
                entry.name
            );
        }
        return;
    }
    let target = tmux::Target::parse(&args[1], &args[2]).unwrap();
    match args[0].as_str() {
        "attach" => panic!("{}", sh(&target.attach()).exec()),
        "proof" => {
            let output = sh(&target.clients()).output().unwrap();
            if !output.status.success()
                || !target.attached(args[3].parse().unwrap(), &output.stdout)
            {
                std::process::exit(1);
            }
        }
        _ => panic!("unknown fixture action"),
    }
}
