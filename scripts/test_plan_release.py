"""Exercise release planning and notes with real Git/git-cliff processes."""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
READ_BREAK = (
    "fix(read)!: distinguish unknown target ends from scan frontiers\n\n"
    "BREAKING CHANGE: ReadResponse.target_end_line is now Option<usize>, "
    "serialized as null for an open-ended bounded target whose end is unknown. "
    "observed_end_line reports the scan frontier separately."
)


class ReleasePlanningTests(unittest.TestCase):
    def setUp(self):
        if not shutil.which("git-cliff"):
            self.fail("git-cliff is required; install the pinned release-workflow version")
        temporary = tempfile.TemporaryDirectory(prefix="leantoken-release-plan-")
        self.addCleanup(temporary.cleanup)
        self.repo = Path(temporary.name)
        # Fixture configuration and all subprocess state stay outside the host
        # home. The production tool's offline flag also forbids remote lookups.
        self.env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        self.env.update(
            HOME=str(self.repo),
            USERPROFILE=str(self.repo),
            GIT_CONFIG_NOSYSTEM="1",
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_AUTHOR_NAME="Release fixture",
            GIT_AUTHOR_EMAIL="fixture@example.invalid",
            GIT_COMMITTER_NAME="Release fixture",
            GIT_COMMITTER_EMAIL="fixture@example.invalid",
        )
        self.git("init", "--quiet")
        shutil.copyfile(ROOT / "cliff.toml", self.repo / "cliff.toml")
        self.seed("0.1.28")

    def run_command(self, *args):
        return subprocess.run(
            args, cwd=self.repo, env=self.env, check=True,
            capture_output=True, text=True, timeout=30,
        ).stdout

    def git(self, *args):
        return self.run_command("git", *args)

    def seed(self, version):
        self.manifest(version)
        self.git("add", ".")
        self.git("commit", "--quiet", "-m", "chore: release fixture baseline")
        self.git("tag", "-a", f"v{version}", "-m", f"v{version}")

    def manifest(self, version):
        (self.repo / "Cargo.toml").write_text(
            f'[package]\nname = "leantoken"\nversion = "{version}"\n',
            encoding="utf-8",
        )

    def commit(self, message):
        self.git("commit", "--quiet", "--allow-empty", "-m", message)

    def plan(self):
        outputs = self.run_command(sys.executable, str(ROOT / "scripts/plan_release.py"))
        return dict(line.split("=", 1) for line in outputs.splitlines())

    def notes(self, version, previous="0.1.28"):
        return self.run_command(
            "git-cliff", "--offline", "--config", "cliff.toml",
            "--tag", f"v{version}", f"v{previous}..HEAD",
        )

    def test_compatible_zero_major_features_stay_in_the_minor_series(self):
        self.commit("feat(cache): compact inactive indexes")
        self.commit("fix(cache): retain active indexes")
        plan = self.plan()
        self.assertEqual(plan["new_version"], "0.1.29")
        self.assertEqual(plan["new_tag"], "v0.1.29")
        self.assertEqual(plan["skip"], "false")

    def test_real_read_break_advances_minor_and_explains_client_migration(self):
        self.commit(READ_BREAK)
        plan = self.plan()
        self.assertEqual(plan["new_version"], "0.2.0")
        notes = self.notes(plan["new_version"])
        self.assertIn("## [0.2.0]", notes)
        self.assertIn("**BREAKING**", notes)
        self.assertIn("ReadResponse.target_end_line is now Option<usize>", notes)
        self.assertIn("serialized as null", notes)
        self.assertIn("observed_end_line reports the scan frontier separately", notes)

    def test_breaking_footer_and_subject_marker_both_change_compatibility(self):
        for message in [
            "fix(read): report unknown ends\n\nBREAKING CHANGE: Accept nullable target ends.",
            "fix(read)!: report unknown ends",
        ]:
            with self.subTest(message=message):
                self.commit(message)
                self.assertEqual(self.plan()["new_version"], "0.2.0")
                self.assertIn("**BREAKING**", self.notes("0.2.0"))
                self.git("reset", "--hard", "v0.1.28")

    def test_stable_versions_follow_feature_and_breaking_semantics(self):
        self.seed("1.2.3")
        self.commit("feat(cache): compact inactive indexes")
        self.assertEqual(self.plan()["new_version"], "1.3.0")
        self.commit(READ_BREAK)
        self.assertEqual(self.plan()["new_version"], "2.0.0")

    def test_no_new_commits_or_pending_tag_do_not_plan_another_release(self):
        self.assertEqual(self.plan()["skip"], "true")
        self.commit(READ_BREAK)
        self.manifest("0.2.0")
        plan = self.plan()
        self.assertEqual(plan["skip"], "true")
        self.assertNotIn("new_version", plan)


if __name__ == "__main__":
    unittest.main()
