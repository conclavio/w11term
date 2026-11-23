"""Exercise the directory setups in real interactive shells."""

import argparse
import os
from pathlib import Path
import re
import subprocess
import tempfile
from urllib.parse import quote_from_bytes, unquote_to_bytes


SCRIPTS = Path(__file__).resolve().parents[1] / "src/app"
REPORT = re.compile(rb"\x1b\]7;([^\x07]*)\x07")
MARK = re.compile(rb"\x1b\]133;([^\x07]*)\x07")


def run(command, commands, environment=None, *, interactive=True, cwd=None, merge_output=False):
    result = subprocess.run(
        [*command, "-i"] if interactive else command,
        input=commands.encode("utf-8"),
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT if merge_output else subprocess.PIPE,
        env={**os.environ, "TERM": "dumb", **(environment or {})},
        cwd=cwd,
        timeout=20,
    )
    assert result.returncode == 0, (result.returncode, result.stdout, result.stderr)
    return result.stdout, result.stderr


def check_directory_report(reports, output):
    location = re.search(rb"directory=<(.*?)>\r?\n", output)
    assert location and location[1].startswith(b"/"), output
    expected = b"file://" + quote_from_bytes(location[1], safe="/").encode("ascii")
    assert expected in reports, reports
    assert expected.endswith(b"/tree%20%23100%25%20%5Bpilot%5D%20%27%20%E9%9B%AA"), expected
    return expected


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
    check_directory_report(reports, stdout)
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


def check_bash_integration(executable, directory):
    command = [executable, "--noprofile", "--norc"]
    setup = "\n".join((SCRIPTS / script).read_text(encoding="utf-8") for script in (
        "bash-directory.sh", "bash-integration.sh",
    ))
    startup = r"""set -u
PS1='fixture:$? > '
PS0='existing-ps0:'
fixture_prompt() {
    builtin printf 'existing-prompt=%s pipe=%s\n' "$1" "$2"
    PS1=$fixture_prompt_text
    PS0='existing-ps0:'
}
fixture_prompt_text=$PS1
trap ':' DEBUG
"""
    tail = r"""builtin printf 'recording-started\n'
cd -- "$W11TERM_TEST_SHELL_DIRECTORY" || exit 73
builtin printf 'directory=<%s>\n' "$PWD"
false | true
true | false
builtin printf 'previous=%s\n' "$?"

fixture_prompt_text='changed:$? > '
set +o history
(exit 23)
trap -p DEBUG
builtin printf 'hooks=<%s><%s>\n' "${PROMPT_COMMAND[2]-}" "${PROMPT_COMMAND[9]-}"
exit 0
"""
    existing = 'fixture_prompt "$?" "${PIPESTATUS[*]}"'
    for prompt_hooks in (
        "PROMPT_COMMAND='" + existing + "'",
        "PROMPT_COMMAND=([2]='" + existing + "' [9]='builtin printf \"second-hook=%s\\n\" \"$?\"')",
    ):
        output, _ = run(
            command,
            "\n".join((startup, prompt_hooks, setup, setup, tail)),
            {"W11TERM_TEST_SHELL_DIRECTORY": str(directory)},
            merge_output=True,
        )
        assert b"command not found" not in output and b"unbound variable" not in output, output
        assert output.count(b"w11term command recording is enabled") == 1, output
        assert output.count(b"w11term directory reporting is enabled") == 1, output
        cycle = output.split(b"recording-started\n", 1)[1]
        check_directory_report(REPORT.findall(cycle), cycle)
        assert b"\x1b]133;A\x07fixture:1 > \x1b]133;B\x07" in cycle, cycle
        assert b"\x1b]133;A\x07changed:23 > \x1b]133;B\x07" in cycle, cycle
        assert b"\x1b]133;D;0\x07existing-prompt=0 pipe=1 0\n" in cycle, cycle
        assert b"\x1b]133;D;1\x07existing-prompt=1 pipe=0 1\n" in cycle, cycle
        assert b"previous=1\n" in cycle and b"trap -- ':' DEBUG\n" in cycle, cycle
        if "[9]" in prompt_hooks:
            assert b"second-hook=1\n" in cycle, cycle
            assert b"hooks=<" + existing.encode() + b"><builtin printf" in cycle, cycle
        marks = [mark for mark in MARK.findall(cycle) if mark.startswith((b"C", b"D"))]
        assert marks == [
            b"D;0",
            b"C", b"D;0",  # cd
            b"C", b"D;0",  # print the new directory
            b"C", b"D;0",  # false | true
            b"C", b"D;1",  # true | false
            b"C", b"D;0",  # print the previous status
            b"C", b"D;0",  # change the prompt
            b"C", b"D;0",  # disable shell history
            b"C", b"D;23",
            b"C", b"D;0",  # inspect the existing trap
            b"C", b"D;0",  # inspect hook indices
            b"C",          # exit doesn't return to a prompt
        ], marks
        assert cycle.count(b"existing-ps0:\x1b]133;C\x07") == marks.count(b"C"), cycle

    for startup, diagnostic in [
        ("shopt -u promptvars", b"requires the promptvars option"),
        ("readonly PS0='existing'", b"needs writable prompts and PROMPT_COMMAND"),
        ("readonly PROMPT_COMMAND=':'", b"needs writable prompts and PROMPT_COMMAND"),
        (r"PS1='\[\e]633;A\a\]other> '", b"already supplies semantic markers"),
    ]:
        stdout, stderr = run(command, "\n".join((startup, setup, "exit 0\n")))
        assert diagnostic in stderr and not MARK.search(stdout + stderr), (stdout, stderr)
    stdout, stderr = run(command, setup, interactive=False)
    assert not stdout and not stderr, (stdout, stderr)
    output, _ = run(
        command,
        "\n".join(("PS0='existing:'; PROMPT_COMMAND='PS0+=.'", setup, r"""shopt -u promptvars
builtin printf 'without-promptvars\n'
shopt -s promptvars
builtin printf 'with-promptvars\n'
exit 0
""")),
        merge_output=True,
    )
    assert b"\x1b]133;C\x07without-promptvars" not in output, output
    assert b"\x1b]133;C\x07with-promptvars\n\x1b]133;D;0\x07" in output, output
    assert MARK.findall(output).count(b"C") == 3, output
    print("Bash command marks preserve PS0, existing hooks and pipeline status.")


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


