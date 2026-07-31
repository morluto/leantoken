use super::*;

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Response projection for repository path discovery.
pub(in crate::mcp) enum FilesMcpProjection {
    /// Preserve the complete files response.
    #[default]
    Full,
    /// Return paths without per-entry kind, language, size, or score metadata.
    Paths,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(transform = add_files_operation_constraints)]
pub(in crate::mcp) struct FilesMcpRequest {
    /// Expected opaque repository identity from an earlier response.
    #[serde(default)]
    #[schemars(schema_with = "expected_repository_id_schema")]
    pub(in crate::mcp) expected_repository_id: Option<String>,
    /// Path operation to perform.
    #[schemars(schema_with = "file_operation_schema")]
    pub(in crate::mcp) operation: FilesMcpOperationInput,
    /// Optional repository-relative directory for `tree`.
    #[serde(default)]
    #[schemars(length(max = 4096))]
    pub(in crate::mcp) path: Option<String>,
    /// Non-empty fuzzy filename or path query for `find`.
    #[serde(default)]
    #[schemars(length(min = 1, max = 65536))]
    pub(in crate::mcp) query: Option<String>,
    /// Non-empty glob pattern for `glob`.
    #[serde(default)]
    #[schemars(length(min = 1, max = 4096))]
    pub(in crate::mcp) pattern: Option<String>,
    /// Maximum entries to return (default 20, maximum 100).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(schema_with = "result_limit_schema", default = "default_result_option")]
    pub(in crate::mcp) max_results: Option<usize>,
    /// Maximum tokens in the final serialized service response.
    #[serde(default)]
    #[schemars(schema_with = "response_token_limit_schema")]
    pub(in crate::mcp) max_response_tokens: Option<usize>,
    /// Cursor returned by the same operation and repository generation.
    #[serde(default)]
    #[schemars(length(max = 4096))]
    pub(in crate::mcp) cursor: Option<String>,
    /// Use `reconcile_working_tree` after edits; otherwise `indexed_generation`.
    #[serde(default)]
    #[schemars(schema_with = "index_consistency_schema")]
    pub(in crate::mcp) consistency: IndexConsistency,
    /// Maximum hierarchy depth below `path` for `tree`.
    #[serde(default)]
    pub(in crate::mcp) depth: Option<usize>,
    /// Response shape: `full` entries (default) or ordered `paths` only.
    #[serde(default)]
    pub(in crate::mcp) projection: FilesMcpProjection,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(in crate::mcp) enum FilesMcpOperationInput {
    Flat(FileOperation),
    Nested(NestedFilesMcpOperation),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::mcp) enum NestedFilesMcpOperation {
    Tree {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        depth: Option<usize>,
    },
    Find {
        query: String,
    },
    Glob {
        pattern: String,
    },
}

impl FilesMcpRequest {
    pub(in crate::mcp) fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        validate_optional_positive_limit("max_results", self.max_results, limits.max_results)?;
        validate_optional_positive_limit(
            "max_response_tokens",
            self.max_response_tokens,
            limits.max_response_tokens,
        )?;
        if matches!(self.operation, FilesMcpOperationInput::Nested(_))
            && (self.path.is_some()
                || self.query.is_some()
                || self.pattern.is_some()
                || self.depth.is_some())
        {
            return Err(crate::Error::InvalidInput {
                field: "operation",
                reason: "nested compatibility arguments cannot be mixed with flat arguments",
            });
        }
        let (operation, path, query, pattern, depth) = match &self.operation {
            FilesMcpOperationInput::Flat(operation) => (
                operation.clone(),
                self.path.as_ref(),
                self.query.as_ref(),
                self.pattern.as_ref(),
                self.depth,
            ),
            FilesMcpOperationInput::Nested(NestedFilesMcpOperation::Tree { path, depth }) => {
                (FileOperation::Tree, path.as_ref(), None, None, *depth)
            }
            FilesMcpOperationInput::Nested(NestedFilesMcpOperation::Find { query }) => {
                (FileOperation::Find, None, Some(query), None, None)
            }
            FilesMcpOperationInput::Nested(NestedFilesMcpOperation::Glob { pattern }) => {
                (FileOperation::Glob, None, None, Some(pattern), None)
            }
        };
        let invalid = match operation {
            FileOperation::Tree => query
                .map(|_| "query")
                .or_else(|| pattern.map(|_| "pattern")),
            FileOperation::Find => path
                .map(|_| "path")
                .or_else(|| pattern.map(|_| "pattern"))
                .or(depth.map(|_| "depth")),
            FileOperation::Glob => path
                .map(|_| "path")
                .or_else(|| query.map(|_| "query"))
                .or(depth.map(|_| "depth")),
        };
        if let Some(field) = invalid {
            return Err(crate::Error::InvalidInput {
                field,
                reason: "does not apply to the selected file operation",
            });
        }
        Ok(())
    }

    pub(in crate::mcp) fn into_parts(
        self,
    ) -> (
        FilesRequest,
        FilesMcpProjection,
        IndexConsistency,
        ServiceCallOptions,
        Option<String>,
    ) {
        let (operation, path, query, pattern, depth) = match self.operation {
            FilesMcpOperationInput::Flat(operation) => {
                (operation, self.path, self.query, self.pattern, self.depth)
            }
            FilesMcpOperationInput::Nested(NestedFilesMcpOperation::Tree { path, depth }) => {
                (FileOperation::Tree, path, None, None, depth)
            }
            FilesMcpOperationInput::Nested(NestedFilesMcpOperation::Find { query }) => {
                (FileOperation::Find, None, Some(query), None, None)
            }
            FilesMcpOperationInput::Nested(NestedFilesMcpOperation::Glob { pattern }) => {
                (FileOperation::Glob, None, None, Some(pattern), None)
            }
        };
        (
            FilesRequest {
                operation,
                path,
                query,
                pattern,
                max_results: self.max_results,
                cursor: self.cursor,
                depth,
            },
            self.projection,
            self.consistency,
            service_call_options(self.max_response_tokens),
            self.expected_repository_id,
        )
    }
}
