use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use crate::model::{
    DiffConfigurationChange, DiffConfigurationChangeKind, DiffOwnerTestCoverage,
    DiffOwnerTestStatus, DiffSemanticChangeReceipt, DiffSymbolChange, DiffSymbolChangeKind,
    DiffSymbolEvidence, DiffSymbolModification,
};
use crate::parser;
use crate::repository::{GitBlobBatch, git_blobs_at_revision};

const MAX_SEMANTIC_TOTAL_BYTES: usize = 8 * 1024 * 1024;
const MAX_SEMANTIC_PARSED_SYMBOLS: usize = 1_024;
const MAX_OWNER_TESTS_PER_PATH: usize = 8;
#[derive(Debug)]
struct ParsedSymbol {
    evidence: DiffSymbolEvidence,
    parent: Option<String>,
    signature: Option<String>,
    content: String,
    body_fingerprint: Option<String>,
    explicitly_public: bool,
}

type SymbolIdentity = (String, String, String, Option<String>);

pub(super) fn classify_revision_changes(
    root: &Path,
    base_revision: &str,
    head_revision: &str,
    changed_paths: &[String],
    max_file_bytes: usize,
    max_changes: usize,
) -> DiffSemanticChangeReceipt {
    let mut receipt = DiffSemanticChangeReceipt {
        symbol_changes: Vec::new(),
        configuration_changes: Vec::new(),
        owner_tests: Vec::new(),
        gaps: Vec::new(),
    };
    let base = match git_blobs_at_revision(
        root,
        base_revision,
        changed_paths,
        max_file_bytes,
        MAX_SEMANTIC_TOTAL_BYTES,
    ) {
        Ok(batch) => batch,
        Err(_) => {
            receipt.gaps.push("base_revision_blob_read_failed".into());
            return receipt;
        }
    };
    let head = match git_blobs_at_revision(
        root,
        head_revision,
        changed_paths,
        max_file_bytes,
        MAX_SEMANTIC_TOTAL_BYTES,
    ) {
        Ok(batch) => batch,
        Err(_) => {
            receipt.gaps.push("head_revision_blob_read_failed".into());
            return receipt;
        }
    };
    append_batch_gaps(&mut receipt.gaps, "base", &base);
    append_batch_gaps(&mut receipt.gaps, "head", &head);

    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut parsed_symbols = 0usize;
    for path in changed_paths {
        let base_content = base.blobs.get(path);
        let head_content = head.blobs.get(path);
        if base_content.is_none() && head_content.is_none() {
            if !base.missing_paths.contains(path) || !head.missing_paths.contains(path) {
                receipt
                    .gaps
                    .push(format!("{path}:semantic_content_unavailable"));
            }
            continue;
        }

        if recognized_json_configuration(path) {
            classify_configuration(
                path,
                base_content.map(String::as_str),
                head_content.map(String::as_str),
                &mut receipt,
            );
            continue;
        }

        let before_symbols = match parse_symbols(path, base_content.map(String::as_str)) {
            Ok(symbols) => symbols,
            Err(reason) => {
                receipt.gaps.push(format!("{path}:base_{reason}"));
                continue;
            }
        };
        let after_symbols = match parse_symbols(path, head_content.map(String::as_str)) {
            Ok(symbols) => symbols,
            Err(reason) => {
                receipt.gaps.push(format!("{path}:head_{reason}"));
                continue;
            }
        };
        let path_symbols = before_symbols.len().saturating_add(after_symbols.len());
        if parsed_symbols.saturating_add(path_symbols) > MAX_SEMANTIC_PARSED_SYMBOLS {
            receipt
                .gaps
                .push(format!("{path}:semantic_symbol_scan_limit"));
            continue;
        }
        parsed_symbols += path_symbols;
        before.extend(before_symbols);
        after.extend(after_symbols);
    }

    receipt.symbol_changes = classify_symbols(&before, &after, &mut receipt.gaps);
    if receipt.symbol_changes.len() > max_changes {
        receipt.symbol_changes.truncate(max_changes);
        receipt
            .gaps
            .push("semantic_symbol_changes_truncated".into());
    }
    if receipt.configuration_changes.len() > max_changes {
        receipt.configuration_changes.truncate(max_changes);
        receipt
            .gaps
            .push("semantic_configuration_changes_truncated".into());
    }
    receipt.gaps.sort();
    receipt.gaps.dedup();
    receipt
}

