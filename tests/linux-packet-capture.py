"""Exercise the remote packet worker with a real loopback exchange."""

import hashlib
from pathlib import Path
import select
import shlex
import socket
import subprocess
import tempfile
import threading
import time
import uuid

SCRIPT = Path("src/ssh/triage/packet-capture.sh").read_text()


def run(mode, root, port, *, mount="", job="", count=3, seconds=8, script=SCRIPT):
    return [
        "/bin/bash", "-c", script, "w11term-packet-capture", mode, "lo", "192",
        str(count), str(seconds), "1024", "yes", str(root), f"udp and dst port {port}",
        mount, job,
    ]


def line(process):
    ready, _, _ = select.select([process.stdout], [], [], 12)
    assert ready, "packet worker didn't reply"
    return process.stdout.readline().decode().rstrip("\n")


def wait_for_file(path):
    deadline = time.monotonic() + 4
    while not path.exists():
        assert time.monotonic() < deadline, path
        time.sleep(0.02)


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
                    out, err = capture.communicate(b"verified\n", timeout=5)
                    assert capture.returncode == 0 and out == b"w11term-packet-complete-v1\n", (out, err)
                    assert b"3 packets captured" in err
                    assert not work.exists()
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
                assert blocked.returncode != 0 and b"already active" in blocked.stderr, (
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
            print("Real tcpdump captured and reopened a loopback exchange. The scratch budget, lease, and disconnect cleanup passed.")


if __name__ == "__main__":
    smoke()
