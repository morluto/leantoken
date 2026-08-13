#[derive(Debug, Clone)]
/// Version and publication state read from the singleton metadata row.
///
/// `repository_generation` identifies an atomically published repository view.
/// A reconciliation plan must retain this record and pass it back to a checked
/// publication method so stale filesystem work cannot overwrite newer state.
pub struct MetaRecord {
    pub schema_version: i64,
    pub index_version: i64,
    pub config_hash: String,
    /// Exact derivation implementation that produced the committed rows.
    pub derivation_fingerprint: String,
    pub repository_generation: u64,
}

#[derive(Debug, Clone)]
pub struct FileRecord {
    pub id: i64,
    pub path: String,
    pub language: Option<String>,
    pub structurally_complete: bool,
    pub size_bytes: u64,
    pub modified_ns: Option<u128>,
    pub content_hash: String,
    pub generation: u64,
}

/// Lean file projection for fuzzy find and other path-only scans.
#[derive(Debug, Clone)]
pub(crate) struct FilePathRecord {
    pub id: i64,
    pub path: String,
    pub language: Option<String>,
    pub size_bytes: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct PathRecord {
    pub path: String,
    pub is_directory: bool,
    pub language: Option<String>,
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ChunkInput {
    pub content: String,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub token_count: usize,
}

#[derive(Debug, Clone)]
pub struct ChunkRecord {
    pub id: i64,
    pub file_id: i64,
    pub content: String,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub token_count: usize,
}

#[derive(Debug, Clone)]
pub struct SymbolInput {
    pub name: String,
    pub kind: String,
    pub parent: Option<String>,
    pub signature: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Debug, Clone)]
pub struct SymbolRecord {
    pub id: i64,
    pub file_id: i64,
    pub name: String,
    pub kind: String,
    pub parent: Option<String>,
    pub signature: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Debug, Clone)]
pub struct ReferenceInput {
    pub name: String,
    pub kind: String,
    pub role: ReferenceRole,
    pub enclosing_symbol: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Debug, Clone)]
pub struct ReferenceRecord {
    pub id: i64,
    pub file_id: i64,
    pub name: String,
    pub kind: String,
    pub role: ReferenceRole,
    pub enclosing_symbol: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Debug, Clone)]
/// One parsed import and the bounded path candidates produced by the indexer's
/// language-specific import policy.
///
/// Storage persists every candidate in priority order for reverse invalidation;
/// `resolved_path` is the first candidate present in the authoritative
/// repository generation, according to that language policy's deterministic
/// precedence.
pub struct ImportInput {
    pub raw_target: String,
    pub resolved_path: Option<String>,
    pub candidate_paths: Vec<String>,
    pub line: usize,
}

#[derive(Debug, Clone)]
pub struct ImportRecord {
    pub id: i64,
    pub file_id: i64,
    pub raw_target: String,
    pub resolved_path: Option<String>,
    pub line: usize,
}

#[derive(Debug)]
pub(crate) struct ImportSeed {
    pub id: i64,
    pub file_id: i64,
    pub source_path: String,
    pub raw_target: String,
}

