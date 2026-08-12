use super::*;

/// Open exactly one repository and let rmcp own protocol lifecycle and
/// cancellation. Index publication is explicit through the refresh tool.
pub(super) async fn run_mcp(cli: Cli, result_mode: mcp::McpResultMode) -> Result<()> {
    let explicitly_configured = cli.max_index_workers.is_some();
    let mut config = tokio::task::spawn_blocking(move || cli.config()).await??;
    config.max_index_workers =
        mcp_index_worker_limit(config.max_index_workers, explicitly_configured);
    let services = tokio::task::spawn_blocking(move || Services::open(config)).await??;
    mcp::serve_stdio(Arc::new(services), result_mode).await
}
