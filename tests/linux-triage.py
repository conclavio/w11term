"""Exercise the exact remote triage supervisor and worker on Linux."""

import base64
import subprocess
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
    marker, *lines, end = completed.stdout.decode("ascii").splitlines()
    assert end == "end", completed.stdout
    version, started, finished, status = marker.split("\t")
    assert version == "w11term-linux-triage-v1" and int(started) <= int(finished)
    checks = {}
    for line in lines:
        name, _, _, code, out, err = line.split("\t")
        stdout = base64.b64decode(out, validate=True)
        stderr = base64.b64decode(err, validate=True)
        assert max(len(stdout), len(stderr)) <= 16384, name
        checks[name] = (code, stdout, stderr)
    assert list(checks) == IDS, checks
    return int(status), checks


def main():
    status, checks = collect(SUPERVISOR, WORKER)
    assert status == 0, status
    for name in ["uptime", "vmstat", "free"]:
        assert checks[name][0] == "0" and checks[name][1], name
    assert checks["top"][1] and (
        checks["top"][0] == "0" or len(checks["top"][1]) == 16384
    ), checks["top"][0]

    workers = WORKER.split("run_check uptime uptime &", 1)[0]
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
