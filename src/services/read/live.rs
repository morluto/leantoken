// Re-tokenize bounded candidate windows instead of guessing a byte/token ratio.
// The hard cap prevents pathological low-token inputs from growing forever.
pub(super) const LIVE_READ_TOKEN_CHECK_BYTES: usize = 64 * 1024;
pub(super) const MAX_LIVE_READ_BYTES: usize = 8 * 1024 * 1024;

pub(in crate::services) fn open_live_file(services: &Services, path: &str) -> Result<File> {
    services
        .repository_root
        .open(path)
        .map(cap_std::fs::File::into_std)
        .map_err(|open_error| {
            // The capability open is authoritative for access. Canonicalization is
            // only used after refusal to preserve the public escape classification.
            match resolve_existing(&services.config.root, path) {
                Err(Error::PathOutsideRoot(external)) => Error::PathOutsideRoot(external),
                _ => Error::Io(open_error),
            }
        })
}

pub(super) fn resolve_read_target(
    session: &IndexReadSnapshot,
    file_id: i64,
    request: &ReadRequest,
    target: &ParsedReadTarget,
    generation: u64,
) -> Result<ResolvedReadTarget> {
    let (target_start_line, target_end_line) = match target {
        ParsedReadTarget::Continuation(cursor) => {
            validate_read_cursor(cursor, generation, &request.path)?;
            if cursor.full != matches!(request.policy, ReadPolicy::Full) {
                return Err(Error::StaleCursor);
            }
            return Ok(ResolvedReadTarget {
                target_start_line: cursor.target_start_line,
                target_end_line: cursor.target_end_line,
                page_start_line: cursor.next_start_line,
                page_start_byte: cursor.next_byte,
                expected_full_hash: cursor.full_hash.clone(),
                expected_prefix_hash: cursor.prefix_hash.clone(),
                expected_file_size: Some(cursor.file_size),
                expected_modified_ns: cursor.modified_ns,
                cursor_full: cursor.full,
            });
        }
        ParsedReadTarget::Symbol(symbol_name) => {
            let symbol = match session.find_symbol(file_id, symbol_name)? {
                crate::symbol_identity::SymbolResolution::Unique(symbol) => symbol,
                crate::symbol_identity::SymbolResolution::NotFound => {
                    return Err(Error::SymbolNotFound {
                        path: request.path.clone(),
                        symbol: symbol_name.clone(),
                    });
                }
                crate::symbol_identity::SymbolResolution::Ambiguous => {
                    return Err(Error::AmbiguousSymbol {
                        path: request.path.clone(),
                        symbol: symbol_name.clone(),
                    });
                }
            };
            (symbol.start_line, Some(symbol.end_line))
        }
        ParsedReadTarget::Heading { name, occurrence } => {
            let heading = session
                .find_document_heading(file_id, name, occurrence.get())?
                .ok_or_else(|| Error::HeadingNotFound {
                    path: request.path.clone(),
                    heading: name.clone(),
                    occurrence: occurrence.get(),
                })?;
            (heading.start_line, Some(heading.end_line))
        }
        ParsedReadTarget::Lines { start, end } => (start.get(), *end),
    };

    if target_start_line == 0
        || target_end_line.is_some_and(|end_line| end_line < target_start_line)
    {
        return Err(invalid_line_range());
    }
    Ok(ResolvedReadTarget {
        target_start_line,
        target_end_line,
        page_start_line: target_start_line,
        page_start_byte: 0,
        expected_full_hash: None,
        expected_prefix_hash: None,
        expected_file_size: None,
        expected_modified_ns: None,
        cursor_full: matches!(request.policy, ReadPolicy::Full),
    })
}

fn modified_ns(file: &File) -> Result<Option<u128>> {
    Ok(file
        .metadata()?
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos()))
}

