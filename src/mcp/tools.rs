#[tool_router]
impl LeanTokenMcp {
    #[tool(
        name = "files",
        description = "Preferred repository path discovery instead of find, ls, or glob. Use tree for hierarchy, find for fuzzy filenames, and glob for path patterns; returns paths, not source. Set projection=paths for opt-in path-only results without kind, language, size, or score metadata. Example: {\"operation\":\"find\",\"query\":\"mcp\"}."
    )]
    async fn leantoken_files(
        &self,
        Parameters(req): Parameters<FilesMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let prepared = match self
            .prepare_retrieval_call(context.ct.clone(), |limits| req.validate_limits(limits))
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let (request, projection, consistency, options, expected_repository_id) = req.into_parts();
        self.run_prepared(
            "files",
            prepared,
            expected_repository_id,
            move |services, cancellation| {
                let request = request.clone();
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
        description = "Preferred indexed source search instead of grep or rg. Finds ranked symbols, references, identifiers, text, or regex matches. Set projection=grouped for opt-in symbol/file summaries. Exhaustive text or regex searches default to projection=occurrences: one excerpt plus every exact line/column coordinate; set coordinates_only=true to omit excerpts and hashes. Use explicit projection=full for legacy per-occurrence hits. Exhaustive scans keep exact returned/total counts and fail instead of silently truncating at internal scan limits. Text and regex hits include the narrowest enclosing_symbol when structural data is available; use that exact name or the returned line range with leantoken.read. Example: {\"query\":\"RetryableConflict\",\"mode\":\"symbol\"}."
    )]
    async fn leantoken_search(
        &self,
        Parameters(req): Parameters<SearchMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let prepared = match self
            .prepare_retrieval_call(context.ct.clone(), |limits| req.validate_limits(limits))
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let (request, projection, coordinates_only, consistency, options, expected_repository_id) =
            req.into_parts();
        self.run_prepared(
            "search",
            prepared,
            expected_repository_id,
            move |services, cancellation| {
                let request = request.clone();
                async move {
                    match projection {
                        SearchMcpProjection::Auto => {
                            unreachable!("search projection is resolved by into_parts")
                        }
                        SearchMcpProjection::Full => services
                            .search_with_options_consistency_cancellable(
                                request,
                                consistency,
                                options,
                                cancellation,
                            )
                            .await
                            .and_then(serialized_response),
                        SearchMcpProjection::Grouped => services
                            .search_grouped_with_options_consistency_cancellable(
                                request,
                                consistency,
                                options,
                                cancellation,
                            )
                            .await
                            .and_then(serialized_response),
                        SearchMcpProjection::Occurrences => services
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
        description = "Inspect file structure without reading whole source files. Prefer this when the file is known but the relevant symbol or range is not; then use leantoken.read. Set projection=signatures to omit imports and byte offsets while retaining path, line range, signature-set hash, parse coverage, freshness, and continuation. Example: {\"paths\":[\"src/mcp.rs\"]}."
    )]
    async fn leantoken_outline(
        &self,
        Parameters(req): Parameters<OutlineMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let prepared = match self
            .prepare_retrieval_call(context.ct.clone(), |limits| req.validate_limits(limits))
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let (request, projection, consistency, options, expected_repository_id) = req.into_parts();
        self.run_prepared(
            "outline",
            prepared,
            expected_repository_id,
            move |services, cancellation| {
                let request = request.clone();
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
        description = "Preferred exact source and Markdown section reader instead of cat, head, or sed. Keep path as a file path; put the owner separately in target. Exact target shapes include {\"kind\":\"symbol\",\"name\":\"LeanTokenMcp\"}, {\"kind\":\"heading\",\"name\":\"## Performance\",\"occurrence\":2}, and {\"kind\":\"lines\",\"start\":120,\"end\":160}. Heading targets accept an exact rendered title or outline signature. Set delta=true to reuse the latest compatible base for the exact target; unchanged content returns not_modified. Pass expected_hash to require one explicit base. Example: {\"path\":\"README.md\",\"target\":{\"kind\":\"heading\",\"name\":\"Installation\"}}."
    )]
    async fn leantoken_read(
        &self,
        Parameters(req): Parameters<ReadMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let prepared = match self
            .prepare_retrieval_call(context.ct.clone(), |limits| req.validate_limits(limits))
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let (request, consistency, options, expected_repository_id) = req.into_parts();
        self.run_prepared(
            "read",
            prepared,
            expected_repository_id,
            move |services, cancellation| {
                let request = request.clone();
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
        description = "Read, diff, batch-diff, or trace parsed symbols across immutable Git revisions. Symbols may use parent.name qualification. diff_symbols resolves one shared range, loads each bounded path once per endpoint, and returns cursor-paged per-symbol outcomes without N Git subprocess chains. diff_symbol returns bounded add/delete diffs when one endpoint is absent; symbol_log traces tracked lines. For immutable range-scoped context, pass BASE..HEAD as context.base_revision with strict_changed_paths. Example: {\"operation\":{\"kind\":\"diff_symbols\",\"targets\":[{\"path\":\"src/services.rs\",\"symbol\":\"Services.meta\"}],\"base_revision\":\"main~1\",\"head_revision\":\"main\"}}."
    )]
    async fn leantoken_history(
        &self,
        Parameters(req): Parameters<HistoryMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let prepared = match self
            .prepare_retrieval_call(context.ct.clone(), |limits| req.validate_limits(limits))
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let (call, options, expected_repository_id) = req.into_parts().map_err(into_mcp_error)?;
        self.run_prepared(
            "history",
            prepared,
            expected_repository_id,
            move |services, cancellation| {
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
        description = "Query, summarize, or compare bounded live JSON without indexing raw artifacts. Select with RFC 6901 JSON Pointer or standard JMESPath; use collapsed, keys, or schema projections for large arrays and objects, numeric_summary for count/min/median/p95/max, and diff_fields for selected values across two files. Keys can be bounded by depth (root is zero) and paginate in depth-then-pointer order; incomplete schemas return a breadth-first shape with explicit omission metadata. Repeat an incomplete keys query with its cursor. Example: {\"operation\":{\"kind\":\"numeric_summary\",\"path\":\"artifacts/results.json\",\"selector\":{\"kind\":\"jmespath\",\"expression\":\"runs[].score\"}}}."
    )]
    async fn leantoken_json(
        &self,
        Parameters(req): Parameters<JsonMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let prepared = match self
            .prepare_retrieval_call(context.ct.clone(), |limits| req.validate_limits(limits))
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let (request, options, execution, expected_repository_id) = req.into_parts();
        self.run_prepared(
            "json",
            prepared,
            expected_repository_id,
            move |services, cancellation| {
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
        description = "DEFAULT FIRST CALL for broad coding, debugging, review, and architecture tasks. Returns the most relevant repository evidence within a strict token budget instead of manually combining search and whole-file reads. For uncertain broad tasks, set plan_only=true to preview bounded ranked paths, ranges, reasons, token estimates, focus coverage, and generated-artifact warnings without source or receipt mutation; then repeat the same request with plan_only=false to materialize. Use include_paths, strict_focus_paths, or strict_changed_paths for hard boundaries; pass BASE..HEAD as base_revision for an immutable Git range. Use minimum_fragments_per_focus_path and must-include constraints for required paths or symbols. When path presence is insufficient, pass required_evidence entries with a path and literal queries; path_scope_satisfied reports only path coverage, while evidence_scope_satisfied requires matching selected evidence. When the caller has directly observed a failure, pass workflow_evidence with bounded failure_traces, symbols, paths, or test_intents; do not infer or copy gold labels into it. Use response_profile=compact for the smallest fail-loud presentation, balanced for the historical default, or explain for bounded omission and diff detail. Legacy verbose_diagnostics=true maps to explain and conflicts with an explicit compact or balanced profile. Oversized diff scopes may return bounded routing suggestions. Reuse receipt fragment_hashes as known_hashes. Set handoff for a compact provenance manifest without copied source. Example: {\"task\":\"Audit MCP tool discovery\"}."
    )]
    async fn leantoken_context(
        &self,
        Parameters(req): Parameters<ContextMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let prepared = match self
            .prepare_retrieval_call(context.ct.clone(), |limits| req.validate_limits(limits))
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
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
            move |services, cancellation| {
                let request = request.clone();
                let handoff = handoff.clone();
                let workflow_evidence = workflow_evidence.clone();
                async move {
                    services
                        .context_with_workflow_evidence_options_consistency_cancellable(
                            request,
                            handoff,
                            workflow,
                            workflow_evidence,
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
        name = "savings",
        description = "Report repository-local observed response accounting, request classifications, expected-hash suppression, service failures, and explicitly unobserved task outcomes. Returns an opaque snapshot; supply it later for a bounded aggregate delta. Source compression and full-response net cost are separate comparisons against represented source, not claims about task success or complete session savings. Example: {}.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn leantoken_savings(
        &self,
        Parameters(req): Parameters<SavingsMcpRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.services.get();
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
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
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
