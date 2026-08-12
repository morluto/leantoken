use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path},
};

use unidiff::{Hunk, PatchSet};

use super::{DynError, Language, LineMap, PatchEvidence, Region};

pub(super) fn extract_patch_evidence(
    patch: &str,
    force_optional: bool,
) -> Result<PatchEvidence, DynError> {
    let mut patch_set = PatchSet::new();
    patch_set.parse(patch)?;
    let mut evidence = PatchEvidence::default();
    for file in patch_set.files() {
        let path = normalize_diff_path(&file.path())?;
        let changed = file.added().saturating_add(file.removed()).max(1);
        if let Some(language) = language_for_path(&path) {
            *evidence.language_weights.entry(language).or_insert(0) += changed;
        }
        if file.is_added_file() {
            evidence.unobservable_added_files += 1;
            continue;
        }
        let destination = if force_optional || is_optional_evidence_path(&path) {
            &mut evidence.optional
        } else {
            &mut evidence.primary
        };
        for hunk in file.hunks() {
            for line in source_anchor_lines(hunk) {
                if line > 0 {
                    destination.entry(path.clone()).or_default().insert(line);
                }
            }
        }
    }
    Ok(evidence)
}

fn source_anchor_lines(hunk: &Hunk) -> BTreeSet<usize> {
    let removed = hunk
        .lines()
        .iter()
        .filter(|line| line.is_removed())
        .filter_map(|line| line.source_line_no)
        .collect::<BTreeSet<_>>();
    if !removed.is_empty() {
        return removed;
    }

    let mut anchors = BTreeSet::new();
    for (index, line) in hunk.lines().iter().enumerate() {
        if !line.is_added() {
            continue;
        }
        if let Some(previous) = hunk.lines()[..index]
            .iter()
            .rev()
            .find_map(|candidate| candidate.source_line_no)
        {
            anchors.insert(previous);
        }
        if let Some(next) = hunk.lines()[index + 1..]
            .iter()
            .find_map(|candidate| candidate.source_line_no)
        {
            anchors.insert(next);
        }
    }
    if anchors.is_empty() && hunk.source_length > 0 {
        anchors.insert(hunk.source_start);
        anchors.insert(hunk.source_start + hunk.source_length - 1);
    }
    anchors
}

pub(super) fn normalize_diff_path(path: &str) -> Result<String, DynError> {
    if path.is_empty() || path == "/dev/null" || path.contains('\0') {
        return Err("diff path is empty, /dev/null, or contains NUL".into());
    }
    let path = path.strip_prefix("a/").unwrap_or(path);
    if path.starts_with('/') || path.starts_with('\\') {
        return Err(format!("diff path must be repository-relative: {path}").into());
    }
    let parsed = Path::new(path);
    if parsed.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) || path.split('/').any(|part| part.is_empty() || part == ".")
    {
        return Err(format!("diff path is not normalized: {path}").into());
    }
    Ok(path.replace('\\', "/"))
}

fn is_optional_evidence_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let components = lower.split('/').collect::<Vec<_>>();
    let file_name = components.last().copied().unwrap_or_default();
    components.iter().any(|component| {
        matches!(
            *component,
            "test"
                | "tests"
                | "testing"
                | "spec"
                | "specs"
                | "fixtures"
                | "__tests__"
                | "__snapshots__"
                | "docs"
                | "doc"
                | "documentation"
                | "vendor"
                | "dist"
                | "generated"
        )
    }) || file_name.starts_with("readme")
        || file_name.starts_with("changelog")
        || file_name.starts_with("changes")
        || file_name.ends_with(".md")
        || file_name.ends_with(".rst")
        || file_name.ends_with(".adoc")
        || file_name.ends_with(".snap")
        || file_name.ends_with(".lock")
        || file_name.contains(".test.")
        || file_name.contains(".spec.")
        || file_name.starts_with("test_")
        || file_name.ends_with("_test.go")
        || file_name.ends_with("_test.rs")
        || file_name.ends_with("_spec.rb")
}

