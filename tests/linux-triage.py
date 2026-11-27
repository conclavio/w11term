"""Exercise the exact remote triage supervisor and worker on Linux."""

import base64
import shlex
import subprocess
import tempfile
import time
from pathlib import Path


SUPERVISOR = Path("src/ssh/triage-supervisor.sh").read_text()
WORKER = Path("src/ssh/triage-worker.sh").read_text()
IDS = ["uptime", "dmesg", "vmstat", "mpstat", "pidstat", "iostat", "free", "sar-dev", "sar-tcp", "top"]


def collect(supervisor, worker):
    completed = subprocess.run(
        ["/bin/sh", "-c", supervisor, "w11term-triage", worker],
        stdin=subprocess.DEVNULL,
        capture_output=True,
        check=True,
        timeout=60,
    )
    status, reason, checks = parse_report(completed.stdout)
    assert reason == [], reason
    return status, checks


def parse_report(output):
    marker, *lines, end = output.decode("ascii").splitlines()
    assert end == "end", output
    version, started, finished, status, *reason = marker.split("\t")
    assert version == "w11term-linux-triage-v1" and int(started) <= int(finished)
    checks = {}
    for line in lines:
        name, _, _, code, out, err = line.split("\t")
        stdout = base64.b64decode(out, validate=True)
        stderr = base64.b64decode(err, validate=True)
        assert max(len(stdout), len(stderr)) <= 16384, name
        checks[name] = (code, stdout, stderr)
    assert list(checks) == IDS, checks
    return int(status), reason, checks


def stop_collection(worker):
    with tempfile.TemporaryDirectory() as temp:
        marker = Path(temp) / "ready"
        worker += f"""
run_check uptime uptime
printf '%s\\n' "$work" > "$work/top.out"
run_check mpstat bash -c '
    trap "" TERM
    printf "%s\\n" "$$" > "$1"
    exec sleep 30
' fixture {shlex.quote(str(marker))} &
wait
"""
        with subprocess.Popen(
            ["/bin/sh", "-c", SUPERVISOR, "w11term-triage", worker],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        ) as child:
            deadline = time.monotonic() + 5
            while not marker.is_file() or not marker.stat().st_size:
                assert child.poll() is None and time.monotonic() < deadline
                time.sleep(0.01)
            stdout, stderr = child.communicate(b"stop\n", timeout=6)
            assert child.returncode == 0, stderr
        status, reason, checks = parse_report(stdout)
        assert status != 0 and reason == ["stopped"], (status, reason)
        assert checks["uptime"][0] == "0" and checks["uptime"][1], checks
        assert checks["mpstat"][0] == "-", checks["mpstat"]
        scratch = Path(checks["top"][1].decode().strip())
        assert scratch.name.startswith("w11term-triage.") and not scratch.exists(), scratch
        proc_stat = Path("/proc") / marker.read_text().strip() / "stat"
        deadline = time.monotonic() + 2
        while True:
            try:
                state = proc_stat.read_text().rsplit(") ", 1)[1][0]
            except FileNotFoundError:
                break
            if state == "Z":
                break
            assert time.monotonic() < deadline, (proc_stat, state)
            time.sleep(0.01)


def main():
    status, checks = collect(SUPERVISOR, WORKER)
    assert status == 0, status
    for name in ["uptime", "vmstat", "free"]:
        assert checks[name][0] == "0" and checks[name][1], name
    assert checks["top"][1] and (
        checks["top"][0] == "0" or len(checks["top"][1]) == 16384
    ), checks["top"][0]

    workers = WORKER.split("run_check uptime uptime &", 1)[0]
    stop_collection(workers)
    workers += """
run_check uptime uptime &
run_check free head -c 100000 /dev/zero &
run_check mpstat sleep 30 &
printf '%s\\n' "$work" > "$work/top.out"
wait
"""
    status, checks = collect(SUPERVISOR.replace(" 50s ", " 2s "), workers)
    assert status == 124, status
    assert len(checks["free"][1]) == 16384, checks["free"][0]
    assert checks["mpstat"][0] == "-", checks["mpstat"]
    directory = Path(checks["top"][1].decode().strip())
    assert directory.name.startswith("w11term-triage.") and not directory.exists()


if __name__ == "__main__":
    main()
