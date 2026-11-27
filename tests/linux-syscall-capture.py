"""Verify the capture supervisor against real strace and local Linux processes."""

import hashlib
from pathlib import Path
import re
import select
import subprocess
import sys
import tempfile
import time
import uuid

SCRIPT = Path("src/ssh/triage/syscall-capture.sh").read_text()
MIB = 1024 * 1024


def command(mode, directory, program, args, *, seconds=15, job="", plan=("", "", "")):
    return [
        "/bin/bash", "-c", SCRIPT, "w11term-syscall-capture", mode,
        str(seconds), str(MIB), "128", "all", directory, program, *plan, job, *args,
    ]


def read_line(process):
    ready, _, _ = select.select([process.stdout], [], [], 25)
    assert ready, "syscall capture did not respond"
    line = process.stdout.readline()
    if not line:
        _, error = process.communicate(timeout=3)
        raise AssertionError((process.returncode, error))
    return line.decode().rstrip("\n")


def wait_for_path(path):
    deadline = time.monotonic() + 5
    while not path.exists() or path.stat().st_size == 0:
        assert time.monotonic() < deadline, path
        time.sleep(0.02)


def reviewed(directory, program, args):
    response = subprocess.run(
        command("inspect", directory, program, args),
        capture_output=True, check=True, timeout=8,
    )
    token, actual_directory, actual_program, identity = response.stdout.decode().rstrip("\n").split("\t")
    assert token == "w11term-syscall-plan-v1"
    assert actual_directory == directory and actual_program == program
    return actual_directory, actual_program, identity


def run(directory, program, args, *, seconds=15, stop_after=None, disconnect_after=None):
    plan = reviewed(directory, program, args)
    job = uuid.uuid4().hex
    work = Path(f"/tmp/w11term-strace-{job}")
    cmd = command("capture", directory, program, args, seconds=seconds, job=job, plan=plan)
    with subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE) as capture:
        try:
            if disconnect_after:
                wait_for_path(disconnect_after)
                out, error = capture.communicate(b"", timeout=6)
                assert capture.returncode != 0 and not out and not work.exists(), (out, error)
                return
            if stop_after:
                wait_for_path(stop_after)
                capture.stdin.write(b"stop\n")
                capture.stdin.flush()
            token, reason, status, size, sha256 = read_line(capture).split("\t")
            assert token == "w11term-syscall-ready-v1"
            raw = (work / "trace.strace").read_bytes()
            assert len(raw) == int(size) and len(raw) <= MIB
            assert hashlib.sha256(raw).hexdigest() == sha256
            assert work.stat().st_mode & 0o777 == 0o700
            out, error = capture.communicate(b"verified\n", timeout=5)
            assert capture.returncode == 0 and out == b"w11term-syscall-complete-v1\n", (out, error)
            assert not work.exists()
            return reason, status, raw
        finally:
            if capture.poll() is None:
                capture.terminate()
                capture.wait(timeout=5)


def dead(pid):
    entry = Path(f"/proc/{pid}/stat")
    try:
        state = entry.read_text().rsplit(") ", 1)[1].split()[0]
        return state == "Z"
    except FileNotFoundError:
        return True


def smoke():
    with tempfile.TemporaryDirectory(prefix="w11term syscall ") as tmp:
        directory = Path(tmp)
        literal = "quoted '$ path.txt"
        (directory / literal).write_text("w11term fixture")
        shell = "/bin/sh"
        args = ["-c", 'cat "$1" & wait "$!"; cat missing-fixture; exit 124', "fixture", literal]
        reason, status, raw = run(tmp, shell, args)
        assert (reason, status) == ("exit", "124")
        assert literal.encode() in raw and b"missing-fixture" in raw and b"ENOENT" in raw
        pids = set(re.findall(rb"^(\d+)\s+\d+\.\d+\s", raw, re.MULTILINE))
        assert len(pids) >= 2 and b"openat(" in raw and b" <0." in raw, raw[:4000]

        timed, status, _ = run(tmp, shell, ["-c", "sleep 30 & wait"], seconds=1)
        assert (timed, status) == ("time", "-")

        for disconnect in (False, True):
            pid_path = directory / ("disconnected" if disconnect else "stopped")
            fixture = [
                "-c", 'sleep 30 & child=$!; printf "%s %s\n" "$$" "$child" > "$1"; wait',
                "fixture", str(pid_path),
            ]
            if disconnect:
                run(tmp, shell, fixture, seconds=30, disconnect_after=pid_path)
            else:
                reason, status, raw = run(tmp, shell, fixture, seconds=30, stop_after=pid_path)
                assert (reason, status) == ("stopped", "-") and b"execve(" in raw
            assert all(dead(int(pid)) for pid in pid_path.read_text().split()), pid_path.read_text()

        reason, status, raw = run(tmp, sys.executable, [
            "-c", 'import os\nfor _ in range(500000):\n os.close(os.open("/dev/null", os.O_RDONLY))',
        ], seconds=20)
        assert (reason, status, len(raw)) == ("bytes", "-", MIB)
        print("Real strace preserved descendant IDs, timing and errno. The byte cap, timeout, stop and disconnect cleanup passed.")


if __name__ == "__main__":
    smoke()
