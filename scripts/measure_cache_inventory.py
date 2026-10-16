"""Linux diagnostic: python3 scripts/measure_cache_inventory.py /path/to/leantoken.

Creates 64 one-file caches in a disposable home; measures one warm-cache list
process at populations 1/16/64, then checks refusal at 4097 root entries.
Requires GNU time and Linux inotify. File-open events are not connection hooks.
This debug/release agnostic probe makes no before/after speedup claim.
"""

import argparse
import ctypes
import hashlib
import json
import os
import platform
from pathlib import Path
import re
import struct
import subprocess
import sys
import tempfile
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("binary", type=Path)
parser.add_argument("--build-profile", required=True)
arguments = parser.parse_args()
binary = str(arguments.binary.resolve(strict=True))
if not sys.platform.startswith("linux") or not Path("/usr/bin/time").is_file():
    raise SystemExit("this diagnostic requires Linux inotify and /usr/bin/time")
with open(binary, "rb") as executable:
    binary_sha256 = hashlib.file_digest(executable, "sha256").hexdigest()
print(json.dumps(dict(record="environment", binary=binary,
                      binary_sha256=binary_sha256, build_profile=arguments.build_profile,
                      platform=platform.platform(), cpus=os.cpu_count(),
                      fixture="one 31-byte Rust source per cache; estimate tokenizer",
                      order="ascending 1/16/64; one sequential warm observation each")), flush=True)
libc = ctypes.CDLL(None, use_errno=True)
IN_OPEN, IN_CLOSE_NOWRITE, IN_Q_OVERFLOW = 0x20, 0x10, 0x4000
with tempfile.TemporaryDirectory(prefix="leantoken-540-measure-") as temporary:
    home = Path(temporary)
    env = dict(os.environ)
    for name in ["HOME", "USERPROFILE", "TMPDIR", "TMP", "TEMP", "SQLITE_TMPDIR"]:
        env[name] = str(home)
    env.update(XDG_CACHE_HOME=str(home / "cache"), LOCALAPPDATA=str(home / "appdata"),
               XDG_CONFIG_HOME=str(home / "config"), XDG_DATA_HOME=str(home / "data"),
               APPDATA=str(home / "appdata-roaming"))
    env.pop("npm_lifecycle_event", None)

    def command(args, timed=False):
        argv = [binary, *args]
        if timed:
            argv = ["/usr/bin/time", "-f", "__MEASURE__ %M %e", *argv]
        return subprocess.run(argv, cwd=home, env=env, capture_output=True, timeout=30)

    databases = []
    for count in range(1, 65):
        repository = home / f"repository-{count}"
        repository.mkdir()
        (repository / "main.rs").write_text("pub fn measured_inventory() {}\n")
        indexed = command(["--root", str(repository), "--tokenizer", "estimate", "index"])
        assert indexed.returncode == 0, indexed.stderr.decode()
        if count not in [1, 16, 64]:
            continue
        listed = command(["--json", "cache", "list", "--limit", "100"])
        assert listed.returncode == 0, listed.stderr.decode()
        complete = json.loads(listed.stdout)
        cache_root = Path(complete["cache_root"])
        assert cache_root.resolve().is_relative_to(home.resolve())
        databases = [Path(entry["path"]) / "index.sqlite" for entry in complete["entries"]]
        assert len(databases) == count
        fd = libc.inotify_init1(os.O_NONBLOCK | os.O_CLOEXEC)
        assert fd >= 0, ctypes.get_errno()
        try:
            for database in databases:
                assert libc.inotify_add_watch(fd, os.fsencode(database), IN_OPEN | IN_CLOSE_NOWRITE) >= 0
            start = time.monotonic()
            result = command(["--json", "cache", "list", "--limit", "1"], timed=True)
            elapsed = time.monotonic() - start
            assert result.returncode == 0, result.stderr.decode()
            report = json.loads(result.stdout)
            assert len(report["entries"]) == 1
            data = os.read(fd, 1024 * 1024)
            offset = opens = 0
            while offset < len(data):
                _, mask, _, length = struct.unpack_from("iIII", data, offset)
                assert not mask & IN_Q_OVERFLOW, "inotify evidence overflow"
                opens += bool(mask & IN_OPEN)
                offset += 16 + length
            assert opens == count, f"expected one database file open per cache, observed {opens}"
            measured = re.search(rb"__MEASURE__ (\d+) ([\d.]+)", result.stderr)
            assert measured, result.stderr
            print(json.dumps(dict(caches=count, page_limit=1, database_file_opens=opens,
                                  peak_rss_kib=int(measured[1]), wall_seconds=round(elapsed, 4),
                                  gnu_time_seconds=float(measured[2]))), flush=True)
        finally:
            os.close(fd)
    for number in range(4097 - 64):
        (cache_root / f"unmanaged-{number}").touch()
    result = command(["--json", "cache", "list", "--limit", "1"], timed=True)
    assert result.returncode != 0
    error = json.loads(result.stderr.split(b"\nCommand exited", 1)[0].split(b"\n__MEASURE__", 1)[0])
    assert error["category"] == "incomplete_inventory", result.stderr
    print(json.dumps(dict(root_entries=4097, boundary_error=error)), flush=True)
