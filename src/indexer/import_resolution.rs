use super::*;

pub(super) const MAX_IMPORT_TARGET_BYTES: usize = 16 * 1024;
// Every Rust module base expands to at most `path.rs` and `path/mod.rs`.
pub(super) const MAX_RUST_MODULE_BASES: usize = MAX_IMPORT_CANDIDATES_PER_IMPORT / 2;

pub(super) fn resolve_imports(
    files: &mut [IndexedFile],
    repository_paths: &HashSet<String>,
    cancellation: &CancellationToken,
) -> Result<()> {
    for file in files {
        check_cancelled(cancellation)?;
        for import in &mut file.imports {
            check_cancelled(cancellation)?;
            let projection =
                derive_import_projection(&file.path, &import.raw_target, repository_paths);
            import.resolved_path = projection.resolved_path;
            import.candidate_paths = projection.candidate_paths;
        }
    }
    Ok(())
}

pub(super) fn derive_import_projection(
    source_path: &str,
    raw_target: &str,
    repository_paths: &HashSet<String>,
) -> ImportProjectionValue {
    let candidate_paths = import_candidates(source_path, raw_target);
    let resolved_path = resolve_import_candidates(&candidate_paths, repository_paths);
    ImportProjectionValue {
        resolved_path,
        candidate_paths,
    }
}

