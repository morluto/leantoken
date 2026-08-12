use super::*;

#[tool_router(vis = "pub(super)")]
impl LeanTokenMcp {
    #[tool(
        name = "refresh",
        description = "Acquire the current repository working tree, build one complete derived generation, and publish it atomically. Search, outline, read, and context continue using the previous generation until publication completes. Call this explicitly after edits or checkout changes."
    )]
    async fn leantoken_refresh(
        &self,
        Parameters(req): Parameters<RefreshMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_admitted(
            Arc::clone(&self.services),
            req.expected_repository_id,
            move |services| async move { services.refresh_cancellable(context.ct).await },
        )
        .await
    }

    #[tool(
        name = "search",
        description = "Search source in the currently published immutable repository generation. Results, counts, and continuation cursors are bound to that generation. Call refresh explicitly to publish working-tree changes."
    )]
    async fn leantoken_search(
        &self,
        Parameters(req): Parameters<SearchMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let protocol = context.protocol_version();
        let prepared = match self
            .prepare_retrieval_call(context.ct.clone(), |limits| req.validate_limits(limits))
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let max_response_tokens = req.max_response_tokens();
        let (request, output, options, expected_repository_id) = req.into_parts();
        let options =
            options.with_mcp_response_shape(self.result_mode.response_shape(protocol.as_ref()));
        self.run_prepared(
            prepared,
            expected_repository_id,
            max_response_tokens,
            protocol,
            move |services, cancellation| {
                let request = request.clone();
                async move {
                    match output {
                        SearchMcpOutput::Full => services
                            .search_with_options_consistency_cancellable(
                                request,
                                IndexConsistency::IndexedGeneration,
                                options,
                                cancellation,
                            )
                            .await
                            .and_then(serialized_response),
                        SearchMcpOutput::Compact => services
                            .search_compact_with_options_consistency_cancellable(
                                request,
                                IndexConsistency::IndexedGeneration,
                                options,
                                cancellation,
                            )
                            .await
                            .and_then(serialized_response),
                        SearchMcpOutput::Grouped => services
                            .search_grouped_with_options_consistency_cancellable(
                                request,
                                IndexConsistency::IndexedGeneration,
                                options,
                                cancellation,
                            )
                            .await
                            .and_then(serialized_response),
                        SearchMcpOutput::Occurrences(output) => services
                            .search_occurrences_with_options_consistency_cancellable(
                                request,
                                output,
                                IndexConsistency::IndexedGeneration,
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
        description = "Return indexed definitions, signatures, ranges, and explicitly labelled import evidence from the currently published immutable generation. Use a returned symbol or range with read."
    )]
    async fn leantoken_outline(
        &self,
        Parameters(req): Parameters<OutlineMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let protocol = context.protocol_version();
        let prepared = match self
            .prepare_retrieval_call(context.ct.clone(), |limits| req.validate_limits(limits))
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let max_response_tokens = req.max_response_tokens;
        let (request, projection, options, expected_repository_id) = req.into_parts();
        let options =
            options.with_mcp_response_shape(self.result_mode.response_shape(protocol.as_ref()));
        self.run_prepared(
            prepared,
            expected_repository_id,
            max_response_tokens,
            protocol,
            move |services, cancellation| {
                let request = request.clone();
                async move {
                    match projection {
                        OutlineMcpProjection::Full => services
                            .outline_with_options_consistency_cancellable(
                                request,
                                IndexConsistency::IndexedGeneration,
                                options,
                                cancellation,
                            )
                            .await
                            .and_then(serialized_response),
                        OutlineMcpProjection::Signatures => services
                            .outline_signatures_with_options_consistency_cancellable(
                                request,
                                IndexConsistency::IndexedGeneration,
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
        description = "Read a symbol, heading, line range, or authenticated continuation from the currently published immutable generation. Working-tree edits are invisible until refresh publishes a new generation."
    )]
    async fn leantoken_read(
        &self,
        Parameters(req): Parameters<ReadMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let protocol = context.protocol_version();
        let prepared = match self
            .prepare_retrieval_call(context.ct.clone(), |limits| req.validate_limits(limits))
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let max_response_tokens = req.max_response_tokens;
        let (request, options, expected_repository_id) = req.into_parts();
        let options =
            options.with_mcp_response_shape(self.result_mode.response_shape(protocol.as_ref()));
        self.run_prepared(
            prepared,
            expected_repository_id,
            max_response_tokens,
            protocol,
            move |services, cancellation| {
                let request = request.clone();
                async move {
                    services
                        .read_with_options_consistency_cancellable(
                            request,
                            IndexConsistency::IndexedGeneration,
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
        name = "context",
        description = "Orchestrate bounded search, outline, and read operations over one published immutable generation for a broad coding task. Supply known content hashes directly; call refresh explicitly before requesting newer source."
    )]
    async fn leantoken_context(
        &self,
        Parameters(req): Parameters<ContextMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let protocol = context.protocol_version();
        let prepared = match self
            .prepare_retrieval_call(context.ct.clone(), |limits| req.validate_limits(limits))
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let max_response_tokens = req.max_response_tokens;
        let (request, workflow, workflow_evidence, options, expected_repository_id) =
            req.into_parts(prepared.limits.default_context_tokens);
        let options =
            options.with_mcp_response_shape(self.result_mode.response_shape(protocol.as_ref()));
        self.run_prepared(
            prepared,
            expected_repository_id,
            max_response_tokens,
            protocol,
            move |services, cancellation| {
                let request = request.clone();
                let workflow_evidence = workflow_evidence.clone();
                async move {
                    services
                        .context_with_workflow_options_consistency_cancellable(
                            crate::services::ContextWorkflowOptions {
                                request,
                                handoff: None,
                                workflow,
                                workflow_evidence,
                                consistency: IndexConsistency::IndexedGeneration,
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
}
