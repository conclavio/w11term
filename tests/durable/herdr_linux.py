"""Exercise the production Herdr viewer against stock hc and a loopback-only sshd."""
import argparse
import contextlib
import getpass
import os
import pathlib
import select
import shlex
import socket
import subprocess
import sys
import tempfile
import time
import tty


def tui():
    # Nothing writes the input history to disk. The second viewer must reach
    # this process to append to the original bytes in raw, alternate-screen mode.
    tty.setraw(0)
    os.write(1, b"\x1b[?1049h")
    history = bytearray()
    previous = None
    while True:
        size = os.get_terminal_size(0)
        state = (size, bytes(history))
        if state != previous:
            title = "w11-renamed-title" if history else "w11-original-title"
            screen = (f"\x1b]2;{title}\x07\x1b[H\x1b[2JW11_INPUT={history.hex()}"
                      f"\r\nW11_GRID={size.columns}x{size.lines}")
            os.write(1, screen.encode())
            previous = state
        if select.select([0], [], [], 0.05)[0]:
            chunk = os.read(0, 4096)
            if not chunk:
                return
            history.extend(chunk)


def run(*args, **kwargs):
    return subprocess.run(args, check=True, timeout=12, **kwargs)


@contextlib.contextmanager
def ssh_server(work, hc, env):
    # Use separate host and user keys. The system sshd and ~/.ssh are untouched.
    for name in ("host", "client"):
        run("ssh-keygen", "-q", "-N", "", "-t", "ed25519", "-f", work / name)
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    exported = ["HC_SESSION", "XDG_RUNTIME_DIR", "XDG_STATE_HOME", "XDG_CONFIG_HOME"]
    bridge = work / "bridge"
    bridge.write_text("#!/bin/sh\nset -eu\n" +
                      '[ "$SSH_ORIGINAL_COMMAND" = "hc connect" ] || exit 64\n' +
                      "".join(f"export {key}={shlex.quote(env[key])}\n" for key in exported) +
                      f"exec {shlex.quote(str(hc))} connect\n")
    bridge.chmod(0o700)
    server_conf = work / "sshd.conf"
    server_conf.write_text(f"""ListenAddress 127.0.0.1
Port {port}
HostKey {work}/host
PidFile {work}/sshd.pid
AuthorizedKeysFile {work}/client.pub
StrictModes no
AllowUsers {getpass.getuser()}
PermitRootLogin no
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM yes
DisableForwarding yes
PermitTTY no
ForceCommand {bridge}
""")
    key = (work / "host.pub").read_text().split()
    (work / "known_hosts").write_text(f"[127.0.0.1]:{port} {key[0]} {key[1]}\n")
    config = work / "client.conf"
    config.write_text(f"""Host fixture
    HostName 127.0.0.1
    Port {port}
    User {getpass.getuser()}
    IdentityFile {work}/client
    IdentitiesOnly yes
    UserKnownHostsFile {work}/known_hosts
    GlobalKnownHostsFile /dev/null
    StrictHostKeyChecking yes
    BatchMode yes
    ConnectTimeout 4
    ControlMaster no
""")
    # /run/sshd is OpenSSH's standard privilege-separation directory.
    run("sudo", "-n", "install", "-d", "-m", "0755", "/run/sshd")
    run("sudo", "-n", "/usr/sbin/sshd", "-t", "-f", server_conf)
    log = work / "sshd.log"
    run("sudo", "-n", "/usr/sbin/sshd", "-f", server_conf, "-E", log)
    try:
        deadline = time.monotonic() + 5
        while not (work / "sshd.pid").is_file():
            if time.monotonic() >= deadline:
                raise RuntimeError("The fixture sshd did not start")
            time.sleep(0.05)
        yield config
    except BaseException:
        if log.exists():
            subprocess.run(["sudo", "-n", "tail", "-n", "35", log], timeout=5)
        raise
    finally:
        pidfile = work / "sshd.pid"
        if pidfile.exists():
            run("sudo", "-n", "kill", "-TERM", str(int(pidfile.read_text())))


def exercise(hc, probe):
    hc, probe = hc.resolve(strict=True), probe.resolve(strict=True)
    if os.geteuid() == 0:
        raise RuntimeError("Run the fixture as a regular user with passwordless sudo for its loopback sshd")
    with tempfile.TemporaryDirectory(prefix="w11-hc-") as scratch:
        work = pathlib.Path(scratch)
        env = {k: v for k, v in os.environ.items() if not k.startswith("HC_")}
        env.update(HC_SESSION="w11test", XDG_RUNTIME_DIR=str(work / "r"),
                   XDG_CONFIG_HOME=str(work / "c"), XDG_STATE_HOME=str(work / "s"))
        for key in ("XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_STATE_HOME"):
            pathlib.Path(env[key]).mkdir(mode=0o700)
        version = run(hc, "--version", capture_output=True, text=True).stdout.strip()
        if version != "hc 0.1.21":
            raise RuntimeError(f"Expected the pinned hc 0.1.21, received {version!r}")
        print(f"Running {version} with isolated XDG state", flush=True)
        try:
            run(hc, "run", "--bg", "--name", "fixture", sys.executable,
                pathlib.Path(__file__).resolve(), "--tui", env=env)
            with ssh_server(work, hc, env) as config:
                subprocess.run([probe, config], check=True, timeout=75)
        finally:
            subprocess.run([hc, "kill", "fixture"], env=env, timeout=10, capture_output=True)
            subprocess.run([hc, "server", "stop"], env=env, timeout=10, capture_output=True)


if __name__ == "__main__":
    if sys.argv[1:] == ["--tui"]:
        tui()
    else:
        parser = argparse.ArgumentParser(description=__doc__)
        parser.add_argument("hc", type=pathlib.Path)
        parser.add_argument("probe", type=pathlib.Path)
        args = parser.parse_args()
        exercise(args.hc, args.probe)
