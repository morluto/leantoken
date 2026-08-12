use super::*;

pub(super) fn resolve_imports(
    files: &mut [IndexedFile],
    repository_paths: &HashSet<String>,
    cancellation: &CancellationToken,
) -> Result<()> {
    for file in files {
        check_cancelled(cancellation)?;
        for import in &mut file.imports {
            check_cancelled(cancellation)?;
            import.candidate_paths = import_candidates(&file.path, &import.raw_target);
            import.resolved_path =
                resolve_import_candidates(&import.candidate_paths, repository_paths);
        }
    }
    Ok(())
}

pub(super) fn import_candidates(source_path: &str, raw_target: &str) -> Vec<String> {
    let source = std::path::Path::new(source_path);
    let repository_source = crate::repository::RepositoryPath::parse(source_path).ok();
    let mut bases = Vec::new();

    if matches!(
        source.extension().and_then(|ext| ext.to_str()),
        Some("py" | "pyi")
    ) {
        bases.extend(python_module_paths(source, raw_target));
    } else if raw_target.starts_with('.') {
        if let Some(joined) = repository_source
            .as_ref()
            .and_then(|source| source.join_relative(raw_target).ok())
        {
            bases.push(std::path::PathBuf::from(joined.as_str()));
        }
    } else if source.extension().and_then(|ext| ext.to_str()) == Some("rs") {
        bases.extend(rust_module_paths(source, raw_target));
    } else if matches!(
        source.extension().and_then(|ext| ext.to_str()),
        Some("tex" | "ltx")
    ) {
        if let Some(joined) = repository_source
            .as_ref()
            .and_then(|source| source.join_relative(raw_target).ok())
        {
            bases.push(std::path::PathBuf::from(joined.as_str()));
        }
    } else {
        return Vec::new();
    }

    let init_file = match source.extension().and_then(|ext| ext.to_str()) {
        Some("py" | "pyi") => Some("__init__"),
        Some("rs") => Some("mod"),
        _ => None,
    };

    let extensions: &[&str] = match source.extension().and_then(|ext| ext.to_str()) {
        Some("js" | "mjs" | "cjs") => &["", "js", "mjs", "cjs"],
        Some("ts" | "mts" | "cts" | "tsx") => &["", "ts", "tsx", "mts", "cts", "js"],
        Some("py" | "pyi") => &["", "py", "pyi"],
        Some("rs") => &["", "rs"],
        Some("tex" | "ltx") => &["", "tex", "ltx"],
        _ => &[""],
    };
    let mut matches = Vec::new();
    for base in bases {
        let Some(base) = normalize_relative(&base) else {
            continue;
        };
        for extension in extensions {
            let exact = if extension.is_empty() || base.extension().is_some() {
                base.clone()
            } else {
                base.with_extension(extension)
            };
            let directory_init = match init_file {
                Some(init) if extension.is_empty() => base.join(init),
                Some(init) => base.join(init).with_extension(extension),
                None if extension.is_empty() => base.join("index"),
                None => base.join("index").with_extension(extension),
            };
            for candidate in [exact, directory_init] {
                let candidate = if candidate.extension().is_some() || extension.is_empty() {
                    candidate
                } else {
                    candidate.with_extension(extension)
                };
                let candidate = candidate.to_string_lossy().replace('\\', "/");
                if !matches.contains(&candidate) {
                    matches.push(candidate);
                }
            }
        }
    }
    matches
}

pub(super) fn python_module_paths(
    source: &std::path::Path,
    raw_target: &str,
) -> Vec<std::path::PathBuf> {
    let level = raw_target.bytes().take_while(|byte| *byte == b'.').count();
    let module = raw_target[level..].replace('.', "/");
    if level == 0 {
        return vec![std::path::PathBuf::from(module)];
    }

    let mut base = source
        .parent()
        .unwrap_or_else(|| std::path::Path::new(""))
        .to_path_buf();
    for _ in 1..level {
        if !base.pop() {
            return Vec::new();
        }
    }
    if !module.is_empty() {
        base.push(module);
    }
    vec![base]
}

