"""Checks for the opt-in perf.data stream on Linux."""

import hashlib
import subprocess
import time


def split_perf_data(stdout):
    header, remainder = stdout.split(b"\n", 1)
    marker, length, digest = header.split(b"\t")
    assert marker == b"w11term-perf-data-v1"
    size = int(length)
    assert 0 < size <= 32 * 1024 * 1024
    recording, text = remainder[:size], remainder[size:]
    assert len(recording) == size
    assert hashlib.sha256(recording).hexdigest().encode() == digest
    return recording, text


def stop_recording(argv, ready, env=None, sample_time=0):
    ready.unlink(missing_ok=True)
    with subprocess.Popen(
        argv, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE
    ) as recording:
        deadline = time.monotonic() + 8
        while not ready.is_file() or not ready.stat().st_size:
            assert recording.poll() is None, recording.stderr.read()
            assert time.monotonic() < deadline, "Recording did not start"
            time.sleep(0.01)
        time.sleep(sample_time)
        stdout, stderr = recording.communicate(b"stop\n", timeout=8)
        return subprocess.CompletedProcess(argv, recording.returncode, stdout, stderr)
