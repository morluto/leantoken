#!/usr/bin/env python3
"""Explicitly refresh immutable release anchors using published Linux binaries.

Run --prepare to download/verify without executing Rust. Every invocation
atomically creates a fresh private (0700) workspace for downloads, extraction,
and execution; no executable is trusted or reused from a shared cache.
Archive/executable digests come from reviewed checked-in v*.json provenance;
mutable endpoint checksums cannot replace those pins.
Successful --prepare runs discard their verified downloads, so generation
redownloads and verifies in its own workspace. A failure preserves that whole
private workspace, including available fixture/log evidence, and reports its
path. Tests never run this generator, download artifacts, or need historical
binaries. Review fixture/manifest diffs, then explicitly update
release_anchors[*].metadata_blake3 in compatibility.json.
Generation-only Python requirement: blake3==1.0.11; --prepare uses the standard
library only.
"""

import hashlib
import io
import json
import lzma
import os
import pathlib
import resource
import shutil
import sqlite3
import struct
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import urllib.request

HERE = pathlib.Path(__file__).resolve().parent
WORKSPACE = HERE.parents[3]
ARCHIVE = "leantoken-x86_64-unknown-linux-gnu.tar.xz"
# Largest pinned archive/executable: 8,295,356 / 45,890,808 bytes (v0.1.28).
# Limits are independent: compressed transfer, expanded tar, metadata and binary.
COPY_BUFFER_BYTES = 64 * 1024
MAX_CHECKSUM_BYTES = 4 * 1024
MAX_PROVENANCE_BYTES = 16 * 1024
MAX_ARCHIVE_BYTES = 16 * 1024 * 1024
MAX_EXECUTABLE_BYTES = 64 * 1024 * 1024
MAX_ARCHIVE_MEMBERS = 64
MAX_ARCHIVE_DECLARED_BYTES = 96 * 1024 * 1024
MAX_TAR_BYTES = 128 * 1024 * 1024
MAX_XZ_MEMORY_BYTES = 128 * 1024 * 1024
MAX_OUTPUT_FILE_BYTES = 4 * 1024 * 1024
RELEASES = {
    "0.1.15": "6934ba9ff0c93e565d556f6f35750f8fe5a91faa",
    "0.1.27": "910ed8a34fbdd60e1c25c916e9ded12c8bf439e3",
    "0.1.28": "62a0ee259508272fa0f0947efcba0e80bbcfc141",
}
SOURCE = 'use std::fmt;\npub fn legacy_widget() { println!("migración"); }\n'


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def copy_bounded(source, output, limit, label):
    """Copy/hash incrementally, rejecting the first byte beyond the limit."""
    digest = hashlib.sha256()
    total = 0
    while True:
        block = source.read(min(COPY_BUFFER_BYTES, limit - total + 1))
        if not block:
            return digest.hexdigest(), total
        total += len(block)
        if total > limit:
            raise ValueError(f"{label} exceeded {limit}-byte limit")
        output.write(block)
        digest.update(block)


def expand_archive(archive, unpacked):
    # Explicit decoder and output limits also cover highly compressed metadata.
    decoder = lzma.LZMADecompressor(memlimit=MAX_XZ_MEMORY_BYTES)
    total = 0
    with archive.open("rb") as source, unpacked.open("xb") as output:
        while not decoder.eof:
            block = source.read(COPY_BUFFER_BYTES) if decoder.needs_input else b""
            if decoder.needs_input and not block:
                raise ValueError("truncated release XZ archive")
            expanded = decoder.decompress(
                block, max_length=min(COPY_BUFFER_BYTES, MAX_TAR_BYTES - total + 1)
            )
            total += len(expanded)
            if total > MAX_TAR_BYTES:
                raise ValueError(f"expanded archive exceeded {MAX_TAR_BYTES}-byte limit")
            output.write(expanded)


class BoundedTarReader(io.BufferedReader):
    def read(self, size=-1):
        # tarfile otherwise reads a complete PAX/GNU metadata body at once.
        if size < 0 or size > COPY_BUFFER_BYTES:
            raise ValueError("archive metadata read exceeded the fixed buffer")
        return super().read(size)


