"""Run on Linux with zmx 1.0.1, Python 3 and Vim. Does not require sshd."""
import argparse
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import time
import uuid


OSC = re.compile(rb"\x1b\]([^\x07\x1b]*)(?:\x07|\x1b\\)")
EFFECTS = {b"52;c;Zml4dHVyZQ==", b"9;w11-notification", b"440;1;play;shot"}
TIMEOUT = 15


def image_trial(binary, env):
    # The application sends one RGB pixel exactly once. A later barrier proves
    # that the restored client has consumed its snapshot without an app redraw.
    name = "w11term-" + uuid.uuid4().hex
    transmit = b"\x1b_Ga=T,f=24,s=1,v=1,i=71,q=2;/wAA\x1b\\"
    program = ("import os, sys\n"
               f"os.write(1, {transmit!r})\n"
               "for line in sys.stdin:\n"
               "    if line.strip() == 'probe': os.write(1, b'RESTORED-IMAGE-BARRIER\\r\\n')\n")
    client = Client([binary, "attach", name, sys.executable, "-c", program], env, (24, 80))
    try:
        eventually(lambda: transmit in client.read(), "live kitty image did not reach the terminal")
        client.close()
        client = None
        client = Client([binary, "attach", name, "/bin/false"], env, (28, 100))
        client.send(b"probe\r")
        eventually(lambda: b"RESTORED-IMAGE-BARRIER" in client.read(), "lost graphics program after resize")
        image_replayed = re.search(rb"\x1b_G[^;]*;[A-Za-z0-9+/=]+\x1b\\", client.output) is not None
        print("Static kitty image on reattach: " +
              ("image data restored" if image_replayed else "application redraw required"))
    finally:
        if client is not None:
            client.close()
        subprocess.run([binary, "kill", name, "--force"], env=env, capture_output=True, timeout=TIMEOUT)


def eventually(check, explanation):
    deadline = time.monotonic() + TIMEOUT
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.03)
    raise AssertionError(explanation)


class Client:
    """An SSH PTY's terminal-facing side; closing the master simulates losing SSH."""

    def __init__(self, args, env, size):
        self.fd, slave = pty.openpty()
        self.output = bytearray()
        try:
            rows, cols = size
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
            self.process = subprocess.Popen(args, stdin=slave, stdout=slave, stderr=slave,
                                            start_new_session=True, env=env, close_fds=True)
        except BaseException:
            os.close(self.fd)
            raise
        finally:
            os.close(slave)

    def read(self, timeout=0.03):
        if select.select([self.fd], [], [], timeout)[0]:
            try:
                self.output.extend(os.read(self.fd, 65536))
            except OSError as e:
                if e.errno != errno.EIO:
                    raise
        return bytes(self.output)

    def send(self, data):
        os.write(self.fd, data)

    def close(self):
        os.close(self.fd)
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
            raise AssertionError("attachment client did not exit after terminal transport loss")


