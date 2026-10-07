//! Resolve configured request bounds before acquiring repository resources.

use super::{validate_positive_request_limit, validate_request_limit};
use crate::{Config, Result};

pub(super) struct RequestLimits {
    pub(super) default_results: usize,
    pub(super) max_results: usize,
    pub(super) max_output_tokens: usize,
    pub(super) context_lines: usize,
}

impl RequestLimits {
    pub(super) fn from_config(config: &Config) -> Self {
        Self {
            default_results: config.default_results,
            max_results: config.max_results,
            max_output_tokens: config.max_output_tokens,
            context_lines: config.context_lines,
        }
    }

    pub(super) fn results(&self, requested: Option<usize>) -> Result<usize> {
        validate_positive_request_limit(
            "max_results",
            requested.unwrap_or(self.default_results),
            self.max_results,
        )
    }

    pub(super) fn tokens(&self, requested: Option<usize>, default: usize) -> Result<usize> {
        validate_positive_request_limit(
            "max_tokens",
            requested.unwrap_or(default),
            self.max_output_tokens,
        )
    }

    pub(super) fn token_budget(&self, requested: usize) -> Result<usize> {
        validate_positive_request_limit("token_budget", requested, self.max_output_tokens)
    }

    pub(super) fn context_lines(&self, requested: Option<usize>) -> Result<usize> {
        validate_request_limit(
            "context_lines",
            requested.unwrap_or(self.context_lines),
            crate::config::MAX_CONTEXT_LINES,
        )
    }
}
