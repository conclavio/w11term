use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn inspect(directory: &Path, options: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_w11term-cli"))
        .current_dir(directory)
        .env_remove("LOCALAPPDATA")
        .args([
            "inspect-syscall-trace",
            "--trace",
            "work.42",
            "--trace",
            "work.43",
        ])
        .args(options)
        .output()
        .unwrap()
}

fn report(output: &Output) -> Value {
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn ranks_recorded_durations_offline_and_links_the_resumed_call() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path();
    fs::write(
        path.join("work.42"),
        concat!(
            "1700000000.000000 read(3, \"x\", 1) = 1 <0.004000>\n",
            "1700000000.002000 read(3,  <unfinished ...>\n",
            "1700000000.009000 <... read resumed>\"y\", 1) = 1 <0.005000>\n",
            "1700000000.010000 close(3) = 0\n",
            "1700000000.011000 write(3,  <unfinished ...>\n",
            "1700000000.012000 --- SIGUSR1 {si_signo=SIGUSR1} ---\n",
        ),
    )
    .unwrap();
    fs::write(path.join("work.43"), concat!(
        "1700000000.005000 poll([], 0, 8) = 0 <0.008000>\n",
        "1700000000.014000 openat(AT_FDCWD, \"/missing\", O_RDONLY) = -1 ENOENT (No such file or directory) <0.000000>\n",
        "1700000000.015000 +++ exited with 0 +++\n",
    )).unwrap();

    let result = inspect(path, &[]);
    let all = report(&result);
    assert_eq!(all["schema_version"], 1);
    assert_eq!(all["file_names"], json!(["work.42", "work.43"]));
    assert_eq!(all["pid"], Value::Null);
    assert_eq!(all["calls"], 6);
    assert_eq!(all["timed_calls"], 4);
    assert_eq!(all["incomplete_calls"], 1);
    assert_eq!(all["errors"], 1);
    assert_eq!(all["other_events"], 2);
    assert_eq!(all["total_duration_ns"], "17000000");
    let rows = all["syscalls"].as_array().unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row["syscall"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["read", "poll", "openat", "close", "write"]
    );
    assert_eq!(
        rows[0],
        json!({
            "syscall": "read", "calls": 2, "errors": 0, "incomplete_calls": 0, "timed_calls": 2,
            "total_duration_ns": "9000000", "max_duration_ns": "5000000",
            "slowest": {"pid": 42, "outcome": "success", "errno": null,
                "sources": [{"file": 1, "line": 2}, {"file": 1, "line": 3}]},
        })
    );
    assert_eq!(rows[2]["total_duration_ns"], "0");
    assert_eq!(rows[2]["slowest"]["errno"], "ENOENT");
    assert_eq!(rows[3]["total_duration_ns"], Value::Null);
    assert_eq!(rows[4]["slowest"], Value::Null);
    assert_eq!(rows[4]["incomplete_calls"], 1);
    assert_eq!(
        report(&inspect(path, &["--sort", "slowest"]))["syscalls"][0]["syscall"],
        "poll"
    );
    assert_eq!(
        report(&inspect(path, &["--sort", "errors"]))["syscalls"][0]["syscall"],
        "openat"
    );
    let scoped = report(&inspect(path, &["--pid", "43", "--sort", "calls"]));
    assert_eq!(scoped["pid"], 43);
    assert_eq!(scoped["calls"], 2);
    assert_eq!(scoped["syscalls"][0]["syscall"], "openat");
    let empty = report(&inspect(path, &["--pid", "999"]));
    assert_eq!(empty["total_duration_ns"], Value::Null);
    assert_eq!(empty["syscalls"], json!([]));
    assert!(!inspect(path, &["--pid", "0"]).status.success());

    let dest = "reviewed summary.json";
    let receipt = report(&inspect(path, &["--output-file", dest]));
    assert_eq!(receipt["ok"], true);
    assert!(
        !inspect(path, &["--pid", "43", "--output-file", dest])
            .status
            .success()
    );
    assert_eq!(fs::read(path.join(dest)).unwrap(), result.stdout);
}
