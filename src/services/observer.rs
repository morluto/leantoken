use crate::model::TokenAccountingOperation;
use crate::storage::Storage;
use crate::tokens::Tokenizer;
use crate::{Error, Result};

/// Records best-effort service outcomes without making telemetry a retrieval
/// dependency. The operation layer owns whether a result is successful; this
/// type owns classification and the bounded storage write.
#[derive(Debug, Clone)]
pub(super) struct ServiceObserver {
    storage: Storage,
    tokenizer: Tokenizer,
}

impl ServiceObserver {
    pub(super) fn new(storage: Storage, tokenizer: Tokenizer) -> Self {
        Self { storage, tokenizer }
    }

    pub(super) fn observe<T>(
        &self,
        operation: TokenAccountingOperation,
        result: Result<T>,
    ) -> Result<T> {
        if let Err(error) = &result {
            self.record_failure(operation, error);
        }
        result
    }

    fn record_failure(&self, operation: TokenAccountingOperation, error: &Error) {
        let category = error.observation_category();
        match self
            .storage
            .record_service_failure(self.tokenizer.name(), operation, category)
        {
            Ok(true) => {}
            Ok(false) => tracing::debug!(
                operation = operation.as_str(),
                error_category = category,
                "service-failure observation skipped a busy writer"
            ),
            Err(observation_error) => tracing::warn!(
                error = %observation_error,
                operation = operation.as_str(),
                error_category = category,
                "service-failure observation was skipped"
            ),
        }
    }
}
