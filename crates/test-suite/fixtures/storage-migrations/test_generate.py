"""Offline generator workspace checks; no release binary or network is used."""

import contextlib
import hashlib
import io
import json
import os
import pathlib
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock

import generate


class FixedBufferResponse(io.BytesIO):
    def read(self, size=-1):
        if size < 0 or size > generate.COPY_BUFFER_BYTES:
            raise AssertionError(f"unbounded transfer read: {size}")
        return super().read(size)


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
        self.version = "0.1.28"
        self.commit = generate.RELEASES[self.version]
        self.provenance = self.root / "provenance"
        self.provenance.mkdir()
        self.binary_bytes = b"verified synthetic executable; never executed"
        self.set_archive_members(["leantoken"])
        self.workspaces = []
        self.stdout = io.StringIO()
        self.stderr = io.StringIO()
        self.requests = []
        patches = contextlib.ExitStack()
        self.addCleanup(patches.close)
        real_mkdtemp = tempfile.mkdtemp

        def private_workspace(**kwargs):
            path = pathlib.Path(real_mkdtemp(dir=self.root, **kwargs))
            self.assert_private_workspace(path)
            self.workspaces.append(path)
            return str(path)

        def download(url):
            self.requests.append(url)
            if url.endswith(".sha256"):
                return FixedBufferResponse(f"{self.checksum}  {generate.ARCHIVE}\n".encode())
            return FixedBufferResponse(self.archive)

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
        patches.enter_context(mock.patch.object(generate, "HERE", self.provenance))
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
        (self.provenance / f"v{self.version}.json").write_text(json.dumps({
            "version": self.version,
            "tag": f"v{self.version}",
            "commit": self.commit,
            "archive_sha256": self.checksum,
            "binary_sha256": hashlib.sha256(self.binary_bytes).hexdigest(),
        }))

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
        self.archive += b"changed after the trusted digest was recorded"
        with self.assertRaisesRegex(ValueError, "archive digest differs from committed release provenance"):
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

    def test_checksum_response_and_archive_transfer_enforce_actual_byte_limits(self):
        for constant, limit, message, artifact in [
            ("MAX_CHECKSUM_BYTES", 16, "checksum response exceeded", None),
            ("MAX_ARCHIVE_BYTES", len(self.archive) - 1, "release archive exceeded", generate.ARCHIVE),
        ]:
            with self.subTest(constant=constant), mock.patch.object(generate, constant, limit):
                with self.assertRaisesRegex(ValueError, message):
                    generate.main(prepare_only=False)
                destination = self.workspaces[-1] / self.version / "generator"
                if artifact is not None:
                    self.assertLessEqual((destination / artifact).stat().st_size, limit)
                else:
                    self.assertFalse((destination / generate.ARCHIVE).exists())
                self.assertFalse((destination / "leantoken").exists())
                self.generate.assert_not_called()

    def test_declared_executable_limit_refuses_before_extraction(self):
        with mock.patch.object(generate, "MAX_EXECUTABLE_BYTES", len(self.binary_bytes) - 1):
            with self.assertRaisesRegex(ValueError, "declared executable exceeded"):
                generate.main(prepare_only=False)
        destination = self.workspaces[0] / self.version / "generator"
        self.assertFalse((destination / "leantoken").exists())
        self.generate.assert_not_called()

    def test_extracted_stream_is_bounded_independently_of_declared_size(self):
        for extra, limit, message in [
            (b"oversized", len(self.binary_bytes), "extracted executable exceeded"),
            (b"x", len(self.binary_bytes) + 8, "size differs from its archive declaration"),
        ]:
            with self.subTest(limit=limit), mock.patch.object(generate, "MAX_EXECUTABLE_BYTES", limit):
                with mock.patch.object(
                    generate.tarfile.TarFile,
                    "extractfile",
                    return_value=FixedBufferResponse(self.binary_bytes + extra),
                ):
                    with self.assertRaisesRegex(ValueError, message):
                        generate.main(prepare_only=False)
                binary = self.workspaces[-1] / self.version / "generator" / "leantoken"
                self.assertLessEqual(binary.stat().st_size, limit)
                self.generate.assert_not_called()

    def test_expanded_member_count_and_declared_sum_limits_refuse_execution(self):
        self.set_archive_members(["leantoken", "notes"])
        for constant, limit, message in [
            ("MAX_TAR_BYTES", 1024, "expanded archive exceeded"),
            ("MAX_ARCHIVE_MEMBERS", 1, "archive exceeded 1-member limit"),
            ("MAX_ARCHIVE_DECLARED_BYTES", len(self.binary_bytes), "archive declared bytes exceeded"),
        ]:
            with self.subTest(constant=constant), mock.patch.object(generate, constant, limit):
                with self.assertRaisesRegex(ValueError, message):
                    generate.main(prepare_only=False)
                destination = self.workspaces[-1] / self.version / "generator"
                self.assertFalse((destination / "leantoken").exists())
                if constant == "MAX_TAR_BYTES":
                    self.assertLessEqual((destination / "release.tar").stat().st_size, limit)
                self.generate.assert_not_called()

    def test_oversized_extended_header_refuses_before_large_metadata_allocation(self):
        self.set_archive_members(["leantoken", "x" * (generate.COPY_BUFFER_BYTES + 1)])
        with self.assertRaisesRegex(ValueError, "archive metadata read exceeded the fixed buffer"):
            generate.main(prepare_only=False)
        self.generate.assert_not_called()

    def test_python_child_logs_preserve_successful_json_without_captured_pipes(self):
        root = self.root / "successful-child"
        root.mkdir()
        command = [
            sys.executable, "-I", "-S", "-c",
            "import os; os.write(1, b'{\"indexed\": 1}\\n'); os.write(2, b'diagnostic\\n')",
        ]
        with mock.patch.object(generate, "MAX_OUTPUT_FILE_BYTES", 4096):
            stdout = generate.run_generator(command, root, {"HOME": str(root), "PATH": os.defpath})
        self.assertEqual(stdout, '{"indexed": 1}\n')
        self.assertEqual(json.loads(stdout), {"indexed": 1})
        self.assertEqual((root / "generator.stderr").read_bytes(), b"diagnostic\n")
        self.assertTrue(stat.S_ISREG((root / "generator.stdout").stat().st_mode))
        self.assertTrue(stat.S_ISREG((root / "generator.stderr").stat().st_mode))

    def test_python_child_output_limit_preserves_bounded_logs_and_failed_workspace(self):
        limit = 4096
        for descriptor, name in [(1, "stdout"), (2, "stderr")]:
            with self.subTest(stream=name):
                def fail_with_real_python_child(version, _binary, _metadata, workspace):
                    root = workspace / version / "fixture"
                    root.mkdir()
                    script = (
                        f"import os; os.write({3 - descriptor}, b'before overflow\\n'); "
                        f"os.write({descriptor}, b'x' * {limit}); os.write({descriptor}, b'!')"
                    )
                    return generate.run_generator(
                        [sys.executable, "-I", "-S", "-c", script],
                        root,
                        {"HOME": str(root), "PATH": os.defpath},
                    )

                self.generate.side_effect = fail_with_real_python_child
                with mock.patch.object(generate, "MAX_OUTPUT_FILE_BYTES", limit):
                    with self.assertRaises(subprocess.CalledProcessError) as failure:
                        generate.main(prepare_only=False)
                self.assertIsNone(failure.exception.stdout)
                self.assertIsNone(failure.exception.stderr)
                workspace = self.workspaces[-1]
                self.assert_private_workspace(workspace)
                root = workspace / self.version / "fixture"
                self.assertEqual((root / f"generator.{name}").stat().st_size, limit)
                for stream in ["stdout", "stderr"]:
                    output = root / f"generator.{stream}"
                    self.assertTrue(stat.S_ISREG(output.stat().st_mode))
                    self.assertLessEqual(output.stat().st_size, limit)
                other = "stderr" if name == "stdout" else "stdout"
                self.assertTrue((root / f"generator.{other}").read_bytes().startswith(b"before overflow\n"))
                self.assertIn(str(workspace), self.stderr.getvalue())

    def test_replaced_archive_and_matching_endpoint_checksum_cannot_replace_trusted_pin(self):
        trusted = (self.provenance / f"v{self.version}.json").read_bytes()
        self.archive += b"replacement release asset"
        self.checksum = hashlib.sha256(self.archive).hexdigest()
        with self.assertRaisesRegex(ValueError, "published checksum differs from committed release provenance"):
            generate.main(prepare_only=False)
        self.assertEqual(len(self.requests), 1, "reject before downloading the replacement archive")
        self.assertTrue(self.requests[0].endswith(".sha256"))
        self.assertFalse((self.workspaces[0] / self.version / "generator" / "leantoken").exists())
        self.assertEqual((self.provenance / f"v{self.version}.json").read_bytes(), trusted)
        self.generate.assert_not_called()

    def test_provenance_identity_and_digest_pins_refuse_before_generation(self):
        path = self.provenance / f"v{self.version}.json"
        trusted = json.loads(path.read_text())
        for field, replacement, message in [
            ("version", "0.0.0", "provenance identity mismatch"),
            ("tag", "v0.0.0", "provenance identity mismatch"),
            ("commit", "0" * 40, "provenance identity mismatch"),
            ("archive_sha256", "invalid", "invalid committed release archive_sha256"),
            ("binary_sha256", "0" * 64, "executable digest differs from committed release provenance"),
        ]:
            with self.subTest(field=field):
                changed = dict(trusted)
                changed[field] = replacement
                path.write_text(json.dumps(changed))
                self.requests.clear()
                with self.assertRaisesRegex(ValueError, message):
                    generate.main(prepare_only=False)
                if field != "binary_sha256":
                    self.assertFalse(self.requests, "validate trusted identity before contacting release endpoints")
                self.generate.assert_not_called()

    def test_cli_accepts_only_generation_or_exact_prepare_mode(self):
        for arguments, prepare_only in [([], False), (["--prepare"], True)]:
            with self.subTest(arguments=arguments), mock.patch.object(generate, "main") as main:
                generate.cli(arguments)
                main.assert_called_once_with(prepare_only=prepare_only)
        self.assertFalse(self.workspaces)
        self.assertFalse(self.requests)
        self.generate.assert_not_called()

    def test_cli_refuses_invalid_arguments_before_any_generation_activity(self):
        for arguments in [
            ["--preapre"], ["--unknown"], ["--prepare", "--prepare"],
            ["--prepare", "extra"], ["extra", "--prepare"], ["--prepare=true"], ["--"],
        ]:
            with self.subTest(arguments=arguments):
                with mock.patch.object(generate, "main") as main, mock.patch.object(generate, "prepare") as prepare:
                    with self.assertRaises(SystemExit) as failure:
                        generate.cli(arguments)
                    self.assertEqual(failure.exception.code, 2)
                    main.assert_not_called()
                    prepare.assert_not_called()
                self.assertFalse(self.workspaces)
                self.assertFalse(self.requests)
                self.generate.assert_not_called()

    def test_optimized_script_entry_refuses_typo_without_workspace_or_network(self):
        path = pathlib.Path(generate.__file__).resolve()
        script = (
            "import runpy, socket, sys, tempfile\n"
            "def forbidden(*args, **kwargs):\n"
            "    raise RuntimeError('generation activity reached before CLI rejection')\n"
            "tempfile.mkdtemp = forbidden\n"
            "socket.create_connection = forbidden\n"
            f"sys.argv = [{str(path)!r}, '--preapre']\n"
            f"runpy.run_path({str(path)!r}, run_name='__main__')\n"
        )
        result = subprocess.run(
            [sys.executable, "-I", "-S", "-O", "-c", script],
            env={"HOME": str(self.root), "TMPDIR": str(self.root), "PATH": os.defpath},
            capture_output=True, text=True, timeout=5,
        )
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("usage: generate.py [--prepare]", result.stderr)
        self.assertFalse(result.stdout)


if __name__ == "__main__":
    unittest.main()
