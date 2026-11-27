"""Exercise the remote packet worker with a real loopback exchange."""

import hashlib
import json
from pathlib import Path
import re
import select
import shlex
import socket
import subprocess
import struct
import sys
import tempfile
import threading
import time
import uuid

SCRIPT = Path("src/ssh/triage/packet-capture.sh").read_text()
RING = Path("src/ssh/triage/packet-ring.py").read_text()


def run(mode, root, port, *, mount="", job="", count=3, seconds=8, script=SCRIPT, slots=None):
    args = [
        "/bin/bash", "-c", script, "w11term-packet-capture", mode, "lo", "192",
        str(count), str(seconds), "1024", "yes", str(root), f"udp and dst port {port}",
        mount, job,
    ]
    if slots is not None:
        args.extend(["w11term-ring-v1", str(slots), RING])
    return args


def line(process):
    ready, _, _ = select.select([process.stdout], [], [], 12)
    assert ready, "packet worker didn't reply"
    return process.stdout.readline().decode().rstrip("\n")


def wait_for_file(path, *, minimum_size=0):
    deadline = time.monotonic() + 4
    while not path.exists() or path.stat().st_size < minimum_size:
        assert time.monotonic() < deadline, path
        time.sleep(0.02)


def verify_ring_writer():
    # Big-endian nanosecond pcap and deliberately reversed timestamps.
    # The ring must preserve record order without sorting by time.
    header = bytes.fromhex("a1b23c4d") + struct.pack(">HHIIII", 2, 4, 0, 0, 64, 1)
    records = [
        struct.pack(">IIII", 100 - index, 0, 48, 80) + bytes([index]) * 48
        for index in range(7)
    ]
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        runner = [sys.executable, "-I", "-S", "-c", RING, str(root), "152", "2", "64"]
        proc = subprocess.run(runner, input=header + b"".join(records),
                              capture_output=True, check=True)
        manifest = json.loads(proc.stdout)
        assert manifest["packets"] == 7
        assert [entry["sequence"] for entry in manifest["segments"]] == [2, 3]
        assert (root / "segment-0.pcap").read_bytes() == header + b"".join(records[4:6])
        assert (root / "segment-1.pcap").read_bytes() == header + records[6]
        assert sum(path.stat().st_size for path in root.glob("*.pcap")) <= 304
        proc = subprocess.run(runner, input=header + records[0][:-1], capture_output=True)
        assert proc.returncode != 0 and not proc.stdout and b"incomplete packet" in proc.stderr


def first_ring_packet(path):
    try:
        with path.open("rb") as source:
            packet = re.search(rb"fixture-ring-(\d{7})", source.read(24 + 16 + 192))
    except FileNotFoundError:
        return None
    return int(packet[1]) if packet else None


def live_ring(root, port, mount, *, stop_early=False):
    job = uuid.uuid4().hex
    work = root / f"w11term-pcap-{job}"
    done = threading.Event()

    def send():
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sender:
            index = 0
            while not done.wait(0.0003):
                payload = f"fixture-ring-{index:07}".encode() + b" " * 300
                sender.sendto(payload, ("127.0.0.1", port))
                index += 1

    traffic = threading.Thread(target=send, daemon=True)
    limit = 0 if stop_early else 6500
    with subprocess.Popen(run("capture", root, port, mount=mount, job=job,
                              slots=2, count=limit, seconds=30),
                          stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE) as capture:
        traffic.start()
        try:
            if stop_early:
                deadline = time.monotonic() + 14
                while True:
                    first = first_ring_packet(work / "segment-0.pcap")
                    second = first_ring_packet(work / "segment-1.pcap")
                    if first is not None and second is not None and first > second:
                        break
                    assert time.monotonic() < deadline, "the rolling capture did not wrap before Stop"
                    time.sleep(0.01)
                capture.stdin.write(b"stop\n")
                capture.stdin.flush()
            marker, reason, metadata = line(capture).split("\t")
            assert (marker, reason) == ("w11term-packet-ring-v1", "stopped" if stop_early else "packets")
            manifest = json.loads(metadata)
            if not stop_early:
                assert manifest["packets"] == limit
            segments = manifest["segments"]
            assert len(segments) == 2
            assert segments[0]["sequence"] > 0
            assert segments[0]["sequence"] + 1 == segments[1]["sequence"]
            assert len(list(work.glob("*.pcap"))) == 2
            assert sum((work / f"segment-{slot}.pcap").stat().st_size for slot in range(2)) <= 1024 * 1024
            packet_ids = []
            for segment in segments:
                path = work / f"segment-{segment['sequence'] % 2}.pcap"
                data = path.read_bytes()
                assert len(data) == segment["bytes"] <= 512 * 1024
                assert hashlib.sha256(data).hexdigest() == segment["sha256"]
                decoded = subprocess.run(["tcpdump", "-n", "-r", path, "-A"],
                                         capture_output=True, check=True, timeout=5)
                packet_ids.extend(map(int, re.findall(rb"fixture-ring-(\d{7})", decoded.stdout)))
            assert 0 < len(packet_ids) < manifest["packets"]
            assert all(a < b for a, b in zip(packet_ids, packet_ids[1:]))
            out, err = capture.communicate(b"verified\n", timeout=5)
            assert capture.returncode == 0 and out == b"w11term-packet-complete-v1\n", (out, err)
            assert not work.exists()
        finally:
            done.set()
            traffic.join(timeout=1)
            if capture.poll() is None:
                capture.kill()


