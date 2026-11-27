"""Exercise the CPU profile launcher with a real Linux process and a perf fixture."""

import base64
import os
import re
import shlex
import subprocess
import sys
import tempfile
from pathlib import Path
from perf_data_fixture import split_perf_data, stop_recording


SUPERVISOR = Path("src/ssh/triage/cpu-profile-supervisor.sh").read_text()
RUNNER = Path("src/ssh/triage/cpu-launch-command.sh").read_text()
WORKER = (Path("src/ssh/triage/cpu-perf-data.sh").read_text()
          + Path("src/ssh/triage/cpu-profile-worker.sh").read_text())


def worker(runner=RUNNER):
    return f"runner={shlex.quote(runner)}\n{WORKER}"


PERF = """#!/usr/bin/env python3
import os
import subprocess
import sys
import time
from pathlib import Path

args = sys.argv[1:]
scenario = os.environ["W11TERM_TEST_PERF"]
if args == ["--version"]:
    sys.exit(0)
if args[0] == "record":
    expected_pid = os.environ["W11TERM_TEST_PID"]
    assert args[:args.index("-o")] == [
        "record", "--no-buildid-cache", "--no-inherit", "--all-user", "--user-callchains",
        "-e", "cpu-clock:u",
        "-F", "99", "--call-graph", "dwarf,8192", "-p", expected_pid,
    ]
    destination = Path(args[args.index("-o") + 1])
    window = args[args.index("--") + 1:]
    assert window[:2] == ["/bin/bash", "-c"]
    assert window[-2:] == ["sleep", "20" if scenario == "stop" else "2"]
    assert destination.parent.stat().st_mode & 0o777 == 0o700
    Path(os.environ["W11TERM_TEST_REMOTE_DIR"]).write_text(str(destination.parent))
    if scenario == "hang":
        time.sleep(30)
    if scenario == "stop":
        subprocess.run(window, check=True)
    if scenario == "deny":
        print("No permission to enable cpu-clock:u event.", file=sys.stderr)
        sys.exit(1)
    if scenario == "large-record":
        destination.write_bytes(b"x" * (1024 * 1024))
    else:
        destination.write_bytes(b"perf-data")
else:
    assert args == [
        "script", "-i", args[2], "--pid", os.environ["W11TERM_TEST_PID"],
        "-F", "pid,time,event,ip,sym,dso",
    ]
    assert Path(args[2]).read_bytes() == b"perf-data"
    if scenario == "large-stacks":
        os.write(1, b"x" * (1024 * 1024))
    else:
        print(f"  {os.environ['W11TERM_TEST_PID']} 22.01: cpu-clock:u:")
        print("\\t   deadbeef hot_loop (/tmp/worker)")
        print("\\t   12ab34cd main (/tmp/worker)")
        print("Processed 3 events and lost 1 chunks!", file=sys.stderr)
"""


