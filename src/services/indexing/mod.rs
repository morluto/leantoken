use super::*;

impl Services {
    /// Acquire the bounded repository snapshot and atomically publish its complete
    /// derived generation. Retrieval never consults the working tree directly.
    pub async fn refresh(&self) -> Result<IndexResponse> {
        self.refresh_cancellable(CancellationToken::new()).await
    }

    /// Refresh the published generation while honoring caller cancellation.
    pub async fn refresh_cancellable(
        &self,
        cancellation: CancellationToken,
    ) -> Result<IndexResponse> {
        let this = self.clone();
        self.process_budget
            .run(cancellation, move |cancellation| {
                let operation = this.coordination.acquire_operation(cancellation)?;
                let result = this
                    .indexer
                    .reconcile_cancellable(IndexingMode::Rebuild, cancellation);
                operation.release()?;
                result
            })
            .await
    }
}
