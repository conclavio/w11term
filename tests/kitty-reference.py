"""Capture the shared VT fixture from kitty or Ghostty under Xvfb on Linux."""
import argparse
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import time

from PIL import Image

FIXTURE = Path(__file__).resolve().parent / "fixtures" / "kitty"
SCENES = {
    "placements": (["placements.vt", "layers.vt"], "samples.json"),
    "clear-visible": (["clear-setup.vt", "clear-visible.vt"], "clear-visible-samples.json"),
    "clear-cell": (["clear-cell.vt"], "clear-cell-samples.json"),
    "clear-groups": (["clear-groups.vt"], "clear-groups-samples.json"),
    "clear-number": (["clear-number.vt"], "clear-number-samples.json"),
    "clear-frame": (["clear-frame.vt"], "clear-frame-samples.json"),
    "independent-placements": (["independent-placements.vt"], "independent-placements-samples.json"),
}


def display(ready, scene):
    import fcntl
    import termios

    for stream in SCENES[scene][0]:
        sys.stdout.buffer.write((FIXTURE / stream).read_bytes())
    sys.stdout.buffer.flush()
    # GTK can resize the PTY after launching this command. Publish measured
    # geometry atomically so a capture never reads a partial or stale startup grid.
    previous = None
    deadline = time.monotonic() + 50
    while time.monotonic() < deadline:
        rows, cols, width, height = struct.unpack(
            "HHHH", fcntl.ioctl(1, termios.TIOCGWINSZ, bytes(8))
        )
        geometry = [cols, rows, width, height]
        if cols and rows and geometry != previous:
            pending = ready.with_suffix(".tmp")
            pending.write_text(json.dumps(geometry))
            pending.replace(ready)
            previous = geometry
        time.sleep(0.05)


def compare(png, geometry, report, scene):
    cols, rows, width, height = geometry
    with Image.open(png) as image:
        image = image.convert("RGB")
        # The Linux terminals use zero padding and no client decorations.
        cell_width = (width or image.width) / cols
        cell_height = (height or image.height) / rows
        measured = []
        for item in json.loads((FIXTURE / SCENES[scene][1]).read_text()):
            x = int((item["column"] + 0.5) * cell_width)
            y = int((item["row"] + 0.5) * cell_height)
            actual = image.getpixel((x, y)) if x < image.width and y < image.height else None
            passed = actual is not None and all(abs(a - b) < 32 for a, b in zip(actual, item["rgb"]))
            measured.append({**item, "pixel": [x, y], "actual": actual, "passed": passed})
        report.write_text(json.dumps({"image_size": image.size, "samples": measured}, indent=2))
        failures = [sample for sample in measured if not sample["passed"]]
        if failures:
            raise ValueError(f"Cell color mismatch in {png}: {failures}")


def launch_command(terminal, title):
    if terminal == "kitty":
        cmd = ["kitty", "--config", "NONE", "--title", title]
        for option in ("linux_display_server=x11", "remember_window_size=no",
                       "initial_window_width=40c", "initial_window_height=12c",
                       "window_padding_width=0", "hide_window_decorations=yes",
                       "background=#000000", "cursor_blink_interval=0"):
            cmd.extend(["--override", option])
        return cmd
    return [
        "ghostty", "--config-default-files=false", "--gtk-single-instance=false",
        f"--title={title}", "--window-decoration=none", "--window-show-tab-bar=never",
        "--window-width=40", "--window-height=12", "--window-padding-x=0",
        "--window-padding-y=0", "--window-padding-balance=false", "--background=000000",
        # At this Xvfb DPI, DejaVu Sans Mono normally yields 10px cells.
        # A 9px cell has one central pixel, like the kitty reference grid.
        "--font-family=DejaVu Sans Mono", "--adjust-cell-width=-1", "--shell-integration=none", "-e",
    ]


def capture(output, terminal, scene):
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as state, (output / f"{terminal}.log").open("w") as log:
        ready = Path(state) / "geometry"
        title = f"w11term-{terminal}-reference-{os.getpid()}"
        cmd = launch_command(terminal, title)
        cmd.extend([sys.executable, str(Path(__file__).resolve()), "--display", str(ready), scene])
        env = {**os.environ, "LIBGL_ALWAYS_SOFTWARE": "1"}
        if terminal == "kitty":
            env["KITTY_CONFIG_DIRECTORY"] = state
        else:
            env.update(GDK_BACKEND="x11", GSK_RENDERER="gl", GDK_SCALE="1",
                       XDG_CONFIG_HOME=state, XDG_STATE_HOME=state)
        proc = subprocess.Popen(cmd, env=env, stdout=log, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic() + 30
            while not ready.exists() and time.monotonic() < deadline:
                if proc.poll() is not None:
                    raise RuntimeError(f"{terminal} exited with {proc.returncode}. See {log.name}")
                time.sleep(0.05)
            if not ready.exists():
                raise RuntimeError(f"Timed out waiting for the PTY in {terminal}. See {log.name}")
            wid = subprocess.check_output(
                ["xdotool", "search", "--sync", "--onlyvisible", "--name", f"^{title}$"],
                text=True, timeout=6
            ).splitlines()[-1]
            png = output / f"{terminal}.png"
            while True:
                geometry = json.loads(ready.read_text())
                (output / "geometry.json").write_text(json.dumps(geometry))
                subprocess.run(["import", "-window", wid, str(png)], check=True, timeout=5)
                try:
                    compare(png, geometry, output / "samples.json", scene)
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
    if sys.argv[1:2] == ["--display"]:
        display(Path(sys.argv[2]), sys.argv[3])
    else:
        parser = argparse.ArgumentParser(description=__doc__)
        parser.add_argument("output", type=Path)
        parser.add_argument("--terminal", choices=("kitty", "ghostty"), default="kitty")
        parser.add_argument("--scene", choices=SCENES, default="placements")
        args = parser.parse_args()
        capture(args.output, args.terminal, args.scene)