#[derive(Debug)]
pub(crate) struct ImportProjection {
    pub id: i64,
    pub file_id: i64,
    pub value: ImportProjectionValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportProjectionValue {
    pub resolved_path: Option<String>,
    pub candidate_paths: Vec<String>,
}

#[derive(Debug, Clone)]
/// Complete derived representation of one file, ready for transactional publication.
///
/// The indexer constructs these values one bounded batch at a time. Storage
/// treats each file's chunks, symbols, references, imports, and path projection
/// as one replacement unit inside the caller's uncommitted generation.
pub struct IndexedFile {
    pub path: String,
    pub language: Option<String>,
    pub structurally_complete: bool,
    pub size_bytes: u64,
    pub modified_ns: Option<u128>,
    pub content_hash: String,
    pub chunks: Vec<ChunkInput>,
    pub symbols: Vec<SymbolInput>,
    pub references: Vec<ReferenceInput>,
    pub imports: Vec<ImportInput>,
}

#[derive(Debug, Clone)]
pub struct ChunkHit {
    pub chunk_id: i64,
    pub file_id: i64,
    pub path: String,
    pub content: String,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub token_count: usize,
    pub generation: u64,
    pub score: f64,
}

#[derive(Debug, Clone)]
pub struct SymbolHit {
    pub path: String,
    pub content_hash: String,
    pub generation: u64,
    pub symbol: SymbolRecord,
}

#[derive(Debug, Clone)]
pub struct ReferenceHit {
    pub path: String,
    pub content_hash: String,
    pub generation: u64,
    pub reference: ReferenceRecord,
}

pub(crate) struct ImportSymbolTarget {
    pub seed_index: usize,
    pub target_file: FileRecord,
    pub symbols: Vec<SymbolRecord>,
}

#[derive(Debug, Clone)]
pub struct StorageCounts {
    pub files: usize,
    pub chunks: usize,
    pub symbols: usize,
    pub source_bytes: u64,
    pub languages: Vec<(String, usize)>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ParserCoverageRows {
    pub languages: Vec<ParserLanguageCoverageRow>,
    pub unrecognized_extensions: Vec<UnrecognizedExtensionCoverageRow>,
}

#[derive(Debug, Clone)]
pub(crate) struct ParserLanguageCoverageRow {
    pub language: String,
    pub structurally_complete: bool,
    pub files: usize,
    pub source_bytes: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct UnrecognizedExtensionCoverageRow {
    pub extension: String,
    pub files: usize,
    pub source_bytes: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct ReadOnlyStatusSnapshot {
    pub generation: u64,
    pub derivation_fingerprint: Option<String>,
    pub counts: StorageCounts,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct TokenSavingsRecord {
    pub tracked_requests: u64,
    pub response_tracked_requests: u64,
    pub response_baseline_requests: u64,
    pub baseline_source_tokens: u64,
    pub response_baseline_source_tokens: u64,
    pub emitted_source_tokens: u64,
    pub estimated_source_tokens_saved: u64,
    pub response_source_tokens: u64,
    pub path_and_metadata_tokens: u64,
    pub protocol_tokens: u64,
    pub total_response_tokens: u64,
    pub receipt_suppressed_exact: u64,
    pub receipt_suppressed_overlap: u64,
    pub expected_hash_not_modified_responses: u64,
    pub expected_hash_suppressed_source_tokens: u64,
    pub useful_requests: u64,
    pub incomplete_requests: u64,
    pub unsupported_requests: u64,
    pub hash_suppressed_requests: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ServiceFailureRecord {
    pub operation: String,
    pub error_category: String,
    pub failed_requests: u64,
}

pub(crate) struct TokenSavingsObservation<'a> {
    pub operation: TokenAccountingOperation,
    pub baseline_source_tokens: Option<usize>,
    pub meta: &'a ResponseMeta,
    pub classification: TokenSavingsRequestClass,
    pub expected_hash_not_modified: bool,
    pub expected_hash_suppressed_source_tokens: usize,
}

/// SQLite-backed repository index with one serialized writer and pooled readers.
///
/// Clones share the same writer mutex and established read pool. Storage-owned
/// read sessions check out one read-only connection and pin a WAL snapshot,
/// while reconciliation publishes through one immediate transaction. Pooling
/// is process-local; repository ownership and cross-process write serialization
/// are enforced separately by the services and coordination layers.
pub struct Storage {
    pub(crate) writer: Arc<Mutex<Connection>>,
    pub(crate) readers: r2d2::Pool<SqliteConnectionManager>,
    pub(crate) path: PathBuf,
    #[cfg(test)]
    pub(crate) diagnostics: Arc<StorageDiagnostics>,
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct StorageDiagnostics {
    pub(crate) active_snapshots: AtomicUsize,
    pub(crate) peak_active_snapshots: AtomicUsize,
    pub(crate) reader_checkout_wait_micros: Mutex<Vec<u64>>,
}

#[cfg(test)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct StorageDiagnosticsSnapshot {
    pub active_snapshots: usize,
    pub peak_active_snapshots: usize,
    pub reader_checkout_wait_micros: Vec<u64>,
}

/// Restricted writer for one uncommitted repository generation.
pub(crate) struct ReconciliationWriter<'transaction, 'connection> {
    pub(crate) transaction: &'transaction Transaction<'connection>,
    pub(crate) generation: i64,
    pub(crate) mode: IndexingMode,
    pub(crate) replacements: usize,
    pub(crate) deletions: HashSet<String>,
    pub(crate) projection_refreshes: usize,
}
use super::*;

/// Maximum persisted resolution candidates for one import syntax fact.
pub(crate) const MAX_IMPORT_CANDIDATES_PER_IMPORT: usize = 64;
/// Maximum UTF-8 bytes in one persisted repository-relative import candidate.
pub(crate) const MAX_IMPORT_CANDIDATE_PATH_BYTES: usize = 4 * 1024;
