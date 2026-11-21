"""Check the Rust encoder's shared keypad vectors with kitty's own encoder."""
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


if __name__ == "__main__":
    check()
