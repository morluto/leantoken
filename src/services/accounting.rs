use crate::model::ResponseMeta;
use crate::receipt::RECEIPT_ID_RESPONSE_RESERVE;
use crate::tokens::{Tokenizer, response_token_accounting};
use crate::{Error, Result};

use super::{RetrievalResponse, ServiceCallOptions};

const MAX_ACCOUNTING_PASSES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReceiptResourceDecoration {
    Omit,
    Include,
}

/// Owns serialized response sizing and caller-ceiling enforcement.
///
/// Keeping this stateful boundary separate from operation orchestration makes
/// the fixed-point accounting rule shared by every retrieval operation while
/// leaving operation-specific truncation and receipt policy with its owner.
#[derive(Debug, Clone, Copy)]
pub(super) struct ResponseAccountant {
    tokenizer: Tokenizer,
}

impl ResponseAccountant {
    pub(super) const fn new(tokenizer: Tokenizer) -> Self {
        Self { tokenizer }
    }

    pub(super) fn finalize<T: RetrievalResponse>(&self, response: &mut T) -> Result<()> {
        self.finalize_for(response, None)
    }

    pub(super) fn finalize_for<T: RetrievalResponse>(
        &self,
        response: &mut T,
        mcp_response_shape: Option<crate::tokens::McpResponseShape>,
    ) -> Result<()> {
        let source_tokens = response.meta_mut().source_tokens;
        self.finalize_accounting(
            response,
            source_tokens,
            mcp_response_shape,
            ReceiptResourceDecoration::Omit,
        )
    }

    pub(super) fn finalized_tokens_for<T>(
        &self,
        response: &T,
        mcp_response_shape: Option<crate::tokens::McpResponseShape>,
    ) -> Result<usize>
    where
        T: RetrievalResponse + Clone,
    {
        let mut sized = response.clone();
        self.finalize_for(&mut sized, mcp_response_shape)?;
        Ok(sized.meta_mut().total_response_tokens)
    }

    pub(super) fn finalized_tokens_with_receipt_resource<T>(
        &self,
        response: &T,
        mcp_response_shape: Option<crate::tokens::McpResponseShape>,
    ) -> Result<usize>
    where
        T: RetrievalResponse + Clone,
    {
        let mut sized = response.clone();
        self.finalize_with_receipt_resource(&mut sized, mcp_response_shape)?;
        Ok(sized.meta_mut().total_response_tokens)
    }

    pub(super) fn finalize_with_receipt_resource<T: RetrievalResponse>(
        &self,
        response: &mut T,
        mcp_response_shape: Option<crate::tokens::McpResponseShape>,
    ) -> Result<()> {
        let source_tokens = response.meta_mut().source_tokens;
        if response.meta_mut().receipt_id.is_none() {
            return self.finalize_for(response, mcp_response_shape);
        }
        self.finalize_accounting(
            response,
            source_tokens,
            mcp_response_shape,
            ReceiptResourceDecoration::Include,
        )
    }

    fn finalize_accounting<T: RetrievalResponse>(
        &self,
        response: &mut T,
        source_tokens: usize,
        mcp_response_shape: Option<crate::tokens::McpResponseShape>,
        receipt_resource: ReceiptResourceDecoration,
    ) -> Result<()> {
        {
            let meta = response.meta_mut();
            meta.protocol_tokens = 0;
            meta.path_and_metadata_tokens = 0;
            meta.total_response_tokens = 0;
        }
        let mut observed = Vec::with_capacity(MAX_ACCOUNTING_PASSES);
        for _ in 0..MAX_ACCOUNTING_PASSES {
            let accounting = self.accounting(
                &*response,
                source_tokens,
                mcp_response_shape,
                receipt_resource,
            )?;
            let meta = response.meta_mut();
            if meta.protocol_tokens == accounting.protocol_tokens
                && meta.path_and_metadata_tokens == accounting.path_and_metadata_tokens
                && meta.total_response_tokens == accounting.total_response_tokens
            {
                return Ok(());
            }
            if observed.contains(&accounting) {
                // Exact inclusive accounting is a self-referential equation:
                // the accounting fields are part of the payload being counted.
                // Exact BPE tokenization can produce a short cycle at a digit
                // boundary rather than a fixed point. Keep the largest observed
                // total as a conservative ceiling instead of rejecting a valid
                // response; ordinary responses still take the exact path above.
                let fallback = observed
                    .iter()
                    .copied()
                    .chain([accounting])
                    .max_by_key(|value| value.total_response_tokens)
                    .expect("accounting cycle has an observed state");
                meta.protocol_tokens = fallback.protocol_tokens;
                meta.path_and_metadata_tokens = fallback.path_and_metadata_tokens;
                meta.total_response_tokens = fallback.total_response_tokens;
                return Ok(());
            }
            observed.push(accounting);
            meta.protocol_tokens = accounting.protocol_tokens;
            meta.path_and_metadata_tokens = accounting.path_and_metadata_tokens;
            meta.total_response_tokens = accounting.total_response_tokens;
        }
        Err(Error::ResponseAccountingInvariant(
            "serialized response accounting did not reach a fixed point".into(),
        ))
    }

    pub(super) fn fits<T>(&self, response: &T, options: ServiceCallOptions) -> Result<bool>
    where
        T: RetrievalResponse + Clone,
    {
        options.max_response_tokens().map_or(Ok(true), |limit| {
            Ok(self.finalized_tokens_for(response, options.mcp_response_shape())? <= limit)
        })
    }

