use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// Input for `leantoken.outline`.
pub struct OutlineRequest {
    /// Repository-relative files to outline.
    pub paths: Vec<String>,
    /// Keep definitions whose names contain this value.
    #[serde(default)]
    pub symbol_name: Option<String>,
    /// Keep definitions of this exact syntax kind.
    #[serde(default)]
    pub symbol_kind: Option<String>,
    /// Maximum definitions and imports to return.
    #[serde(default)]
    pub max_results: Option<usize>,
    /// Maximum tokens across signatures and import targets.
    #[serde(default)]
    pub max_tokens: Option<usize>,
    /// Server-managed receipt whose previously returned evidence should be suppressed.
    #[serde(default)]
    pub receipt_id: Option<String>,
    /// Opaque cursor returned when `max_results` leaves outline entries unread.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Indexed availability for one requested outline path.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutlinePathStatus {
    /// The path is present in the request's pinned index snapshot.
    Indexed,
    /// The path has no row in the pinned index snapshot.
    NotIndexed,
}

/// Per-input outcome for a requested outline path.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct OutlinePathResult {
    /// Zero-based position in the complete ordered request.
    pub request_index: usize,
    /// Caller-supplied path normalized to repository-relative form.
    pub path: String,
    pub status: OutlinePathStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OutlineFile {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Whether structural parsing covered the complete indexed file.
    #[serde(default)]
    pub parse_complete: bool,
    /// Compatibility alias for `parse_complete`.
    pub structurally_complete: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<Symbol>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<Import>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OutlineResponse {
    pub files: Vec<OutlineFile>,
    /// Ordered outcome for every requested path, including paths absent from the index.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path_results: Vec<OutlinePathResult>,
    /// Whether every requested path was indexed and parsed completely.
    #[serde(default)]
    pub parse_complete: bool,
    /// Whether every path was indexed and this response contains every filtered entry.
    #[serde(default)]
    pub result_complete: bool,
    /// Exact filtered symbol count across indexed requested files.
    #[serde(default)]
    pub total_symbols: usize,
    /// Symbols returned in this response.
    #[serde(default)]
    pub returned_symbols: usize,
    /// Exact import count across indexed requested files.
    #[serde(default)]
    pub total_imports: usize,
    /// Imports returned in this response.
    #[serde(default)]
    pub returned_imports: usize,
    /// Whether the result cap left outline entries for another page.
    #[serde(default)]
    pub truncated_by_max_results: bool,
    /// Whether signatures or imports were omitted by the token budget.
    #[serde(default)]
    pub truncated_by_max_tokens: bool,
    /// Exact filtered symbol counts grouped by syntax kind.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub symbol_counts_by_kind: BTreeMap<String, usize>,
    pub meta: ResponseMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// Signature-only symbol identity with line coordinates.
pub struct OutlineSignature {
    pub name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// One file in a signature-only outline response.
pub struct OutlineSignaturesFile {
    pub path: String,
    /// Hash of the serialized ordered `signatures` array.
    pub content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub parse_complete: bool,
    pub structurally_complete: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signatures: Vec<OutlineSignature>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// Opt-in outline response that omits imports and symbol byte offsets.
pub struct OutlineSignaturesResponse {
    pub files: Vec<OutlineSignaturesFile>,
    /// Ordered outcome for every requested path, including paths absent from the index.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path_results: Vec<OutlinePathResult>,
    pub parse_complete: bool,
    pub result_complete: bool,
    pub total_symbols: usize,
    pub returned_symbols: usize,
    pub truncated_by_max_results: bool,
    pub truncated_by_max_tokens: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub symbol_counts_by_kind: BTreeMap<String, usize>,
    pub meta: ResponseMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Import {
    pub raw_target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_path: Option<String>,
    pub line: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceRole {
    Definition,
    Reference,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Reference {
    pub name: String,
    pub kind: String,
    pub role: ReferenceRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enclosing_symbol: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
}