def git_bytes(revision, path):
    return subprocess.check_output(["git", "show", f"{revision}:{path}"], cwd=WORKSPACE)


def release_pin(version, commit):
    # The reviewed repository provenance is authoritative; mutable release
    # assets and their adjacent checksum cannot update these execution pins.
    if RELEASES.get(version) != commit:
        raise ValueError("release is not in the fixed compatibility matrix")
    with (HERE / f"v{version}.json").open("rb") as source, io.BytesIO() as output:
        copy_bounded(source, output, MAX_PROVENANCE_BYTES, "committed release provenance")
        pin = json.loads(output.getvalue())
    if (pin.get("version"), pin.get("tag"), pin.get("commit")) != (version, f"v{version}", commit):
        raise ValueError("committed release provenance identity mismatch")
    for field in ["archive_sha256", "binary_sha256"]:
        value = pin.get(field)
        if not isinstance(value, str) or len(value) != 64 or any(byte not in "0123456789abcdef" for byte in value):
            raise ValueError(f"invalid committed release {field}")
    return pin


def prepare(version, commit, workspace):
    pin = release_pin(version, commit)
    checksum = pin["archive_sha256"]
    destination = workspace / version / "generator"
    destination.mkdir(parents=True)
    url = f"https://github.com/morluto/leantoken/releases/download/v{version}/{ARCHIVE}"
    with urllib.request.urlopen(url + ".sha256") as source, io.BytesIO() as response:
        copy_bounded(source, response, MAX_CHECKSUM_BYTES, "checksum response")
        fields = response.getvalue().decode().split()
    if not fields or len(fields[0]) != 64 or any(byte not in "0123456789abcdef" for byte in fields[0]):
        raise ValueError("invalid published archive checksum")
    if fields[0] != checksum:
        raise ValueError("published checksum differs from committed release provenance")
    archive = destination / ARCHIVE
    with urllib.request.urlopen(url) as source, archive.open("xb") as output:
        archive_digest, _ = copy_bounded(source, output, MAX_ARCHIVE_BYTES, "release archive")
    if archive_digest != checksum:
        raise ValueError("archive digest differs from committed release provenance")
    unpacked = destination / "release.tar"
    expand_archive(archive, unpacked)
    with BoundedTarReader(unpacked.open("rb", buffering=0)) as source, tarfile.open(fileobj=source, mode="r:") as bundle:
        executable = None
        declared_bytes = 0
        for count, member in enumerate(bundle, 1):
            if count > MAX_ARCHIVE_MEMBERS:
                raise ValueError(f"archive exceeded {MAX_ARCHIVE_MEMBERS}-member limit")
            declared_bytes += member.size
            if member.size < 0 or declared_bytes > MAX_ARCHIVE_DECLARED_BYTES:
                raise ValueError(f"archive declared bytes exceeded {MAX_ARCHIVE_DECLARED_BYTES}-byte limit")
            if pathlib.PurePosixPath(member.name).name != "leantoken":
                continue
            if executable is not None or not member.isfile():
                raise ValueError("release archive must contain exactly one regular leantoken executable")
            if member.size > MAX_EXECUTABLE_BYTES:
                raise ValueError(f"declared executable exceeded {MAX_EXECUTABLE_BYTES}-byte limit")
            executable = member
        if executable is None:
            raise ValueError("release archive must contain exactly one regular leantoken executable")
        binary = destination / "leantoken"
        with bundle.extractfile(executable) as executable_source, binary.open("xb") as output:
            binary_digest, binary_bytes = copy_bounded(
                executable_source, output, MAX_EXECUTABLE_BYTES, "extracted executable"
            )
        if binary_bytes != executable.size:
            raise ValueError("extracted executable size differs from its archive declaration")
        if binary_digest != pin["binary_sha256"]:
            raise ValueError("executable digest differs from committed release provenance")
        binary.chmod(0o755)
    resolved = subprocess.check_output(["git", "rev-parse", f"v{version}^{{}}"], cwd=WORKSPACE).decode().strip()
    if resolved != commit:
        raise ValueError("release tag moved; compatibility decision required")
    schema_path = "src/storage.rs" if version == "0.1.15" else "src/storage/schema.rs"
    lock = git_bytes(commit, "Cargo.lock")
    packages = {package["name"]: package["version"] for package in tomllib.loads(lock.decode())["package"]}
    cli_path = "src/cli.rs" if version == "0.1.15" else "src/cli/mod.rs"
    supports_max_workers = b"max_index_workers" in git_bytes(commit, cli_path)
    metadata = {
        "version": version,
        "tag": f"v{version}",
        "commit": commit,
        "archive_url": url,
        "archive_sha256": checksum,
        "binary_sha256": binary_digest,
        "schema_source": schema_path,
        "schema_source_sha256": sha256(git_bytes(commit, schema_path)),
        "cargo_lock_sha256": sha256(lock),
        "rusqlite": packages["rusqlite"],
        "libsqlite3_sys": packages["libsqlite3-sys"],
        "rusqlite_migration": packages["rusqlite_migration"],
        "max_index_workers_supported": supports_max_workers,
        "generator_target": "x86_64-unknown-linux-gnu",
        "fixture": f"v{version}.sqlite",
        "source": SOURCE,
        "command": ["leantoken", "--root", "<temporary>/repo", "--database", "<fixture>.sqlite", "--json", "index"],
        "seed_scenario": "one Rust source with import, function, reference, UTF-8 content; indexed by the released executable",
        "supported_until": "explicit compatibility removal decision with changelog entry; retain previous released format before schema release",
    }
    (destination / "prepared.json").write_text(json.dumps(metadata, indent=2) + "\n")
    return binary, metadata


