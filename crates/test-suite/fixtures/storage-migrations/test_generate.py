"""Offline generator workspace checks; no release binary or network is used."""

import contextlib
import hashlib
import io
import pathlib
import stat
import tarfile
import tempfile
import unittest
from unittest import mock

import generate


class PrivateGeneratorWorkspaceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="leantoken-generator-test-")
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        attacker = self.root / "preexisting"
        attacker.mkdir()
        self.untrusted_binary = attacker / "leantoken"
        self.untrusted_binary.write_bytes(b"untrusted cached executable")
        (self.root / "leantoken-release-migration-generators").symlink_to(
            attacker, target_is_directory=True
        )
        self.binary_bytes = b"verified synthetic executable; never executed"
        self.set_archive_members(["leantoken"])
        self.version = "0.1.28"
        self.commit = generate.RELEASES[self.version]
        self.workspaces = []
        self.stdout = io.StringIO()
        self.stderr = io.StringIO()
        patches = contextlib.ExitStack()
        self.addCleanup(patches.close)
        real_mkdtemp = tempfile.mkdtemp

        def private_workspace(**kwargs):
            path = pathlib.Path(real_mkdtemp(dir=self.root, **kwargs))
            self.assert_private_workspace(path)
            self.workspaces.append(path)
            return str(path)

        def download(url):
            if url.endswith(".sha256"):
                return io.BytesIO(f"{self.checksum}  {generate.ARCHIVE}\n".encode())
            return io.BytesIO(self.archive)

        def git_source(_revision, path):
            if path == "Cargo.lock":
                return b'''[[package]]
name = "rusqlite"
version = "0.40.1"
[[package]]
name = "libsqlite3-sys"
version = "0.38.1"
[[package]]
name = "rusqlite_migration"
version = "2.6.0"
'''
            return b"synthetic pinned source with max_index_workers"

        patches.enter_context(contextlib.redirect_stdout(self.stdout))
        patches.enter_context(contextlib.redirect_stderr(self.stderr))
        patches.enter_context(mock.patch.object(generate.tempfile, "mkdtemp", private_workspace))
        patches.enter_context(mock.patch.object(generate, "RELEASES", {self.version: self.commit}))
        patches.enter_context(mock.patch.object(generate.urllib.request, "urlopen", download))
        patches.enter_context(mock.patch.object(generate, "git_bytes", git_source))
        self.git_resolve = patches.enter_context(
            mock.patch.object(generate.subprocess, "check_output", return_value=f"{self.commit}\n".encode())
        )
        self.generate = patches.enter_context(mock.patch.object(generate, "generate"))

    def set_archive_members(self, names, kind=tarfile.REGTYPE):
        stream = io.BytesIO()
        with tarfile.open(fileobj=stream, mode="w:xz") as bundle:
            for name in names:
                member = tarfile.TarInfo(name)
                member.type = kind
                member.mode = 0o755
                if member.isfile():
                    member.size = len(self.binary_bytes)
                    bundle.addfile(member, io.BytesIO(self.binary_bytes))
                else:
                    member.linkname = "other-executable"
                    bundle.addfile(member)
        self.archive = stream.getvalue()
        self.checksum = hashlib.sha256(self.archive).hexdigest()

    def assert_private_workspace(self, workspace):
        self.assertEqual(stat.S_IMODE(workspace.stat().st_mode), 0o700)
        self.assertEqual(workspace.parent, self.root)
        self.assertFalse(workspace.is_symlink())

    def test_prepare_discards_downloads_and_generation_uses_a_fresh_private_workspace(self):
        generate.main(prepare_only=True)
        self.generate.assert_not_called()
        prepared = self.workspaces[0]
        self.assertFalse(prepared.exists(), "--prepare must not leave a reusable executable")

        def inspect_generation(version, binary, metadata, workspace):
            self.assert_private_workspace(workspace)
            self.assertNotEqual(workspace, prepared)
            self.assertEqual(binary.parent, workspace / version / "generator")
            self.assertEqual(binary.read_bytes(), self.binary_bytes)
            self.assertEqual(metadata["archive_sha256"], self.checksum)
            self.assertEqual(metadata["binary_sha256"], hashlib.sha256(self.binary_bytes).hexdigest())

        self.generate.side_effect = inspect_generation
        generate.main(prepare_only=False)
        self.generate.assert_called_once()
        self.assertEqual(len(self.workspaces), 2)
        self.assertFalse(self.workspaces[1].exists())
        self.assertEqual(self.untrusted_binary.read_bytes(), b"untrusted cached executable")

    def test_generation_failure_preserves_download_and_available_fixture_evidence(self):
        def fail_generation(version, binary, _metadata, workspace):
            self.assert_private_workspace(workspace)
            self.assertEqual(binary.read_bytes(), self.binary_bytes)
            fixture = workspace / version / "fixture"
            fixture.mkdir()
            (fixture / "index.sqlite").write_bytes(b"failed main evidence")
            (fixture / "index.sqlite-wal").write_bytes(b"failed WAL evidence")
            (fixture / "generator.stderr").write_text("synthetic failure")
            raise RuntimeError("synthetic generation failure")

        self.generate.side_effect = fail_generation
        with self.assertRaisesRegex(RuntimeError, "synthetic generation failure"):
            generate.main(prepare_only=False)
        workspace = self.workspaces[0]
        self.assert_private_workspace(workspace)
        self.assertEqual((workspace / self.version / "generator" / generate.ARCHIVE).read_bytes(), self.archive)
        fixture = workspace / self.version / "fixture"
        self.assertEqual((fixture / "index.sqlite").read_bytes(), b"failed main evidence")
        self.assertEqual((fixture / "index.sqlite-wal").read_bytes(), b"failed WAL evidence")
        self.assertEqual((fixture / "generator.stderr").read_text(), "synthetic failure")
        self.assertIn(str(workspace), self.stderr.getvalue())

    def test_checksum_failure_preserves_private_download_without_extraction_or_execution(self):
        self.checksum = "0" * 64
        with self.assertRaisesRegex(ValueError, "published archive checksum mismatch"):
            generate.main(prepare_only=False)
        workspace = self.workspaces[0]
        self.assert_private_workspace(workspace)
        destination = workspace / self.version / "generator"
        self.assertEqual((destination / generate.ARCHIVE).read_bytes(), self.archive)
        self.assertFalse((destination / "leantoken").exists())
        self.generate.assert_not_called()
        self.assertIn(str(workspace), self.stderr.getvalue())

    def test_missing_duplicate_and_nonregular_archive_members_refuse_execution(self):
        for names, kind in [
            (["README"], tarfile.REGTYPE),
            (["leantoken", "nested/leantoken"], tarfile.REGTYPE),
            (["leantoken"], tarfile.SYMTYPE),
        ]:
            with self.subTest(names=names, kind=kind):
                self.set_archive_members(names, kind)
                with self.assertRaisesRegex(ValueError, "exactly one regular leantoken executable"):
                    generate.main(prepare_only=False)
                workspace = self.workspaces[-1]
                self.assert_private_workspace(workspace)
                self.assertFalse((workspace / self.version / "generator" / "leantoken").exists())
                self.generate.assert_not_called()
                self.assertIn(str(workspace), self.stderr.getvalue())

    def test_moved_pinned_tag_refuses_execution_and_preserves_verified_artifact(self):
        self.git_resolve.return_value = b"0000000000000000000000000000000000000000\n"
        with self.assertRaisesRegex(ValueError, "release tag moved; compatibility decision required"):
            generate.main(prepare_only=False)
        workspace = self.workspaces[0]
        self.assert_private_workspace(workspace)
        self.assertEqual(
            (workspace / self.version / "generator" / "leantoken").read_bytes(),
            self.binary_bytes,
        )
        self.generate.assert_not_called()
        self.assertIn(str(workspace), self.stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
