use serde::{Deserialize, Serialize};

use crate::model::{QueryReceiptScopeRelation, SearchMode, SearchOccurrence, SearchRequest};
use crate::{Error, Result};

pub(crate) const MAX_QUERY_RECEIPT_ID_BYTES: usize = 128;
pub(crate) const MAX_QUERY_PREDICATE_BYTES: usize = 64 * 1024;
pub(crate) const QUERY_RECEIPT_SEMANTICS_VERSION: u64 = 2;
const SQLITE_POSITIVE_INTEGER_MAX: u64 = i64::MAX as u64;

/// Compute a fingerprint of the actual search semantics, derived from the
/// algorithm configuration rather than a manually maintained integer.
///
/// This fingerprint includes:
/// - The package version (changes on every release)
/// - The index content version (changes when the index format changes)
/// - The query receipt semantics version (manual baseline)
///
/// The semantics version is the explicit compatibility boundary for external
/// dependencies and implementation changes. Bump it whenever either can
/// change exhaustive-search behavior.
///
/// When any of these change, old receipts are automatically invalidated. See
/// issue #545.
pub(crate) fn search_semantics_fingerprint() -> u64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"leantoken-search-semantics-v1\0");
    hasher.update(&(env!("CARGO_PKG_VERSION").len() as u64).to_le_bytes());
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    hasher.update(&crate::config::INDEX_CONTENT_VERSION.to_le_bytes());
    hasher.update(&QUERY_RECEIPT_SEMANTICS_VERSION.to_le_bytes());
    let digest = hasher.finalize();
    let fingerprint = u64::from_le_bytes(digest.as_bytes()[..8].try_into().unwrap_or([0; 8]));
    fingerprint % SQLITE_POSITIVE_INTEGER_MAX + 1
}
pub(crate) const QUERY_RECEIPT_ID_RESPONSE_RESERVE: &str =
    "q0a1b2c3d4e5f60718293a4b5c6d7e8f901a2b3c4d5e6f708192a3b4c5d6e7f80";
const _: () = assert!(QUERY_RECEIPT_ID_RESPONSE_RESERVE.len() == 65);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExactQueryMode {
    Text,
    Regex,
}