/// Stream a file without loading it into memory and capture its live identity.
pub(super) fn stream_snapshot(file: &File) -> Result<LiveFileSnapshot> {
    let mut file = file.try_clone()?;
    let file_size = file.metadata()?.len().try_into().unwrap_or(usize::MAX);
    let file_modified_ns = modified_ns(&file)?;
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 65_536];
    let mut bytes_seen = 0usize;
    let mut newline_count = 0usize;
    let mut last_byte_was_newline = false;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        bytes_seen = bytes_seen.saturating_add(n);
        newline_count =
            newline_count.saturating_add(buf[..n].iter().filter(|byte| **byte == b'\n').count());
        last_byte_was_newline = buf[n - 1] == b'\n';
    }
    let end_line = if bytes_seen == 0 {
        1
    } else {
        newline_count.saturating_add(usize::from(!last_byte_was_newline))
    };
    Ok(LiveFileSnapshot {
        content_hash: Some(
            hasher.finalize().to_hex()[..crate::text::CONTENT_FINGERPRINT_HEX_LEN].to_string(),
        ),
        end_line,
        bytes_read: bytes_seen,
        file_size,
        modified_ns: file_modified_ns,
    })
}

/// Hash the live file and read a resolved range in one forward stream.
///
/// When `full` is true the complete file is hashed and `content_hash` is
/// populated. When `full` is false the stream stops as soon as the target
/// range is satisfied or the token bound is reached, `content_hash` is `None`,
/// and `bytes_read` reflects only the bytes consumed before early termination.
pub(super) fn observe_live_range(
    file: &File,
    target_start_line: usize,
    target_end_line: Option<usize>,
    page_start_byte: usize,
    max_tokens: usize,
    tokenizer: crate::tokens::Tokenizer,
    full: bool,
) -> Result<LiveReadObservation> {
    let mut file = file.try_clone()?;
    let file_size = file.metadata()?.len().try_into().unwrap_or(usize::MAX);
    let file_modified_ns = modified_ns(&file)?;
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut hasher = blake3::Hasher::new();
    let mut selected = Vec::with_capacity(LIVE_READ_TOKEN_CHECK_BYTES);
    let mut current_line = 1usize;
    let mut target_finished = false;
    let mut token_bound_reached = false;
    let mut final_target_checked = false;
    let mut target_bytes = 0usize;
    let mut page_start_line = target_start_line;
    let mut next_token_check = LIVE_READ_TOKEN_CHECK_BYTES;
    let mut utf8_pending = Vec::new();
    let mut bytes_seen = 0usize;
    let mut newline_count = 0usize;
    let mut last_byte_was_newline = false;
    let requested_end_line = target_end_line.unwrap_or(usize::MAX);

    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            break;
        }
        let mut validation_chunk = Vec::new();
        let mut consumed = 0usize;
        if !target_finished {
            for &byte in buffer {
                consumed = consumed.saturating_add(1);
                let in_target =
                    current_line >= target_start_line && current_line <= requested_end_line;
                if in_target {
                    validation_chunk.push(byte);
                    if target_bytes < page_start_byte {
                        if byte == b'\n' {
                            page_start_line = current_line.saturating_add(1);
                        }
                    } else if !token_bound_reached {
                        selected.push(byte);
                    }
                    target_bytes = target_bytes.saturating_add(1);
                }
                if byte == b'\n' {
                    if requested_end_line == current_line {
                        target_finished = true;
                        break;
                    }
                    current_line = current_line.saturating_add(1);
                }
            }
        }
        // For full reads, always consume the entire buffer so the complete file
        // is hashed. For bounded reads, consume only what the target scan used.
        if full && consumed < buffer.len() {
            consumed = buffer.len();
        }
        if consumed == 0 {
            consumed = buffer.len();
        }
        let partial_buffer = consumed < buffer.len();
        let consumed_buffer = &buffer[..consumed];
        if full {
            hasher.update(consumed_buffer);
        }
        bytes_seen = bytes_seen.saturating_add(consumed);
        newline_count = newline_count.saturating_add(
            consumed_buffer
                .iter()
                .filter(|byte| **byte == b'\n')
                .count(),
        );
        last_byte_was_newline = consumed_buffer.last() == Some(&b'\n');
        let final_chunk = target_finished || consumed == 0 || partial_buffer;
        reader.consume(consumed);
        validate_utf8_chunk(&mut utf8_pending, &validation_chunk, final_chunk)?;

        if !token_bound_reached
            && ((target_finished && !final_target_checked)
                || selected.len() >= next_token_check
                || selected.len() >= MAX_LIVE_READ_BYTES)
        {
            match std::str::from_utf8(&selected) {
                Ok(content) if tokenizer.count(content) > max_tokens => {
                    token_bound_reached = true;
                    final_target_checked = target_finished;
                }
                Ok(_) => {
                    if selected.len() >= MAX_LIVE_READ_BYTES {
                        return Err(Error::LimitExceeded);
                    }
                    next_token_check = selected.len().saturating_add(LIVE_READ_TOKEN_CHECK_BYTES);
                    final_target_checked = target_finished;
                }
                Err(error) if error.error_len().is_none() => {
                    if target_finished || selected.len() >= MAX_LIVE_READ_BYTES {
                        return Err(Error::InvalidInput {
                            field: "path",
                            reason: "must identify UTF-8 text",
                        });
                    }
                }
                Err(_) => {
                    return Err(Error::InvalidInput {
                        field: "path",
                        reason: "must identify UTF-8 text",
                    });
                }
            }
        }
        // Bounded reads stop as soon as the target is finished or the token
        // bound is reached. Full reads continue to EOF to hash the complete
        // file.
        if !full && (target_finished || token_bound_reached) {
            break;
        }
    }

    validate_utf8_chunk(&mut utf8_pending, &[], true)?;
    if !utf8_pending.is_empty() {
        return Err(Error::InvalidInput {
            field: "path",
            reason: "must identify UTF-8 text",
        });
    }

    if page_start_byte > target_bytes {
        return Err(Error::StaleCursor);
    }
    let content = String::from_utf8(selected).map_err(|_| Error::InvalidInput {
        field: "path",
        reason: "must identify UTF-8 text",
    })?;
    let end_line = if bytes_seen == 0 {
        1
    } else {
        newline_count.saturating_add(usize::from(!last_byte_was_newline))
    };
    Ok(LiveReadObservation {
        snapshot: LiveFileSnapshot {
            content_hash: full.then(|| {
                hasher.finalize().to_hex()[..crate::text::CONTENT_FINGERPRINT_HEX_LEN].to_string()
            }),
            end_line,
            bytes_read: bytes_seen,
            file_size,
            modified_ns: file_modified_ns,
        },
        range: LiveReadRange {
            content,
            page_start_line,
            target_bytes,
        },
    })
}