def generate(version, binary, metadata, workspace):
    from blake3 import blake3  # Generation-only dependency: blake3==1.0.11.

    root = workspace / version / "fixture"
    root.mkdir()
    try:
        repository = root / "repo"
        (repository / "src").mkdir(parents=True)
        (repository / "src/lib.rs").write_text(SOURCE)
        database = root / "index.sqlite"
        command = [str(binary), "--root", str(repository), "--database", str(database), "--json", "index"]
        if metadata["max_index_workers_supported"]:
            command += ["--max-index-workers", "1"]
            metadata["command"] += ["--max-index-workers", "1"]
        environment = dict(os.environ)
        for variable in ("HOME", "USERPROFILE", "XDG_CACHE_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "APPDATA", "LOCALAPPDATA", "TMP", "TMPDIR", "TEMP"):
            isolated = root / "environment" / variable.lower()
            isolated.mkdir(parents=True)
            environment[variable] = str(isolated)
        (root / "generation.json").write_text(json.dumps(metadata, indent=2) + "\n")
        metadata["generator_stdout"] = json.loads(run_generator(command, root, environment))
        if not database.exists():
            raise ValueError("released generator did not create its database")
        payload = database.read_bytes()
        wal_path = pathlib.Path(str(database) + "-wal")
        wal = wal_path.read_bytes() if wal_path.exists() else b""
        if len(payload) > 1024 * 1024:
            raise ValueError("release fixture exceeded 1 MiB")
        if len(wal) > 4 * 1024 * 1024:
            raise ValueError("release WAL exceeded 4 MiB")
        # A completed released process may leave a committed WAL. Preserve both
        # durable byte streams verbatim instead of checkpointing with Python's
        # different SQLite build. SHM is a derived, non-durable WAL index.
        metadata["sqlite_writer_version_number"] = writer_version(payload, wal)
        metadata["wal_snapshot_policy"] = "released main plus committed WAL bytes, no checkpoint or Python writes; SHM is derived"
        metadata["wal_fixture"] = f"v{version}.released-wal" if wal else None
        metadata["wal_bytes"] = len(wal)
        metadata["wal_sha256"] = sha256(wal) if wal else None
        metadata["wal_blake3"] = blake3(wal).hexdigest() if wal else None
        # Read-only inspection includes committed WAL. Save original bytes first:
        # connection close may remove SHM/WAL bookkeeping, never our artifact.
        with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as connection:
            metadata["migration_version"] = connection.execute("PRAGMA user_version").fetchone()[0]
            metadata["schema_version"] = connection.execute("SELECT schema_version FROM meta").fetchone()[0]
            schema = connection.execute("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name").fetchall()
            metadata["schema_sha256"] = sha256(json.dumps(schema, ensure_ascii=False, separators=(",", ":")).encode())
            if connection.execute("PRAGMA integrity_check").fetchall() != [("ok",)]:
                raise ValueError("released fixture failed integrity_check")
            if connection.execute("SELECT count(*) FROM files").fetchone()[0] != 1:
                raise ValueError("released fixture must contain one indexed file")
        metadata["fixture_sha256"] = sha256(payload)
        metadata["fixture_blake3"] = blake3(payload).hexdigest()
        metadata["fixture_bytes"] = len(payload)
        (HERE / metadata["fixture"]).write_bytes(payload)
        if wal:
            (HERE / metadata["wal_fixture"]).write_bytes(wal)
        (HERE / f"v{version}.json").write_text(json.dumps(metadata, indent=2, ensure_ascii=False) + "\n")
    except Exception:
        print(f"Release generation failed; disposable main/WAL/source/log evidence preserved: {root}", file=sys.stderr)
        raise


def run_generator(command, root, environment):
    # Regular files make RLIMIT_FSIZE cover child logs as well as SQLite output.
    # They remain available even when the process fails or exceeds its timeout.
    stdout_path = root / "generator.stdout"
    with stdout_path.open("xb") as stdout, (root / "generator.stderr").open("xb") as stderr:
        subprocess.run(command, check=True, stdout=stdout, stderr=stderr, timeout=60, env=environment, preexec_fn=bound_output_files)
    with stdout_path.open("rb") as source, io.BytesIO() as output:
        copy_bounded(source, output, MAX_OUTPUT_FILE_BYTES, "generator stdout")
        return output.getvalue().decode("utf-8")


def bound_output_files():
    # This generator already requires Linux release assets. The kernel limit
    # independently bounds main/WAL output and both regular child log files.
    resource.setrlimit(resource.RLIMIT_FSIZE, (MAX_OUTPUT_FILE_BYTES, MAX_OUTPUT_FILE_BYTES))


def writer_version(database, wal):
    """Read SQLite's writer identity from page1 of the committed WAL snapshot."""
    page_one = database[:100]
    if wal:
        if len(wal) < 32:
            raise ValueError("truncated WAL header")
        page_size = struct.unpack(">I", wal[8:12])[0]
        if not 512 <= page_size <= 65536:
            raise ValueError("invalid WAL page size")
        frame_size = 24 + page_size
        if (len(wal) - 32) % frame_size != 0:
            raise ValueError("truncated WAL frame")
        frames = [wal[offset:offset + frame_size] for offset in range(32, len(wal), frame_size)]
        committed = [index for index, frame in enumerate(frames) if struct.unpack(">I", frame[4:8])[0] != 0]
        if committed:
            for frame in frames[:committed[-1] + 1]:
                if struct.unpack(">I", frame[:4])[0] == 1:
                    page_one = frame[24:124]
    if page_one[:16] != b"SQLite format 3\0":
        raise ValueError("missing committed database header")
    return struct.unpack(">I", page_one[96:100])[0]


def main(prepare_only):
    # mkdtemp creates this unpredictable directory atomically with mode 0700.
    # All executable bytes stay under it until this invocation has finished.
    workspace = pathlib.Path(tempfile.mkdtemp(prefix="leantoken-release-migration-"))
    try:
        for version, commit in RELEASES.items():
            binary, metadata = prepare(version, commit, workspace)
            if not prepare_only:
                generate(version, binary, metadata, workspace)
            print(f"{version}: {'prepared' if prepare_only else 'generated'} {metadata['fixture']}")
    except BaseException:
        print(f"Release preparation/generation failed; private workspace and available evidence preserved: {workspace}", file=sys.stderr)
        raise
    else:
        shutil.rmtree(workspace)


if __name__ == "__main__":
    main(prepare_only="--prepare" in sys.argv)