pub(super) fn import_candidates(source_path: &str, raw_target: &str) -> Vec<String> {
    if raw_target.len() > MAX_IMPORT_TARGET_BYTES {
        return Vec::new();
    }
    let source = std::path::Path::new(source_path);
    match ImportResolutionPolicy::for_source(source) {
        ImportResolutionPolicy::Python => python_import_candidates(source, raw_target),
        ImportResolutionPolicy::JavaScript => javascript_import_candidates(source_path, raw_target),
        ImportResolutionPolicy::TypeScript => typescript_import_candidates(source_path, raw_target),
        ImportResolutionPolicy::Rust => rust_import_candidates(source, raw_target),
        ImportResolutionPolicy::Latex => latex_import_candidates(source_path, raw_target),
        ImportResolutionPolicy::WebResource => {
            web_resource_import_candidates(source_path, raw_target)
        }
        ImportResolutionPolicy::Go => go_import_candidates(source, raw_target),
        ImportResolutionPolicy::Unsupported => Vec::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImportResolutionPolicy {
    Python,
    JavaScript,
    TypeScript,
    Rust,
    Latex,
    WebResource,
    Go,
    Unsupported,
}

impl ImportResolutionPolicy {
    fn for_source(source: &std::path::Path) -> Self {
        match source.extension().and_then(|extension| extension.to_str()) {
            Some("py" | "pyi") => Self::Python,
            Some("js" | "mjs" | "cjs" | "jsx") => Self::JavaScript,
            Some("ts" | "mts" | "cts" | "tsx") => Self::TypeScript,
            Some("rs") => Self::Rust,
            Some("tex" | "ltx") => Self::Latex,
            Some("html" | "htm") => Self::WebResource,
            Some("go") => Self::Go,
            _ => Self::Unsupported,
        }
    }
}

fn python_import_candidates(source: &std::path::Path, raw_target: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    for base in python_module_paths(source, raw_target) {
        let Some(base) = normalize_relative(&base) else {
            continue;
        };
        // CPython checks a package directory before a same-named module file.
        for extension in ["py", "pyi"] {
            push_candidate(
                &mut candidates,
                base.join("__init__").with_extension(extension),
            );
        }
        for extension in ["py", "pyi"] {
            push_candidate(&mut candidates, base.with_extension(extension));
        }
    }
    candidates
}

fn javascript_import_candidates(source_path: &str, raw_target: &str) -> Vec<String> {
    let Some(base) = relative_import_base(source_path, raw_target) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    if base.extension().is_some() {
        push_candidate(&mut candidates, base);
        return candidates;
    }
    for extension in ["js", "mjs", "cjs", "jsx"] {
        push_candidate(&mut candidates, base.with_extension(extension));
    }
    for extension in ["js", "mjs", "cjs", "jsx"] {
        push_candidate(
            &mut candidates,
            base.join("index").with_extension(extension),
        );
    }
    candidates
}

fn typescript_import_candidates(source_path: &str, raw_target: &str) -> Vec<String> {
    let Some(base) = relative_import_base(source_path, raw_target) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    let extension = base.extension().and_then(|extension| extension.to_str());
    let source_extension = std::path::Path::new(source_path)
        .extension()
        .and_then(|extension| extension.to_str());
    // `.mts` establishes Node ESM semantics without consulting project
    // configuration. Extensionless relative imports are invalid there, so do
    // not apply the classic/bundler heuristic used for context-dependent `.ts`
    // and `.tsx` sources.
    if extension.is_none() && source_extension == Some("mts") {
        return candidates;
    }
    let substitutions: &[&str] = match extension {
        Some("js") => &["ts", "tsx", "d.ts", "js", "jsx"],
        Some("jsx") => &["tsx", "d.ts", "jsx"],
        Some("mjs") => &["mts", "d.mts", "mjs"],
        Some("cjs") => &["cts", "d.cts", "cjs"],
        Some(_) => &[],
        // TypeScript may add the runtime `.js` family under classic,
        // CommonJS, or bundler resolution, then substitute its source/type
        // counterparts. It never implicitly adds `.mjs`/`.mts` or
        // `.cjs`/`.cts`; those runtime extensions must be explicit.
        None => &["ts", "tsx", "d.ts", "js", "jsx"],
    };
    if substitutions.is_empty() {
        push_candidate(&mut candidates, base);
        return candidates;
    }
    for substitution in substitutions {
        push_candidate(&mut candidates, base.with_extension(substitution));
    }
    if extension.is_none() {
        for substitution in substitutions {
            push_candidate(
                &mut candidates,
                base.join("index").with_extension(substitution),
            );
        }
    }
    candidates
}

fn rust_import_candidates(source: &std::path::Path, raw_target: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    for base in rust_module_paths(source, raw_target) {
        let Some(base) = normalize_relative(&base) else {
            continue;
        };
        if base.extension().is_some() {
            push_candidate(&mut candidates, base);
        } else {
            push_candidate(&mut candidates, base.with_extension("rs"));
            push_candidate(&mut candidates, base.join("mod.rs"));
        }
    }
    candidates
}

fn latex_import_candidates(source_path: &str, raw_target: &str) -> Vec<String> {
    let Some(base) = source_relative_base(source_path, raw_target) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    if base.extension().is_some() {
        push_candidate(&mut candidates, base);
    } else {
        for extension in ["tex", "ltx"] {
            push_candidate(&mut candidates, base.with_extension(extension));
        }
    }
    candidates
}

fn web_resource_import_candidates(source_path: &str, raw_target: &str) -> Vec<String> {
    // HTML resource URLs have much broader semantics than repository paths.
    // Preserve the existing safe subset: an explicit dot-relative URL with no
    // query or fragment, resolved to exactly one repository-owned file.
    if raw_target.contains(['?', '#']) {
        return Vec::new();
    }
    let Some(base) = relative_import_base(source_path, raw_target) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    push_candidate(&mut candidates, base);
    candidates
}

fn relative_import_base(source_path: &str, raw_target: &str) -> Option<std::path::PathBuf> {
    if !raw_target.starts_with('.') {
        return None;
    }
    source_relative_base(source_path, raw_target)
}

fn source_relative_base(source_path: &str, raw_target: &str) -> Option<std::path::PathBuf> {
    crate::repository::RepositoryPath::parse(source_path)
        .ok()?
        .join_relative(raw_target)
        .ok()
        .map(|path| std::path::PathBuf::from(path.as_str()))
}

fn push_candidate(candidates: &mut Vec<String>, candidate: std::path::PathBuf) {
    if candidates.len() >= MAX_IMPORT_CANDIDATES_PER_IMPORT {
        return;
    }
    let Some(candidate) = normalize_relative(&candidate) else {
        return;
    };
    let candidate = candidate.to_string_lossy().replace('\\', "/");
    if candidate.len() > MAX_IMPORT_CANDIDATE_PATH_BYTES {
        return;
    }
    if !candidates.contains(&candidate) {
        candidates.push(candidate);
    }
}

pub(super) fn python_module_paths(
    source: &std::path::Path,
    raw_target: &str,
) -> Vec<std::path::PathBuf> {
    let level = raw_target.bytes().take_while(|byte| *byte == b'.').count();
    let module = raw_target[level..].replace('.', "/");
    if level == 0 {
        let module = std::path::PathBuf::from(module);
        let mut paths = Vec::with_capacity(2);
        if let Some(root) = conventional_python_source_root(source) {
            paths.push(root.join(&module));
        }
        paths.push(module);
        paths.dedup();
        return paths;
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

fn conventional_python_source_root(source: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut prefix = std::path::PathBuf::new();
    let mut source_root = None;
    for component in source.parent()?.components() {
        let std::path::Component::Normal(component) = component else {
            return None;
        };
        prefix.push(component);
        if component == "src" {
            source_root = Some(prefix.clone());
        }
    }
    source_root
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
    let crate_root = rust_crate_roots(source)
        .into_iter()
        .next()
        .unwrap_or_default();
    let (stripped, roots) = if let Some(target) = trimmed.strip_prefix("crate::") {
        (target, vec![crate_root.clone()])
    } else if let Some(target) = trimmed.strip_prefix("self::") {
        (target, vec![rust_source_module_dir(source, &crate_root)])
    } else if trimmed.starts_with("super::") {
        let mut target = trimmed;
        let mut root = rust_source_module_dir(source, &crate_root);
        while let Some(rest) = target.strip_prefix("super::") {
            if root == crate_root || !root.pop() {
                return Vec::new();
            }
            target = rest;
        }
        (target, vec![root])
    } else {
        (trimmed, vec![crate_root])
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
            if targets.len() > MAX_RUST_MODULE_BASES {
                return Vec::new();
            }
        }
    } else {
        let single = stripped.split(" as ").next().unwrap_or(stripped).trim();
        if !single.is_empty() {
            targets.push(single.to_string());
        }
    }

    let mut parsed_targets = Vec::with_capacity(targets.len());
    let mut required_bases = 0usize;
    for target in &targets {
        let segments = target
            .split("::")
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        if segments.is_empty() {
            continue;
        }
        required_bases = match required_bases.checked_add(segments.len()) {
            Some(total) if total <= MAX_RUST_MODULE_BASES => total,
            _ => return Vec::new(),
        };
        parsed_targets.push(segments);
    }

    let mut bases = Vec::with_capacity(required_bases);
    for segments in parsed_targets {
        for prefix_len in (1..=segments.len()).rev() {
            let path_str = segments[..prefix_len].join("/");
            for root in &roots {
                bases.push(root.join(&path_str));
            }
        }
    }
    bases
}

pub(super) fn rust_source_module_dir(
    source: &std::path::Path,
    crate_root: &std::path::Path,
) -> std::path::PathBuf {
    let parent = source.parent().unwrap_or_else(|| std::path::Path::new(""));
    match source.file_stem().and_then(|stem| stem.to_str()) {
        Some("lib" | "main" | "mod") | None => parent.to_path_buf(),
        Some(_)
            if parent.as_os_str().is_empty()
                || (parent == crate_root
                    && parent.file_name().is_some_and(|name| {
                        matches!(
                            name.to_str(),
                            Some("bin" | "tests" | "examples" | "benches")
                        )
                    })) =>
        {
            parent.to_path_buf()
        }
        Some(stem) => parent.join(stem),
    }
}

pub(super) fn rust_crate_roots(source: &std::path::Path) -> Vec<std::path::PathBuf> {
    let components = source
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(component) => Some(component.to_os_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let file_index = components.len().saturating_sub(1);
    let mut root = std::path::PathBuf::new();

    if let Some(index) = components[..file_index]
        .iter()
        .rposition(|component| component == "src")
    {
        for component in &components[..=index] {
            root.push(component);
        }
        if components
            .get(index + 1)
            .is_some_and(|value| value == "bin")
        {
            root.push("bin");
            if file_index > index + 2 {
                root.push(&components[index + 2]);
            }
        }
        return vec![root];
    }

    if let Some(index) = components[..file_index]
        .iter()
        .rposition(|component| matches!(component.to_str(), Some("tests" | "examples" | "benches")))
    {
        for component in &components[..=index] {
            root.push(component);
        }
        if file_index > index + 1 {
            root.push(&components[index + 1]);
        }
        return vec![root];
    }

    vec![
        source
            .parent()
            .unwrap_or_else(|| std::path::Path::new(""))
            .to_path_buf(),
    ]
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

/// Resolve Go imports using module-path to directory mapping.
///
/// Go imports are module paths like "example.com/acme/pkg/internal/service".
/// We resolve them by finding go.mod files in the repository, parsing the
/// `module` directive, and mapping the import path to a directory by
/// stripping the module prefix and appending the remainder as a relative path.
fn go_import_candidates(source: &std::path::Path, raw_target: &str) -> Vec<String> {
    let target = raw_target.trim_matches(|c| c == '"' || c == '\'');
    if target.is_empty() {
        return Vec::new();
    }
    // Walk up from the source file to find a go.mod in an ancestor directory.
    let mut dir = source.parent();
    while let Some(parent) = dir {
        let go_mod = parent.join("go.mod");
        if go_mod.is_file() {
            if let Ok(content) = std::fs::read_to_string(&go_mod) {
                for line in content.lines() {
                    let line = line.trim();
                    if let Some(module_path) = line.strip_prefix("module ") {
                        let module_path = module_path.trim();
                        if !module_path.is_empty() && target.starts_with(module_path) {
                            // Strip the module prefix to get the package-relative path
                            let remainder = &target[module_path.len()..];
                            let remainder = remainder.strip_prefix('/').unwrap_or(remainder);
                            if remainder.is_empty() {
                                // Import is the module root itself
                                return vec![".".into()];
                            }
                            // Convert the module-relative path to a directory path
                            let path = std::path::Path::new(remainder)
                                .components()
                                .map(|c| c.as_os_str().to_owned())
                                .collect::<std::path::PathBuf>();
                            return vec![path.to_string_lossy().into_owned()];
                        }
                    }
                }
            }
            break; // Found go.mod but module path didn't match — stop walking up
        }
        dir = parent.parent();
    }
    Vec::new()
}
