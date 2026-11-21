"""Capture both ends of an interactive SSH PTY running a stock Linux multiplexer."""
import argparse
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
import time

from linux_pty import Client


INITIAL = (30, 100)
RESIZED = (38, 126)
CSI = rb"\x1b\["
SIXEL = re.compile(rb"\x1b\[[0-9]+;[0-9]+H\x1bP[0-9;]*q[^\x1b]+\x1b\\")
# Zellij 0.46.0 probes both uncompressed and compressed Kitty transfers.
# Windows replay compares the *entire* reply stream with the native engine,
# including any probes for which w11term sends nothing.
REPLIES = {
    b"\x1b[c": b"\x1b[?65;4;6;18;22;52c",
    b"\x1b[0c": b"\x1b[?65;4;6;18;22;52c",
    b"\x1b[14t": b"\x1b[4;%d;%dt",
    b"\x1b[16t": b"\x1b[6;18;9t",
    b"\x1b[18t": b"\x1b[8;%d;%dt",
    b"\x1b]11;?\x1b\\": b"\x1b]11;rgb:0e14/1020/13f7\x1b\\",
    b"\x1b]10;?\x1b\\": b"\x1b]10;rgb:dc28/e146/e8f4\x1b\\",
    b"\x1b[?2026$p": b"\x1b[?2026;2$y",
    b"\x1b[?2027$p": b"\x1b[?2027;3$y",
    b"\x1b_Ga=q,i=31,s=1,v=1,t=d,f=24;AAAA\x1b\\": b"\x1b_Gi=31;OK\x1b\\",
    b"\x1b_Ga=q,i=32,s=4,v=4,t=d,f=24,o=z;eJxjYCANAAAAMAAB\x1b\\": b"\x1b_Gi=32;OK\x1b\\",
}
PROBES = re.compile(b"|".join(map(re.escape, REPLIES)))
PROBE_TAIL = max(map(len, REPLIES)) - 1


def pixels(rows, cols):
    return rows, cols, cols * 9, rows * 18


class Capture:
    def __init__(self, client, report):
        self.client = client
        self.report = report
        self.scanned = 0
        self.size = INITIAL
        self.replies = bytearray()

    def read(self):
        data = self.client.read()
        # Queries may straddle SSH packets. Rescan a short tail and answer each
        # complete query exactly once with the native terminal's DA1 / geometry.
        rows, cols, width, height = pixels(*self.size)
        for match in PROBES.finditer(data, max(0, self.scanned - PROBE_TAIL)):
            if match.end() <= self.scanned:
                continue
            query = match.group()
            reply = REPLIES[query]
            if query == b"\x1b[14t":
                reply = reply % (height, width)
            elif query == b"\x1b[18t":
                reply = reply % (rows, cols)
            self.client.send(reply)
            self.replies.extend(reply)
        self.scanned = len(data)
        return data

    def wait(self, stage, previous=None):
        deadline = time.monotonic() + 25
        while time.monotonic() < deadline:
            self.read()
            if self.report.exists():
                name, cols, rows = self.report.read_text().split()
                size = (int(rows), int(cols))
                if name == stage and (previous is None or size != previous):
                    return size
            if self.client.process.poll() is not None:
                break
        raise AssertionError(f"pane did not reach {stage}, report={self.report.exists()}, "
                             f"terminal tail={self.client.output[-1200:]!r}")

    def flush_frame(self):
        # The pane's report acknowledges its PTY write, not the mux renderer.
        # Wait for output to settle before splitting the two recorded grids.
        until = time.monotonic() + 3
        quiet = time.monotonic()
        while time.monotonic() < until and time.monotonic() - quiet < 0.45:
            before = self.scanned
            self.read()
            if self.scanned != before:
                quiet = time.monotonic()


