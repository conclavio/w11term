"""Exercise command review, perf launch, and deadline cleanup on Linux."""

import base64
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
from perf_data_fixture import split_perf_data

SCRIPTS = Path("src/ssh/triage")
SUPERVISOR = (SCRIPTS / "cpu-profile-supervisor.sh").read_text()
WORKER = (SCRIPTS / "cpu-perf-data.sh").read_text() + (SCRIPTS / "cpu-launch-worker.sh").read_text()
RUNNER = (SCRIPTS / "cpu-launch-command.sh").read_text()
PERF = """#!/usr/bin/env python3
import os
from pathlib import Path
import subprocess
import sys
import resource

args = sys.argv[1:]
if args == ["--version"]:
    sys.exit(0)
if args[0] == "record":
    assert "--no-inherit" not in args and "-p" not in args
    path = Path(args[args.index("-o") + 1])
    assert resource.getrlimit(resource.RLIMIT_FSIZE)[0] == 64 * 1024
    Path(os.environ["W11TERM_TEST_RECORD_DIR"]).write_text(str(path.parent))
    subprocess.run(args[args.index("--") + 1:], check=True)
    path.write_bytes(b"perf-data")
else:
    assert args == ["script", "-i", args[2], "-F", "pid,time,event,ip,sym,dso"]
    assert Path(args[2]).read_bytes() == b"perf-data"
    print("42 12.5: cpu-clock:u: ab original (/tmp/cmd)")
    print("45 12.5: cpu-clock:u: ab child (/tmp/cmd)")
"""
COMMAND = """#!/usr/bin/env python3
import json
from pathlib import Path
import subprocess
import sys
import time

Path("args.json").write_text(json.dumps(sys.argv[1:]))
Path("regular-output").write_bytes(b"x" * 128 * 1024)
child = subprocess.Popen(["sleep", "30"])
Path("child-pid").write_text(str(child.pid))
if sys.argv[1] == "delay":
    time.sleep(30)
sys.exit(124)
"""


def arguments(script, mode, root, executable, reviewed, argv, record_kib="64", stacks_kib="16"):
    seconds = "1"
    deadline = "4" if mode == "inspect" else "9"
    return [
        "/bin/sh", "-c", script, "w11term-cpu-profile", WORKER, deadline, mode,
        RUNNER, seconds, "99", "fp", record_kib, stacks_kib, str(root), executable,
        *reviewed, *argv,
    ]


def run(*args, **kwargs):
    return subprocess.run(
        arguments(*args), stdin=subprocess.DEVNULL,
        capture_output=True, timeout=14, **kwargs,
    )


def paths(stdout):
    header, rest = stdout.split(b"\n", 1)
    marker, directory, program = header.split(b"\t")
    assert marker == b"w11term-cpu-launch-v1"
    return [base64.b64decode(value).decode() for value in (directory, program)], rest


def assert_child_stopped(root):
    stat = Path("/proc") / (root / "child-pid").read_text() / "stat"
    try:
        assert stat.read_text().rsplit(") ", 1)[1].split()[0] == "Z", stat
    except FileNotFoundError:
        pass


def fixture():
    with tempfile.TemporaryDirectory(prefix="w11term command launch ") as directory:
        root = Path(directory)
        for name, content in (("perf", PERF), ("cpu worker", COMMAND)):
            path = root / name
            path.write_text(content)
            path.chmod(0o700)
        record_directory = root / "record-dir"
        env = {**os.environ, "W11TERM_TEST_RECORD_DIR": str(record_directory)}
        supervisor = SUPERVISOR.replace(
            "PATH=/usr/sbin:", f"PATH={shlex.quote(str(root))}:/usr/sbin:", 1
        )
        argv = ["quote' and $(literal)", "", "-literal", "two\nlines"]
        preview = run(supervisor, "inspect", root, "./cpu worker", ["", ""], argv, env=env)
        assert preview.returncode == 0, preview.stderr
        reviewed, rest = paths(preview.stdout)
        assert reviewed == [str(root), str(root / "./cpu worker")] and not rest
        assert not (root / "args.json").exists()
        rejected = run(supervisor, "launch", root, "./cpu worker", [reviewed[0], "/wrong"], argv, env=env)
        assert rejected.returncode != 0 and b"changed" in rejected.stderr
        assert not (root / "args.json").exists()

        for status, values in ((b"exit\t124", argv), (b"timeout", ["delay"])):
            taken = run(supervisor, "launch", root, "./cpu worker", reviewed, values, env=env)
            assert taken.returncode == 0, taken.stderr
            actual, stacks = paths(taken.stdout)
            assert actual == reviewed
            assert stacks.startswith(status + b"\n"), stacks
            assert b"child (/tmp/cmd)" in stacks
            assert stacks.endswith(b"\nw11term-cpu-profile-end\n")
            assert json.loads((root / "args.json").read_text()) == values
            assert (root / "regular-output").stat().st_size == 128 * 1024
            assert_child_stopped(root)
            assert not Path(record_directory.read_text()).exists()


def live():
    with tempfile.TemporaryDirectory(prefix="w11term-cpu-launch-live.") as directory:
        root = Path(directory)
        subprocess.run(
            ["cc", "-x", "c", "-", "-g", "-O1", "-fno-omit-frame-pointer",
             "-fno-optimize-sibling-calls", "-o", root / "hotspot"],
            input=b"""
volatile unsigned long counter;
__attribute__((noinline)) void launched_hotspot(void) { for (;;) ++counter; }
int main(void) { launched_hotspot(); return 0; }
""",
            check=True, timeout=10,
        )
        preview = run(SUPERVISOR, "inspect", root, "./hotspot", ["", ""], [])
        assert preview.returncode == 0, preview.stderr
        reviewed, _ = paths(preview.stdout)
        capture = subprocess.run(
            arguments(SUPERVISOR, "launch-raw", root, "./hotspot", reviewed, [], "32768", "4096"),
            capture_output=True, timeout=14,
        )
        assert capture.returncode == 0, capture.stderr
        recording, text = split_perf_data(capture.stdout)
        actual, stacks = paths(text)
        assert actual == reviewed and stacks.startswith(b"timeout\n")
        assert b" launched_hotspot " in stacks, stacks[:2048]
        assert stacks.endswith(b"\nw11term-cpu-profile-end\n")
        raw_file = root / "launched.perf.data"
        raw_file.write_bytes(recording)
        reopened = subprocess.run(
            ["perf", "report", "--stdio", "-i", raw_file],
            capture_output=True, check=True, timeout=10,
        )
        assert b"launched_hotspot" in reopened.stdout, reopened.stdout[:2000]
        print("Live perf report reopened the raw launch recording after the time limit.")


if __name__ == "__main__":
    if sys.argv[1:] == ["--live"]:
        live()
    else:
        assert len(sys.argv) == 1, "Usage: linux-cpu-launch.py [--live]"
        fixture()
