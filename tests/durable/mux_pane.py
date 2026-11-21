"""Tiny interactive alternate-screen application run inside a real mux pane."""
import os
from pathlib import Path
import select
import tty


def draw(stage, size):
    rows, cols = size.lines, size.columns
    row, col = rows // 2, cols // 2
    screen = (f"\x1b[2J\x1b[2;3HW11{stage} {cols}x{rows}"
              f"\x1b[{row};{col - 9}HW11ANCHOR\x1b[{row};{col}H").encode()
    if os.environ["W11_MUX"] == "venti":
        screen += b'\x1bPq"1;1;8;6#1;2;100;0;0#1!8~\x1b\\'
    os.write(1, screen)


tty.setraw(0)
os.write(1, b"\x1b[?1049h")
report = Path(os.environ["W11_REPORT"])
last = None
stage = "READY"
keys = bytearray()
while True:
    size = os.get_terminal_size(0)
    current = (size, stage)
    if current != last:
        draw(stage, size)
        temporary = report.with_suffix(".tmp")
        temporary.write_text(f"{stage} {size.columns} {size.lines}\n")
        temporary.replace(report)
        last = current
    if select.select([0], [], [], 0.05)[0]:
        key = os.read(0, 1)
        if not key:
            break
        keys.extend(key)
        if keys.endswith(b"a") and stage == "READY":
            stage = "FIRST"
            keys.clear()
        elif keys.endswith(b"\x1b[C") and stage == "FIRST":
            stage = "MOVED"
            keys.clear()
