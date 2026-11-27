"""Retain complete classic pcap records in a bounded set of files."""

from collections import deque
from hashlib import sha256
import json
from pathlib import Path
import struct
import sys


def file_header(stream):
    header = stream.read(24)
    order = {
        b"\xd4\xc3\xb2\xa1": "<",
        b"\x4d\x3c\xb2\xa1": "<",
        b"\xa1\xb2\xc3\xd4": ">",
        b"\xa1\xb2\x3c\x4d": ">",
    }.get(header[:4])
    if len(header) != 24 or not order:
        raise ValueError("tcpdump did not produce a classic pcap header")
    major, minor, _, _, snap_length, _ = struct.unpack(f"{order}HHIIII", header[4:])
    if (major, minor) != (2, 4) or snap_length == 0:
        raise ValueError("unsupported classic pcap header")
    return header, order, snap_length


def records(stream, order, snap_length):
    header_format = struct.Struct(f"{order}IIII")
    while packet := stream.read(16):
        if len(packet) != 16:
            raise ValueError("tcpdump produced an incomplete packet header")
        _, _, captured, original = header_format.unpack(packet)
        if captured > min(snap_length, original):
            raise ValueError("tcpdump produced an invalid packet length")
        payload = stream.read(captured)
        if len(payload) != captured:
            raise ValueError("tcpdump produced an incomplete packet")
        yield packet, payload


def rotate(stream, root, slot_limit, slots, reviewed_snap_length):
    header, order, snap_length = file_header(stream)
    if slot_limit < 24 + 16 + reviewed_snap_length:
        raise ValueError("a rolling capture file cannot fit one reviewed packet")
    retained = deque(maxlen=slots)
    sequence = 0
    packets = 0
    size = len(header)
    output = (root / "segment-0.pcap").open("wb")
    try:
        output.write(header)
        for packet, payload in records(stream, order, min(snap_length, reviewed_snap_length)):
            occupied = len(packet) + len(payload)
            if size + occupied > slot_limit:
                output.close()
                retained.append((sequence, size))
                sequence += 1
                output = (root / f"segment-{sequence % slots}.pcap").open("wb")
                output.write(header)
                size = len(header)
            output.write(packet)
            output.write(payload)
            size += occupied
            packets += 1
    finally:
        output.close()
    retained.append((sequence, size))

    manifest = []
    for sequence, size in retained:
        path = root / f"segment-{sequence % slots}.pcap"
        digest = sha256()
        with path.open("rb") as source:
            while block := source.read(64 * 1024):
                digest.update(block)
        manifest.append({"sequence": sequence, "bytes": size, "sha256": digest.hexdigest()})
    return {"packets": packets, "segments": manifest}


if __name__ == "__main__":
    directory, limit, count, snap = sys.argv[1:]
    try:
        segments = rotate(sys.stdin.buffer, Path(directory), int(limit), int(count), int(snap))
        print(json.dumps(segments, separators=(",", ":")))
    except (OSError, ValueError) as error:
        sys.exit(f"Rolling packet capture: {error}")