def smoke():
    with tempfile.TemporaryDirectory(prefix="w11term packet ", dir="/dev/shm") as directory:
        root = Path(directory)
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as receiver:
            receiver.bind(("127.0.0.1", 0))
            port = receiver.getsockname()[1]
            preflight = subprocess.run(run("inspect", root, port), capture_output=True, check=True, timeout=10)
            marker, kind, mount_id, resolved = preflight.stdout.decode().strip().split("\t")
            assert marker == "w11term-packet-plan-v1" and kind == "tmpfs" and resolved == str(root)
            job = uuid.uuid4().hex
            work = root / f"w11term-pcap-{job}"
            done = threading.Event()

            def send():
                with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sender:
                    while not done.wait(0.03):
                        sender.sendto(b"w11term pcap fixture", ("127.0.0.1", port))

            traffic = threading.Thread(target=send, daemon=True)
            with subprocess.Popen(run("capture", root, port, mount=mount_id, job=job),
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE) as capture:
                traffic.start()
                try:
                    status, reason, size, sha256 = line(capture).split("\t")
                    assert status == "w11term-packet-ready-v1" and reason == "packets"
                    path = work / "capture.pcap"
                    data = path.read_bytes()
                    assert len(data) == int(size) and hashlib.sha256(data).hexdigest() == sha256
                    decoded = subprocess.run(["tcpdump", "-nn", "-r", path, "-A"],
                                             capture_output=True, check=True, timeout=5)
                    assert decoded.stdout.count(b"w11term pcap fixture") == 3, decoded.stdout
                    # A stop crossing the ready record must allow verification.
                    out, err = capture.communicate(b"stop\nverified\n", timeout=5)
                    assert capture.returncode == 0 and out == b"w11term-packet-complete-v1\n", (out, err)
                    assert b"3 packets captured" in err
                    assert not work.exists()
                finally:
                    done.set()
                    traffic.join(timeout=1)
                    if capture.poll() is None:
                        capture.kill()

            stopped_job = uuid.uuid4().hex
            stopped = root / f"w11term-pcap-{stopped_job}"
            done.clear()
            traffic = threading.Thread(target=send, daemon=True)
            with subprocess.Popen(run("capture", root, port, mount=mount_id,
                                      job=stopped_job, count=2048, seconds=20),
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE) as capture:
                traffic.start()
                try:
                    path = stopped / "capture.pcap"
                    wait_for_file(path, minimum_size=25)
                    capture.stdin.write(b"stop\n")
                    capture.stdin.flush()
                    status, reason, size, digest = line(capture).split("\t")
                    assert (status, reason) == ("w11term-packet-ready-v1", "stopped")
                    data = path.read_bytes()
                    assert len(data) == int(size) and hashlib.sha256(data).hexdigest() == digest
                    decoded = subprocess.run(["tcpdump", "-nn", "-r", path, "-A"],
                                             capture_output=True, check=True, timeout=5)
                    packets = decoded.stdout.count(b"w11term pcap fixture")
                    assert 0 < packets < 2048, packets
                    out, err = capture.communicate(b"verified\n", timeout=5)
                    assert capture.returncode == 0 and out == b"w11term-packet-complete-v1\n", (out, err)
                    assert f"{packets} packets captured".encode() in err
                    assert not stopped.exists()
                finally:
                    done.set()
                    traffic.join(timeout=1)
                    if capture.poll() is None:
                        capture.kill()

            abandoned_job = uuid.uuid4().hex
            abandoned = root / f"w11term-pcap-{abandoned_job}"
            with subprocess.Popen(run("capture", root, port, mount=mount_id,
                                      job=abandoned_job, count=100, seconds=30),
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE) as capture:
                wait_for_file(abandoned / "capture.pcap")
                blocked = subprocess.run(run("capture", root, port, mount=mount_id, job=uuid.uuid4().hex),
                                         input=b"", capture_output=True, timeout=4)
                assert blocked.returncode == 2 and b"Another packet capture is active" in blocked.stderr, (
                    blocked.returncode, blocked.stdout, blocked.stderr, capture.poll(),
                )
                out, err = capture.communicate(b"", timeout=5)
                assert capture.returncode != 0 and not out, (out, err)
                assert not abandoned.exists()

            timed_job = uuid.uuid4().hex
            timed = root / f"w11term-pcap-{timed_job}"
            with subprocess.Popen(run("capture", root, port, mount=mount_id,
                                      job=timed_job, count=100, seconds=1),
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE) as capture:
                status, reason, size, _ = line(capture).split("\t")
                assert (status, reason, int(size)) == ("w11term-packet-ready-v1", "time", 24)
                out, err = capture.communicate(b"verified\n", timeout=5)
                assert capture.returncode == 0 and b"0 packets captured" in err, (out, err)
                assert not timed.exists()

            # Exercise the low-capacity path without filling the runner's RAM.
            tools = root / "fixtures"
            tools.mkdir()
            df = tools / "df"
            df.write_text("#!/bin/sh\nprintf 'Avail\\n16\\n'\n")
            df.chmod(0o700)
            worker = SCRIPT.replace("PATH=/usr/sbin:", f"PATH={shlex.quote(str(tools))}:/usr/sbin:", 1)
            low = subprocess.run(run("inspect", root, port, script=worker),
                                 capture_output=True, timeout=5)
            assert low.returncode != 0 and b"cannot meet the capture budget" in low.stderr
            live_ring(root, port, mount_id)
            live_ring(root, port, mount_id, stop_early=True)
            print("Real tcpdump rolled and reopened the retained packet files in capture order within 1 MiB.")
            print("Real tcpdump stopped and finalized a wrapped buffer without a packet limit.")
            print("Real tcpdump passed packet and time limits, early stop, late stop acknowledgement, and disconnect cleanup.")


if __name__ == "__main__":
    verify_ring_writer()
    if sys.argv[1:] != ["--ring-unit"]:
        smoke()
