"""Run w11term's exact discovery, PTY attach and proof against a private stock server."""
import contextlib
import fcntl
import os
import pathlib
import pty
import select
import struct
import subprocess
import tempfile
import termios
import time


def run(*args):
    return subprocess.check_output(args, text=True).strip()


def until(predicate):
    deadline = time.monotonic() + 6
    while not predicate():
        if time.monotonic() >= deadline:
            raise AssertionError("timed out waiting for tmux")
        time.sleep(0.05)


@contextlib.contextmanager
def attach(helper, identity):
    pid, fd = pty.fork()
    if pid == 0:
        fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 27, 109, 0, 0))
        os.execl(helper, helper, "attach", *identity)
    try:
        header = b""
        while b"\n" not in header:
            assert select.select([fd], [], [], 5)[0], "tmux PID header missing"
            header += os.read(fd, 4096)
        yield fd, str(int(header.splitlines()[0]))
    finally:
        os.close(fd)
        until(lambda: os.waitpid(pid, os.WNOHANG)[0] == pid)


def exercise(directory):
    helper = directory / "commands"
    source = pathlib.Path(__file__).with_name("tmux_commands.rs")
    subprocess.run(["rustc", "--edition=2024", source, "-o", helper], check=True)
    name = "ops 'weekly' $(display)"
    run("tmux", "-f", "/dev/null", "new-session", "-ds", name, "exec cat")
    run("tmux", "new-window", "-d", "-t", name)
    run("tmux", "new-session", "-ds", "unrelated", "exec cat")
    rows = [line.split(" ", 4) for line in run(helper, "list").splitlines()]
    row = next(row for row in rows if row[4] == name)
    assert len(rows) == 2 and row[2:4] == ["2", "0"], rows
    old = row[:2]
    run("tmux", "rename-session", "-t", old[0], "renamed")
    with attach(helper, old) as (fd, client):
        until(lambda: subprocess.run([helper, "proof", *old, client]).returncode == 0)
        assert run("tmux", "list-clients", "-F", "#{client_width}x#{client_height}") == "109x27"
        os.write(fd, b"W11_TMUX_SELECTED\r")
        until(lambda: "W11_TMUX_SELECTED" in run("tmux", "capture-pane", "-pt", old[0]))
        assert "W11_TMUX_SELECTED" not in run("tmux", "capture-pane", "-pt", "unrelated")
    assert len(run(helper, "list").splitlines()) == 2
    run("tmux", "kill-server")
    until(lambda: subprocess.run(["tmux", "-N", "list-sessions"], capture_output=True).returncode != 0)
    run("tmux", "-f", "/dev/null", "new-session", "-ds", "replacement")
    new = run(helper, "list").split(" ", 4)
    assert new[0] == old[0] and new[1] != old[1]
    with attach(helper, old) as (fd, _):
        # The stale attach command must finish of its own accord, even though a
        # different live session now has the same numeric ID.
        deadline = time.monotonic() + 5
        while True:
            assert select.select([fd], [], [], max(0, deadline - time.monotonic()))[0]
            try:
                if not os.read(fd, 4096):
                    break
            except OSError as e:
                if e.errno != 5:  # EIO marks a closed Linux PTY.
                    raise
                break
    assert run(helper, "list").split(" ", 4) == new and new[3] == "0"


if __name__ == "__main__":
    with tempfile.TemporaryDirectory() as temp:
        os.environ.update(TMUX_TMPDIR=temp, TERM="xterm-256color")
        os.environ.pop("TMUX", None)
        try:
            exercise(pathlib.Path(temp))
        finally:
            subprocess.run(["tmux", "-N", "kill-server"], capture_output=True)
