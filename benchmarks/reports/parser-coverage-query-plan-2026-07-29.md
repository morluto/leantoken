# Parser-coverage metadata query plan

Date: 2026-07-29

Decision: keep parser coverage as an explicit O(files) diagnostic over the
current `files` metadata table. Do not add a storage index solely for this
opt-in report.

## Evidence

The plan was captured with SQLite against LeanToken's 526-file generation at
`/root/leantoken`, after running the new `coverage` command:

```text
SELECT language, structurally_complete, count(*),
       coalesce(sum(size_bytes), 0)
FROM files
WHERE language IS NOT NULL
GROUP BY language, structurally_complete
ORDER BY language, structurally_complete DESC

QUERY PLAN
|--SCAN files
`--USE TEMP B-TREE FOR GROUP BY
```

```text
SELECT path, size_bytes
FROM files
WHERE language IS NULL
ORDER BY path

QUERY PLAN
`--SCAN files USING INDEX sqlite_autoindex_files_1
```

Both statements run inside the same deferred read transaction that supplies the
reported repository generation. The first scans only stored language,
completeness, and size metadata and groups the bounded set of supported parser
labels. The second uses the existing unique path index for deterministic order
and reads only path and size metadata. Each path is classified through the
service-owned safe-label function while the SQLite cursor is folded; paths are
not retained or returned. Only aggregate extension counts cross the storage
boundary, so retained memory is O(distinct safe extension groups), not
O(unrecognized files).

No chunks, symbols, FTS tables, source blobs, or working-tree files are read.
The configured index file ceiling bounds both scans. Adding a
`(language, structurally_complete)` index would increase every publication's
storage and write work for an explicit diagnostic whose dominant unsupported
extension fold still visits unrecognized file metadata, so this trial does not
justify that permanent cost.