pub(super) fn owner_test_coverage(
    changed_paths: &[&String],
    relationships: &BTreeSet<(String, String, String)>,
    scan_truncated: bool,
    gaps: &mut Vec<String>,
) -> Vec<DiffOwnerTestCoverage> {
    changed_paths
        .iter()
        .map(|changed_path| {
            if looks_like_test_path(changed_path) {
                return DiffOwnerTestCoverage {
                    changed_path: (*changed_path).clone(),
                    status: DiffOwnerTestStatus::Found,
                    paths: vec![(*changed_path).clone()],
                };
            }
            let mut paths = relationships
                .iter()
                .filter(|(owner, _, signal)| owner == *changed_path && signal == "test_name_match")
                .map(|(_, related, _)| related.clone())
                .take(MAX_OWNER_TESTS_PER_PATH + 1)
                .collect::<Vec<_>>();
            let truncated = paths.len() > MAX_OWNER_TESTS_PER_PATH;
            paths.truncate(MAX_OWNER_TESTS_PER_PATH);
            if truncated {
                gaps.push(format!(
                    "{}:owner_test_paths_truncated",
                    changed_path.as_str()
                ));
            }
            let status = if paths.is_empty() {
                if scan_truncated {
                    DiffOwnerTestStatus::Unknown
                } else {
                    DiffOwnerTestStatus::Missing
                }
            } else {
                DiffOwnerTestStatus::Found
            };
            DiffOwnerTestCoverage {
                changed_path: (*changed_path).clone(),
                status,
                paths,
            }
        })
        .collect()
}

fn append_batch_gaps(gaps: &mut Vec<String>, side: &str, batch: &GitBlobBatch) {
    gaps.extend(
        batch
            .oversized_paths
            .iter()
            .map(|path| format!("{path}:{side}_file_exceeds_semantic_limit")),
    );
    gaps.extend(
        batch
            .total_limit_paths
            .iter()
            .map(|path| format!("{path}:{side}_semantic_total_bytes_limit")),
    );
    gaps.extend(
        batch
            .invalid_utf8_paths
            .iter()
            .map(|path| format!("{path}:{side}_semantic_content_not_utf8")),
    );
    gaps.extend(
        batch
            .unsupported_paths
            .iter()
            .map(|path| format!("{path}:{side}_semantic_git_entry_unsupported")),
    );
}

fn parse_symbols(
    path: &str,
    content: Option<&str>,
) -> std::result::Result<Vec<ParsedSymbol>, &'static str> {
    let Some(content) = content else {
        return Ok(Vec::new());
    };
    let parsed = parser::parse(path, content).map_err(|_| "semantic_parse_failed")?;
    if parsed.language.is_none() {
        return Err("semantic_language_unsupported");
    }
    if !parsed.structurally_complete {
        return Err("semantic_parse_incomplete");
    }
    parsed
        .symbols
        .into_iter()
        .map(|symbol| {
            let symbol_content = content
                .get(symbol.start_byte..symbol.end_byte)
                .ok_or("semantic_symbol_range_invalid")?
                .to_owned();
            let body_fingerprint =
                symbol_body_fingerprint(&symbol_content, symbol.signature.as_deref());
            let explicitly_public = explicitly_public(symbol.signature.as_deref());
            Ok(ParsedSymbol {
                evidence: DiffSymbolEvidence {
                    path: path.to_owned(),
                    name: symbol.name,
                    kind: symbol.kind,
                    start_line: symbol.start_line,
                    end_line: symbol.end_line,
                },
                parent: symbol.parent,
                signature: symbol.signature,
                content: symbol_content,
                body_fingerprint,
                explicitly_public,
            })
        })
        .collect()
}

