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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;

    // Deliberately customized values catch hard-coded protocol defaults.
    fn limits() -> RequestLimits {
        RequestLimits {
            default_results: 3,
            max_results: 7,
            max_output_tokens: 91,
            context_lines: 4,
        }
    }

    fn assert_positive_limits(
        field: &'static str,
        maximum: usize,
        resolve: impl Fn(usize) -> Result<usize>,
    ) {
        for requested in [1, maximum] {
            assert_eq!(resolve(requested).unwrap(), requested);
        }
        assert!(
            matches!(resolve(0), Err(Error::InvalidInput { field: actual, reason: "must be greater than zero" }) if actual == field)
        );
        for requested in [maximum + 1, usize::MAX] {
            assert!(
                matches!(resolve(requested), Err(Error::RequestLimitExceeded { field: actual, requested: value, limit }) if actual == field && value == requested && limit == maximum)
            );
        }
    }

    #[test]
    fn files_enforces_result_limit_contract() {
        let limits = limits();
        assert_eq!(limits.results(None).unwrap(), 3);
        assert_positive_limits("max_results", 7, |value| limits.results(Some(value)));
    }

    #[test]
    fn search_enforces_all_limit_contracts() {
        let limits = limits();
        assert_eq!(limits.results(None).unwrap(), 3);
        assert_eq!(limits.tokens(None, 17).unwrap(), 17);
        assert_eq!(limits.context_lines(None).unwrap(), 4);
        assert_positive_limits("max_results", 7, |value| limits.results(Some(value)));
        assert_positive_limits("max_tokens", 91, |value| limits.tokens(Some(value), 17));
        for value in [0, 1, crate::config::MAX_CONTEXT_LINES] {
            assert_eq!(limits.context_lines(Some(value)).unwrap(), value);
        }
        for value in [crate::config::MAX_CONTEXT_LINES + 1, usize::MAX] {
            assert!(
                matches!(limits.context_lines(Some(value)), Err(Error::RequestLimitExceeded { field: "context_lines", requested, limit: crate::config::MAX_CONTEXT_LINES }) if requested == value)
            );
        }
    }

    #[test]
    fn outline_enforces_result_and_token_limit_contracts() {
        let limits = limits();
        assert_eq!(limits.results(None).unwrap(), 3);
        assert_eq!(limits.tokens(None, 17).unwrap(), 17);
        assert_positive_limits("max_results", 7, |value| limits.results(Some(value)));
        assert_positive_limits("max_tokens", 91, |value| limits.tokens(Some(value), 17));
    }

    #[test]
    fn read_enforces_token_limit_contract() {
        let limits = limits();
        assert_eq!(limits.tokens(None, 17).unwrap(), 17);
        assert_positive_limits("max_tokens", 91, |value| limits.tokens(Some(value), 17));
    }

    #[test]
    fn context_enforces_token_budget_contract() {
        assert_positive_limits("token_budget", 91, |value| limits().token_budget(value));
    }
}
