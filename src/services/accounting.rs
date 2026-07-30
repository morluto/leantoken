use crate::model::ResponseMeta;
use crate::receipt::RECEIPT_ID_RESPONSE_RESERVE;
use crate::tokens::{Tokenizer, response_token_accounting};
use crate::{Error, Result};

use super::{RetrievalResponse, ServiceCallOptions};

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
        let source_tokens = {
            let meta = response.meta_mut();
            meta.protocol_tokens = 0;
            meta.path_and_metadata_tokens = 0;
            meta.total_response_tokens = 0;
            meta.source_tokens
        };
        const MAX_ACCOUNTING_PASSES: usize = 32;
        for _ in 0..MAX_ACCOUNTING_PASSES {
            let accounting = response_token_accounting(&*response, source_tokens, &self.tokenizer)?;
            let meta = response.meta_mut();
            if meta.protocol_tokens == accounting.protocol_tokens
                && meta.path_and_metadata_tokens == accounting.path_and_metadata_tokens
                && meta.total_response_tokens == accounting.total_response_tokens
            {
                return Ok(());
            }
            meta.protocol_tokens = accounting.protocol_tokens;
            meta.path_and_metadata_tokens = accounting.path_and_metadata_tokens;
            meta.total_response_tokens = accounting.total_response_tokens;
        }
        Err(Error::ResponseAccountingInvariant(
            "serialized response accounting did not reach a fixed point".into(),
        ))
    }

    pub(super) fn finalized_tokens<T>(&self, response: &T) -> Result<usize>
    where
        T: RetrievalResponse + Clone,
    {
        let mut sized = response.clone();
        self.finalize(&mut sized)?;
        Ok(sized.meta_mut().total_response_tokens)
    }

    pub(super) fn fits<T>(&self, response: &T, options: ServiceCallOptions) -> Result<bool>
    where
        T: RetrievalResponse + Clone,
    {
        options.max_response_tokens().map_or(Ok(true), |limit| {
            Ok(self.finalized_tokens(response)? <= limit)
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
            Ok(self.finalized_tokens_with_receipt_reserve(response, returned_items)? <= limit)
        })
    }

    pub(super) fn finalized_tokens_with_receipt_reserve<T>(
        &self,
        response: &T,
        returned_items: usize,
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
        self.finalized_tokens(&sized)
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
    ) -> Result<Error>
    where
        T: RetrievalResponse + Clone,
    {
        let mut mandatory = response.clone();
        self.finalize(&mut mandatory)?;
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
    ) -> Result<Error>
    where
        T: RetrievalResponse + Clone,
    {
        let minimum_required_response_tokens =
            self.finalized_tokens_with_receipt_reserve(response, returned_items)?;
        let mut mandatory = response.clone();
        self.finalize(&mut mandatory)?;
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
        self.finalize(response)?;
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
}
