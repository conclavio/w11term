"""Exercise both directory setups in real interactive shells."""

import argparse
import os
from pathlib import Path
import re
import subprocess
import tempfile
from urllib.parse import quote_from_bytes


SCRIPTS = Path(__file__).resolve().parents[1] / "src/app"
REPORT = re.compile(rb"\x1b\]7;([^\x07]*)\x07")


def run(command, commands, environment=None, *, interactive=True):
    result = subprocess.run(
        [*command, "-i"] if interactive else command,
        input=commands.encode("utf-8"),
        capture_output=True,
        env={**os.environ, **(environment or {}), "TERM": "dumb"},
        timeout=20,
        check=True,
    )
    return result.stdout, result.stderr


def check_prompt(command, setup, startup, hook_array, directory, expected_hooks):
    hook_items = "${" + hook_array + "[@]}"
    tail = rf"""cd -- "$W11TERM_TEST_SHELL_DIRECTORY" || exit 73
printf 'directory=<%s>\n' "$PWD"
false
printf 'previous=%s\n' "$?"
printf 'hook=<%s>\n' "{hook_items}"
PWD=$'/control\a\e\n'
PWD=relative
printf 'done\n'
exit 0
"""
    stdout, stderr = run(
        command,
        "\n".join((startup, setup, setup, tail)),
        {"W11TERM_TEST_SHELL_DIRECTORY": str(directory)},
    )
    reports = REPORT.findall(stdout)
    stdout = REPORT.sub(b"", stdout)
    location = re.search(rb"^directory=<(.*?)>$", stdout, re.MULTILINE)
    assert location and location[1].startswith(b"/"), stdout
    escaped = quote_from_bytes(location[1], safe="/").encode("ascii")
    assert b"file://" + escaped in reports, reports
    suffix = b"/tree%20%23100%25%20%5Bpilot%5D%20%27%20%E9%9B%AA"
    assert any(uri.endswith(suffix) for uri in reports), reports
    assert b"file:///control%07%1B%0A" in reports, reports
    assert b"file://" in reports, reports
    assert b"previous=1\n" in stdout and b"prompt:1" in stdout + stderr, (stdout, stderr)
    assert b"command not found" not in stderr, stderr
    hooks = re.findall(rb"^hook=<(.*?)>$", stdout, re.MULTILINE)
    assert hooks == expected_hooks, hooks
    assert stdout.count(b"w11term directory reporting is enabled") == 1, stdout
    return stdout


def check_bash(executable, directory):
    command = [executable, "--noprofile", "--norc"]
    setup = (SCRIPTS / "bash-directory.sh").read_text(encoding="utf-8")
    installed = b"__w11term_report_directory"

    def check(startup, hooks):
        return check_prompt(
            command, setup, "PS1='prompt:$? > '\n" + startup,
            "PROMPT_COMMAND", directory, [*hooks, installed],
        )

    check("unset PROMPT_COMMAND; set -u", [])
    scalar = b'printf "scalar-status=%s\\n" "$?"'
    stdout = check("PROMPT_COMMAND='" + scalar.decode() + "'", [scalar])
    assert b"scalar-status=1\n" in stdout, stdout

    first = b'printf "first-status=%s\\n" "$?"'
    last = b'printf "last-status=%s\\n" "$?"'
    stdout = check(
        "PROMPT_COMMAND=([2]='" + first.decode() + "' [9]='" + last.decode() + "')",
        [first, last],
    )
    assert b"first-status=1\nlast-status=1\n" in stdout, stdout

    stdout, stderr = run(
        command, "readonly PROMPT_COMMAND='printf \"original hook\\n\"'\n" + setup + "\nexit 0\n"
    )
    assert b"original hook\n" in stdout and not REPORT.search(stdout), stdout
    assert b"needs a writable Bash PROMPT_COMMAND" in stderr, stderr
    stdout, stderr = run(command, setup, interactive=False)
    assert not stdout and not stderr, (stdout, stderr)
    print("Bash directory setup preserves hooks and status and reports encoded OSC 7 paths.")


def check_zsh(executable, directory):
    command = [executable, "-f"]
    setup = (SCRIPTS / "zsh-directory.zsh").read_text(encoding="utf-8")
    installed = b"__w11term_report_zsh_directory"
    check_prompt(
        command, setup, "PS1='prompt:%? > '\nunset precmd_functions",
        "precmd_functions", directory, [installed],
    )
    startup = r"""PS1='prompt:%? > '
first() { builtin printf 'first-status=%s\n' "$?"; }
last() { builtin printf 'last-status=%s\n' "$?"; }
precmd_functions=(first last)
setopt KSH_ARRAYS
"""
    stdout = check_prompt(
        command, setup, startup,
        "precmd_functions", directory, [b"first", b"last", installed],
    )
    assert b"first-status=1\nlast-status=1\n" in stdout, stdout
    stdout, stderr = run(
        command,
        r"""original() { builtin printf 'original hook\n'; }
typeset -gra precmd_functions=(original)
""" + setup + "\nexit 0\n",
    )
    assert b"original hook\n" in stdout and not REPORT.search(stdout), stdout
    assert b"needs writable Zsh prompt hooks" in stderr, stderr
    stdout, stderr = run(command, setup, interactive=False)
    assert not stdout and not stderr, (stdout, stderr)
    print("Zsh directory setup preserves hooks and status and reports encoded OSC 7 paths.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--shell", choices=("all", "bash", "zsh"), default="all")
    parser.add_argument("--bash", default="bash")
    parser.add_argument("--zsh", default="zsh")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="w11term-shell-") as temp:
        directory = Path(temp) / "tree #100% [pilot] ' 雪"
        directory.mkdir()
        if args.shell in ("all", "bash"):
            check_bash(args.bash, directory)
        if args.shell in ("all", "zsh"):
            check_zsh(args.zsh, directory)


if __name__ == "__main__":
    main()
