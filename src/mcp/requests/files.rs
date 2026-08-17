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

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(in crate::mcp) struct FilesMcpRequest {
    /// Optional name of an approved repository context.
    #[serde(default)]
    #[schemars(schema_with = "repository_context_schema")]
    pub(in crate::mcp) repository_context: Option<String>,
    /// Expected opaque repository identity from an earlier response.
    #[serde(default)]
    #[schemars(schema_with = "expected_repository_id_schema")]
    pub(in crate::mcp) expected_repository_id: Option<String>,
    /// Tagged path operation. Bounds and cursors live on the variant that uses them.
    pub(in crate::mcp) operation: FilesMcpOperation,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::mcp) enum FilesMcpOperation {
    Tree {
        /// Optional repository-relative directory for `tree`.
        #[serde(default)]
        #[schemars(length(max = 4096))]
        path: Option<RepositoryPath>,
        /// Maximum hierarchy depth below `path`.
        #[serde(default)]
        depth: Option<usize>,
        #[serde(default)]
        #[schemars(schema_with = "result_limit_schema")]
        max_results: Option<usize>,
        #[serde(default)]
        #[schemars(schema_with = "response_token_limit_schema")]
        max_response_tokens: Option<usize>,
        #[serde(default)]
        #[schemars(schema_with = "files_cursor_schema")]
        cursor: Option<String>,
        #[serde(default)]
        #[schemars(schema_with = "index_consistency_schema")]
        consistency: IndexConsistency,
        #[serde(default)]
        projection: FilesMcpProjection,
    },
    Find {
        /// Non-empty fuzzy filename or path query for `find`.
        #[schemars(length(min = 1, max = 65536))]
        query: NonEmptyText,
        #[serde(default)]
        #[schemars(schema_with = "result_limit_schema")]
        max_results: Option<usize>,
        #[serde(default)]
        #[schemars(schema_with = "response_token_limit_schema")]
        max_response_tokens: Option<usize>,
        #[serde(default)]
        #[schemars(schema_with = "files_cursor_schema")]
        cursor: Option<String>,
        #[serde(default)]
        #[schemars(schema_with = "index_consistency_schema")]
        consistency: IndexConsistency,
        #[serde(default)]
        projection: FilesMcpProjection,
    },
    Glob {
        /// Non-empty glob pattern for `glob`.
        #[schemars(length(min = 1, max = 4096))]
        pattern: RepositoryPattern,
        #[serde(default)]
        #[schemars(schema_with = "result_limit_schema")]
        max_results: Option<usize>,
        #[serde(default)]
        #[schemars(schema_with = "response_token_limit_schema")]
        max_response_tokens: Option<usize>,
        #[serde(default)]
        #[schemars(schema_with = "files_cursor_schema")]
        cursor: Option<String>,
        #[serde(default)]
        #[schemars(schema_with = "index_consistency_schema")]
        consistency: IndexConsistency,
        #[serde(default)]
        projection: FilesMcpProjection,
    },
}

fn files_cursor_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "description": "Opaque cursor returned by the preceding files page; reuse the exact operation.",
        "type": ["string", "null"],
        "maxLength": crate::services::MAX_FILES_CURSOR_ENCODED_BYTES
    })
}

impl FilesMcpRequest {
    pub(in crate::mcp) fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        let (max_results, max_response_tokens) = match &self.operation {
            FilesMcpOperation::Tree {
                max_results,
                max_response_tokens,
                ..
            }
            | FilesMcpOperation::Find {
                max_results,
                max_response_tokens,
                ..
            }
            | FilesMcpOperation::Glob {
                max_results,
                max_response_tokens,
                ..
            } => (*max_results, *max_response_tokens),
        };
        validate_optional_positive_limit("max_results", max_results, limits.max_results)?;
        validate_optional_positive_limit(
            "max_response_tokens",
            max_response_tokens,
            limits.max_response_tokens,
        )
    }

    pub(in crate::mcp) fn max_response_tokens(&self) -> Option<usize> {
        match &self.operation {
            FilesMcpOperation::Tree {
                max_response_tokens,
                ..
            }
            | FilesMcpOperation::Find {
                max_response_tokens,
                ..
            }
            | FilesMcpOperation::Glob {
                max_response_tokens,
                ..
            } => *max_response_tokens,
        }
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
        let (
            operation,
            path,
            query,
            pattern,
            max_results,
            cursor,
            depth,
            projection,
            consistency,
            max_response_tokens,
        ) = match self.operation {
            FilesMcpOperation::Tree {
                path,
                depth,
                max_results,
                max_response_tokens,
                cursor,
                consistency,
                projection,
            } => (
                FileOperation::Tree,
                path.map(RepositoryPath::into_string),
                None,
                None,
                max_results,
                cursor,
                depth,
                projection,
                consistency,
                max_response_tokens,
            ),
            FilesMcpOperation::Find {
                query,
                max_results,
                max_response_tokens,
                cursor,
                consistency,
                projection,
            } => (
                FileOperation::Find,
                None,
                Some(query.into_string()),
                None,
                max_results,
                cursor,
                None,
                projection,
                consistency,
                max_response_tokens,
            ),
            FilesMcpOperation::Glob {
                pattern,
                max_results,
                max_response_tokens,
                cursor,
                consistency,
                projection,
            } => (
                FileOperation::Glob,
                None,
                None,
                Some(pattern.as_str().to_owned()),
                max_results,
                cursor,
                None,
                projection,
                consistency,
                max_response_tokens,
            ),
        };
        (
            FilesRequest {
                operation,
                path,
                query,
                pattern,
                max_results,
                cursor,
                depth,
            },
            projection,
            consistency,
            service_call_options(max_response_tokens),
            self.expected_repository_id,
        )
    }
}
