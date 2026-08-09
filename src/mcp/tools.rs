use super::*;

#[tool_router(vis = "pub(super)")]
impl LeanTokenMcp {
    #[tool(
        name = "files",
        description = "Preferred over native find, ls, or glob for repository path discovery. Discover repository paths and metadata. Select a tagged operation with operation.kind: tree for hierarchy, find for fuzzy filenames, or glob for path patterns; returns paths, not source. Set the selected operation's projection=paths to omit per-entry metadata. Next: use outline or read once the file is known. Example: {\"operation\":{\"kind\":\"find\",\"query\":\"mcp\"}}."
    )]
    async fn leantoken_files(
        &self,
        Parameters(req): Parameters<FilesMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let prepared = match self
            .prepare_retrieval_call(
                context.ct.clone(),
                req.repository_context.as_deref(),
                |limits| req.validate_limits(limits),
            )
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let max_response_tokens = req.max_response_tokens();
        let (request, projection, consistency, options, expected_repository_id) = req.into_parts();
        self.run_prepared(
            "files",
            prepared,
            expected_repository_id,
            max_response_tokens,
            move |services, cancellation, deadline| {
                let request = request.clone();
                let options = options.with_initial_reconciliation_deadline(deadline);
                async move {
                    match projection {
                        FilesMcpProjection::Full => services
                            .files_with_options_consistency_cancellable(
                                request,
                                consistency,
                                options,
                                cancellation,
                            )
                            .await
                            .and_then(serialized_response),
                        FilesMcpProjection::Paths => services
                            .files_paths_with_options_consistency_cancellable(
                                request,
                                consistency,
                                options,
                                cancellation,
                            )
                            .await
                            .and_then(serialized_response),
                    }
                }
            },
        )
        .await
    }

    #[tool(
        name = "search",
        description = "Preferred over native grep or rg for repository source search. Search indexed source with a tagged operation.kind: auto, symbol, reference, identifier, text, or regex. Symbol and structural modes are ranked; projection=compact returns source-free coordinates and symbol identity without score/hash metadata. all_occurrences=true requires text or regex mode. projection=occurrences also requires all_occurrences=true; coordinates_only omits excerpts. query_receipt records or reuses only complete coverage and fails closed when relevant indexed files change. Counts are exact and bounded; enclosing_symbol and ranges identify the next read target. max_results uses the repository's configured cap (default 100; the active cap may be lower), and requests above it fail with the reported limit. Example: {\"operation\":{\"kind\":\"symbol\",\"query\":\"InternalFailure\",\"projection\":\"compact\"}}; exhaustive example: {\"operation\":{\"kind\":\"text\",\"query\":\"InternalFailure\",\"all_occurrences\":true,\"projection\":\"occurrences\"}}."
    )]
    async fn leantoken_search(
        &self,
        Parameters(req): Parameters<SearchMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let prepared = match self
            .prepare_retrieval_call(
                context.ct.clone(),
                req.repository_context.as_deref(),
                |limits| req.validate_limits(limits),
            )
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let max_response_tokens = req.max_response_tokens();
        let (request, output, consistency, options, expected_repository_id) = req.into_parts();
        self.run_prepared(
            "search",
            prepared,
            expected_repository_id,
            max_response_tokens,
            move |services, cancellation, deadline| {
                let request = request.clone();
                let options = options.with_initial_reconciliation_deadline(deadline);
                async move {
                    match output {
                        SearchMcpOutput::Full => services
                            .search_with_options_consistency_cancellable(
                                request,
                                consistency,
                                options,
                                cancellation,
                            )
                            .await
                            .and_then(serialized_response),
                        SearchMcpOutput::Compact => services
                            .search_compact_with_options_consistency_cancellable(
                                request,
                                consistency,
                                options,
                                cancellation,
                            )
                            .await
                            .and_then(serialized_response),
                        SearchMcpOutput::Grouped => services
                            .search_grouped_with_options_consistency_cancellable(
                                request,
                                consistency,
                                options,
                                cancellation,
                            )
                            .await
                            .and_then(serialized_response),
                        SearchMcpOutput::Occurrences { coordinates_only } => services
                            .search_occurrences_with_options_consistency_cancellable(
                                request,
                                coordinates_only,
                                consistency,
                                options,
                                cancellation,
                            )
                            .await
                            .and_then(serialized_response),
                    }
                }
            },
        )
        .await
    }

    #[tool(
        name = "outline",
        description = "Preferred before native whole-file reads when the file is known but the relevant range is not. Inspect known files without reading whole source files. Returns definitions, imports, ranges, and parse coverage; set projection=signatures to keep only compact signatures. Next: pass a returned symbol or range to read. Example: {\"paths\":[\"src/mcp/mod.rs\"]}."
    )]
    async fn leantoken_outline(
        &self,
        Parameters(req): Parameters<OutlineMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let prepared = match self
            .prepare_retrieval_call(
                context.ct.clone(),
                req.repository_context.as_deref(),
                |limits| req.validate_limits(limits),
            )
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let max_response_tokens = req.max_response_tokens;
        let (request, projection, consistency, options, expected_repository_id) = req.into_parts();
        self.run_prepared(
            "outline",
            prepared,
            expected_repository_id,
            max_response_tokens,
            move |services, cancellation, deadline| {
                let request = request.clone();
                let options = options.with_initial_reconciliation_deadline(deadline);
                async move {
                    match projection {
                        OutlineMcpProjection::Full => services
                            .outline_with_options_consistency_cancellable(
                                request,
                                consistency,
                                options,
                                cancellation,
                            )
                            .await
                            .and_then(serialized_response),
                        OutlineMcpProjection::Signatures => services
                            .outline_signatures_with_options_consistency_cancellable(
                                request,
                                consistency,
                                options,
                                cancellation,
                            )
                            .await
                            .and_then(serialized_response),
                    }
                }
            },
        )
        .await
    }

    #[tool(
        name = "read",
        description = "Preferred over native Read, cat, head, or sed for supported repository source. Read an exact source symbol, Markdown heading, line range, or continuation. Keep path separate from target; use the symbol or range returned by search or outline. Set delta=true or pass expected_hash to suppress unchanged content; truncated reads return a continuation cursor and source-budget guidance. Example: {\"path\":\"README.md\",\"target\":{\"kind\":\"heading\",\"name\":\"Installation\"}}."
    )]
    async fn leantoken_read(
        &self,
        Parameters(req): Parameters<ReadMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let prepared = match self
            .prepare_retrieval_call(
                context.ct.clone(),
                req.repository_context.as_deref(),
                |limits| req.validate_limits(limits),
            )
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let max_response_tokens = req.max_response_tokens;
        let (request, consistency, options, expected_repository_id) = req.into_parts();
        self.run_prepared(
            "read",
            prepared,
            expected_repository_id,
            max_response_tokens,
            move |services, cancellation, deadline| {
                let request = request.clone();
                let options = options.with_initial_reconciliation_deadline(deadline);
                async move {
                    services
                        .read_with_options_consistency_cancellable(
                            request,
                            consistency,
                            options,
                            cancellation,
                        )
                        .await
                }
            },
        )
        .await
    }

    #[tool(
        name = "history",
        description = "Preferred over native git show, diff, or log -L for parsed symbol history. Read, diff, batch-diff, or trace parsed symbols across immutable Git revisions. Use parent.name for qualified symbols; diff_symbols shares one range and returns bounded cursor-paged outcomes. For immutable context, pass BASE..HEAD as context.base_revision with strict_changed_paths. Example: {\"operation\":{\"kind\":\"diff_symbols\",\"targets\":[{\"path\":\"src/services.rs\",\"symbol\":\"Services.meta\"}],\"base_revision\":\"main~1\",\"head_revision\":\"main\"}}."
    )]
    async fn leantoken_history(
        &self,
        Parameters(req): Parameters<HistoryMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let prepared = match self
            .prepare_retrieval_call(
                context.ct.clone(),
                req.repository_context.as_deref(),
                |limits| req.validate_limits(limits),
            )
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let max_response_tokens = req.max_response_tokens();
        let (call, options, expected_repository_id) = req.into_parts().map_err(into_mcp_error)?;
        self.run_prepared(
            "history",
            prepared,
            expected_repository_id,
            max_response_tokens,
            move |services, cancellation, _deadline| {
                let call = call.clone();
                async move {
                    match call {
                        HistoryMcpCall::Single(request) => services
                            .history_cancellable_with_options(request, options, cancellation)
                            .await
                            .and_then(serialized_response),
                        HistoryMcpCall::DiffSymbols(request) => services
                            .history_diff_symbols_cancellable_with_options(
                                request,
                                options,
                                cancellation,
                            )
                            .await
                            .and_then(serialized_response),
                    }
                }
            },
        )
        .await
    }

    #[tool(
        name = "json",
        description = "Preferred over native jq or whole-file reads for bounded JSON inspection. Query, summarize, or compare bounded live JSON without indexing raw artifacts. The operation kinds are query, numeric_summary, and diff_fields; collapsed, keys, and schema are query projections, not operation kinds. Use JSON Pointer or JMESPath selectors; keys paginate in depth-then-pointer order with explicit omission metadata. JSON requests do not accept consistency. JMESPath expressions evaluate against the selected document root. Example: {\"operation\":{\"kind\":\"query\",\"path\":\"benchmarks/reports/graph-signal-ablation-v1.json\",\"projection\":\"keys\"}} or {\"operation\":{\"kind\":\"numeric_summary\",\"path\":\"benchmarks/reports/graph-signal-ablation-v1.json\",\"selector\":{\"kind\":\"jmespath\",\"expression\":\"graph_index.corpora[].cold_index_ms\"}}}. A zero numeric count means the selected path has no numeric leaves; it does not by itself mean the file is malformed."
    )]
    async fn leantoken_json(
        &self,
        Parameters(req): Parameters<JsonMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let prepared = match self
            .prepare_retrieval_call(
                context.ct.clone(),
                req.repository_context.as_deref(),
                |limits| req.validate_limits(limits),
            )
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let max_response_tokens = req.max_response_tokens();
        let (request, options, execution, expected_repository_id) = req.into_parts();
        self.run_prepared(
            "json",
            prepared,
            expected_repository_id,
            max_response_tokens,
            move |services, cancellation, _deadline| {
                let request = request.clone();
                async move {
                    services
                        .json_cancellable_with_execution_options(
                            request,
                            options,
                            execution,
                            cancellation,
                        )
                        .await
                }
            },
        )
        .await
    }

    #[tool(
        name = "context",
        description = "DEFAULT FIRST CALL for broad repository coding, debugging, review, or architecture triage. Build a bounded, ranked evidence bundle for a broad coding, debugging, review, or architecture task. Returns fragments, coverage, omissions, and an optional receipt; plan_only previews candidates without source or receipt mutation. Use strict scopes and required evidence when coverage must be explicit, response_profile=compact for the smallest fail-loud result, and known_hashes or receipt_id for follow-ups. Example: {\"task\":\"Audit MCP tool discovery\"}."
    )]
    async fn leantoken_context(
        &self,
        Parameters(req): Parameters<ContextMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let prepared = match self
            .prepare_retrieval_call(
                context.ct.clone(),
                req.repository_context.as_deref(),
                |limits| req.validate_limits(limits),
            )
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let max_response_tokens = req.max_response_tokens;
        let (
            request,
            workflow,
            workflow_evidence,
            consistency,
            options,
            expected_repository_id,
            handoff,
        ) = req.into_parts(prepared.limits.default_context_tokens);
        self.run_prepared(
            "context",
            prepared,
            expected_repository_id,
            max_response_tokens,
            move |services, cancellation, deadline| {
                let request = request.clone();
                let handoff = handoff.clone();
                let workflow_evidence = workflow_evidence.clone();
                let options = options.with_initial_reconciliation_deadline(deadline);
                async move {
                    services
                        .context_with_workflow_options_consistency_cancellable(
                            crate::services::ContextWorkflowOptions {
                                request,
                                handoff,
                                workflow,
                                workflow_evidence,
                                consistency,
                                options,
                                cancellation,
                            },
                        )
                        .await
                }
            },
        )
        .await
    }

    #[tool(
        name = "receipt_rebase",
        description = "Explicitly carry only exactly unchanged evidence from a stale server-managed receipt into the current committed generation. Requires the same repository/cache/scope identity and exact path, line coordinates, and content hash; never guesses line shifts, renames, symbol relocation, overlap, near-duplicates, or fuzzy matches. Returns complete carried/changed/missing/unmapped counts, bounded source-free samples, and a digest. Example: {\"receipt_id\":\"r...\",\"consistency\":\"reconcile_working_tree\"}."
    )]
    async fn leantoken_receipt_rebase(
        &self,
        Parameters(req): Parameters<ReceiptRebaseMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let prepared = match self
            .prepare_retrieval_call(
                context.ct.clone(),
                req.repository_context.as_deref(),
                |limits| req.validate_limits(limits),
            )
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let max_response_tokens = req.max_response_tokens;
        let (request, consistency, options, expected_repository_id) = req.into_parts();
        self.run_prepared(
            "receipt_rebase",
            prepared,
            expected_repository_id,
            max_response_tokens,
            move |services, cancellation, deadline| {
                let request = request.clone();
                let options = options.with_initial_reconciliation_deadline(deadline);
                async move {
                    services
                        .rebase_receipt_with_options_consistency_cancellable(
                            request,
                            consistency,
                            options,
                            cancellation,
                        )
                        .await
                        .and_then(serialized_response)
                }
            },
        )
        .await
    }

    #[tool(
        name = "savings",
        description = "Report repository-local response accounting, request classifications, expected-hash suppression, service failures, and unobserved task outcomes. Returns an opaque snapshot for a later bounded delta; savings are retrieval accounting, not task-success claims. Example: {}.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    pub(super) async fn leantoken_savings(
        &self,
        Parameters(req): Parameters<SavingsMcpRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let mcp_services = self
            .contexts
            .resolve(req.repository_context.as_deref())
            .map_err(into_mcp_error)?;
        let state = mcp_services.get();
        let services = match self.services(&state) {
            Ok(services) => services,
            Err(result) => return Ok(result),
        };
        self.run_admitted(services, None, |services| async move {
            services.observed_token_savings_snapshot(req.snapshot).await
        })
        .await
    }
}

#[tool_handler(name = "leantoken")]
impl ServerHandler for LeanTokenMcp {
    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, ErrorData>> + Send + '_ {
        std::future::ready(Ok(self.list_receipt_resources(context.protocol_version())))
    }

    fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourceTemplatesResult, ErrorData>> + Send + '_ {
        std::future::ready(Ok(
            self.list_receipt_resource_templates(context.protocol_version())
        ))
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ReadResourceResponse, ErrorData>> + Send + '_ {
        self.read_receipt_resource(request.uri, context.protocol_version())
    }

    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(rmcp::model::Implementation::new(
            "leantoken",
            mcp_runtime_version(),
        ))
        .with_instructions(MCP_INSTRUCTIONS.to_string())
    }

    fn on_initialized(
        &self,
        _context: NotificationContext<RoleServer>,
    ) -> impl Future<Output = ()> + Send + '_ {
        self.services.mark_protocol_initialized();
        std::future::ready(())
    }
}
