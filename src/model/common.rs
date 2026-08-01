use std::borrow::Cow;

use schemars::{Schema, SchemaGenerator};

use super::*;
use crate::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// State of the committed index while a response is produced.
pub enum Freshness {
    /// No reconciliation is active.
    Current,
    /// A query used the last committed generation during reconciliation.
    Reconciling,
}

/// Repository coverage boundary represented by one committed index.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IndexScopeMode {
    /// The index covers the complete ignore-visible repository.
    #[default]
    Full,
    /// Explicit include or exclude patterns constrain indexed membership.
    Scoped,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Readiness of the repository index for retrieval.
pub enum IndexState {
    /// No index generation has completed.
    Uninitialized,
    /// At least one committed generation is available.
    Ready,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Consistency boundary applied before repository retrieval.
pub enum IndexConsistency {
    /// Query the latest completed index generation without scanning filesystem changes.
    #[default]
    IndexedGeneration,
    /// Reconcile the current working tree before querying the resulting generation.
    ReconcileWorkingTree,
}

/// Requested or resolved evidence workflow for context retrieval.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextWorkflow {
    /// Infer a workflow only from high-confidence task language.
    #[default]
    Auto,
    /// General feature, fix, and refactor implementation evidence.
    Implementation,
    /// Repository guidance, templates, validation, changed files, and owner tests.
    Contribution,
    /// Changed code, repository guidance, validation, and review evidence.
    Review,
    /// Diagnostic evidence for tracing behavior and root causes.
    Investigation,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct SymbolIdentity {
    #[schemars(length(min = 1, max = 4096))]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 4096))]
    pub parent: Option<String>,
}

impl<'de> Deserialize<'de> for SymbolIdentity {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawSymbolIdentity {
            name: String,
            #[serde(default)]
            parent: Option<String>,
        }

        let raw = RawSymbolIdentity::deserialize(deserializer)?;
        Self::new(raw.name, raw.parent).map_err(|error| serde::de::Error::custom(error.to_string()))
    }
}

impl SymbolIdentity {
    pub fn new(name: impl Into<String>, parent: Option<impl Into<String>>) -> Result<Self> {
        let name = name.into().trim().to_owned();
        if name.is_empty() {
            return Err(Error::InvalidInput {
                field: "symbol.name",
                reason: "must not be empty",
            });
        }
        let parent = parent
            .map(Into::into)
            .map(|value: String| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        Ok(Self { name, parent })
    }

    pub fn from_qualified(value: &str) -> Result<Self> {
        let value = value.trim();
        let (parent, name) = value
            .rsplit_once('.')
            .map_or((None, value), |(parent, name)| (Some(parent), name));
        Self::new(name, parent)
    }

    #[must_use]
    pub fn qualified_name(&self) -> String {
        self.parent.as_ref().map_or_else(
            || self.name.clone(),
            |parent| format!("{parent}.{}", self.name),
        )
    }
}

/// Trimmed, non-empty text accepted at a serialized service boundary.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct NonEmptyText(String);

impl JsonSchema for NonEmptyText {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "NonEmptyText".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        schemars::json_schema!({
            "description": "Trimmed, non-empty text accepted at a serialized service boundary.",
            "type": "string",
            "minLength": 1,
            "maxLength": 65536
        })
    }
}

impl<'de> Deserialize<'de> for NonEmptyText {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let value = value.trim();
        (!value.is_empty())
            .then(|| Self(value.to_owned()))
            .ok_or_else(|| serde::de::Error::custom("must not be empty or whitespace-only"))
    }
}