def fixture_tests():
    with tempfile.TemporaryDirectory(prefix="w11term-perf-test.") as root:
        root = Path(root)
        fixture = root / "perf"
        fixture.write_text(PERF)
        fixture.chmod(0o700)
        recorded_directory = root / "worker-directory"
        # Use an awkward Linux comm to test /proc/<pid>/stat parsing.
        Path("/proc/self/comm").write_text("cpu) work\nname")
        target_pid = str(os.getpid())
        env = {
            **os.environ,
            "W11TERM_TEST_PID": target_pid,
            "W11TERM_TEST_REMOTE_DIR": str(recorded_directory),
        }
        script = SUPERVISOR.replace(
            "PATH=/usr/sbin:", f"PATH={shlex.quote(str(root))}:/usr/sbin:", 1
        )

        def run(scenario="ok", mode="capture", start="0", deadline="6"):
            return subprocess.run(
                ["/bin/sh", "-c", script, "w11term-cpu-profile", worker(), deadline,
                 mode, target_pid, start, "2", "99", "dwarf,8192", "64", "16"],
                env={**env, "W11TERM_TEST_PERF": scenario},
                capture_output=True,
                timeout=12,
            )

        preview = run(mode="inspect")
        assert preview.returncode == 0, preview.stderr
        version, recorded_start, name = preview.stdout.rstrip(b"\n").split(b"\t")
        assert version == b"w11term-cpu-profile-v1"
        assert base64.b64decode(name) == b"cpu) work\nname"
        start = recorded_start.decode()
        assert start == Path("/proc/self/stat").read_text().rsplit(") ", 1)[1].split()[19]
        assert not recorded_directory.exists()

        reused = run(start=str(int(start) + 1))
        assert b"target PID was reused" in reused.stderr and reused.returncode != 0
        assert not recorded_directory.exists(), "perf must not start for a reused PID"

        capture = run(start=start)
        assert capture.returncode == 0, capture.stderr
        assert capture.stdout.startswith(preview.stdout)
        assert b"hot_loop" in capture.stdout
        assert capture.stdout.endswith(b"\nw11term-cpu-profile-end\n")
        assert b"lost 1 chunks" in capture.stderr
        assert not Path(recorded_directory.read_text()).exists()

        stopped = stop_recording(
            ["/bin/sh", "-c", script, "w11term-cpu-profile", worker(), "29",
             "capture-raw", target_pid, start, "20", "99", "dwarf,8192", "64", "16"],
            recorded_directory,
            env={**env, "W11TERM_TEST_PERF": "stop"},
        )
        assert stopped.returncode == 0, stopped.stderr
        recording, text = split_perf_data(stopped.stdout)
        assert recording == b"perf-data"
        assert text.startswith(preview.stdout + b"w11term-cpu-profile-stopped-v1\n")
        assert b"hot_loop" in text
        assert not Path(recorded_directory.read_text()).exists()

        raw_only = run(scenario="large-stacks", mode="capture-raw", start=start)
        assert raw_only.returncode == 0, raw_only.stderr
        recording, text = split_perf_data(raw_only.stdout)
        assert recording == b"perf-data"
        assert text == preview.stdout + b"\nw11term-cpu-profile-end\n"
        assert b"stack output limit" in raw_only.stderr or b"stacks reached" in raw_only.stderr
        assert not Path(recorded_directory.read_text()).exists()

        for scenario, expected, deadline in [
            ("deny", b"No permission to enable", "6"),
            ("large-record", b"limit", "6"),
            ("large-stacks", b"limit", "6"),
            ("hang", b"exceeded its deadline", "1"),
        ]:
            failed = run(scenario=scenario, start=start, deadline=deadline)
            assert failed.returncode != 0 and expected in failed.stderr, failed
            assert b"w11term-cpu-profile-end" not in failed.stdout
            assert not Path(recorded_directory.read_text()).exists(), scenario


def live_perf_smoke():
    with tempfile.TemporaryDirectory(prefix="w11term-perf-live.") as root:
        executable = Path(root) / "hotspot"
        subprocess.run(
            ["cc", "-x", "c", "-", "-g", "-O1", "-fno-omit-frame-pointer",
             "-fno-optimize-sibling-calls", "-o", executable],
            input=b"""
volatile unsigned long counter;
__attribute__((noinline)) void hot_loop(void) { for (;;) ++counter; }
int main(void) { hot_loop(); return 0; }
""",
            check=True,
            timeout=10,
        )
        with subprocess.Popen([executable]) as hot:
            try:
                pid = str(hot.pid)
                ticks = Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()[19]
                ready = Path(root) / "recording-started"
                runner = f"printf 'ready\\n' > {shlex.quote(str(ready))}\n{RUNNER}"
                profile = stop_recording(
                    ["/bin/sh", "-c", SUPERVISOR, "w11term-cpu-profile", worker(runner), "29",
                     "capture-raw", pid, ticks, "20", "99", "fp", "32768", "4096"],
                    ready, sample_time=0.5,
                )
                assert profile.returncode == 0, profile.stderr
                assert hot.poll() is None, "Stopping perf killed the attached process"
                recording, stacks = split_perf_data(profile.stdout)
                assert b"w11term-cpu-profile-stopped-v1\n" in stacks
                assert re.search(rb"(?m)^\s*" + pid.encode() + rb" +\d+\.\d+: +cpu-clock(?::u)?:",
                                 stacks), stacks[:2000]
                assert b" hot_loop " in stacks, stacks[:2000]
                assert stacks.endswith(b"\nw11term-cpu-profile-end\n")
                raw_file = Path(root) / "captured.perf.data"
                raw_file.write_bytes(recording)
                reopened = subprocess.run(
                    ["perf", "report", "--stdio", "-i", raw_file],
                    capture_output=True, check=True, timeout=10,
                )
                assert b"hot_loop" in reopened.stdout, reopened.stdout[:2000]
                print("Live perf report reopened the raw recording and resolved the hotspot.")
            finally:
                hot.terminate()


if __name__ == "__main__":
    if sys.argv[1:] == ["--live"]:
        live_perf_smoke()
    else:
        assert len(sys.argv) == 1, "Usage: linux-cpu-profile.py [--live]"
        fixture_tests()
