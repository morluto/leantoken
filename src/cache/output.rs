fn scope_label(entry: &CacheEntry) -> String {
    match entry.index_scope {
        IndexScopeMode::Full => "scope=full".into(),
        IndexScopeMode::Scoped => entry.index_scope_digest.as_deref().map_or_else(
            || "scope=scoped".into(),
            |digest| format!("scope=scoped:{digest}"),
        ),
    }
}

/// Print a cache-list report as JSON or concise human-readable output.
pub fn print_list(report: &CacheListReport, json_output: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    if json_output {
        serde_json::to_writer(&mut output, report)?;
        output.write_all(b"\n")?;
        return Ok(());
    }
    writeln!(
        output,
        "Managed cache root: {}",
        report.cache_root.display()
    )?;
    writeln!(
        output,
        "{} total cache(s), {} bytes; {} matched, {} bytes; {} returned",
        report.total_entries,
        report.total_bytes,
        report.matched_entries,
        report.matched_bytes,
        report.returned_entries
    )?;
    let state_counts = report
        .state_counts
        .iter()
        .map(|(state, count)| format!("{state}={count}"))
        .collect::<Vec<_>>()
        .join(" ");
    writeln!(
        output,
        "states: {state_counts}; active={}; missing_roots={}; ignored={}",
        report.active_entries, report.missing_root_entries, report.ignored_entries
    )?;
    for entry in &report.entries {
        writeln!(
            output,
            "{}  {} bytes  {}  {}  {}  last_access={}  root_available={}  {}",
            entry.id,
            entry.size_bytes,
            if entry.active { "active" } else { "inactive" },
            entry.state.label(),
            scope_label(entry),
            entry
                .last_access_unix_seconds
                .map_or_else(|| "unknown".into(), |timestamp| timestamp.to_string()),
            entry
                .repository_available
                .map_or("unknown", |available| if available { "yes" } else { "no" }),
            entry
                .repository_root
                .as_deref()
                .map_or_else(|| "unknown root".into(), |root| root.display().to_string())
        )?;
    }
    if let Some(cursor) = &report.next_cursor {
        writeln!(output, "next_cursor={cursor}")?;
    }
    Ok(())
}

/// Print a versioned cache-list report as JSON or concise human-readable output.
pub fn print_list_v2(report: &CacheListV2Report, json_output: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    if json_output {
        serde_json::to_writer(&mut output, report)?;
        output.write_all(b"\n")?;
        return Ok(());
    }
    writeln!(
        output,
        "Managed cache root: {}",
        report.cache_root.display()
    )?;
    writeln!(
        output,
        "{} total cache(s), {} bytes; {} matched, {} bytes; {} returned",
        report.total_entries,
        report.total_bytes,
        report.matched_entries,
        report.matched_bytes,
        report.returned_entries
    )?;
    let state_counts = report
        .state_counts
        .iter()
        .map(|(state, count)| format!("{state}={count}"))
        .collect::<Vec<_>>()
        .join(" ");
    let compatibility_counts = report
        .compatibility_counts
        .iter()
        .map(|(compatibility, summary)| {
            format!("{compatibility}={}/{}B", summary.entries, summary.bytes)
        })
        .collect::<Vec<_>>()
        .join(" ");
    writeln!(
        output,
        "states: {state_counts}; active={}; missing_roots={}; ignored={}",
        report.active_entries, report.missing_root_entries, report.ignored_entries
    )?;
    writeln!(
        output,
        "compatibility: {compatibility_counts}; safely_reclaimable={}/{}B",
        report.safely_reclaimable_incompatible_entries,
        report.safely_reclaimable_incompatible_bytes
    )?;
    for entry in &report.entries {
        writeln!(
            output,
            "{}  {} bytes  {}  {}  {}  {}  last_access={}  root_available={}  {}",
            entry.entry.id,
            entry.entry.size_bytes,
            if entry.entry.active {
                "active"
            } else {
                "inactive"
            },
            entry.entry.state.label(),
            entry.compatibility.label(),
            scope_label(&entry.entry),
            entry
                .entry
                .last_access_unix_seconds
                .map_or_else(|| "unknown".into(), |timestamp| timestamp.to_string()),
            entry
                .entry
                .repository_available
                .map_or("unknown", |available| if available { "yes" } else { "no" }),
            entry
                .entry
                .repository_root
                .as_deref()
                .map_or_else(|| "unknown root".into(), |root| root.display().to_string())
        )?;
    }
    if let Some(cursor) = &report.next_cursor {
        writeln!(output, "next_cursor={cursor}")?;
    }
    Ok(())
}

/// Print a cache-prune report as JSON or concise human-readable output.
pub fn print_prune(report: &CachePruneReport, json_output: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    if json_output {
        serde_json::to_writer(&mut output, report)?;
        output.write_all(b"\n")?;
        return Ok(());
    }
    writeln!(
        output,
        "Managed cache prune{}: {} -> {} bytes",
        if report.dry_run { " dry-run" } else { "" },
        report.total_bytes_before,
        report.total_bytes_after
    )?;
    for result in &report.results {
        let detail = result.error.as_deref().or(result.detail.as_deref());
        writeln!(
            output,
            "{}  {}  {} bytes{}{}",
            result.action.label(),
            result.id,
            result.size_bytes,
            if result.reasons.is_empty() {
                String::new()
            } else {
                format!("  {}", result.reasons.join(","))
            },
            detail.map_or_else(String::new, |detail| format!("  {detail}"))
        )?;
    }
    Ok(())
}
