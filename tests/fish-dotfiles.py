"""Exercise the installed dotfile hook in a real Fish process."""
import os
from pathlib import Path
import subprocess
import tempfile


def check(home, flags, expected):
    fish = subprocess.run(
        ["fish", *flags, "--command",
         "printf '%s|%s|%s\\n' \"$EXISTING\" \"$MANAGED_ENV\" \"$MANAGED_PROMPT\""],
        env={**os.environ, "HOME": str(home), "XDG_CONFIG_HOME": str(home / ".config"),
             "TERM": "xterm-256color", "LC_ALL": "C", "EXISTING": "",
             "MANAGED_ENV": "", "MANAGED_PROMPT": ""},
        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        text=True, check=True, timeout=5,
    )
    assert fish.stdout == expected + "\n", (flags, fish.stdout, fish.stderr)


hook = Path(__file__).resolve().parents[1] / "src/sftp/dotfiles/startup.fish"
with tempfile.TemporaryDirectory(prefix="fish dotfiles ") as directory:
    home = Path(directory)
    config = home / ".config/fish/config.fish"
    config.parent.mkdir(parents=True)
    config.write_text("set -gx EXISTING keep\n" + hook.read_text(encoding="utf-8"), encoding="utf-8")
    store = home / ".config/w11term/dotfiles"
    selector = store / "fish-current"
    version = "a" * 64
    bundle = store / f"fish-{version}"
    bundle.mkdir(parents=True)
    (bundle / "environment.fish").write_text("set -gx MANAGED_ENV loaded\n", encoding="utf-8")
    (bundle / "prompt.fish").write_text("set -gx MANAGED_PROMPT loaded\n", encoding="utf-8")
    check(home, [], "keep||")
    selector.write_text(version + "\n", encoding="utf-8")
    check(home, [], "keep|loaded|")
    check(home, ["--interactive"], "keep|loaded|loaded")
    selector.write_text(version + "\nmanual\n", encoding="utf-8")
    check(home, ["--interactive"], "keep|loaded|loaded")
    for inactive in ["disabled", "automatic", version + "/../other", " " + version]:
        selector.write_text(inactive + "\n", encoding="utf-8")
        check(home, ["--interactive"], "keep||")
