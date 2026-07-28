fn prepare_batch_end(
    candidates: &[DiscoveredFile],
    start: usize,
    limits: crate::DiscoveryLimits,
) -> usize {
    let mut end = start;
    let mut batch_bytes = 0u64;
    while end < candidates.len() && end - start < limits.max_prepare_batch_files {
        let observed = batch_bytes.saturating_add(candidates[end].size_bytes);
        if observed > limits.max_prepare_batch_bytes {
            break;
        }
        batch_bytes = observed;
        end += 1;
    }
    if end == start && start < candidates.len() {
        start + 1
    } else {
        end
    }
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn check_cancelled(cancellation: &CancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

fn prepare_file(
    root: &Dir,
    file: &DiscoveredFile,
    chunk_lines: usize,
    chunk_bytes: usize,
    tokenizer: crate::tokens::Tokenizer,
    max_file_bytes: u64,
    cancellation: &CancellationToken,
) -> Result<PreparedFile> {
    prepare_file_inner(
        root,
        file,
        chunk_lines,
        chunk_bytes,
        tokenizer,
        max_file_bytes,
        cancellation,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn prepare_file_profiled(
    root: &Dir,
    file: &DiscoveredFile,
    chunk_lines: usize,
    chunk_bytes: usize,
    tokenizer: crate::tokens::Tokenizer,
    max_file_bytes: u64,
    cancellation: &CancellationToken,
    diagnostics: &mut FilePreparationDiagnostics,
) -> Result<PreparedFile> {
    diagnostics.files_profiled = 1;
    let started = Instant::now();
    let result = prepare_file_inner(
        root,
        file,
        chunk_lines,
        chunk_bytes,
        tokenizer,
        max_file_bytes,
        cancellation,
        Some(diagnostics),
    );
    diagnostics.total = started.elapsed();
    result
}

#[allow(clippy::too_many_arguments)]
fn prepare_file_inner(
    root: &Dir,
    file: &DiscoveredFile,
    chunk_lines: usize,
    chunk_bytes: usize,
    tokenizer: crate::tokens::Tokenizer,
    max_file_bytes: u64,
    cancellation: &CancellationToken,
    mut diagnostics: Option<&mut FilePreparationDiagnostics>,
) -> Result<PreparedFile> {
    let read_started = diagnostics.is_some().then(Instant::now);
    let bytes = match read_bounded(root, &file.relative_path, max_file_bytes) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            record_preparation_duration(diagnostics.as_deref_mut(), read_started, |detail| {
                &mut detail.read
            });
            return Ok(PreparedFile::Oversized(file.relative_path.clone()));
        }
        Err(error) => {
            record_preparation_duration(diagnostics.as_deref_mut(), read_started, |detail| {
                &mut detail.read
            });
            return Ok(PreparedFile::Failed(
                file.relative_path.clone(),
                error.to_string(),
            ));
        }
    };
    record_preparation_duration(diagnostics.as_deref_mut(), read_started, |detail| {
        &mut detail.read
    });
    let size_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let text_prepare_started = diagnostics.is_some().then(Instant::now);
    let prepared = PreparedText::from_vec(bytes, chunk_lines, chunk_bytes);
    record_preparation_duration(diagnostics.as_deref_mut(), text_prepare_started, |detail| {
        &mut detail.text_prepare
    });
    if prepared.kind == TextKind::Binary {
        return Ok(PreparedFile::Binary(file.relative_path.clone()));
    }
    let hash_started = diagnostics.is_some().then(Instant::now);
    let content_hash = hash_bytes(prepared.content.as_bytes());
    record_preparation_duration(diagnostics.as_deref_mut(), hash_started, |detail| {
        &mut detail.hash
    });

    let parse_started = diagnostics.is_some().then(Instant::now);
    let (parsed, warning) =
        match parser::parse_with_cancellation(&file.relative_path, &prepared.content, cancellation)
        {
            Ok(parsed) => (parsed, None),
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(error) => (
                ParseOutput {
                    language: parser::language_by_path(&file.relative_path),
                    structurally_complete: false,
                    symbols: Vec::new(),
                    references: Vec::new(),
                    imports: Vec::new(),
                },
                Some(format!(
                    "{}: structural parse failed; text remains searchable: {error}",
                    file.relative_path
                )),
            ),
        };
    record_preparation_duration(diagnostics.as_deref_mut(), parse_started, |detail| {
        &mut detail.parse
    });

    let source_tokens_started = diagnostics.is_some().then(Instant::now);
    let source_token_count = tokenizer.count(&prepared.content);
    record_preparation_duration(
        diagnostics.as_deref_mut(),
        source_tokens_started,
        |detail| &mut detail.source_token_count,
    );
    let chunk_tokens_started = diagnostics.is_some().then(Instant::now);
    let chunks = prepared
        .chunks
        .into_iter()
        .map(|chunk| ChunkInput {
            token_count: tokenizer.count(&chunk.content),
            content: chunk.content,
            start_line: chunk.start_line,
            end_line: chunk.end_line,
            start_byte: chunk.start_byte,
            end_byte: chunk.end_byte,
        })
        .collect();
    record_preparation_duration(diagnostics.as_deref_mut(), chunk_tokens_started, |detail| {
        &mut detail.chunk_token_count
    });
    let projection_started = diagnostics.is_some().then(Instant::now);
    let symbols = parsed
        .symbols
        .into_iter()
        .map(|symbol| SymbolInput {
            name: symbol.name,
            kind: symbol.kind,
            parent: symbol.parent,
            signature: symbol.signature,
            start_line: symbol.start_line,
            end_line: symbol.end_line,
            start_byte: symbol.start_byte,
            end_byte: symbol.end_byte,
        })
        .collect();
    let references = parsed
        .references
        .into_iter()
        .map(|reference| ReferenceInput {
            name: reference.name,
            kind: reference.kind,
            role: reference.role,
            enclosing_symbol: reference.enclosing_symbol,
            start_line: reference.start_line,
            end_line: reference.end_line,
            start_byte: reference.start_byte,
            end_byte: reference.end_byte,
        })
        .collect();
    let imports = parsed
        .imports
        .into_iter()
        .map(|import| ImportInput {
            raw_target: import.raw_target,
            resolved_path: import.resolved_path,
            candidate_paths: Vec::new(),
            line: import.line,
        })
        .collect();
    record_preparation_duration(diagnostics, projection_started, |detail| {
        &mut detail.projection
    });

    Ok(PreparedFile::Indexed(
        Box::new(IndexedFile {
            path: file.relative_path.clone(),
            language: parsed.language,
            structurally_complete: parsed.structurally_complete,
            size_bytes,
            modified_ns: file.modified_ns,
            content_hash,
            chunks,
            symbols,
            references,
            imports,
        }),
        source_token_count,
        warning,
    ))
}

fn record_preparation_duration(
    diagnostics: Option<&mut FilePreparationDiagnostics>,
    started: Option<Instant>,
    select: impl FnOnce(&mut FilePreparationDiagnostics) -> &mut Duration,
) {
    if let (Some(diagnostics), Some(started)) = (diagnostics, started) {
        *select(diagnostics) += started.elapsed();
    }
}

fn read_bounded(root: &Dir, path: &str, max_bytes: u64) -> std::io::Result<Option<Vec<u8>>> {
    read_bounded_file(root.open(path)?.into_std(), max_bytes)
}

#[cfg(test)]
fn read_bounded_path(path: &Path, max_bytes: u64) -> std::io::Result<Option<Vec<u8>>> {
    read_bounded_file(fs::File::open(path)?, max_bytes)
}

fn read_bounded_file(file: fs::File, max_bytes: u64) -> std::io::Result<Option<Vec<u8>>> {
    let mut bytes =
        Vec::with_capacity(usize::try_from(max_bytes.min(64 * 1024)).unwrap_or(64 * 1024));
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        Ok(None)
    } else {
        Ok(Some(bytes))
    }
}

fn push_warning(warnings: &mut Vec<String>, warning: String) {
    const MAX_WARNINGS: usize = 100;
    if warnings.len() < MAX_WARNINGS {
        warnings.push(warning);
    }
}

/// Return whether the on-disk file still matches the indexed content hash.
///
/// Used when size and mtime look unchanged so full reconcile cannot skip a
/// content rewrite that preserved those metadata fields.
fn content_unchanged(root: &Dir, path: &str, expected_hash: &str, max_file_bytes: u64) -> bool {
    match read_bounded(root, path, max_file_bytes) {
        Ok(Some(bytes)) => hash_bytes(&bytes) == expected_hash,
        Err(_) => false,
        Ok(None) => false,
    }
}
