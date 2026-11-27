"""Exercise the CPU profile launcher with a real Linux process and a perf fixture."""

import base64
import os
import re
import shlex
import subprocess
import sys
import tempfile
from pathlib import Path


SUPERVISOR = Path("src/ssh/triage/cpu-profile-supervisor.sh").read_text()
WORKER = Path("src/ssh/triage/cpu-profile-worker.sh").read_text()
PERF = """#!/usr/bin/env python3
import os
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
    assert args[-3:] == ["--", "sleep", "2"]
    assert destination.parent.stat().st_mode & 0o777 == 0o700
    Path(os.environ["W11TERM_TEST_REMOTE_DIR"]).write_text(str(destination.parent))
    if scenario == "hang":
        time.sleep(30)
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
                ["/bin/sh", "-c", script, "w11term-cpu-profile", WORKER, deadline,
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
                profile = subprocess.run(
                    ["/bin/sh", "-c", SUPERVISOR, "w11term-cpu-profile", WORKER, "10",
                     "capture", pid, ticks, "2", "99", "fp", "32768", "4096"],
                    capture_output=True,
                    timeout=14,
                )
                assert profile.returncode == 0, profile.stderr
                assert re.search(rb"(?m)^\s*" + pid.encode() + rb" +\d+\.\d+: +cpu-clock(?::u)?:",
                                 profile.stdout), profile.stdout[:2000]
                assert b" hot_loop " in profile.stdout, profile.stdout[:2000]
                assert profile.stdout.endswith(b"\nw11term-cpu-profile-end\n")
                print("Live perf smoke resolved the hotspot.")
            finally:
                hot.terminate()


if __name__ == "__main__":
    if sys.argv[1:] == ["--live"]:
        live_perf_smoke()
    else:
        assert len(sys.argv) == 1, "Usage: linux-cpu-profile.py [--live]"
        fixture_tests()
