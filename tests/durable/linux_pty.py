"""Terminal-facing Linux PTY used by durable-shell and remote mux fixtures."""
import errno
import fcntl
import os
import pty
import select
import struct
import subprocess
import termios
import time


TIMEOUT = 15


def eventually(check, explanation):
    deadline = time.monotonic() + TIMEOUT
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.03)
    raise AssertionError(explanation)


class Client:
    """An SSH PTY's terminal-facing side; closing the master simulates losing SSH."""

    def __init__(self, args, env, size, *, controlling_tty=False):
        self.fd, slave = pty.openpty()
        self.output = bytearray()
        try:
            self.resize(*size)
            self.process = subprocess.Popen(args, stdin=slave, stdout=slave, stderr=slave,
                                            start_new_session=True, env=env, close_fds=True,
                                            preexec_fn=self.acquire_tty if controlling_tty else None)
        except BaseException:
            os.close(self.fd)
            raise
        finally:
            os.close(slave)

    @staticmethod
    def acquire_tty():
        fcntl.ioctl(0, termios.TIOCSCTTY, 0)

    def resize(self, rows, cols, pixel_width=0, pixel_height=0):
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ,
                    struct.pack("HHHH", rows, cols, pixel_width, pixel_height))

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
