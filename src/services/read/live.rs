// Re-tokenize bounded candidate windows instead of guessing a byte/token ratio.
// The hard cap prevents pathological low-token inputs from growing forever.
const LIVE_READ_TOKEN_CHECK_BYTES: usize = 64 * 1024;
const MAX_LIVE_READ_BYTES: usize = 8 * 1024 * 1024;

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

fn resolve_read_target(
    session: &ReadSession,
    file_id: i64,
    request: &ReadRequest,
    generation: u64,
) -> Result<ResolvedReadTarget> {
    if let Some(cursor) = request.continuation_cursor.as_deref() {
        let cursor = parse_read_cursor(cursor, generation, &request.path)?;
        return Ok(ResolvedReadTarget {
            target_start_line: cursor.target_start_line,
            target_end_line: Some(cursor.target_end_line),
            page_start_line: cursor.next_start_line,
            page_start_byte: cursor.next_byte,
            expected_full_hash: Some(cursor.full_hash),
        });
    }

    let (target_start_line, target_end_line) = if let Some(symbol_name) = &request.symbol {
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
    } else if let Some(heading_name) = &request.heading {
        let occurrence = request.heading_occurrence.unwrap_or(1);
        let heading = session
            .find_document_heading(file_id, heading_name, occurrence)?
            .ok_or_else(|| Error::HeadingNotFound {
                path: request.path.clone(),
                heading: heading_name.clone(),
                occurrence,
            })?;
        (heading.start_line, Some(heading.end_line))
    } else {
        (request.start_line.unwrap_or(1), request.end_line)
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
    })
}

/// Stream a file without loading it into memory and capture its live identity.
fn stream_snapshot(file: &File) -> Result<LiveFileSnapshot> {
    let mut file = file.try_clone()?;
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
        content_hash: hasher.finalize().to_hex()[..crate::text::CONTENT_FINGERPRINT_HEX_LEN]
            .to_string(),
        end_line,
    })
}

/// Hash the live file and read a resolved range in one forward stream.
fn observe_live_range(
    file: &File,
    target_start_line: usize,
    target_end_line: Option<usize>,
    page_start_byte: usize,
    max_tokens: usize,
    tokenizer: crate::tokens::Tokenizer,
) -> Result<LiveReadObservation> {
    let mut file = file.try_clone()?;
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
        hasher.update(buffer);
        bytes_seen = bytes_seen.saturating_add(buffer.len());
        newline_count =
            newline_count.saturating_add(buffer.iter().filter(|byte| **byte == b'\n').count());
        last_byte_was_newline = buffer.last() == Some(&b'\n');

        let mut validation_chunk = Vec::new();
        if !target_finished {
            for &byte in buffer {
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
        let consumed = buffer.len();
        reader.consume(consumed);
        validate_utf8_chunk(
            &mut utf8_pending,
            &validation_chunk,
            target_finished || consumed == 0,
        )?;

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
            content_hash: hasher.finalize().to_hex()[..crate::text::CONTENT_FINGERPRINT_HEX_LEN]
                .to_string(),
            end_line,
        },
        range: LiveReadRange {
            content,
            page_start_line,
            target_bytes,
        },
    })
}

fn validate_utf8_chunk(pending: &mut Vec<u8>, bytes: &[u8], final_chunk: bool) -> Result<()> {
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

fn invalid_line_range() -> Error {
    Error::InvalidInput {
        field: "line range",
        reason: "must be ordered and within the requested file",
    }
}
