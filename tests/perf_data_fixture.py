"""Checks for the opt-in perf.data stream on Linux."""

import hashlib


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
