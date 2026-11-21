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
PROBES = re.compile(CSI + rb"(?:0?c|14t|16t|18t)")


def pixels(rows, cols):
    return rows, cols, cols * 9, rows * 18


class Capture:
    def __init__(self, client, report):
        self.client = client
        self.report = report
        self.scanned = 0
        self.size = INITIAL

    def read(self):
        data = self.client.read()
        # Queries may straddle SSH packets. Rescan a short tail and answer each
        # complete query exactly once with the native terminal's DA1 / geometry.
        rows, cols, width, height = pixels(*self.size)
        for match in PROBES.finditer(data, max(0, self.scanned - 8)):
            if match.end() <= self.scanned:
                continue
            code = match.group()[2:]
            reply = {b"14t": f"4;{height};{width}t", b"16t": "6;18;9t",
                     b"18t": f"8;{rows};{cols}t"}.get(code, "?65;4;6;18;22;52c")
            self.client.send(b"\x1b[" + reply.encode())
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
        layout = directory / "layout.kdl"
        layout.write_text(f'layout {{\n pane command="{sys.executable}" {{\n'
                          f'  args "{pane}"\n }}\n}}\n')
        command = [binary, "--new-session-with-layout", str(layout), "--session", name]
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
        capture.size = RESIZED
        client.resize(*pixels(*RESIZED))
        resized = capture.wait("FIRST", initial)
        assert tuple(b - a for a, b in zip(initial, resized)) == (8, 26), (initial, resized)
        client.send(b"\x1b[C")
        assert capture.wait("MOVED") == resized
        capture.flush_frame()
        second = bytes(client.output[len(first):])
        (args.out / f"{name}-moved.vt").write_bytes(second)
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
        for case in [("zellij", "zellij", "xterm-256color"),
                     ("venti-default", "venti", "xterm-256color"),
                     ("venti-sixel", "venti", "xterm-sixel")]:
            run(args, Path(tmp), *case)