def run(args, root, name, mux, term):
    directory = root / name
    directory.mkdir(mode=0o700)
    runtime = directory / "run"
    runtime.mkdir(mode=0o700)
    config = directory / "config"
    report = directory / "pane"
    pane = Path(__file__).with_name("mux_pane.py").resolve()
    env = dict(PATH="/usr/bin:/bin", LANG="C.UTF-8", HOME=str(directory), SHELL="/bin/sh",
               XDG_CONFIG_HOME=str(config), XDG_RUNTIME_DIR=str(runtime),
               W11_REPORT=str(report), W11_MUX=mux)
    binary = getattr(args, mux)
    if mux == "venti":
        (config / "venti").mkdir(parents=True)
        (config / "venti/config.toml").write_text(
            f'[general]\nshell = "{sys.executable} {pane}"\ngraphics = true\n'
            '[ui]\nshow_welcome = false\nclock = false\n')
        doctor = subprocess.check_output([binary, "doctor"],
                                         env={**env, "TERM": term, "COLORTERM": "truecolor"})
        clean = re.sub(CSI + rb"[0-9;]*m", b"", doctor)
        expected = b"none" if term == "xterm-256color" else b"sixel"
        assert re.search(rb"(?m)^\s*graphics\s+" + expected + rb"\s*$", clean), clean
        command = [binary, "new", "--session", name]
        stop = [binary, "kill", "--target", name]
    else:
        (config / "zellij").mkdir(parents=True)
        (config / "zellij/config.kdl").write_text("show_startup_tips false\nshow_release_notes false\n")
        # An explicit initial command suppresses Zellij's first-run desktop
        # integration wizard, which otherwise receives our pane's first key.
        command = [binary, "attach", "--create", name, "--", sys.executable, str(pane)]
        stop = [binary, "kill-session", name]

    ssh_env = {**os.environ, "TERM": "xterm-256color", "COLORTERM": "truecolor"}
    # Require the SSH PTY and env requests to arrive before the optional
    # remote-side Sixel override. Keep developer/runner terminal markers out.
    prefix = 'test "$TERM/$COLORTERM" = xterm-256color/truecolor && exec env -i '
    variables = [f"{k}={v}" for k, v in env.items()]
    remote = prefix + 'COLORTERM="$COLORTERM" ' + (
        'TERM="$TERM" ' if term == "xterm-256color" else "TERM=xterm-sixel "
    ) + shlex.join([*variables, *command])
    client = Client(["ssh", "-F", args.ssh_config, "-tt", "mux-loopback", remote],
                    ssh_env, pixels(*INITIAL), controlling_tty=True)
    capture = Capture(client, report)
    try:
        capture.wait("READY")
        client.send(b"a")
        initial = capture.wait("FIRST")
        capture.flush_frame()
        first = bytes(client.output)
        (args.out / f"{name}-first.vt").write_bytes(first)
        (args.out / f"{name}-first.replies").write_bytes(capture.replies)
        capture.replies.clear()
        capture.size = RESIZED
        client.resize(*pixels(*RESIZED))
        resized = capture.wait("FIRST", initial)
        assert tuple(b - a for a, b in zip(initial, resized)) == (8, 26), (initial, resized)
        client.send(b"\x1b[C")
        assert capture.wait("MOVED") == resized
        capture.flush_frame()
        second = bytes(client.output[len(first):])
        (args.out / f"{name}-moved.vt").write_bytes(second)
        (args.out / f"{name}-moved.replies").write_bytes(capture.replies)
        for frame in [first, second]:
            graphics = SIXEL.search(frame)
            if name == "venti-sixel":
                assert graphics, "Sixel was not forwarded at an addressed cell"
            elif mux == "venti":
                assert b"\x1bPq" not in frame and not graphics, "unexpected default Sixel passthrough"
        print(f"{name}: input and resize {initial} -> {resized}", flush=True)
    finally:
        (args.out / f"{name}-full.vt").write_bytes(client.output)
        try:
            subprocess.run(stop, env={**env, "TERM": term}, capture_output=True, timeout=8)
        finally:
            client.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for arg in ("ssh-config", "venti", "zellij"):
        parser.add_argument("--" + arg, required=True)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="mux-") as tmp:
        failures = []
        for case in [("zellij", "zellij", "xterm-256color"),
                     ("venti-default", "venti", "xterm-256color"),
                     ("venti-sixel", "venti", "xterm-sixel")]:
            try:
                run(args, Path(tmp), *case)
            except Exception as error:
                error.add_note(case[0])
                failures.append(error)
        if failures:
            raise ExceptionGroup("Remote mux checks failed", failures)
