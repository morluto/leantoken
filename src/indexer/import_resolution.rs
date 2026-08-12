use super::*;

/// Preserve parser-observed import text without claiming compiler semantics.
/// Language-specific compiler/LSP adapters may populate resolved edges later;
/// the generic repository index intentionally does not guess them.
pub(super) fn resolve_imports(
    files: &mut [IndexedFile],
    _repository_paths: &HashSet<String>,
    cancellation: &CancellationToken,
) -> Result<()> {
    for file in files {
        check_cancelled(cancellation)?;
        for import in &mut file.imports {
            import.candidate_paths.clear();
            import.resolved_path = None;
        }
    }
    Ok(())
}
