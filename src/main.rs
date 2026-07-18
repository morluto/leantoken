use std::{io::Write, sync::Arc};

use clap::Parser;
use leantoken::{
    Config, Result,
    cli::{AppRequest, Cli},
    doctor, mcp,
    services::Services,
    setup::{self, SetupOperation},
    upgrade,
    watcher::{RepositoryWatcher, WatcherMessage},
};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() {
    init_tracing();
    if let Err(error) = run().await {
        if std::env::args_os().any(|argument| argument == "--json") {
            eprintln!("{}", serde_json::json!({ "error": error.to_string() }));
        } else {
            eprintln!("Error: {error}");
        }
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let json = cli.json;

    if matches!(
        &cli.command,
        leantoken::cli::Commands::Update(_) | leantoken::cli::Commands::Upgrade(_)
    ) {
        let AppRequest::Upgrade { check, yes } = cli.app_request() else {
            unreachable!("upgrade command checked above")
        };
        upgrade::run(upgrade::UpgradeOptions { check, yes, json })?;
        return Ok(());
    }

    if matches!(
        &cli.command,
        leantoken::cli::Commands::Setup(_) | leantoken::cli::Commands::Remove(_)
    ) {
        let (operation, request) = match cli.app_request() {
            AppRequest::Setup(request) => (SetupOperation::Setup, request),
            AppRequest::Remove(request) => (SetupOperation::Remove, request),
            _ => unreachable!("integration command checked above"),
        };
        let report = setup::run(operation, request, json)?;
        setup::print_report(&report, json)?;
        if report.has_failures() {
            return Err(leantoken::Error::InvalidRequest(
                "one or more MCP client configurations failed".into(),
            ));
        }
        return Ok(());
    }

    let config = cli.config()?;
    let request = cli.app_request();

    if let AppRequest::Doctor = request {
        if !json {
            doctor::print_progress()?;
        }
        let report = doctor::run(&config)?;
        doctor::print_report(&report, json)?;
        return Ok(());
    }

    if let AppRequest::Mcp { result_mode } = request {
        return run_mcp(config, result_mode).await;
    }

    let services = Arc::new(Services::open(config)?);

    match request {
        AppRequest::Index { rebuild } => print(&services.index(rebuild).await?, json),
        AppRequest::Status => print(&services.status().await?, json),
        AppRequest::Files(request) => print(&services.files(request).await?, json),
        AppRequest::Search(request) => print(&services.search(request).await?, json),
        AppRequest::Outline(request) => print(&services.outline(request).await?, json),
        AppRequest::Read(request) => print(&services.read(request).await?, json),
        AppRequest::Context(request) => print(&services.context(request).await?, json),
        AppRequest::Doctor => unreachable!("handled before service setup"),
        AppRequest::Mcp { .. } => unreachable!("handled before service setup"),
        AppRequest::Setup(_) | AppRequest::Remove(_) => {
            unreachable!("handled before service setup")
        }
        AppRequest::Upgrade { .. } => unreachable!("handled before repository setup"),
    }
}

async fn run_mcp(config: Config, result_mode: mcp::McpResultMode) -> Result<()> {
    let (server, service_state) = mcp::LeanTokenMcp::pending();
    let server = server.with_result_mode(result_mode);
    let mut server_task = tokio::spawn(mcp::serve_stdio_server(server));

    tokio::select! {
        result = &mut server_task => return result?,
        () = service_state.wait_initialized() => {}
    }

    let cancellation = CancellationToken::new();
    let runtime_cancellation = cancellation.clone();
    let runtime_state = service_state.clone();
    let mut runtime_task =
        tokio::spawn(
            async move { run_mcp_runtime(config, runtime_state, runtime_cancellation).await },
        );
    let failure_state = service_state;

    tokio::select! {
        server = &mut server_task => {
            cancellation.cancel();
            let server = server?;
            let runtime = runtime_task.await?;
            server?;
            match runtime {
                Ok(()) | Err(leantoken::Error::Cancelled) => Ok(()),
                Err(error) => Err(error),
            }
        }
        runtime = &mut runtime_task => {
            let error = match runtime {
                Ok(Ok(())) => leantoken::Error::McpRuntimeStopped,
                Ok(Err(error)) => error,
                Err(error) => error.into(),
            };
            failure_state.set_failed();
            tracing::error!(%error, "MCP indexing runtime failed");

            match server_task.await {
                Ok(Ok(())) => {}
                Ok(Err(server_error)) => {
                    tracing::warn!(%server_error, "MCP transport failed after indexing runtime stopped");
                }
                Err(join_error) => {
                    tracing::warn!(%join_error, "MCP transport task failed after indexing runtime stopped");
                }
            }
            Err(error)
        }
    }
}

async fn run_mcp_runtime(
    config: Config,
    service_state: mcp::McpServices,
    cancellation: CancellationToken,
) -> Result<()> {
    let startup_cancellation = cancellation.clone();
    let services = Arc::new(
        tokio::task::spawn_blocking(move || {
            Services::open_cancellable(config, &startup_cancellation)
        })
        .await??,
    );
    if cancellation.is_cancelled() {
        return Err(leantoken::Error::Cancelled);
    }
    service_state.set_ready(Arc::clone(&services));

    loop {
        if cancellation.is_cancelled() {
            return Ok(());
        }
        let services_for_leadership = Arc::clone(&services);
        let leader = tokio::task::spawn_blocking(move || {
            services_for_leadership.try_acquire_index_leadership()
        })
        .await??;

        if let Some(leader) = leader {
            let result = run_index_leader(Arc::clone(&services), cancellation.clone()).await;
            drop(leader);
            if cancellation.is_cancelled() {
                return Ok(());
            }
            if let Err(error) = result {
                tracing::error!(%error, "automatic indexing leadership failed");
            }
        }

        tokio::select! {
            _ = cancellation.cancelled() => return Ok(()),
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
        }
    }
}

async fn run_index_leader(services: Arc<Services>, cancellation: CancellationToken) -> Result<()> {
    let (watcher, mut changes) = RepositoryWatcher::start(
        &services.config().root,
        256,
        services.config().watcher_debounce,
        cancellation.clone(),
    )
    .await?;

    // The watcher is registered before the scan. Events queued during the scan
    // are applied afterward, closing the startup gap without a second walk.
    let indexed = match services
        .index_cancellable(false, cancellation.clone())
        .await
    {
        Ok(indexed) => indexed,
        Err(leantoken::Error::Cancelled) if cancellation.is_cancelled() => {
            return watcher.shutdown().await;
        }
        Err(error) => {
            if let Err(shutdown_error) = watcher.shutdown().await {
                tracing::warn!(%shutdown_error, "watcher shutdown failed after index error");
            }
            return Err(error);
        }
    };
    for warning in &indexed.warnings {
        tracing::warn!(%warning, "index warning");
    }

    loop {
        tokio::select! {
            _ = cancellation.cancelled() => break,
            message = changes.recv() => {
                let Some(message) = message else { break };
                let result = match message {
                    WatcherMessage::Changed { paths } => {
                        let paths = paths
                            .into_iter()
                            .filter(|path| !services.config().is_database_artifact(path))
                            .collect::<Vec<_>>();
                        if paths.is_empty() {
                            continue;
                        }
                        tracing::debug!(changed_paths = paths.len(), "repository change detected");
                        services
                            .index_paths_cancellable(paths, cancellation.clone())
                            .await
                    }
                    WatcherMessage::ReconcileRequired => {
                        tracing::warn!("watcher requested full reconciliation");
                        services
                            .index_cancellable(false, cancellation.clone())
                            .await
                    }
                };
                match result {
                    Ok(indexed) => {
                        for warning in &indexed.warnings {
                            tracing::warn!(%warning, "index warning");
                        }
                    }
                    Err(leantoken::Error::Cancelled) if cancellation.is_cancelled() => break,
                    Err(error) => {
                        tracing::error!(%error, "background reconciliation failed");
                    }
                }
            }
        }
    }

    watcher.shutdown().await
}

fn print<T: Serialize>(value: &T, compact: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    if compact {
        serde_json::to_writer(&mut lock, value)?;
    } else {
        serde_json::to_writer_pretty(&mut lock, value)?;
    }
    lock.write_all(b"\n")?;
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::filter::FilterFn;
    use tracing_subscriber::prelude::*;

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));

    // Safety-net filter: reject any log event that carries a field name which
    // could contain source content.  By design, LeanToken logs only paths,
    // counts, hashes, timings, and error summaries.  This filter acts as a
    // structural invariant that prevents source bodies from ever appearing in
    // structured log output.
    let scrub_fields = FilterFn::new(|meta: &tracing::Metadata<'_>| -> bool {
        let forbidden = [
            "source_body",
            "source_text",
            "file_content",
            "body",
            "token_text",
        ];
        !meta.fields().iter().any(|f| forbidden.contains(&f.name()))
    });

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_target(false)
                .with_filter(env_filter)
                .with_filter(scrub_fields),
        )
        .init();
}
