//! Bounded ranked candidate collection shared by search and context.

use tokio_util::sync::CancellationToken;

use super::validation::check_cancelled;
use crate::Result;

pub(super) const MAX_FILTER_SCAN_ROWS: usize = 10_000;
const FILTER_SCAN_PAGE_SIZE: usize = 256;

pub(super) struct RankedCandidates<T> {
    pub(super) hits: Vec<T>,
    /// The scan stopped before filling the requested candidate set or reaching EOF.
    pub(super) scan_limited: bool,
}

pub(super) fn collect_ranked_candidates<T>(
    max_candidates: usize,
    cancellation: &CancellationToken,
    mut fetch_page: impl FnMut(usize, usize) -> Result<Vec<T>>,
    mut allows: impl FnMut(&T) -> bool,
) -> Result<RankedCandidates<T>> {
    let mut hits = Vec::new();
    let mut offset = 0usize;
    while hits.len() < max_candidates && offset < MAX_FILTER_SCAN_ROWS {
        check_cancelled(cancellation)?;
        let page_limit = if offset == 0 {
            max_candidates.min(FILTER_SCAN_PAGE_SIZE)
        } else {
            FILTER_SCAN_PAGE_SIZE
        }
        .min(MAX_FILTER_SCAN_ROWS - offset);
        let page = fetch_page(offset, page_limit)?;
        let page_len = page.len();
        for hit in page {
            check_cancelled(cancellation)?;
            if allows(&hit) {
                hits.push(hit);
                if hits.len() == max_candidates {
                    break;
                }
            }
        }
        offset = offset.saturating_add(page_len);
        if page_len < page_limit {
            return Ok(RankedCandidates {
                hits,
                scan_limited: false,
            });
        }
    }
    let scan_limited = hits.len() < max_candidates && offset == MAX_FILTER_SCAN_ROWS;
    Ok(RankedCandidates { hits, scan_limited })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejected_candidates_do_not_consume_the_result_limit() {
        let source = (0..80).collect::<Vec<_>>();
        let result = collect_ranked_candidates(
            3,
            &CancellationToken::new(),
            |offset, limit| Ok(source.iter().skip(offset).take(limit).copied().collect()),
            |value| *value >= 60,
        )
        .expect("ranked scan");
        assert_eq!(result.hits, [60, 61, 62]);
        assert!(!result.scan_limited);
    }

    #[test]
    fn scan_exhaustion_is_explicit_and_cancellation_propagates() {
        let result = collect_ranked_candidates(
            1,
            &CancellationToken::new(),
            |offset, limit| Ok((offset..offset + limit).collect()),
            |_| false,
        )
        .expect("bounded scan");
        assert!(result.hits.is_empty());
        assert!(result.scan_limited);
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert!(matches!(
            collect_ranked_candidates(1, &cancellation, |_, _| Ok(vec![1]), |_| true),
            Err(crate::Error::Cancelled)
        ));
    }
}
