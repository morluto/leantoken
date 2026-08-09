//! Token-bounded JSON query, summary, and diff service.
//!
//! The public entry points live on `Services` below; cohesive behavior is
//! decomposed into private submodules:
//! - [`execution`] — cursor versions, key ordering, execution options.
//! - [`cursor`] — cursor encoding, decoding, and query-hash derivation.
//! - [`validation`] — request, selector, limit, depth, and cursor validation.
//! - [`source`] — live JSON loading and token accounting.
//! - [`selection`] — JSON Pointer and JMESPath selection.
//! - [`projection`] — shared projection state and small value helpers.
//! - [`keys`] — keys projection and token-bounded pagination.
//! - [`schema`] — breadth-first schema inference and pagination.
//! - [`collapsed`] — collapsed projection with bounded array sampling.
//! - [`numeric`] — numeric summary aggregation.
//! - [`dispatch`] — the synchronous dispatcher and response fitting.

use tokio_util::sync::CancellationToken;

use super::{ServiceCallOptions, Services};
use crate::Result;
use crate::model::TokenAccountingOperation;

mod collapsed;
mod cursor;
mod dispatch;
mod execution;
mod keys;
mod numeric;
mod projection;
mod schema;
mod selection;
mod source;
mod validation;

pub(crate) use execution::{JsonExecutionOptions, MAX_JSON_DEPTH};

const DEFAULT_JSON_ITEMS: usize = 1_000;
const MAX_JSON_ITEMS: usize = 10_000;
const DEFAULT_ARRAY_SAMPLE_SIZE: usize = 3;
const MAX_ARRAY_SAMPLE_SIZE: usize = 20;
const MAX_JSON_SELECTORS: usize = 100;
const MAX_SCHEMA_OMITTED_POINTERS: usize = 32;

impl Services {
    /// Query, summarize, or compare bounded live JSON structures.
    pub async fn json(
        &self,
        request: crate::model::JsonRequest,
    ) -> Result<crate::model::JsonResponse> {
        self.json_with_options(request, ServiceCallOptions::new())
            .await
    }

    /// Query live JSON structures under serialized-response controls.
    pub async fn json_with_options(
        &self,
        request: crate::model::JsonRequest,
        options: ServiceCallOptions,
    ) -> Result<crate::model::JsonResponse> {
        self.json_cancellable_with_options(request, options, CancellationToken::new())
            .await
    }

    /// Query live JSON structures with cooperative cancellation.
    pub async fn json_cancellable(
        &self,
        request: crate::model::JsonRequest,
        cancellation: CancellationToken,
    ) -> Result<crate::model::JsonResponse> {
        self.json_cancellable_with_options(request, ServiceCallOptions::new(), cancellation)
            .await
    }

    /// Query live JSON structures under response controls and cancellation.
    pub async fn json_cancellable_with_options(
        &self,
        request: crate::model::JsonRequest,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<crate::model::JsonResponse> {
        self.json_cancellable_with_execution_options(
            request,
            options,
            execution::JsonExecutionOptions::standard(),
            cancellation,
        )
        .await
    }

    pub(crate) async fn json_cancellable_with_execution_options(
        &self,
        request: crate::model::JsonRequest,
        options: ServiceCallOptions,
        execution: JsonExecutionOptions,
        cancellation: CancellationToken,
    ) -> Result<crate::model::JsonResponse> {
        let operation = TokenAccountingOperation::Json;
        self.observe_service_result(operation, self.validate_call_options(options))?;
        let this = self.clone();
        let result = self
            .blocking_executor
            .run(cancellation, move |cancellation| {
                let request = validation::parse_json_request(request, execution)?;
                this.json_sync(request, options, execution, cancellation)
            })
            .await;
        self.observe_service_result(operation, result)
    }
}

#[cfg(test)]
mod tests;
