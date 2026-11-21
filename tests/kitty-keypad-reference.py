"""Check shared keyboard vectors using kitty 0.32.2's encode_key_for_tty."""
import json
from pathlib import Path

from kitty import fast_data_types as kitty


def check():
    fixture = Path(__file__).parent / "fixtures" / "keyboard" / "keypad.json"
    cases = json.loads(fixture.read_text(encoding="utf-8"))
    for case in cases:
        wire = kitty.encode_key_for_tty(
            key=getattr(kitty, "GLFW_FKEY_" + case["key"]),
            key_encoding_flags=case["flags"],
            action=getattr(kitty, "GLFW_" + case["action"].upper()),
            mods=kitty.GLFW_MOD_NUM_LOCK if case.get("num_lock") else 0,
            text=case.get("text"),
        )
        if wire != case["wire"]:
            raise AssertionError(f"{case!r} produced {wire!r} on stock kitty")
    print(f"Matched {len(cases)} keypad events against stock kitty")

    fixture = fixture.with_name("physical-layout.json")
    cases = json.loads(fixture.read_text(encoding="utf-8"))
    for case in cases:
        mods = 0
        for mod in case["mods"]:
            mods |= getattr(kitty, "GLFW_MOD_" + ("CONTROL" if mod == "CTRL" else mod))
        wire = kitty.encode_key_for_tty(
            key=ord(case["logical"]),
            shifted_key=ord(case["shifted"]),
            alternate_key=ord(case["physical"]),
            mods=mods,
            key_encoding_flags=case["flags"],
            action=getattr(kitty, "GLFW_" + case["action"].upper()),
            text=case.get("text"),
        )
        if wire != case["wire"]:
            raise AssertionError(f"{case!r} produced {wire!r} on stock kitty")
    print(f"Matched {len(cases)} physical alternate-key events against stock kitty")


if __name__ == "__main__":
    check()
