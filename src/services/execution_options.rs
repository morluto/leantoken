use tokio_util::sync::CancellationToken;

use super::ServiceCallOptions;
use crate::model::IndexConsistency;

/// Canonical internal controls for one retrieval execution.
///
/// `consistency` remains optional because compatibility wrappers without an
/// explicit consistency boundary historically enter the blocking executor
/// before request validation. Keeping that distinction preserves cancellation
/// and validation ordering while still giving each service one execution path.
#[derive(Debug, Clone)]
pub(super) struct RetrievalExecution {
    pub(super) consistency: Option<IndexConsistency>,
    pub(super) options: ServiceCallOptions,
    pub(super) cancellation: CancellationToken,
}

impl RetrievalExecution {
    pub(super) fn direct(options: ServiceCallOptions, cancellation: CancellationToken) -> Self {
        Self {
            consistency: None,
            options,
            cancellation,
        }
    }

    pub(super) fn consistent(
        consistency: IndexConsistency,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            consistency: Some(consistency),
            options,
            cancellation,
        }
    }
}
