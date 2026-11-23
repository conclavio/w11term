"""Exercise the pasted directory setup in a real interactive Bash process."""

import argparse
import os
from pathlib import Path
import re
import subprocess
import tempfile
from urllib.parse import quote_from_bytes


SETUP = (Path(__file__).resolve().parents[1] / "src/app/bash-directory.sh").read_text(encoding="utf-8")
REPORT = re.compile(rb"\x1b\]7;([^\x07]*)\x07")


def run(bash, commands, environment=None, *, interactive=True):
    args = [bash, "--noprofile", "--norc"]
    if interactive:
        args.append("-i")
    result = subprocess.run(
        args,
        input=commands.encode("utf-8"),
        capture_output=True,
        env={**os.environ, **(environment or {})},
        timeout=20,
        check=True,
    )
    return result.stdout, result.stderr


def check_prompt(bash, existing_hook, directory, expected_hooks):
    stdout, stderr = run(
        bash,
        f"""PS1='prompt:$? > '
{existing_hook}
{SETUP}
{SETUP}
cd -- "$W11TERM_TEST_BASH_DIRECTORY" || exit 73
printf 'directory=<%s>\\n' "$PWD"
false
printf 'previous=%s\\n' "$?"
printf 'hook=<%s>\\n' "${{PROMPT_COMMAND[@]}}"
PWD=$'/control\\a\\e\\n'
PWD=relative
printf 'done\\n'
exit 0
""",
        {"W11TERM_TEST_BASH_DIRECTORY": str(directory)},
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
    assert b"previous=1\n" in stdout and b"prompt:1" in stderr, (stdout, stderr)
    hooks = re.findall(rb"^hook=<(.*?)>$", stdout, re.MULTILINE)
    assert hooks == expected_hooks + [b"__w11term_report_directory"], hooks
    assert stdout.count(b"w11term directory reporting is enabled") == 1, stdout
    return stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bash", default="bash")
    args = parser.parse_args()
    bash = args.bash
    with tempfile.TemporaryDirectory(prefix="w11term-bash-") as temp:
        directory = Path(temp) / "tree #100% [pilot] ' 雪"
        directory.mkdir()
        check_prompt(bash, "unset PROMPT_COMMAND; set -u", directory, [])
        scalar = b'printf "scalar-status=%s\\n" "$?"'
        stdout = check_prompt(
            bash, "PROMPT_COMMAND='" + scalar.decode() + "'", directory, [scalar]
        )
        assert b"scalar-status=1\n" in stdout, stdout

        first = b'printf "first-status=%s\\n" "$?"'
        last = b'printf "last-status=%s\\n" "$?"'
        stdout = check_prompt(
            bash,
            "PROMPT_COMMAND=([2]='" + first.decode() + "' [9]='" + last.decode() + "')",
            directory,
            [first, last],
        )
        assert b"first-status=1\nlast-status=1\n" in stdout, stdout

    stdout, stderr = run(
        bash, "readonly PROMPT_COMMAND='printf \"original hook\\n\"'\n" + SETUP + "\nexit 0\n"
    )
    assert b"original hook\n" in stdout and not REPORT.search(stdout), stdout
    assert b"needs a writable Bash PROMPT_COMMAND" in stderr, stderr
    stdout, stderr = run(bash, SETUP, interactive=False)
    assert not stdout and not stderr, (stdout, stderr)
    print("Bash directory setup preserves hooks and status and reports encoded OSC 7 paths.")


if __name__ == "__main__":
    main()
