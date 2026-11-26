"""Loopback SSH peer for testing server-driven authentication continuation."""

import base64
import hashlib
import socket
import sys
from pathlib import Path
from threading import Event

import paramiko
from paramiko import AUTH_FAILED, AUTH_PARTIALLY_SUCCESSFUL, AUTH_SUCCESSFUL


class Peer(paramiko.ServerInterface):
    def __init__(self, mode, expected):
        self.mode = mode
        self.expected = expected
        self.methods = "publickey"
        self.round = 0
        self.shell = Event()

    def get_allowed_auths(self, username):
        return self.methods

    def check_auth_publickey(self, username, key):
        blob = key.public_blob.key_blob if key.public_blob else key.asbytes()
        if self.mode == "rejected":
            self.methods = "password"
            return AUTH_FAILED
        if username != "pilot+character" or blob != self.expected:
            return AUTH_FAILED
        self.methods = {
            "password": "password",
            "interactive": "keyboard-interactive",
            "alternatives": "password,keyboard-interactive",
            "two-keys": "publickey",
        }[self.mode]
        return AUTH_PARTIALLY_SUCCESSFUL

    def check_auth_password(self, username, password):
        if (
            username == "pilot+character"
            and "password" in self.methods.split(",")
            and password == "fixture-password"
        ):
            return AUTH_SUCCESSFUL
        return AUTH_FAILED

    def check_auth_interactive(self, username, submethods):
        if username != "pilot+character" or "keyboard-interactive" not in self.methods.split(","):
            return AUTH_FAILED
        return paramiko.InteractiveQuery("Test MFA", "Enter your code", ("One-time code", False))

    def check_auth_interactive_response(self, responses):
        if self.mode == "alternatives":
            self.methods = "password"
            return AUTH_FAILED
        if self.round == 0 and responses == ["123456"]:
            self.round += 1
            return paramiko.InteractiveQuery("Test MFA", "", ("Device name", True))
        if self.round == 1 and responses == ["test-desktop"]:
            return AUTH_SUCCESSFUL
        return AUTH_FAILED

    def check_channel_request(self, kind, chanid):
        if kind == "session":
            return paramiko.OPEN_SUCCEEDED
        return paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED

    def check_channel_pty_request(self, channel, term, width, height, pixelwidth, pixelheight, modes):
        return True

    def check_channel_shell_request(self, channel):
        self.shell.set()
        return True


def serve(connection, host_key, peer):
    transport = paramiko.Transport(connection)
    try:
        transport.add_server_key(host_key)
        transport.start_server(server=peer)
        channel = transport.accept(15)
        if channel is not None and peer.shell.wait(15):
            channel.sendall(b"mfa-fixture-ready\r\n")
            channel.recv(4096)  # A test message or client disconnect closes the link.
    except (EOFError, OSError):
        pass
    finally:
        transport.close()


if __name__ == "__main__":
    mode, public, ready = sys.argv[1:]
    expected = base64.b64decode(Path(public).read_text().split()[1])
    host_key = paramiko.ECDSAKey.generate()
    pin = base64.b64encode(hashlib.sha256(host_key.asbytes()).digest()).decode().rstrip("=")
    with socket.socket() as server:
        server.bind(("127.0.0.1", 0))
        server.listen()
        ready_path = Path(ready)
        pending = ready_path.with_suffix(".tmp")
        pending.write_text(f"{server.getsockname()[1]} SHA256:{pin}", encoding="ascii")
        pending.replace(ready_path)
        while True:
            connection, _ = server.accept()
            with connection:
                serve(connection, host_key, Peer(mode, expected))
