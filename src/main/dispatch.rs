use super::*;

pub(super) async fn run(cli: Cli) -> Result<()> {
    let json = cli.json;
    match cli.app_request() {
        AppRequest::Mcp { result_mode } => run_mcp(cli, result_mode).await,
        request => {
            let services = Arc::new(Services::open(cli.config()?)?);
            match request {
                AppRequest::Refresh => print(&services.refresh().await?, json),
                AppRequest::Search {
                    request,
                    projection,
                    max_response_tokens,
                } => {
                    let options = service_call_options(max_response_tokens);
                    match projection {
                        SearchProjectionArg::Full => {
                            print(&services.search_with_options(request, options).await?, json)
                        }
                        SearchProjectionArg::Compact => print(
                            &services
                                .search_compact_with_options(request, options)
                                .await?,
                            json,
                        ),
                        SearchProjectionArg::Occurrences => print(
                            &services
                                .search_occurrences_with_options(
                                    request,
                                    SearchOccurrenceOutput::Excerpts,
                                    options,
                                )
                                .await?,
                            json,
                        ),
                        SearchProjectionArg::Coordinates => print(
                            &services
                                .search_occurrences_with_options(
                                    request,
                                    SearchOccurrenceOutput::Coordinates,
                                    options,
                                )
                                .await?,
                            json,
                        ),
                    }
                }
                AppRequest::Outline {
                    request,
                    max_response_tokens,
                } => print(
                    &services
                        .outline_with_options(request, service_call_options(max_response_tokens))
                        .await?,
                    json,
                ),
                AppRequest::Read {
                    request,
                    max_response_tokens,
                } => print(
                    &services
                        .read_with_options(request, service_call_options(max_response_tokens))
                        .await?,
                    json,
                ),
                AppRequest::Context {
                    request,
                    workflow,
                    workflow_evidence,
                    max_response_tokens,
                    response_profile,
                } => {
                    let mut options = service_call_options(max_response_tokens);
                    if let Some(profile) = response_profile {
                        options = options.with_context_response_profile(profile);
                    }
                    print(
                        &services
                            .context_with_workflow_options_consistency_cancellable(
                                leantoken::services::ContextWorkflowOptions {
                                    request: *request,
                                    handoff: None,
                                    workflow,
                                    workflow_evidence,
                                    consistency: IndexConsistency::IndexedGeneration,
                                    options,
                                    cancellation: CancellationToken::new(),
                                },
                            )
                            .await?,
                        json,
                    )
                }
                AppRequest::Mcp { .. } => unreachable!("handled before opening services"),
            }
        }
    }
}
