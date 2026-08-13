use super::*;

pub(super) async fn run(cli: Cli) -> Result<()> {
    let json = cli.json;
    match cli.app_request() {
        AppRequest::Upgrade { check, yes } => {
            upgrade::run(upgrade::UpgradeOptions { check, yes, json })
        }
        AppRequest::CacheList(request) => {
            let report = cache::list_with(&request)?;
            cache::print_list(&report, json)
        }
        AppRequest::CachePrune(request) => {
            let report = cache::prune(&request)?;
            cache::print_prune(&report, json)?;
            ensure_cache_prune_succeeded(&report)
        }
        AppRequest::RuntimeList => setup::print_runtime_list(&setup::list_runtimes()?, json),
        AppRequest::RuntimePrune(request) => {
            let report = setup::prune_runtimes(request)?;
            setup::print_runtime_prune(&report, json)?;
            if report.has_failures() {
                return Err(leantoken::Error::SetupFailure(
                    "one or more private runtimes could not be pruned".into(),
                ));
            }
            Ok(())
        }
        AppRequest::EpisodeAudit(request) => run_episode_audit(&request, json),
        AppRequest::Setup(request) => run_integration(SetupOperation::Setup, request, json),
        AppRequest::Remove(request) => run_integration(SetupOperation::Remove, request, json),
        AppRequest::Mcp { result_mode } => run_mcp(cli, result_mode).await,
        AppRequest::Doctor {
            ready_timeout,
            client,
        } => {
            let config = cli.config()?;
            if !json {
                doctor::print_progress()?;
            }
            let report = client.map_or_else(
                || doctor::run(&config, ready_timeout),
                |client| doctor::run_configured_client(&config, ready_timeout, client),
            )?;
            doctor::print_report(&report, json)
        }
        AppRequest::Status => print(&Services::status_without_initializing(cli.config()?)?, json),
        AppRequest::Refresh { mode } => {
            let services = repository_services(&cli)?;
            print(&services.refresh_report(mode).await?, json)
        }
        AppRequest::Coverage => {
            let services = repository_services(&cli)?;
            print(&services.parser_coverage().await?, json)
        }
        AppRequest::Savings => {
            let services = repository_services(&cli)?;
            savings::print_report(&services.observed_token_savings_snapshot(None).await?, json)
        }
        AppRequest::SavingsDelta { snapshot } => {
            let services = repository_services(&cli)?;
            savings::print_report(
                &services
                    .observed_token_savings_snapshot(Some(snapshot))
                    .await?,
                json,
            )
        }
        AppRequest::Files {
            request,
            max_response_tokens,
        } => {
            let services = repository_services(&cli)?;
            print(
                &services
                    .files_with_options_cancellable(
                        request,
                        service_call_options(max_response_tokens),
                        CancellationToken::new(),
                    )
                    .await?,
                json,
            )
        }
        AppRequest::Search {
            request,
            max_response_tokens,
            projection,
        } => {
            let services = repository_services(&cli)?;
            match projection {
                SearchProjectionArg::Full => print(
                    &services
                        .search_with_options_cancellable(
                            request,
                            service_call_options(max_response_tokens),
                            CancellationToken::new(),
                        )
                        .await?,
                    json,
                ),
                SearchProjectionArg::Compact => print(
                    &services
                        .search_compact_with_options_cancellable(
                            request,
                            service_call_options(max_response_tokens),
                            CancellationToken::new(),
                        )
                        .await?,
                    json,
                ),
                SearchProjectionArg::Occurrences => print(
                    &services
                        .search_occurrences_with_options_cancellable(
                            request,
                            SearchOccurrenceOutput::Excerpts,
                            service_call_options(max_response_tokens),
                            CancellationToken::new(),
                        )
                        .await?,
                    json,
                ),
                SearchProjectionArg::Coordinates => print(
                    &services
                        .search_occurrences_with_options_cancellable(
                            request,
                            SearchOccurrenceOutput::Coordinates,
                            service_call_options(max_response_tokens),
                            CancellationToken::new(),
                        )
                        .await?,
                    json,
                ),
            }
        }
        AppRequest::Outline {
            request,
            max_response_tokens,
        } => {
            let services = repository_services(&cli)?;
            print(
                &services
                    .outline_with_options_cancellable(
                        request,
                        service_call_options(max_response_tokens),
                        CancellationToken::new(),
                    )
                    .await?,
                json,
            )
        }
        AppRequest::Read {
            request,
            max_response_tokens,
        } => {
            let services = repository_services(&cli)?;
            print(
                &services
                    .read_with_options_cancellable(
                        request,
                        service_call_options(max_response_tokens),
                        CancellationToken::new(),
                    )
                    .await?,
                json,
            )
        }
        AppRequest::History {
            request,
            max_response_tokens,
        } => {
            let services = repository_services(&cli)?;
            print(
                &services
                    .history_with_options(request, service_call_options(max_response_tokens))
                    .await?,
                json,
            )
        }
        AppRequest::Json {
            request,
            max_response_tokens,
        } => {
            let services = repository_services(&cli)?;
            print(
                &services
                    .json_with_options(request, service_call_options(max_response_tokens))
                    .await?,
                json,
            )
        }
        AppRequest::Context {
            request,
            consistency,
            workflow,
            workflow_evidence,
            handoff,
            max_response_tokens,
            response_profile,
        } => {
            let services = repository_services(&cli)?;
            let mut options = service_call_options(max_response_tokens);
            if let Some(profile) = response_profile {
                options = options.with_context_response_profile(profile);
            }
            let response = services
                .context_with_workflow_options_consistency_cancellable(
                    leantoken::services::ContextWorkflowOptions {
                        request,
                        handoff,
                        workflow,
                        workflow_evidence,
                        consistency,
                        options,
                        cancellation: CancellationToken::new(),
                    },
                )
                .await?;
            print(&response, json)
        }
    }
}

fn repository_services(cli: &Cli) -> Result<Arc<Services>> {
    Ok(Arc::new(Services::open(cli.config()?)?))
}

fn run_episode_audit(request: &episode::EpisodeAuditRequest, json: bool) -> Result<()> {
    let report = episode::audit_episode(request).map_err(|error| match error {
        episode::Error::Io(error) => leantoken::Error::Io(error),
        episode::Error::InvalidRequest(message) => leantoken::Error::InvalidRequest(message),
    })?;
    if json {
        return print(&report, true);
    }
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    lock.write_all(report.to_markdown().as_bytes())?;
    Ok(())
}

fn ensure_cache_prune_succeeded(report: &cache::CachePruneReport) -> Result<()> {
    if report.has_failures() {
        return Err(leantoken::Error::CachePruneFailure(
            "one or more managed caches could not be pruned".into(),
        ));
    }
    Ok(())
}

fn run_integration(
    operation: SetupOperation,
    request: setup::SetupRequest,
    json: bool,
) -> Result<()> {
    let report = setup::run(operation, request, json)?;
    setup::print_report(&report, json)?;
    let message = match (
        report.has_apply_failure(),
        report.has_client_failures(),
        report.has_verification_failure(),
    ) {
        (false, false, false) => return Ok(()),
        (true, _, true) => "setup transaction and launcher verification failed",
        (true, _, false) => "setup transaction failed",
        (false, true, true) => "MCP client configuration and launcher verification failed",
        (false, true, false) => "one or more MCP client configurations failed",
        (false, false, true) => "MCP launcher verification failed",
    };
    Err(leantoken::Error::SetupFailure(message.into()))
}
