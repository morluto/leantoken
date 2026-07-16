use std::{io::Write, sync::Arc};

use clap::Parser;
use leantoken::{
    Result,
    cli::{AppRequest, Cli},
    mcp,
    services::Services,
    setup::{self, SetupOperation},
    watcher::{RepositoryWatcher, WatcherMessage},
};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() {
    init_tracing();
    if let Err(error) = run().await {
        tracing::error!(%error, "leantoken failed");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let json = cli.json;

    if matches!(
        &cli.command,
        leantoken::cli::Commands::Setup(_) | leantoken::cli::Commands::Remove(_)
    ) {
        let (operation, request) = match cli.app_request() {
            AppRequest::Setup(request) => (SetupOperation::Setup, request),
            AppRequest::Remove(request) => (SetupOperation::Remove, request),
            _ => unreachable!("integration command checked above"),
        };
        let report = setup::run(operation, request)?;
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
    let services = Arc::new(Services::open(config)?);

    match request {
        AppRequest::Index { rebuild } => print(&services.index(rebuild).await?, json),
        AppRequest::Status => print(&services.status().await?, json),
        AppRequest::Files(request) => print(&services.files(request).await?, json),
        AppRequest::Search(request) => print(&services.search(request).await?, json),
        AppRequest::Outline(request) => print(&services.outline(request).await?, json),
        AppRequest::Read(request) => print(&services.read(request).await?, json),
        AppRequest::Context(request) => print(&services.context(request).await?, json),
        AppRequest::Mcp { result_mode } => run_mcp(services, result_mode).await,
        AppRequest::Setup(_) | AppRequest::Remove(_) => {
            unreachable!("handled before service setup")
        }
    }
}

async fn run_mcp(services: Arc<Services>, result_mode: mcp::McpResultMode) -> Result<()> {
    let indexed = services.index(false).await?;
    for warning in &indexed.warnings {
        tracing::warn!(%warning, "index warning");
    }

    let cancellation = CancellationToken::new();
    let (watcher, mut changes) = RepositoryWatcher::start(
        &services.config().root,
        256,
        services.config().watcher_debounce,
        cancellation.clone(),
    )
    .await?;

    // Close the gap between the initial reconciliation and watcher startup.
    // Changes during this pass are already observed by the watcher and cause
    // another reconciliation through the task below.
    let caught_up = match services.index(false).await {
        Ok(indexed) => indexed,
        Err(error) => {
            cancellation.cancel();
            if let Err(shutdown_error) = watcher.shutdown().await {
                tracing::warn!(%shutdown_error, "watcher shutdown failed after index error");
            }
            return Err(error);
        }
    };
    for warning in &caught_up.warnings {
        tracing::warn!(%warning, "index warning");
    }

    let reconcile_services = Arc::clone(&services);
    let reconcile_cancellation = cancellation.clone();
    let reconcile_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = reconcile_cancellation.cancelled() => break,
                message = changes.recv() => {
                    let Some(message) = message else { break };
                    let result = match message {
                        WatcherMessage::Changed { paths } => {
                            let paths = paths
                                .into_iter()
                                .filter(|path| !reconcile_services.config().is_database_artifact(path))
                                .collect::<Vec<_>>();
                            if paths.is_empty() {
                                continue;
                            }
                            tracing::debug!(changed_paths = paths.len(), "repository change detected");
                            reconcile_services.index_paths(paths).await
                        }
                        WatcherMessage::ReconcileRequired => {
                            tracing::warn!("watcher requested full reconciliation");
                            reconcile_services.index(false).await
                        }
                    };
                    if let Err(error) = result {
                        tracing::error!(%error, "background reconciliation failed");
                    }
                }
            }
        }
    });

    let serve_result = mcp::serve_stdio(services, result_mode).await;
    cancellation.cancel();
    let watcher_result = watcher.shutdown().await;
    let reconcile_result = reconcile_task.await;

    serve_result?;
    watcher_result?;
    reconcile_result?;
    Ok(())
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
