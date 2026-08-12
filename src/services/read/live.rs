pub(super) fn resolve_read_target(
    services: &Services,
    session: &IndexReadSnapshot,
    file_id: i64,
    request: &ReadInput,
    generation: u64,
) -> Result<ResolvedReadTarget> {
    let target = match &request.mode {
        ReadMode::Direct(ReadTargetInput::Continuation(cursor)) => {
            let digest = read_request_digest(&request.path, request.policy)?;
            let cursor: ReadPosition = services.cursor_codec.open(cursor, generation, &digest)?;
            return Ok(ResolvedReadTarget {
                target_start_line: cursor.target_start_line,
                target_end_line: cursor.target_end_line,
                page_start_line: cursor.next_start_line,
                page_start_byte: cursor.next_byte,
            });
        }
        ReadMode::Direct(ReadTargetInput::New(target)) => target,
    };
    let (target_start_line, target_end_line) = match target {
        NewReadTarget::Symbol(symbol_name) => {
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
        NewReadTarget::Heading { name, occurrence } => {
            let heading = session
                .find_document_heading(file_id, name, occurrence.get())?
                .ok_or_else(|| Error::HeadingNotFound {
                    path: request.path.clone(),
                    heading: name.clone(),
                    occurrence: occurrence.get(),
                })?;
            (heading.start_line, Some(heading.end_line))
        }
        NewReadTarget::Lines { start, end } => (start.get(), *end),
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
    })
}

pub(super) fn invalid_line_range() -> Error {
    Error::InvalidInput {
        field: "line range",
        reason: "must be ordered and within the requested file",
    }
}
use super::*;
