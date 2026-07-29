async fn run(cli: Cli) -> Result<()> {
    let json = cli.json;
    match &cli.command {
        leantoken::cli::Commands::Update(_) | leantoken::cli::Commands::Upgrade(_) => {
            run_upgrade_command(cli, json)
        }
        leantoken::cli::Commands::Cache(_) => run_cache_command(cli, json),
        leantoken::cli::Commands::Episode(_) => run_episode_command(cli, json),
        leantoken::cli::Commands::Setup(_) | leantoken::cli::Commands::Remove(_) => {
            run_integration_command(cli, json)
        }
        leantoken::cli::Commands::Mcp(args) => {
            let result_mode = args.result_mode;
            run_mcp(cli, result_mode).await
        }
        _ => run_repository_command(cli, json).await,
    }
}

fn run_episode_command(cli: Cli, json: bool) -> Result<()> {
    let AppRequest::EpisodeAudit(request) = cli.app_request() else {
        unreachable!("episode command checked by dispatch")
    };
    let report = episode::audit_episode(&request)?;
    if json {
        return print(&report, true);
    }
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    lock.write_all(report.to_markdown().as_bytes())?;
    Ok(())
}

fn run_upgrade_command(cli: Cli, json: bool) -> Result<()> {
    let AppRequest::Upgrade { check, yes } = cli.app_request() else {
        unreachable!("upgrade command checked by dispatch")
    };
    upgrade::run(upgrade::UpgradeOptions { check, yes, json })
}

fn run_cache_command(cli: Cli, json: bool) -> Result<()> {
    match cli.app_request() {
        AppRequest::CacheList(request) => {
            let report = cache::list_with(&request)?;
            cache::print_list(&report, json)
        }
        AppRequest::CachePrune(request) => {
            let report = cache::prune(&request)?;
            cache::print_prune(&report, json)?;
            ensure_cache_prune_succeeded(&report)
        }
        AppRequest::CacheListV2(request) => {
            let report = cache::list_v2_with(&request)?;
            cache::print_list_v2(&report, json)
        }
        AppRequest::CachePruneV2(request) => {
            let report = cache::prune_v2(&request)?;
            cache::print_prune(&report, json)?;
            ensure_cache_prune_succeeded(&report)
        }
        _ => unreachable!("cache command checked by dispatch"),
    }
}

fn ensure_cache_prune_succeeded(report: &cache::CachePruneReport) -> Result<()> {
    if report.has_failures() {
        return Err(leantoken::Error::InternalFailure(
            "one or more managed caches could not be pruned".into(),
        ));
    }
    Ok(())
}

fn run_integration_command(cli: Cli, json: bool) -> Result<()> {
    let (operation, request) = match cli.app_request() {
        AppRequest::Setup(request) => (SetupOperation::Setup, request),
        AppRequest::Remove(request) => (SetupOperation::Remove, request),
        _ => unreachable!("integration command checked by dispatch"),
    };
    let report = setup::run(operation, request, json)?;
    setup::print_report(&report, json)?;
    if report.has_failures() {
        return Err(leantoken::Error::InternalFailure(
            "one or more MCP client configurations failed".into(),
        ));
    }
    Ok(())
}

async fn run_repository_command(cli: Cli, json: bool) -> Result<()> {
    let requested_consistency = cli.retrieval_consistency();
    let config = cli.config()?;
    let request = cli.app_request();

    if let AppRequest::Doctor { ready_timeout } = request {
        if !json {
            doctor::print_progress()?;
        }
        let report = doctor::run(&config, ready_timeout)?;
        doctor::print_report(&report, json)?;
        return Ok(());
    }

    if matches!(&request, AppRequest::Status) {
        return print(&Services::status_without_initializing(config)?, json);
    }

    let services = Arc::new(Services::open(config)?);
    let consistency = resolve_retrieval_consistency(&services, requested_consistency).await?;
    dispatch_repository_request(services, request, consistency, json).await
}

async fn resolve_retrieval_consistency(
    services: &Services,
    requested: Option<IndexConsistency>,
) -> Result<IndexConsistency> {
    match requested {
        Some(IndexConsistency::ReconcileWorkingTree)
            if services.status().await?.index_state == IndexState::Uninitialized =>
        {
            Ok(IndexConsistency::IndexedGeneration)
        }
        Some(consistency) => Ok(consistency),
        None => Ok(IndexConsistency::IndexedGeneration),
    }
}