def check_zsh_integration(executable, directory):
    command = [executable, "-f"]
    setup = "\n".join((SCRIPTS / script).read_text(encoding="utf-8") for script in (
        "zsh-directory.zsh", "zsh-integration.zsh",
    ))
    startup = r"""PS1='fixture:%? > '
PROMPT_EOL_MARK=''
fixture_precmd() { builtin printf 'existing-prompt=%s\n' "$?"; }
fixture_preexec() { builtin printf 'existing-preexec\n'; }
precmd_functions=(fixture_precmd)
preexec_functions=(fixture_preexec)
setopt KSH_ARRAYS
"""
    tail = r"""builtin printf 'recording-started\n'
cd -- "$W11TERM_TEST_SHELL_DIRECTORY" || exit 73
builtin printf 'directory=<%s>\n' "$PWD"
(exit 23)
builtin printf 'previous=%s\n' "$?"

PS1='changed:%? > '
builtin printf 'raw-prompt=<%s>\n' "$PS1"
builtin printf '%s\n' '雪+;%'
(
    builtin :
)
"""
    tail += "builtin : " + "x" * 4096 + "\nexit 0\n"
    output, _ = run(
        command,
        "\n".join((startup, setup, setup, tail)),
        {"W11TERM_TEST_SHELL_DIRECTORY": str(directory)},
        merge_output=True,
    )
    assert b"command not found" not in output, output
    assert output.count(b"w11term command recording is enabled") == 1, output
    assert output.count(b"w11term directory reporting is enabled") == 1, output
    cycle = output.split(b"recording-started\n", 1)[1]
    check_directory_report(REPORT.findall(cycle), cycle)
    assert b"\x1b]133;A\x07fixture:23 > \x1b]133;B\x07" in cycle, cycle
    assert b"\x1b]133;A\x07changed:0 > \x1b]133;B\x07" in cycle, cycle
    assert b"raw-prompt=<changed:%? > >\n" in cycle, cycle
    assert b"previous=23\n" in cycle and b"existing-prompt=23\n" in cycle, cycle
    assert b"existing-preexec\n\x1b]133;C;cmdline_url=" in cycle, cycle
    assert b"\x1b]133;D;23\x07existing-prompt=23\n" in cycle, cycle
    marks = [mark for mark in MARK.findall(cycle) if mark.startswith((b"C", b"D"))]
    assert [b"C" if mark.startswith(b"C") else mark for mark in marks] == [
        b"D;0",
        b"C", b"D;0",  # cd
        b"C", b"D;0",  # print the new directory
        b"C", b"D;23",
        b"C", b"D;0",  # print the previous status
        b"C", b"D;0",  # replace PS1
        b"C", b"D;0",  # inspect PS1
        b"C", b"D;0",  # print Unicode and punctuation
        b"C", b"D;0",  # multiline input
        b"C", b"D;0",  # oversized input uses a plain start mark
        b"C",          # exit doesn't return to a prompt
    ], marks
    original_lines = [
        unquote_to_bytes(mark.removeprefix(b"C;cmdline_url="))
        for mark in marks if mark.startswith(b"C;cmdline_url=")
    ]
    assert br"builtin printf '%s\n' '" + "雪+;%".encode() + b"'" in original_lines, original_lines
    assert b"(\n    builtin :\n)" in original_lines, original_lines
    assert marks.count(b"C") == 1, marks

    for startup, diagnostic in [
        ("unsetopt PROMPT_PERCENT", b"requires the PROMPT_PERCENT option"),
        ("typeset -gra preexec_functions=()", b"needs a writable prompt and prompt hooks"),
        (r"PS1=$'%{\033]633;A\007%}other> '", b"already supplies semantic markers"),
    ]:
        stdout, stderr = run(command, "\n".join((startup, setup, "exit 0\n")))
        assert diagnostic in stderr and not MARK.search(stdout + stderr), (stdout, stderr)
    stdout, stderr = run(command, setup, interactive=False)
    assert not stdout and not stderr, (stdout, stderr)
    print("Zsh shell integration pairs command starts and exits and preserves prompt hooks.")