/// Hash only the requested target prefix represented by a bounded cursor.
/// The read is capped by the cursor's already-bounded byte offset and does
/// not load or hash the rest of a large file.
pub(super) fn hash_live_range_prefix(
    file: &File,
    target_start_line: usize,
    target_end_line: Option<usize>,
    prefix_bytes: usize,
) -> Result<String> {
    if prefix_bytes > MAX_LIVE_READ_BYTES {
        return Err(Error::StaleCursor);
    }
    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut current_line = 1usize;
    let mut selected = Vec::with_capacity(prefix_bytes);
    let requested_end_line = target_end_line.unwrap_or(usize::MAX);
    let mut buffer = [0u8; 65_536];
    while selected.len() < prefix_bytes {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        for &byte in &buffer[..read] {
            if current_line >= target_start_line && current_line <= requested_end_line {
                selected.push(byte);
                if selected.len() == prefix_bytes {
                    break;
                }
            }
            if byte == b'\n' {
                if current_line == requested_end_line {
                    return Err(Error::StaleCursor);
                }
                current_line = current_line.saturating_add(1);
            }
        }
    }
    if selected.len() != prefix_bytes {
        return Err(Error::StaleCursor);
    }
    Ok(hash(std::str::from_utf8(&selected).map_err(|_| {
        Error::InvalidInput {
            field: "path",
            reason: "must identify UTF-8 text",
        }
    })?))
}

pub(super) fn validate_utf8_chunk(
    pending: &mut Vec<u8>,
    bytes: &[u8],
    final_chunk: bool,
) -> Result<()> {
    pending.extend_from_slice(bytes);
    match std::str::from_utf8(pending) {
        Ok(_) => {
            pending.clear();
            Ok(())
        }
        Err(error) if error.error_len().is_none() && !final_chunk => {
            let valid_up_to = error.valid_up_to();
            pending.drain(..valid_up_to);
            Ok(())
        }
        Err(_) => Err(Error::InvalidInput {
            field: "path",
            reason: "must identify UTF-8 text",
        }),
    }
}

pub(super) fn invalid_line_range() -> Error {
    Error::InvalidInput {
        field: "line range",
        reason: "must be ordered and within the requested file",
    }
}
use super::*;