impl ExactQueryMode {
    fn from_search_mode(mode: SearchMode) -> Result<Self> {
        match mode {
            SearchMode::Text => Ok(Self::Text),
            SearchMode::Regex => Ok(Self::Regex),
            _ => Err(Error::InvalidInput {
                field: "query_receipt",
                reason: "requires text or regex mode",
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ExactQueryPredicate {
    semantics_version: u64,
    mode: ExactQueryMode,
    query_blake3: String,
    case_sensitive: bool,
    include_paths: Vec<String>,
    exclude_paths: Vec<String>,
}

impl ExactQueryPredicate {
    pub(crate) fn from_request(request: &SearchRequest) -> Result<Self> {
        let predicate = Self {
            semantics_version: search_semantics_fingerprint(),
            mode: ExactQueryMode::from_search_mode(request.mode)?,
            query_blake3: blake3::hash(request.query.as_bytes()).to_hex().to_string(),
            case_sensitive: request.case_sensitive,
            include_paths: normalize_patterns(&request.include_paths)?,
            exclude_paths: normalize_patterns(&request.exclude_paths)?,
        };
        predicate.serialized()?;
        Ok(predicate)
    }

    pub(crate) fn serialized(&self) -> Result<String> {
        let serialized = serde_json::to_string(self)?;
        if serialized.len() > MAX_QUERY_PREDICATE_BYTES {
            return Err(Error::InputTooLong {
                field: "normalized query predicate",
                max_bytes: MAX_QUERY_PREDICATE_BYTES,
            });
        }
        Ok(serialized)
    }

    pub(crate) fn digest(&self) -> Result<String> {
        Ok(blake3::hash(self.serialized()?.as_bytes())
            .to_hex()
            .to_string())
    }

    pub(crate) fn scope_relation_to(&self, recorded: &Self) -> Option<QueryReceiptScopeRelation> {
        if !self.same_query_semantics(recorded) {
            return None;
        }
        if self.include_paths == recorded.include_paths
            && self.exclude_paths == recorded.exclude_paths
        {
            return Some(QueryReceiptScopeRelation::Exact);
        }
        let includes_are_subset = recorded.include_paths.is_empty()
            || (!self.include_paths.is_empty()
                && self
                    .include_paths
                    .iter()
                    .all(|pattern| recorded.include_paths.binary_search(pattern).is_ok()));
        let excludes_only_narrow = recorded
            .exclude_paths
            .iter()
            .all(|pattern| self.exclude_paths.binary_search(pattern).is_ok());
        (includes_are_subset && excludes_only_narrow).then_some(QueryReceiptScopeRelation::Subset)
    }

    pub(crate) fn include_paths(&self) -> &[String] {
        &self.include_paths
    }

    pub(crate) fn exclude_paths(&self) -> &[String] {
        &self.exclude_paths
    }

    fn same_query_semantics(&self, other: &Self) -> bool {
        self.semantics_version == other.semantics_version
            && self.mode == other.mode
            && self.query_blake3 == other.query_blake3
            && self.case_sensitive == other.case_sensitive
    }
}

fn normalize_patterns(patterns: &[String]) -> Result<Vec<String>> {
    let mut normalized =
        crate::repository::RepositoryPatternSet::new(patterns)?.canonical_strings();
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct QueryPartition {
    pub digest: String,
    pub file_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct QueryReceiptRecord {
    pub repository_generation: u64,
    pub config_hash: String,
    pub predicate: ExactQueryPredicate,
    pub predicate_blake3: String,
    pub partition: QueryPartition,
    pub match_count: usize,
    pub result_blake3: String,
}

#[derive(Debug, Clone)]
pub(crate) struct StoredQueryReceipt {
    pub receipt_id: String,
    pub repository_generation: u64,
    pub config_hash: String,
    pub predicate: ExactQueryPredicate,
    pub predicate_blake3: String,
    pub partition: QueryPartition,
    pub match_count: usize,
    pub result_blake3: String,
}

pub(crate) fn exhaustive_result_digest<'a>(
    hits: impl IntoIterator<Item = (&'a str, &'a SearchOccurrence)>,
) -> String {
    let mut occurrences = hits.into_iter().collect::<Vec<_>>();
    occurrences.sort_by(|(left_path, left), (right_path, right)| {
        left_path
            .cmp(right_path)
            .then_with(|| left.start_byte.cmp(&right.start_byte))
            .then_with(|| left.end_byte.cmp(&right.end_byte))
    });
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"leantoken-exact-query-result-v1\0");
    hasher.update(&(occurrences.len() as u64).to_le_bytes());
    for (path, occurrence) in occurrences {
        hash_bytes(&mut hasher, path.as_bytes());
        hash_occurrence(&mut hasher, occurrence);
    }
    hasher.finalize().to_hex().to_string()
}

fn hash_bytes(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn hash_occurrence(hasher: &mut blake3::Hasher, occurrence: &SearchOccurrence) {
    for value in [
        occurrence.start_line,
        occurrence.end_line,
        occurrence.start_column,
        occurrence.end_column,
        occurrence.start_byte,
        occurrence.end_byte,
    ] {
        hasher.update(&(value as u64).to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(include_paths: &[&str], exclude_paths: &[&str]) -> SearchRequest {
        SearchRequest {
            query: "Needle".into(),
            mode: SearchMode::Text,
            include_paths: include_paths.iter().map(|value| (*value).into()).collect(),
            exclude_paths: exclude_paths.iter().map(|value| (*value).into()).collect(),
            focus_paths: Vec::new(),
            max_results: None,
            max_tokens: None,
            context_lines: None,
            case_sensitive: true,
            all_occurrences: true,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        }
    }

    #[test]
    fn predicate_normalization_is_deterministic_and_scope_subset_is_conservative() {
        let recorded = ExactQueryPredicate::from_request(&request(
            &["src/", "tests/**/*.rs"],
            &["src/generated"],
        ))
        .expect("recorded predicate");
        let equivalent = ExactQueryPredicate::from_request(&request(
            &["tests/**/*.rs", "./src", "src"],
            &["./src/generated/"],
        ))
        .expect("equivalent predicate");
        assert_eq!(recorded, equivalent);
        assert_eq!(
            recorded.scope_relation_to(&equivalent),
            Some(QueryReceiptScopeRelation::Exact)
        );

        let subset =
            ExactQueryPredicate::from_request(&request(&["src"], &["src/generated", "src/vendor"]))
                .expect("subset predicate");
        assert_eq!(
            subset.scope_relation_to(&recorded),
            Some(QueryReceiptScopeRelation::Subset)
        );
        assert_eq!(recorded.scope_relation_to(&subset), None);
    }

    #[test]
    fn response_reserve_covers_generated_ids_across_tokenizers() {
        use crate::tokens::Tokenizer;

        let tokenizers = [
            Tokenizer::Cl100kBase,
            Tokenizer::O200kBase,
            Tokenizer::O200kHarmony,
            Tokenizer::P50kBase,
            Tokenizer::R50kBase,
            Tokenizer::Gpt2,
            Tokenizer::P50kEdit,
            Tokenizer::Estimate,
        ];
        for seed in 0u64..4_096 {
            let id = format!("q{}", blake3::hash(&seed.to_le_bytes()).to_hex());
            for tokenizer in tokenizers {
                assert!(
                    tokenizer.count(&id) <= tokenizer.count(QUERY_RECEIPT_ID_RESPONSE_RESERVE),
                    "{} under-reserved generated query receipt {id}",
                    tokenizer.name()
                );
            }
        }
    }
}