async fn dispatch_repository_request(
    services: Arc<Services>,
    request: AppRequest,
    consistency: IndexConsistency,
    json: bool,
) -> Result<()> {
    match request {
        AppRequest::Index { rebuild } => print(&services.index_report(rebuild).await?, json),
        AppRequest::Coverage => print(&services.parser_coverage().await?, json),
        AppRequest::Savings => {
            savings::print_report(&services.observed_token_savings_snapshot(None).await?, json)
        }
        AppRequest::SavingsDelta { snapshot } => savings::print_report(
            &services
                .observed_token_savings_snapshot(Some(snapshot))
                .await?,
            json,
        ),
        AppRequest::Files(request) => print(
            &services
                .files_with_consistency_cancellable(request, consistency, CancellationToken::new())
                .await?,
            json,
        ),
        AppRequest::FilesWithOptions {
            request,
            max_response_tokens,
        } => print(
            &services
                .files_with_options_consistency_cancellable(
                    request,
                    consistency,
                    service_call_options(Some(max_response_tokens)),
                    CancellationToken::new(),
                )
                .await?,
            json,
        ),
        AppRequest::Search(request) => print(
            &services
                .search_with_consistency_cancellable(request, consistency, CancellationToken::new())
                .await?,
            json,
        ),
        AppRequest::SearchWithOptions {
            request,
            max_response_tokens,
        } => print(
            &services
                .search_with_options_consistency_cancellable(
                    request,
                    consistency,
                    service_call_options(Some(max_response_tokens)),
                    CancellationToken::new(),
                )
                .await?,
            json,
        ),
        AppRequest::Outline(request) => print(
            &services
                .outline_with_consistency_cancellable(
                    request,
                    consistency,
                    CancellationToken::new(),
                )
                .await?,
            json,
        ),
        AppRequest::OutlineWithOptions {
            request,
            max_response_tokens,
        } => print(
            &services
                .outline_with_options_consistency_cancellable(
                    request,
                    consistency,
                    service_call_options(Some(max_response_tokens)),
                    CancellationToken::new(),
                )
                .await?,
            json,
        ),
        AppRequest::Read(request) => print(
            &services
                .read_with_consistency_cancellable(request, consistency, CancellationToken::new())
                .await?,
            json,
        ),
        AppRequest::ReadWithOptions {
            request,
            max_response_tokens,
        } => print(
            &services
                .read_with_options_consistency_cancellable(
                    request,
                    consistency,
                    service_call_options(Some(max_response_tokens)),
                    CancellationToken::new(),
                )
                .await?,
            json,
        ),
        AppRequest::History(request) => print(&services.history(request).await?, json),
        AppRequest::HistoryWithOptions {
            request,
            max_response_tokens,
        } => print(
            &services
                .history_with_options(request, service_call_options(Some(max_response_tokens)))
                .await?,
            json,
        ),
        AppRequest::Json(request) => print(&services.json(request).await?, json),
        AppRequest::JsonWithOptions {
            request,
            max_response_tokens,
        } => print(
            &services
                .json_with_options(request, service_call_options(Some(max_response_tokens)))
                .await?,
            json,
        ),
        AppRequest::Context {
            request,
            workflow,
            workflow_evidence,
            handoff,
            max_response_tokens,
            response_profile,
        } => {
            let mut options = service_call_options(max_response_tokens);
            if let Some(profile) = response_profile {
                options = options.with_context_response_profile(profile);
            }
            let response = services
                .context_with_workflow_evidence_options_consistency_cancellable(
                    request,
                    handoff.map(|handoff| *handoff),
                    workflow,
                    workflow_evidence,
                    consistency,
                    options,
                    CancellationToken::new(),
                )
                .await?;
            print(&response, json)
        }
        AppRequest::Status | AppRequest::Doctor { .. } => {
            unreachable!("handled before repository service setup")
        }
        AppRequest::Mcp { .. }
        | AppRequest::Setup(_)
        | AppRequest::Remove(_)
        | AppRequest::CacheList(_)
        | AppRequest::CachePrune(_)
        | AppRequest::CacheListV2(_)
        | AppRequest::CachePruneV2(_)
        | AppRequest::EpisodeAudit(_)
        | AppRequest::Upgrade { .. } => {
            unreachable!("repository-free command handled by top-level dispatch")
        }
    }
}
