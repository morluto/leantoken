#!/usr/bin/env python3
"""Explicitly refresh immutable release anchors using published Linux binaries.

Run --prepare to download/verify without executing Rust. Every invocation
atomically creates a fresh private (0700) workspace for downloads, extraction,
and execution; no executable is trusted or reused from a shared cache.
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
import json
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
RELEASES = {
    "0.1.15": "6934ba9ff0c93e565d556f6f35750f8fe5a91faa",
    "0.1.27": "910ed8a34fbdd60e1c25c916e9ded12c8bf439e3",
    "0.1.28": "62a0ee259508272fa0f0947efcba0e80bbcfc141",
}
SOURCE = 'use std::fmt;\npub fn legacy_widget() { println!("migración"); }\n'


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def git_bytes(revision, path):
    return subprocess.check_output(["git", "show", f"{revision}:{path}"], cwd=WORKSPACE)


def prepare(version, commit, workspace):
    destination = workspace / version / "generator"
    destination.mkdir(parents=True)
    url = f"https://github.com/morluto/leantoken/releases/download/v{version}/{ARCHIVE}"
    checksum = urllib.request.urlopen(url + ".sha256").read().decode().split()[0]
    archive = destination / ARCHIVE
    with urllib.request.urlopen(url) as source, archive.open("xb") as output:
        shutil.copyfileobj(source, output)
    if sha256(archive.read_bytes()) != checksum:
        raise ValueError("published archive checksum mismatch")
    with tarfile.open(archive, "r:xz") as bundle:
        members = [item for item in bundle.getmembers() if pathlib.PurePosixPath(item.name).name == "leantoken"]
        if len(members) != 1 or not members[0].isfile():
            raise ValueError("release archive must contain exactly one regular leantoken executable")
        binary = destination / "leantoken"
        with binary.open("xb") as output:
            output.write(bundle.extractfile(members[0]).read())
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
        "binary_sha256": sha256(binary.read_bytes()),
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
        result = subprocess.run(command, check=True, capture_output=True, text=True, timeout=60, env=environment, preexec_fn=bound_output_files)
        (root / "generator.stdout").write_text(result.stdout)
        (root / "generator.stderr").write_text(result.stderr)
        metadata["generator_stdout"] = json.loads(result.stdout)
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
    except Exception as failure:
        for stream in ("stdout", "stderr"):
            output = getattr(failure, stream, None)
            if output:
                (root / f"generator.{stream}").write_text(output if isinstance(output, str) else output.decode(errors="replace"))
        print(f"Release generation failed; disposable main/WAL/source/log evidence preserved: {root}", file=sys.stderr)
        raise


def bound_output_files():
    # The fixture has one small source file; a4MiB/file hard limit also bounds failed
    # main/WAL generation. This generator already requires Linux release assets.
    resource.setrlimit(resource.RLIMIT_FSIZE, (4 * 1024 * 1024, 4 * 1024 * 1024))


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
