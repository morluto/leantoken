use super::*;
/// A candidate with a fully resolved score, token count, content hash, and
/// score-per-token diagnostic.
#[derive(Debug, Clone)]
#[must_use]
pub struct ScoredCandidate {
    pub candidate: Candidate,
    pub score: f64,
    pub token_count: usize,
    pub content_hash: String,
    pub marginal_score: f64,
}

impl ScoredCandidate {
    #[allow(clippy::cast_precision_loss)]
    pub fn new(candidate: Candidate, weights: &Weights) -> Self {
        Self::new_with_tokenizer(candidate, weights, tokens::Tokenizer::default())
    }

    #[allow(clippy::cast_precision_loss)]
    pub(in crate::ranking) fn new_with_tokenizer(
        candidate: Candidate,
        weights: &Weights,
        tokenizer: tokens::Tokenizer,
    ) -> Self {
        let token_count = candidate.token_count_with(tokenizer).max(1);
        let content_hash = candidate.content_hash();
        let score = candidate.score(weights, token_count);
        let marginal_score = score / token_count as f64;
        Self {
            candidate,
            score,
            token_count,
            content_hash,
            marginal_score,
        }
    }
}

/// Score all candidates and sort by descending combined score.  Ties are
/// broken by path and then starting line for deterministic ordering.
#[must_use]
pub fn rank(candidates: Vec<Candidate>, weights: &Weights) -> Vec<ScoredCandidate> {
    rank_with_tokenizer(candidates, weights, tokens::Tokenizer::default())
}

pub(in crate::ranking) fn rank_with_tokenizer(
    candidates: Vec<Candidate>,
    weights: &Weights,
    tokenizer: tokens::Tokenizer,
) -> Vec<ScoredCandidate> {
    let mut scored: Vec<ScoredCandidate> = candidates
        .into_iter()
        .map(|candidate| ScoredCandidate::new_with_tokenizer(candidate, weights, tokenizer))
        .collect();

    scored.sort_by(|a, b| {
        let ord = b.score.total_cmp(&a.score);
        if ord != Ordering::Equal {
            return ord;
        }
        let ord = a.candidate.path.cmp(&b.candidate.path);
        if ord != Ordering::Equal {
            return ord;
        }
        a.candidate.start_line.cmp(&b.candidate.start_line)
    });

    scored
}
