"""Exercise the installed dotfile and tool PATH hooks in a real Fish process."""
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile


def run_fish(home, flags, command, home_variable=None):
    return subprocess.run(
        ["fish", *flags, "--command", command],
        env={**os.environ, "HOME": home_variable or str(home), "XDG_CONFIG_HOME": str(home / ".config"),
             "TERM": "xterm-256color", "LC_ALL": "C", "EXISTING": "",
             "MANAGED_ENV": "", "MANAGED_PROMPT": ""},
        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        text=True, check=True, timeout=5,
    )

def check(home, flags, expected):
    command = "printf '%s|%s|%s\\n' \"$EXISTING\" \"$MANAGED_ENV\" \"$MANAGED_PROMPT\""
    fish = run_fish(home, flags, command)
    assert fish.stdout == expected + "\n", (flags, fish.stdout, fish.stderr)


def check_tool_path(home, config, dotfiles_hook, template):
    bin_dir = ".local/bin"
    user_bin = home / "operator bin"
    managed_bin = home / bin_dir
    for directory, name in [(user_bin, "path-conflict"), (managed_bin, "path-conflict"),
                            (managed_bin, "managed-command")]:
        directory.mkdir(parents=True, exist_ok=True)
        command = directory / name
        command.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        command.chmod(0o700)
    marker = hashlib.sha256(bin_dir.encode()).hexdigest()
    path_block = template.replace("@BIN@", bin_dir).replace("@ID@", marker)
    base = f"set -gx PATH '{user_bin}' /usr/bin /bin\n" + dotfiles_hook
    config.write_text(base + path_block + path_block, encoding="utf-8")
    command = (
        'command -s path-conflict; command -s managed-command; '
        '/usr/bin/printenv PATH MANAGED_ENV'
    )
    expected = (
        f"{user_bin}/path-conflict\n{managed_bin}/managed-command\n"
        f"{user_bin}:/usr/bin:/bin:{managed_bin}\nloaded\n"
    )
    for flags in [[], ["--interactive"]]:
        fish = run_fish(home, flags, command, str(home) + "/")
        assert fish.stdout == expected, (flags, fish.stdout, fish.stderr)
    config.write_text(base, encoding="utf-8")
    fish = run_fish(home, [], 'command -sq managed-command; and exit 1; printf "%s\\n" "$MANAGED_ENV"')
    assert fish.stdout == "loaded\n", (fish.stdout, fish.stderr)

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
    selector.write_text(version + "\n", encoding="utf-8")
    check_tool_path(home, config, hook.read_text(encoding="utf-8"),
                    (hook.parent / "tool-path.fish").read_text(encoding="utf-8"))
