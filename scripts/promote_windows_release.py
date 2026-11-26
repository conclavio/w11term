"""Select Latest from verified releases in main's first-parent order."""

import json
import os
import subprocess


ARCHIVE = "w11term-windows-x64.zip"
REQUIRED_ASSETS = {ARCHIVE, f"{ARCHIVE}.sha256"}


def newest_release(history, tag_targets, releases):
    complete = set()
    for release in releases:
        if release["draft"] or release["prerelease"]:
            continue
        assets = {
            asset["name"]
            for asset in release["assets"]
            if asset["state"] == "uploaded" and asset["size"] > 0
        }
        if REQUIRED_ASSETS <= assets:
            complete.add(release["tag_name"])

    for commit in history:
        tag = f"build-{commit[:12]}"
        if tag in complete and tag_targets.get(tag) == commit:
            return tag
    return None


def output(*command):
    return subprocess.check_output(command, encoding="utf-8")


def choose_release(releases):
    refs = output(
        "git",
        "for-each-ref",
        "--format=%(refname:strip=2)%09%(objectname)%09%(*objectname)",
        "refs/tags/build-*",
    )
    targets = {}
    for line in refs.splitlines():
        tag, target, peeled = line.split("\t")
        targets[tag] = peeled or target
    history = output("git", "rev-list", "--first-parent", "refs/remotes/origin/main")
    return newest_release(history.splitlines(), targets, releases)


def main():
    endpoint = f"repos/{os.environ['GH_REPO']}/releases?per_page=100"
    pages = json.loads(output("gh", "api", "--paginate", "--slurp", endpoint))
    latest = choose_release(release for page in pages for release in page)
    if latest is None:
        raise SystemExit("No complete Windows build release found on main")
    subprocess.run(["gh", "release", "edit", latest, "--latest"], check=True)


if __name__ == "__main__":
    main()