def check_fish(executable, directory):
    setup = (SCRIPTS / "fish-directory.fish").read_text(encoding="utf-8")
    # A forced interactive shell handles explicit prompt events even with
    # piped stdio. Isolate Fish's universal variables, cache and config.
    config = directory.parent / "fish-state"
    config.mkdir()
    shell_root, stderr = run(
        [executable, "--no-config", "--command", "builtin pwd -P"],
        "", interactive=False, cwd=directory.parent,
    )
    assert not stderr, stderr
    shell_config = shell_root.decode("utf-8").rstrip("\r\n") + "/fish-state"
    environment = {
        "XDG_CONFIG_HOME": shell_config,
        "XDG_DATA_HOME": shell_config,
        "XDG_CACHE_HOME": shell_config,
        "FISH_UNIT_TESTS_RUNNING": "1",
        "TERM": "xterm-256color",
        "W11TERM_TEST_SHELL_DIRECTORY": "./" + directory.name,
    }
    startup = r"""set -g fish_greeting
function fixture_before --on-event fish_prompt
    builtin printf 'before-status=%s\n' "$status"
end
"""
    tail = r"""function fixture_after --on-event fish_prompt
    builtin printf 'after-status=%s\n' "$status"
end
builtin cd -- "$W11TERM_TEST_SHELL_DIRECTORY"; or exit 73
builtin printf 'directory=<%s>\n' "$PWD"
builtin printf 'report-start\n'
false; emit fish_prompt
builtin printf 'report-end\n'
"""
    # Exercise both a current prompt and setup sourced before Fish's first prompt.
    for initial_prompt in ("emit fish_prompt", ""):
        script = "\n".join((startup, initial_prompt, setup, setup, tail))
        stdout, stderr = run(
            [executable, "--interactive", "--command", script],
            "", environment, interactive=False, cwd=directory.parent,
        )
        assert not stderr, stderr
        assert stdout.count(b"w11term directory reporting is enabled") == 1, stdout
        segment = stdout.split(b"report-start\n", 1)[1].split(b"report-end\n", 1)[0]
        reports = REPORT.findall(segment)
        expected = check_directory_report(reports, stdout)
        assert reports.count(expected) == 1 and reports[-1] == expected, reports
        assert b"before-status=1\n" in segment and b"after-status=1\n" in segment, segment

    if os.name != "nt":
        control_directory = directory.parent / "control\x07\x1b\n"
        control_directory.mkdir()
        environment["W11TERM_TEST_SHELL_DIRECTORY"] = "./" + control_directory.name
        command = 'builtin cd -- "$W11TERM_TEST_SHELL_DIRECTORY"; or exit 73; emit fish_prompt'
        stdout, stderr = run(
            [executable, "--no-config", "--interactive", "--command", setup + "\n" + command],
            "", environment, interactive=False, cwd=directory.parent,
        )
        assert not stderr, stderr
        assert REPORT.findall(stdout)[-1].endswith(b"/control%07%1B%0A"), stdout

    stdout, stderr = run(
        [executable, "--no-config"], setup, environment, interactive=False, cwd=directory.parent,
    )
    assert not stdout and not stderr, (stdout, stderr)
    print("Fish directory setup preserves other prompt events and emits one usable OSC 7 report.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--shell", choices=("all", "bash", "zsh", "fish"), default="all")
    parser.add_argument("--bash", default="bash")
    parser.add_argument("--zsh", default="zsh")
    parser.add_argument("--fish", default="fish")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="w11term-shell-") as temp:
        directory = Path(temp) / "tree #100% [pilot] ' 雪"
        directory.mkdir()
        if args.shell in ("all", "bash"):
            check_bash(args.bash, directory)
            check_bash_integration(args.bash, directory)
        if args.shell in ("all", "zsh"):
            check_zsh(args.zsh, directory)
            check_zsh_integration(args.zsh, directory)
        if args.shell in ("all", "fish"):
            check_fish(args.fish, directory)


if __name__ == "__main__":
    main()
