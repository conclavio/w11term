import subprocess
import tempfile
import unittest
from unittest.mock import patch

from scripts.promote_windows_release import ARCHIVE, choose_release, newest_release


OLD = "a" * 40
NEW = "b" * 40


def build_tag(commit):
    return f"build-{commit[:12]}"


def uploaded(name):
    return {"name": name, "state": "uploaded", "size": 1}


def release(commit, **overrides):
    return {
        "tag_name": build_tag(commit),
        "draft": False,
        "prerelease": False,
        "assets": [uploaded(ARCHIVE), uploaded(f"{ARCHIVE}.sha256")],
        **overrides,
    }


class ReleaseOrdering(unittest.TestCase):
    def test_an_older_build_published_last_cannot_replace_a_newer_build(self):
        targets = {build_tag(OLD): OLD, build_tag(NEW): NEW}
        releases = [
            release(OLD, published_at="2026-11-26T12:00:00Z"),
            release(NEW, published_at="2026-11-26T11:00:00Z"),
        ]
        self.assertEqual(newest_release([NEW, OLD], targets, releases), build_tag(NEW))

    def test_latest_can_fall_back_to_the_last_complete_release(self):
        targets = {build_tag(OLD): OLD, build_tag(NEW): NEW}
        incomplete = [
            release(NEW, draft=True),
            release(NEW, prerelease=True),
            release(NEW, assets=[uploaded(ARCHIVE)]),
            release(NEW, assets=[uploaded(f"{ARCHIVE}.sha256"), {**uploaded(ARCHIVE), "size": 0}]),
            release(NEW, assets=[uploaded(ARCHIVE), {**uploaded(f"{ARCHIVE}.sha256"), "state": "starter"}]),
        ]
        for candidate in incomplete:
            with self.subTest(candidate=candidate):
                self.assertEqual(
                    newest_release([NEW, OLD], targets, [candidate, release(OLD)]), build_tag(OLD)
                )

    def test_an_unpublished_or_wrongly_targeted_tag_cannot_be_selected(self):
        self.assertIsNone(newest_release([NEW], {build_tag(NEW): OLD}, [release(NEW)]))
        self.assertIsNone(newest_release([NEW], {build_tag(NEW): NEW}, []))

    def test_reads_main_first_parent_order_and_uses_actual_tag_targets(self):
        with tempfile.TemporaryDirectory() as repo:
            def git(*args):
                return subprocess.check_output(
                    [
                        "git", "-c", "user.name=Release Tests", "-c", "user.email=tests@example.invalid",
                        "-c", "commit.gpgsign=false", "-c", "tag.gpgSign=false", *args,
                    ],
                    cwd=repo,
                    encoding="utf-8",
                ).strip()

            git("init", "-q", "-b", "main", "--template=")
            git("commit", "-q", "--allow-empty", "-m", "Older merge")
            older = git("rev-parse", "HEAD")
            git("tag", build_tag(older), older)
            git("switch", "-q", "-c", "topic")
            git("commit", "-q", "--allow-empty", "-m", "Topic commit")
            topic = git("rev-parse", "HEAD")
            git("tag", build_tag(topic), topic)
            git("switch", "-q", "main")
            git("merge", "-q", "--no-ff", "topic", "-m", "Merge topic")
            tip = git("rev-parse", "HEAD")
            git("tag", "-a", build_tag(tip), "-m", "Published build", tip)
            git("update-ref", "refs/remotes/origin/main", tip)
            # A publisher can be running from an older checked-out commit.
            git("switch", "-q", "--detach", older)

            def output(executable, *args):
                self.assertEqual(executable, "git")
                return subprocess.check_output(["git", *args], cwd=repo, encoding="utf-8")

            with patch("scripts.promote_windows_release.output", side_effect=output):
                self.assertEqual(
                    choose_release([release(topic), release(older), release(tip)]), build_tag(tip)
                )
                self.assertEqual(choose_release([release(topic), release(older)]), build_tag(older))