/// Decompose a raw Rust `use` target into candidate module paths.
///
/// Handles grouped imports (`a::{b, c}`), aliases (`a::b as c`), and
/// leading path qualifiers (`crate`, `self`, `super`). For each concrete
/// target, all module prefixes are tried from longest to shortest so that
/// `module::symbol` resolves to `module` when the full `module/symbol`
/// path does not exist.
pub(super) fn rust_module_paths(
    source: &std::path::Path,
    raw_target: &str,
) -> Vec<std::path::PathBuf> {
    let trimmed = raw_target.trim();
    let (stripped, roots) = if let Some(target) = trimmed.strip_prefix("crate::") {
        (target, rust_crate_roots(source))
    } else if let Some(target) = trimmed.strip_prefix("self::") {
        (target, vec![rust_source_module_dir(source)])
    } else if trimmed.starts_with("super::") {
        let mut target = trimmed;
        let mut root = rust_source_module_dir(source);
        while let Some(rest) = target.strip_prefix("super::") {
            if !root.pop() {
                return Vec::new();
            }
            target = rest;
        }
        (target, vec![root])
    } else {
        (trimmed, rust_crate_roots(source))
    };

    let mut targets = Vec::new();
    let before_brace = stripped
        .split('{')
        .next()
        .unwrap_or("")
        .trim_end_matches(':');
    let group_body = stripped.find('{').and_then(|_| {
        stripped
            .split('{')
            .nth(1)
            .and_then(|rest| rest.split('}').next())
    });

    if let Some(group) = group_body {
        let prefix = before_brace.trim_end_matches(':');
        for item in group.split(',') {
            let item = item.trim();
            let item = item.split(" as ").next().unwrap_or(item).trim();
            if item.is_empty() {
                continue;
            }
            let full = if prefix.is_empty() {
                item.to_string()
            } else {
                format!("{prefix}::{item}")
            };
            targets.push(full);
        }
    } else {
        let single = stripped.split(" as ").next().unwrap_or(stripped).trim();
        if !single.is_empty() {
            targets.push(single.to_string());
        }
    }

    let mut bases = Vec::new();
    for target in targets {
        let segments: Vec<&str> = target.split("::").filter(|s| !s.is_empty()).collect();
        if segments.is_empty() {
            continue;
        }
        for prefix_len in (1..=segments.len()).rev() {
            let path_str = segments[..prefix_len].join("/");
            for root in &roots {
                bases.push(root.join(&path_str));
            }
        }
    }
    bases
}

pub(super) fn rust_source_module_dir(source: &std::path::Path) -> std::path::PathBuf {
    let parent = source.parent().unwrap_or_else(|| std::path::Path::new(""));
    match source.file_stem().and_then(|stem| stem.to_str()) {
        Some("lib" | "main" | "mod") | None => parent.to_path_buf(),
        Some(stem) => parent.join(stem),
    }
}

pub(super) fn rust_crate_roots(source: &std::path::Path) -> Vec<std::path::PathBuf> {
    if source.starts_with("src") {
        vec![std::path::PathBuf::from("src"), std::path::PathBuf::new()]
    } else {
        vec![std::path::PathBuf::new(), std::path::PathBuf::from("src")]
    }
}

pub(super) fn resolve_import_candidates(
    candidates: &[String],
    repository_paths: &HashSet<String>,
) -> Option<String> {
    // Candidates are ordered from most-specific to least-specific. Return the
    // first existing candidate; a more specific match always wins over a
    // shorter prefix fallback. This preserves the conservative contract for
    // same-priority candidates (e.g. exact file vs directory init) while
    // allowing module prefix fallback for Rust imports.
    for candidate in candidates {
        if repository_paths.contains(candidate) {
            return Some(candidate.clone());
        }
    }
    None
}

pub(super) fn normalize_relative(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut normalized = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(part) => normalized.push(part),
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => return None,
        }
    }
    Some(normalized)
}
