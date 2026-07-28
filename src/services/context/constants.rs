const GIT_CHANGED_PATHS_MAX: usize = 512;
/// Maximum explicit changed paths accepted from a diff-scoped request.
const MAX_DIFF_CHANGED_PATHS: usize = 512;
/// Maximum bytes for a base revision string.
const MAX_BASE_REVISION_BYTES: usize = 256;
/// Maximum context query terms (symbols/refs/FTS fan-out budget).
pub(super) const MAX_CONTEXT_QUERIES: usize = 12;
/// Per-term symbol/reference candidate cap for context assembly.
pub(super) const MAX_CONTEXT_HITS_PER_SOURCE: usize = 20;
/// Per-term FTS candidate cap for context assembly.
pub(super) const MAX_CONTEXT_LEXICAL_HITS: usize = 30;
/// Maximum values accepted in each workflow-evidence class.
const MAX_WORKFLOW_EVIDENCE_ITEMS_PER_CLASS: usize = 8;
/// Maximum UTF-8 bytes accepted in one workflow-evidence value.
const MAX_WORKFLOW_EVIDENCE_ITEM_BYTES: usize = 8 * 1024;
/// Maximum UTF-8 bytes accepted across all workflow-evidence classes.
const MAX_WORKFLOW_EVIDENCE_TOTAL_BYTES: usize = 32 * 1024;
/// Maximum focus patterns eligible for per-scope candidate generation.
pub(crate) const MAX_CONTEXT_FOCUS_PATTERNS: usize = 32;
/// Indexed files inspected for task-relevant candidates per focus pattern.
const MAX_CONTEXT_FOCUS_FILES_PER_PATTERN: usize = 4;
/// File-local chunks inspected per focused file.
const MAX_CONTEXT_FOCUS_CHUNKS_PER_FILE: usize = 256;
/// File-local symbols inspected per focused file.
const MAX_CONTEXT_FOCUS_SYMBOLS_PER_FILE: usize = 128;
/// Candidates retained from each focus pattern before global ranking.
pub(crate) const MAX_CONTEXT_FOCUS_CANDIDATES_PER_PATTERN: usize = 8;
/// Maximum explicit path-scoped evidence contracts accepted per request.
const MAX_CONTEXT_REQUIRED_EVIDENCE: usize = 32;
/// Maximum alternative literal queries accepted in one evidence contract.
const MAX_CONTEXT_EVIDENCE_QUERIES: usize = 16;
/// Maximum UTF-8 bytes accepted across all explicit evidence queries.
const MAX_CONTEXT_EVIDENCE_QUERY_BYTES: usize = 64 * 1024;
/// Per-import symbol scan cap for concept-corroborated structural expansion.
const MAX_IMPORT_SYMBOLS: usize = 128;
/// Exact constraint names retained per storage batch.
const MAX_EXACT_SYMBOL_BATCH_NAMES: usize = 32;
const MIN_CORROBORATED_QUERY_WEIGHT: f64 = 0.65;
const SYMBOL_CONTEXT_TOKEN_CAP: usize = 768;
const REFERENCE_CONTEXT_TOKEN_CAP: usize = 256;
const TEXT_CONTEXT_TOKEN_CAP: usize = 256;
const IMPORT_SYMBOL_CONTEXT_TOKEN_CAP: usize = 384;
const MAX_DIFF_EVIDENCE_SYMBOLS: usize = 64;
const MAX_DIFF_EVIDENCE_RELATIONSHIPS: usize = 64;
const MAX_DIFF_EVIDENCE_PATHS: usize = 64;
const MAX_WORKFLOW_SCAN_FILES: usize = 8_192;
const MAX_OWNER_TEST_SCAN_FILES: usize = 4_096;
const MAX_REFERENCES_PER_CHANGED_SYMBOL: usize = 8;
const OVERSIZED_CHANGE_PATHS: usize = 32;
const MIN_OVERSIZED_PATH_GROUPS: usize = 3;
const MAX_ROUTING_GROUPS: usize = 5;
const MAX_ROUTING_SUGGESTIONS: usize = 3;
const LEXICAL_OCCURRENCE_SATURATION: usize = 25;
