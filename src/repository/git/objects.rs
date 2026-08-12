/// Load a bounded set of UTF-8 repository files from one immutable revision.
///
/// A single tree query resolves path-to-object identities, followed by one
/// `cat-file --batch` call for the selected unique blobs.
pub(crate) fn git_blobs_at_revision(
    root: &Path,
    revision: &str,
    paths: &[String],
    max_file_bytes: usize,
    max_total_bytes: usize,
) -> Result<GitBlobBatch> {
    let timeout = Duration::from_millis(2_000);
    let program = Path::new("git");
    let revision = resolve_revision_sha_for_field(root, program, revision, timeout, "revision")?;
    git_blobs_at_resolved_revision(root, &revision, paths, max_file_bytes, max_total_bytes)
}

/// Load bounded UTF-8 blobs after the caller has resolved the immutable revision.
///
/// This executes one `ls-tree` subprocess and at most one `cat-file --batch`
/// subprocess, independent of the number of requested paths.
pub(crate) fn git_blobs_at_resolved_revision(
    root: &Path,
    revision: &str,
    paths: &[String],
    max_file_bytes: usize,
    max_total_bytes: usize,
) -> Result<GitBlobBatch> {
    let timeout = Duration::from_millis(2_000);
    let program = Path::new("git");
    let revision = revision.to_owned();
    let prefix = git_worktree_prefix(root);
    let requested = paths.iter().cloned().collect::<BTreeSet<_>>();
    if requested.is_empty() {
        return Ok(GitBlobBatch {
            revision,
            blobs: BTreeMap::new(),
            missing_paths: Vec::new(),
            oversized_paths: Vec::new(),
            total_limit_paths: Vec::new(),
            invalid_utf8_paths: Vec::new(),
            unsupported_paths: Vec::new(),
        });
    }

    let mut args = vec![
        "ls-tree".into(),
        "-r".into(),
        "-z".into(),
        "-l".into(),
        "--full-tree".into(),
        revision.clone(),
        "--".into(),
    ];
    args.extend(requested.iter().map(|path| format!("{prefix}{path}")));
    let tree_output_limit = requested.iter().fold(1_024usize, |limit, path| {
        limit.saturating_add(path.len()).saturating_add(160)
    });
    let tree_output = run_git_capture(
        root,
        program,
        &args,
        GitCaptureOptions {
            timeout,
            field: "path",
            timeout_reason: "git ls-tree timed out",
            failure_reason: "failed to inspect files at revision",
            max_output_bytes: tree_output_limit,
        },
    )?;

    let mut objects = BTreeMap::<String, (String, usize)>::new();
    let mut present_paths = BTreeSet::new();
    let mut unsupported_paths = Vec::new();
    for record in tree_output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
            return Err(Error::OperationFailure("invalid git ls-tree record".into()));
        };
        let metadata = std::str::from_utf8(&record[..tab])
            .map_err(|_| Error::OperationFailure("invalid git ls-tree metadata".into()))?;
        let mut fields = metadata.split_whitespace();
        let _mode = fields.next();
        let object_type = fields.next();
        let object_id = fields.next();
        let size = fields.next().and_then(|value| value.parse::<usize>().ok());
        let path = std::str::from_utf8(&record[tab + 1..])
            .map_err(|_| Error::OperationFailure("invalid git ls-tree path".into()))?;
        let Some(path) = path.strip_prefix(&prefix) else {
            continue;
        };
        if requested.contains(path) {
            present_paths.insert(path.to_owned());
        }
        if object_type == Some("blob")
            && let (Some(object_id), Some(size)) = (object_id, size)
            && requested.contains(path)
        {
            objects.insert(path.to_owned(), (object_id.to_owned(), size));
        } else if requested.contains(path) {
            unsupported_paths.push(path.to_owned());
        }
    }

    let mut missing_paths = Vec::new();
    let mut oversized_paths = Vec::new();
    let mut total_limit_paths = Vec::new();
    let mut selected = Vec::new();
    let mut total_bytes = 0usize;
    for path in &requested {
        let Some((object_id, size)) = objects.get(path) else {
            if !present_paths.contains(path) {
                missing_paths.push(path.clone());
            }
            continue;
        };
        if *size > max_file_bytes {
            oversized_paths.push(path.clone());
            continue;
        }
        if total_bytes.saturating_add(*size) > max_total_bytes {
            total_limit_paths.push(path.clone());
            continue;
        }
        total_bytes += *size;
        selected.push((path.clone(), object_id.clone(), *size));
    }

    let unique_objects = selected
        .iter()
        .map(|(_, object_id, size)| (object_id.clone(), *size))
        .collect::<BTreeMap<_, _>>();
    let mut input = Vec::new();
    for object_id in unique_objects.keys() {
        input.extend_from_slice(object_id.as_bytes());
        input.push(b'\n');
    }
    let batch_output_limit = total_bytes
        .saturating_add(unique_objects.len().saturating_mul(96))
        .saturating_add(1_024);
    let batch_output = if input.is_empty() {
        Vec::new()
    } else {
        run_git_capture_with_input(
            root,
            program,
            &["cat-file".into(), "--batch".into()],
            &input,
            GitCaptureOptions {
                timeout,
                field: "path",
                timeout_reason: "git cat-file batch timed out",
                failure_reason: "failed to load files at revision",
                max_output_bytes: batch_output_limit,
            },
        )?
    };
    let mut contents = BTreeMap::<String, Vec<u8>>::new();
    let mut cursor = 0usize;
    for (expected_object, expected_size) in &unique_objects {
        let header_end = batch_output[cursor..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|offset| cursor + offset)
            .ok_or_else(|| Error::OperationFailure("invalid git cat-file header".into()))?;
        let header = std::str::from_utf8(&batch_output[cursor..header_end])
            .map_err(|_| Error::OperationFailure("invalid git cat-file header".into()))?;
        let mut fields = header.split_whitespace();
        let object_id = fields.next();
        let object_type = fields.next();
        let size = fields.next().and_then(|value| value.parse::<usize>().ok());
        if object_id != Some(expected_object.as_str())
            || object_type != Some("blob")
            || size != Some(*expected_size)
        {
            return Err(Error::OperationFailure(
                "unexpected git cat-file batch response".into(),
            ));
        }
        let content_start = header_end + 1;
        let content_end = content_start
            .checked_add(*expected_size)
            .ok_or_else(|| Error::OperationFailure("git blob size overflow".into()))?;
        if batch_output.get(content_end) != Some(&b'\n') {
            return Err(Error::OperationFailure(
                "truncated git cat-file batch response".into(),
            ));
        }
        contents.insert(
            expected_object.clone(),
            batch_output[content_start..content_end].to_vec(),
        );
        cursor = content_end + 1;
    }
    if cursor != batch_output.len() {
        return Err(Error::OperationFailure(
            "unexpected trailing git cat-file output".into(),
        ));
    }

    let mut blobs = BTreeMap::new();
    let mut invalid_utf8_paths = Vec::new();
    for (path, object_id, _) in selected {
        let content = contents
            .get(&object_id)
            .ok_or_else(|| Error::OperationFailure("missing batched git blob".into()))?
            .clone();
        match String::from_utf8(content) {
            Ok(content) => {
                blobs.insert(path, content);
            }
            Err(_) => invalid_utf8_paths.push(path),
        }
    }
    Ok(GitBlobBatch {
        revision,
        blobs,
        missing_paths,
        oversized_paths,
        total_limit_paths,
        invalid_utf8_paths,
        unsupported_paths,
    })
}

use super::*;