impl NonEmptyText {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

/// Caller-observed workflow signals kept separate from the natural-language task.
///
/// Use the builder methods instead of constructing this non-exhaustive type
/// directly. Values are validated by [`crate::services::Services`] before retrieval.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[schemars(
    description = "Caller-observed workflow signals kept separate from the natural-language task.\n\nUse the builder methods instead of constructing this non-exhaustive type\ndirectly. Values are validated by [`crate::Services`] before retrieval."
)]
#[non_exhaustive]
#[serde(default, deny_unknown_fields)]
pub struct WorkflowEvidence {
    /// Bounded compiler, test, runtime, or log excerpts.
    #[schemars(length(max = 8), inner(length(min = 1, max = 8192)))]
    pub failure_traces: Vec<String>,
    /// Exact or qualified identifiers observed by the caller.
    #[schemars(length(max = 8), inner(length(min = 1, max = 8192)))]
    pub symbols: Vec<String>,
    /// Normalized repository-relative paths observed by the caller.
    #[schemars(length(max = 8), inner(length(min = 1, max = 8192)))]
    pub paths: Vec<String>,
    /// Test names, commands, or behavioral checks relevant to the task.
    #[schemars(length(max = 8), inner(length(min = 1, max = 8192)))]
    pub test_intents: Vec<String>,
}

impl WorkflowEvidence {
    /// Construct an empty evidence contract.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            failure_traces: Vec::new(),
            symbols: Vec::new(),
            paths: Vec::new(),
            test_intents: Vec::new(),
        }
    }

    /// Attach caller-observed failure traces.
    #[must_use]
    pub fn with_failure_traces(mut self, values: impl IntoIterator<Item = String>) -> Self {
        self.failure_traces = values.into_iter().collect();
        self
    }

    /// Attach caller-observed exact or qualified symbols.
    #[must_use]
    pub fn with_symbols(mut self, values: impl IntoIterator<Item = String>) -> Self {
        self.symbols = values.into_iter().collect();
        self
    }

    /// Attach caller-observed repository-relative paths.
    #[must_use]
    pub fn with_paths(mut self, values: impl IntoIterator<Item = String>) -> Self {
        self.paths = values.into_iter().collect();
        self
    }

    /// Attach caller-observed test names, commands, or behavioral checks.
    #[must_use]
    pub fn with_test_intents(mut self, values: impl IntoIterator<Item = String>) -> Self {
        self.test_intents = values.into_iter().collect();
        self
    }

    /// Return whether the contract contains no workflow evidence.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.failure_traces.is_empty()
            && self.symbols.is_empty()
            && self.paths.is_empty()
            && self.test_intents.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResponseMeta {
    /// Stable opaque identity for the canonical repository root.
    pub repository_id: String,
    pub repository_generation: u64,
    pub freshness: Freshness,
    /// Whether negative evidence is valid for the full ignore-visible repository.
    #[serde(default)]
    pub index_scope: IndexScopeMode,
    /// Compact opaque identity for a scoped index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_scope_digest: Option<String>,
    /// Tokens in source content selected for the response.
    #[serde(default)]
    pub source_tokens: usize,
    /// Tokens in the compact JSON response envelope after values and result items are removed.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub protocol_tokens: usize,
    /// Tokens attributed to paths, metadata values, and repeated result structure.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub path_and_metadata_tokens: usize,
    /// Tokens in the final serialized service response, including accounting fields.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub total_response_tokens: usize,
    /// Tokenizer used for source and serialized response accounting.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tokenizer: String,
    /// Whether the configured tokenizer produces exact local counts.
    pub token_count_exact: bool,
    /// Opaque server-managed retrieval receipt for suppressing repeated evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<String>,
    /// Evidence omitted because its content hash was already recorded by the receipt.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub receipt_suppressed_exact: usize,
    /// Evidence omitted because its source range overlaps evidence recorded by the receipt.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub receipt_suppressed_overlap: usize,
    /// Returned evidence that is semantically close to evidence recorded by the receipt.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub receipt_near_duplicates: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

pub(super) fn is_zero(value: &usize) -> bool {
    *value == 0
}

pub(super) fn is_source_representation(value: &String) -> bool {
    value == "source"
}

pub(super) fn is_false(value: &bool) -> bool {
    !value
}

pub(super) fn source_representation() -> String {
    "source".to_owned()
}