fn language_for_path(path: &str) -> Option<Language> {
    let extension = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "c" => Some(Language::C),
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => Some(Language::Cpp),
        "go" => Some(Language::Go),
        "java" => Some(Language::Java),
        "js" | "jsx" | "mjs" | "cjs" => Some(Language::Javascript),
        "ts" | "tsx" | "mts" | "cts" => Some(Language::Typescript),
        "php" => Some(Language::Php),
        "rb" => Some(Language::Ruby),
        "rs" => Some(Language::Rust),
        _ => None,
    }
}

pub(super) fn infer_task_language(
    repository: &str,
    language_weights: &BTreeMap<Language, usize>,
) -> Option<Language> {
    let fixed = match repository {
        "redis/redis" | "jqlang/jq" | "micropython/micropython" | "valkey-io/valkey" => {
            Some(Language::C)
        }
        "nlohmann/json" | "fmtlib/fmt" => Some(Language::Cpp),
        "caddyserver/caddy"
        | "hashicorp/terraform"
        | "prometheus/prometheus"
        | "gohugoio/hugo"
        | "gin-gonic/gin" => Some(Language::Go),
        "google/gson"
        | "apache/druid"
        | "projectlombok/lombok"
        | "apache/lucene"
        | "reactivex/rxjava"
        | "javaparser/javaparser" => Some(Language::Java),
        "phpoffice/phpspreadsheet"
        | "laravel/framework"
        | "php-cs-fixer/php-cs-fixer"
        | "briannesbitt/carbon" => Some(Language::Php),
        "jekyll/jekyll" | "fluent/fluentd" | "fastlane/fastlane" | "jordansissel/fpm"
        | "faker-ruby/faker" | "rubocop/rubocop" => Some(Language::Ruby),
        "tokio-rs/tokio" | "uutils/coreutils" | "nushell/nushell" | "tokio-rs/axum"
        | "burntsushi/ripgrep" | "sharkdp/bat" | "astral-sh/ruff" => Some(Language::Rust),
        "babel/babel"
        | "vuejs/core"
        | "facebook/docusaurus"
        | "immutable-js/immutable-js"
        | "mrdoob/three.js"
        | "preactjs/preact"
        | "axios/axios" => None,
        _ => return None,
    };
    fixed.or_else(|| dominant_script_language(language_weights))
}

fn dominant_script_language(weights: &BTreeMap<Language, usize>) -> Option<Language> {
    let javascript = weights.get(&Language::Javascript).copied().unwrap_or(0);
    let typescript = weights.get(&Language::Typescript).copied().unwrap_or(0);
    match javascript.cmp(&typescript) {
        std::cmp::Ordering::Greater if javascript > 0 => Some(Language::Javascript),
        std::cmp::Ordering::Less if typescript > 0 => Some(Language::Typescript),
        _ => None,
    }
}

pub(super) fn merge_line_maps(target: &mut LineMap, source: LineMap) {
    for (path, lines) in source {
        target.entry(path).or_default().extend(lines);
    }
}

pub(super) fn subtract_line_map(target: &mut LineMap, excluded: &LineMap) {
    for (path, lines) in target.iter_mut() {
        if let Some(excluded) = excluded.get(path) {
            lines.retain(|line| !excluded.contains(line));
        }
    }
    target.retain(|_, lines| !lines.is_empty());
}

pub(super) fn line_map_len(lines: &LineMap) -> Result<usize, DynError> {
    lines.values().try_fold(0usize, |total, lines| {
        total
            .checked_add(lines.len())
            .ok_or_else(|| "labeled line count overflow".into())
    })
}

pub(super) fn regions_from_lines(lines: &mut LineMap) -> Result<Vec<Region>, DynError> {
    let mut regions = Vec::new();
    for (path, lines) in lines {
        let mut iterator = lines.iter().copied();
        let Some(mut start) = iterator.next() else {
            continue;
        };
        let mut end = start;
        for line in iterator {
            if line == end.saturating_add(1) {
                end = line;
            } else {
                regions.push(validated_region(path, start, end)?);
                start = line;
                end = line;
            }
        }
        regions.push(validated_region(path, start, end)?);
    }
    Ok(regions)
}

fn validated_region(path: &str, start_line: usize, end_line: usize) -> Result<Region, DynError> {
    if start_line == 0 || end_line < start_line {
        return Err(format!("invalid region {path}:{start_line}-{end_line}").into());
    }
    Ok(Region {
        path: path.to_owned(),
        start_line,
        end_line,
    })
}
