"""Capture the checked-in VT fixture from a real kitty under Xvfb on Linux."""
import fcntl
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import termios
import time

from PIL import Image

FIXTURE = Path(__file__).resolve().parent / "fixtures" / "kitty"


def display(ready):
    sys.stdout.buffer.write((FIXTURE / "placements.vt").read_bytes())
    sys.stdout.buffer.flush()
    rows, cols, width, height = struct.unpack(
        "HHHH", fcntl.ioctl(1, termios.TIOCGWINSZ, bytes(8))
    )
    ready.write_text(json.dumps([cols, rows, width, height]))
    time.sleep(35)


def compare(png, geometry):
    cols, rows, width, height = geometry
    with Image.open(png) as image:
        image = image.convert("RGB")
        # The isolated kitty process has no padding, tab bar, or decorations.
        cell_width = (width or image.width) / cols
        cell_height = (height or image.height) / rows
        for item in json.loads((FIXTURE / "samples.json").read_text()):
            x = int((item["column"] + 0.5) * cell_width)
            y = int((item["row"] + 0.5) * cell_height)
            actual = image.getpixel((x, y))
            if any(abs(a - b) >= 32 for a, b in zip(actual, item["rgb"])):
                raise ValueError(f"{item}: pixel ({x},{y}) was {actual}")


def capture(output):
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as state:
        ready = Path(state) / "geometry"
        cmd = ["kitty", "--config", "NONE", "--title", "w11term-graphics-reference"]
        for option in ("linux_display_server=x11", "remember_window_size=no",
                       "initial_window_width=40c", "initial_window_height=12c",
                       "window_padding_width=0", "hide_window_decorations=yes",
                       "background=#000000", "cursor_blink_interval=0"):
            cmd.extend(["--override", option])
        cmd.extend([sys.executable, str(Path(__file__).resolve()), "--display", str(ready)])
        env = {**os.environ, "LIBGL_ALWAYS_SOFTWARE": "1", "KITTY_CONFIG_DIRECTORY": state}
        proc = subprocess.Popen(cmd, env=env)
        try:
            deadline = time.monotonic() + 20
            while not ready.exists() and time.monotonic() < deadline:
                if proc.poll() is not None:
                    raise RuntimeError(f"kitty exited with {proc.returncode}")
                time.sleep(0.05)
            geometry = json.loads(ready.read_text())
            (output / "geometry.json").write_text(json.dumps(geometry))
            wid = subprocess.check_output(
                ["xdotool", "search", "--onlyvisible", "--name", "^w11term-graphics-reference$"],
                text=True, timeout=4
            ).splitlines()[-1]
            png = output / "kitty.png"
            while True:
                subprocess.run(["import", "-window", wid, str(png)], check=True, timeout=5)
                try:
                    compare(png, geometry)
                    break
                except ValueError:
                    if time.monotonic() >= deadline:
                        raise
                    time.sleep(0.15)
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()


if __name__ == "__main__":
    if sys.argv[1] == "--display":
        display(Path(sys.argv[2]))
    else:
        capture(Path(sys.argv[1]))