fn classify_symbols(
    before: &[ParsedSymbol],
    after: &[ParsedSymbol],
    gaps: &mut Vec<String>,
) -> Vec<DiffSymbolChange> {
    let mut before_identities = BTreeMap::<SymbolIdentity, Vec<usize>>::new();
    let mut after_identities = BTreeMap::<SymbolIdentity, Vec<usize>>::new();
    for (index, symbol) in before.iter().enumerate() {
        before_identities
            .entry(symbol_identity(symbol))
            .or_default()
            .push(index);
    }
    for (index, symbol) in after.iter().enumerate() {
        after_identities
            .entry(symbol_identity(symbol))
            .or_default()
            .push(index);
    }

    let identities = before_identities
        .keys()
        .chain(after_identities.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut before_used = vec![false; before.len()];
    let mut after_used = vec![false; after.len()];
    let mut changes = Vec::new();
    for identity in identities {
        let before_matches = before_identities
            .get(&identity)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let after_matches = after_identities
            .get(&identity)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if before_matches.len() > 1 || after_matches.len() > 1 {
            before_matches
                .iter()
                .for_each(|index| before_used[*index] = true);
            after_matches
                .iter()
                .for_each(|index| after_used[*index] = true);
            gaps.push(format!(
                "{}:{}:ambiguous_symbol_identity",
                identity.0, identity.1
            ));
            continue;
        }
        let (Some(before_index), Some(after_index)) =
            (before_matches.first(), after_matches.first())
        else {
            continue;
        };
        before_used[*before_index] = true;
        after_used[*after_index] = true;
        let old = &before[*before_index];
        let new = &after[*after_index];
        if old.content == new.content {
            continue;
        }
        let modification = modification(old.signature.as_deref(), new.signature.as_deref());
        changes.push(DiffSymbolChange {
            kind: DiffSymbolChangeKind::Modified,
            before: Some(old.evidence.clone()),
            after: Some(new.evidence.clone()),
            modification: Some(modification),
            public_contract_changed: modification == DiffSymbolModification::SignatureChanged
                && (old.explicitly_public || new.explicitly_public),
        });
    }

    let mut removed_fingerprints = BTreeMap::<(String, String), Vec<usize>>::new();
    let mut added_fingerprints = BTreeMap::<(String, String), Vec<usize>>::new();
    for (index, symbol) in before
        .iter()
        .enumerate()
        .filter(|(index, _)| !before_used[*index])
    {
        let Some(body_fingerprint) = &symbol.body_fingerprint else {
            continue;
        };
        removed_fingerprints
            .entry((symbol.evidence.kind.clone(), body_fingerprint.clone()))
            .or_default()
            .push(index);
    }
    for (index, symbol) in after
        .iter()
        .enumerate()
        .filter(|(index, _)| !after_used[*index])
    {
        let Some(body_fingerprint) = &symbol.body_fingerprint else {
            continue;
        };
        added_fingerprints
            .entry((symbol.evidence.kind.clone(), body_fingerprint.clone()))
            .or_default()
            .push(index);
    }
    for fingerprint in removed_fingerprints
        .keys()
        .filter(|key| added_fingerprints.contains_key(*key))
    {
        let removed = &removed_fingerprints[fingerprint];
        let added = &added_fingerprints[fingerprint];
        if removed.len() != 1 || added.len() != 1 {
            gaps.push("ambiguous_symbol_rename_fingerprint".into());
            continue;
        }
        let before_index = removed[0];
        let after_index = added[0];
        let old = &before[before_index];
        let new = &after[after_index];
        if old.evidence.name == new.evidence.name {
            continue;
        }
        before_used[before_index] = true;
        after_used[after_index] = true;
        changes.push(DiffSymbolChange {
            kind: DiffSymbolChangeKind::Renamed,
            before: Some(old.evidence.clone()),
            after: Some(new.evidence.clone()),
            modification: None,
            public_contract_changed: old.explicitly_public || new.explicitly_public,
        });
    }

    changes.extend(
        before
            .iter()
            .enumerate()
            .filter(|(index, _)| !before_used[*index])
            .map(|(_, symbol)| DiffSymbolChange {
                kind: DiffSymbolChangeKind::Removed,
                before: Some(symbol.evidence.clone()),
                after: None,
                modification: None,
                public_contract_changed: symbol.explicitly_public,
            }),
    );
    changes.extend(
        after
            .iter()
            .enumerate()
            .filter(|(index, _)| !after_used[*index])
            .map(|(_, symbol)| DiffSymbolChange {
                kind: DiffSymbolChangeKind::Added,
                before: None,
                after: Some(symbol.evidence.clone()),
                modification: None,
                public_contract_changed: symbol.explicitly_public,
            }),
    );
    changes.sort_by_key(change_sort_key);
    changes
}

fn symbol_identity(symbol: &ParsedSymbol) -> SymbolIdentity {
    (
        symbol.evidence.path.clone(),
        symbol.evidence.name.clone(),
        symbol.evidence.kind.clone(),
        symbol.parent.clone(),
    )
}

fn change_sort_key(change: &DiffSymbolChange) -> (String, usize, String, u8) {
    let evidence = change.after.as_ref().or(change.before.as_ref());
    (
        evidence.map(|item| item.path.clone()).unwrap_or_default(),
        evidence.map_or(0, |item| item.start_line),
        evidence.map(|item| item.name.clone()).unwrap_or_default(),
        match change.kind {
            DiffSymbolChangeKind::Added => 0,
            DiffSymbolChangeKind::Removed => 1,
            DiffSymbolChangeKind::Renamed => 2,
            DiffSymbolChangeKind::Modified => 3,
        },
    )
}

fn modification(
    before_signature: Option<&str>,
    after_signature: Option<&str>,
) -> DiffSymbolModification {
    if normalize_signature(before_signature) == normalize_signature(after_signature) {
        DiffSymbolModification::BodyOnly
    } else {
        DiffSymbolModification::SignatureChanged
    }
}

fn normalize_signature(signature: Option<&str>) -> Option<String> {
    signature.map(|signature| signature.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn explicitly_public(signature: Option<&str>) -> bool {
    let Some(signature) = signature else {
        return false;
    };
    signature
        .split(|character: char| character.is_whitespace() || character == '(')
        .any(|token| matches!(token, "pub" | "public" | "export"))
}

fn symbol_body_fingerprint(content: &str, signature: Option<&str>) -> Option<String> {
    let normalized_content = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let body = normalize_signature(signature)
        .and_then(|signature| {
            normalized_content
                .strip_prefix(&signature)
                .map(str::trim)
                .map(str::to_owned)
        })
        .unwrap_or(normalized_content);
    (!body.is_empty()).then(|| crate::text::hash(&body))
}

fn recognized_json_configuration(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    name == "package.json"
        || name == "deno.json"
        || name == "composer.json"
        || name == "config.json"
        || name == "settings.json"
        || (name.starts_with("tsconfig") && name.ends_with(".json"))
        || name.ends_with(".config.json")
        || (name.starts_with('.') && name.ends_with("rc.json"))
}

fn classify_configuration(
    path: &str,
    before: Option<&str>,
    after: Option<&str>,
    receipt: &mut DiffSemanticChangeReceipt,
) {
    let before = match before
        .map(serde_json::from_str::<serde_json::Value>)
        .transpose()
    {
        Ok(value) => value,
        Err(_) => {
            receipt
                .gaps
                .push(format!("{path}:base_configuration_json_invalid"));
            return;
        }
    };
    let after = match after
        .map(serde_json::from_str::<serde_json::Value>)
        .transpose()
    {
        Ok(value) => value,
        Err(_) => {
            receipt
                .gaps
                .push(format!("{path}:head_configuration_json_invalid"));
            return;
        }
    };
    let mut before_keys = BTreeMap::new();
    let mut after_keys = BTreeMap::new();
    if let Some(value) = &before {
        collect_json_keys(value, String::new(), &mut before_keys);
    }
    if let Some(value) = &after {
        collect_json_keys(value, String::new(), &mut after_keys);
    }
    for key_path in before_keys
        .keys()
        .chain(after_keys.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
    {
        let kind = match (before_keys.get(&key_path), after_keys.get(&key_path)) {
            (None, Some(_)) => Some(DiffConfigurationChangeKind::Added),
            (Some(_), None) => Some(DiffConfigurationChangeKind::Removed),
            (Some(before), Some(after)) if before != after => {
                Some(DiffConfigurationChangeKind::Modified)
            }
            _ => None,
        };
        if let Some(kind) = kind {
            receipt.configuration_changes.push(DiffConfigurationChange {
                path: path.to_owned(),
                key_path,
                kind,
            });
        }
    }
}

fn collect_json_keys(
    value: &serde_json::Value,
    pointer: String,
    keys: &mut BTreeMap<String, String>,
) {
    if let serde_json::Value::Object(object) = value
        && !object.is_empty()
    {
        for (key, value) in object {
            let escaped = key.replace('~', "~0").replace('/', "~1");
            collect_json_keys(value, format!("{pointer}/{escaped}"), keys);
        }
        return;
    }
    keys.insert(pointer, json_fingerprint(value));
}

fn json_fingerprint(value: &serde_json::Value) -> String {
    fn update(hasher: &mut blake3::Hasher, value: &serde_json::Value) {
        match value {
            serde_json::Value::Null => {
                hasher.update(b"n");
            }
            serde_json::Value::Bool(value) => {
                hasher.update(if *value { b"b1" } else { b"b0" });
            }
            serde_json::Value::Number(value) => {
                hasher.update(b"d");
                hasher.update(value.to_string().as_bytes());
            }
            serde_json::Value::String(value) => {
                hasher.update(b"s");
                hasher.update(&(value.len() as u64).to_le_bytes());
                hasher.update(value.as_bytes());
            }
            serde_json::Value::Array(values) => {
                hasher.update(b"a");
                hasher.update(&(values.len() as u64).to_le_bytes());
                values.iter().for_each(|value| update(hasher, value));
            }
            serde_json::Value::Object(values) => {
                hasher.update(b"o");
                hasher.update(&(values.len() as u64).to_le_bytes());
                let mut entries = values.iter().collect::<Vec<_>>();
                entries.sort_by_key(|(key, _)| *key);
                for (key, value) in entries {
                    hasher.update(&(key.len() as u64).to_le_bytes());
                    hasher.update(key.as_bytes());
                    update(hasher, value);
                }
            }
        }
    }

    let mut hasher = blake3::Hasher::new();
    update(&mut hasher, value);
    hasher.finalize().to_hex().to_string()
}

fn looks_like_test_path(path: &str) -> bool {
    let lower = path.replace('\\', "/").to_ascii_lowercase();
    lower.starts_with("test")
        || lower.starts_with("spec")
        || lower.contains("/test")
        || lower.contains("/spec")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(name: &str, body_fingerprint: &str) -> ParsedSymbol {
        ParsedSymbol {
            evidence: DiffSymbolEvidence {
                path: "src/lib.rs".into(),
                name: name.into(),
                kind: "function".into(),
                start_line: 1,
                end_line: 1,
            },
            parent: None,
            signature: Some(format!("fn {name}()")),
            content: format!("fn {name}() {{ 1 }}"),
            body_fingerprint: Some(body_fingerprint.into()),
            explicitly_public: false,
        }
    }

    #[test]
    fn test_ambiguous_body_fingerprint_does_not_claim_rename() {
        let before = [parsed("old_one", "same"), parsed("old_two", "same")];
        let after = [parsed("new_one", "same"), parsed("new_two", "same")];
        let mut gaps = Vec::new();
        let changes = classify_symbols(&before, &after, &mut gaps);
        assert!(
            changes
                .iter()
                .all(|change| change.kind != DiffSymbolChangeKind::Renamed)
        );
        assert!(gaps.contains(&"ambiguous_symbol_rename_fingerprint".into()));
    }

    #[test]
    fn test_declarations_without_bodies_do_not_claim_rename() {
        let mut before = parsed("old_name", "unused");
        before.body_fingerprint = None;
        let mut after = parsed("new_name", "unused");
        after.body_fingerprint = None;
        let mut gaps = Vec::new();
        let changes = classify_symbols(&[before], &[after], &mut gaps);
        assert!(
            changes
                .iter()
                .all(|change| change.kind != DiffSymbolChangeKind::Renamed)
        );
    }

    #[test]
    fn test_json_fingerprint_ignores_object_key_order() {
        let left = serde_json::json!({"one": 1, "two": 2});
        let right = serde_json::json!({"two": 2, "one": 1});
        assert_eq!(json_fingerprint(&left), json_fingerprint(&right));
    }

    #[test]
    fn test_explicit_public_detection_is_conservative() {
        assert!(explicitly_public(Some("pub fn visible()")));
        assert!(explicitly_public(Some("export const visible =")));
        assert!(!explicitly_public(Some("fn publicize()")));
        assert!(!explicitly_public(Some("fn private()")));
    }
}