def run(binary, vim, directory):
    directory = Path(directory)
    report = directory / "report.json"
    source = directory / "unsaved.txt"
    session = "w11-" + uuid.uuid4().hex
    generation = uuid.uuid4().hex
    env = {**os.environ, "ZMX_DIR": str(directory / "sockets"), "HOME": str(directory),
           "TERM": "xterm-256color", "SHELL": "/bin/sh", "W11_REPORT": str(report)}
    # A nested "attach" switches an already attached client. This fixture must own its instance.
    for name in ("ZMX_SESSION", "ZMX_SESSION_PREFIX", "ZMX_NO_DETACH_KEY", "VIMINIT", "EXINIT"):
        env.pop(name, None)
    Path(env["ZMX_DIR"]).mkdir(mode=0o700)

    def command(*args, check=True):
        return subprocess.run([binary, *args], stdin=subprocess.DEVNULL, capture_output=True,
                              env=env, check=check, timeout=TIMEOUT)

    def same_generation():
        result = command("get", session, "w11term_generation", check=False)
        return result.returncode == 0 and result.stdout.strip() == generation.encode()

    def daemon_info():
        for line in command("list").stdout.decode().splitlines():
            fields = dict(item.split("=", 1) for item in line.strip().split("\t") if "=" in item)
            if fields.get("name") == session:
                return fields
        return {}

    def report_data():
        return json.loads(report.read_text()) if report.exists() else {}

    def live_effects(seen):
        return EFFECTS <= set(OSC.findall(seen)) and b"\x07" in OSC.sub(b"", seen)

    assert re.search(rb"\b1\.0\.1\b", command("version").stdout), "requires zmx 1.0.1"
    assert not same_generation(), "the non-creating probe must fail for an unknown session"
    assert not daemon_info(), "probe created the missing session"
    size = (28, 94)
    new_size = (33, 107)
    args = [binary, "attach", "--labels", "w11term_generation=" + generation, session, vim,
            "-Nu", "NONE", "-U", "NONE", "-i", "NONE", "-n", "--noplugin", "-N",
            "-S", str(Path(__file__).with_name("editor.vim").resolve()), str(source)]
    client = None
    try:
        client = Client(args, env, size)
        eventually(lambda: bool(client.read() and report_data() and same_generation()),
                   "editor did not start with its generation label")
        pid = report_data()["pid"]
        daemon = daemon_info()["pid"]
        client.send(b"gQ")
        eventually(lambda: live_effects(client.read()), "missing live BEL, OSC 52, notification or sound")
        marker = b"unsaved-" + uuid.uuid4().hex[:12].encode()
        client.send(b"i" + marker)
        eventually(lambda: marker in command("history", session, "--screen").stdout,
                   "typed text was not visible before detach")
        assert not source.exists(), "editor saved its buffer unexpectedly"
        lost, client = client, None
        lost.close()
        eventually(lambda: daemon_info().get("clients") == "0", "daemon did not notice the lost client")
        assert same_generation() and daemon_info()["pid"] == daemon
        assert Path(f"/proc/{pid}").is_dir(), "editor exited with its SSH-facing client"
        # Existing sessions ignore the command. A disappearance between the label
        # probe and attach can spawn only /bin/false, never a replacement login shell.
        client = Client([binary, "attach", session, "/bin/false"], env, new_size)
        eventually(lambda: marker in client.read(), "alternate-screen editor was not restored before input")
        assert same_generation() and daemon_info()["pid"] == daemon
        restored = bytes(client.output)
        assert re.search(rb"\x1b\[\?(?:[0-9]+;)*(?:47|1047|1049)(?:;[0-9]+)*h", restored), (
            "restored display lacked an alternate-screen transition")
        # The very first byte remains editor input in the previous insert mode.
        client.send(b"Z\x1bgR")
        expected = (marker + b"Z").decode()
        eventually(lambda: b"2;w11-final-report" in OSC.findall(client.read())
                   and report_data().get("line") == expected,
                   "first input did not extend the original unsaved buffer")
        restored = bytes(client.output)
        assert not any(osc.split(b";", 1)[0] in {b"52", b"9", b"440"} for osc in OSC.findall(restored)), (
            "old clipboard, notification or sound OSC replayed on attach")
        assert b"\x07" not in OSC.sub(b"", restored), "standalone bell replayed on attach"
        final = report_data()
        assert (final["pid"], final["modified"], final["rows"], final["cols"]) == (pid, 1, *new_size), final
        assert not source.exists()
        print(f"zmx 1.0.1 restored Vim PID {pid}, its unsaved buffer and {new_size[1]}×{new_size[0]} PTY")
        print("First input stayed in insert mode; previous clipboard, sound, notification and bell did not replay.")
        lost, client = client, None
        lost.close()
        command("kill", session)
        eventually(lambda: not same_generation() and not daemon_info(),
                   "an ended session still claims its prior identity")
        # Exercise the stock attach-on-missing behavior with a harmless, observable
        # fallback command; a predecessor's successful probe cannot promise an attach.
        expired_marker = directory / "expired-fallback"
        client = Client([binary, "attach", session, "/usr/bin/touch", str(expired_marker)], env, size)
        eventually(expired_marker.exists, "stock attach did not create its fallback command")
        assert not same_generation(), "expired fallback acquired the old generation"
        print("Confirmed that stock zmx attach creates a command when the named session has expired.")
        image_trial(binary, env)
    finally:
        try:
            if client is not None:
                client.close()
        finally:
            command("kill", session, "--force", check=False)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--zmx", default=shutil.which("zmx"))
    parser.add_argument("--vim", default=shutil.which("vim"))
    args = parser.parse_args()
    if not args.zmx or not args.vim:
        parser.error("pass --zmx and --vim or install both on PATH")
    with tempfile.TemporaryDirectory(prefix="w11-zmx-") as directory:
        run(os.path.abspath(args.zmx), os.path.abspath(args.vim), directory)