    pub(super) fn fits_with_receipt_reserve<T>(
        &self,
        response: &T,
        returned_items: usize,
        options: ServiceCallOptions,
    ) -> Result<bool>
    where
        T: RetrievalResponse + Clone,
    {
        options.max_response_tokens().map_or(Ok(true), |limit| {
            Ok(
                self.finalized_tokens_with_receipt_reserve(response, returned_items, options)?
                    <= limit,
            )
        })
    }

    pub(super) fn finalized_tokens_with_receipt_reserve<T>(
        &self,
        response: &T,
        returned_items: usize,
        options: ServiceCallOptions,
    ) -> Result<usize>
    where
        T: RetrievalResponse + Clone,
    {
        let mut sized = response.clone();
        {
            let meta = sized.meta_mut();
            meta.receipt_id = Some(
                meta.receipt_id
                    .clone()
                    .unwrap_or_else(|| RECEIPT_ID_RESPONSE_RESERVE.into()),
            );
            meta.receipt_suppressed_exact = returned_items;
            meta.receipt_suppressed_overlap = returned_items;
            meta.receipt_near_duplicates = returned_items;
        }
        self.finalize_with_receipt_resource(&mut sized, options.mcp_response_shape())?;
        Ok(sized.meta_mut().total_response_tokens)
    }

    pub(super) fn budget_exceeded(
        meta: &ResponseMeta,
        provided_max_response_tokens: usize,
        minimum_required_response_tokens: usize,
    ) -> Error {
        debug_assert_eq!(
            meta.total_response_tokens,
            meta.source_tokens
                .saturating_add(meta.protocol_tokens)
                .saturating_add(meta.path_and_metadata_tokens)
        );
        debug_assert!(minimum_required_response_tokens >= meta.total_response_tokens);
        Error::ResponseBudgetExceeded {
            provided_max_response_tokens,
            minimum_required_response_tokens,
            retry_with_at_least: minimum_required_response_tokens,
            breakdown: crate::ResponseBudgetBreakdown {
                mandatory_response_tokens: meta.total_response_tokens,
                source_tokens: meta.source_tokens,
                protocol_tokens: meta.protocol_tokens,
                path_and_metadata_tokens: meta.path_and_metadata_tokens,
                receipt_reserve_tokens: minimum_required_response_tokens
                    .saturating_sub(meta.total_response_tokens),
            },
        }
    }

    pub(super) fn budget_error<T>(
        &self,
        response: &T,
        provided_max_response_tokens: usize,
        options: ServiceCallOptions,
    ) -> Result<Error>
    where
        T: RetrievalResponse + Clone,
    {
        let mut mandatory = response.clone();
        self.finalize_for(&mut mandatory, options.mcp_response_shape())?;
        let meta = mandatory.meta_mut();
        Ok(Self::budget_exceeded(
            meta,
            provided_max_response_tokens,
            meta.total_response_tokens,
        ))
    }

    pub(super) fn budget_error_with_receipt_reserve<T>(
        &self,
        response: &T,
        returned_items: usize,
        provided_max_response_tokens: usize,
        options: ServiceCallOptions,
    ) -> Result<Error>
    where
        T: RetrievalResponse + Clone,
    {
        let minimum_required_response_tokens =
            self.finalized_tokens_with_receipt_reserve(response, returned_items, options)?;
        let mut mandatory = response.clone();
        self.finalize_for(&mut mandatory, options.mcp_response_shape())?;
        Ok(Self::budget_exceeded(
            mandatory.meta_mut(),
            provided_max_response_tokens,
            minimum_required_response_tokens,
        ))
    }

    pub(super) fn finalize_bounded<T>(
        &self,
        response: &mut T,
        options: ServiceCallOptions,
    ) -> Result<()>
    where
        T: RetrievalResponse,
    {
        self.finalize_for(response, options.mcp_response_shape())?;
        if let Some(limit) = options.max_response_tokens()
            && response.meta_mut().total_response_tokens > limit
        {
            let minimum_required_response_tokens = response.meta_mut().total_response_tokens;
            return Err(Self::budget_exceeded(
                response.meta_mut(),
                limit,
                minimum_required_response_tokens,
            ));
        }
        Ok(())
    }

    fn accounting<T: serde::Serialize>(
        &self,
        response: &T,
        source_tokens: usize,
        mcp_response_shape: Option<crate::tokens::McpResponseShape>,
        receipt_resource: ReceiptResourceDecoration,
    ) -> serde_json::Result<crate::tokens::ResponseTokenAccounting> {
        if mcp_response_shape.is_none()
            && matches!(receipt_resource, ReceiptResourceDecoration::Omit)
        {
            return response_token_accounting(response, source_tokens, &self.tokenizer);
        }
        let mut value = serde_json::to_value(response)?;
        if matches!(receipt_resource, ReceiptResourceDecoration::Include) {
            let receipt_id = value
                .pointer("/meta/receipt_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            if let (Some(receipt_id), Some(object)) = (receipt_id, value.as_object_mut()) {
                object.insert(
                    "receipt_resource".into(),
                    serde_json::json!({
                        "kind": "retrieval_receipt",
                        "id": receipt_id,
                        "uri": format!("leantoken://receipt/v1/{receipt_id}"),
                    }),
                );
            }
        }
        match mcp_response_shape {
            Some(shape) => response_token_accounting(
                &crate::tokens::model_visible_mcp_result(value, shape),
                source_tokens,
                &self.tokenizer,
            ),
            None => response_token_accounting(&value, source_tokens, &self.tokenizer),
        }
    }
}
