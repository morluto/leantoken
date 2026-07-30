use super::*;
/// Linear scoring weights for ranking signals.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Weights {
    pub exact: f64,
    pub symbol: f64,
    pub reference: f64,
    pub bm25: f64,
    pub path: f64,
    pub lexical_frequency_penalty: f64,
    pub size: f64,
    pub focus: f64,
    pub import: f64,
    pub change: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            exact: 1.0,
            symbol: 0.8,
            reference: 0.5,
            bm25: 0.4,
            path: 0.25,
            lexical_frequency_penalty: 0.25,
            size: 0.15,
            focus: 0.35,
            import: 0.25,
            change: 0.2,
        }
    }
}

/// Internal candidate carrying every signal used by the ranker.
#[derive(Debug, Clone)]
#[must_use]
pub struct Candidate {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
    pub match_kinds: Vec<String>,
    pub concepts: Vec<String>,
    pub concept_weight: f64,
    pub representation: String,
    pub symbol_name: Option<String>,
    pub target_start_line: Option<usize>,
    pub target_end_line: Option<usize>,
    pub exact: f64,
    pub symbol: f64,
    pub reference: f64,
    pub bm25: f64,
    pub path_score: f64,
    pub lexical_frequency_penalty: f64,
    pub size_score: f64,
    pub focus_boost: f64,
    pub import_boost: f64,
    pub change_boost: f64,
}

impl Candidate {
    /// Create a candidate with all signals initialized to zero and a default
    /// `representation` of `"source"`.
    pub fn new(
        path: impl Into<String>,
        start_line: usize,
        end_line: usize,
        content: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            start_line,
            end_line,
            content: content.into(),
            match_kinds: Vec::new(),
            concepts: Vec::new(),
            concept_weight: 0.0,
            representation: "source".into(),
            symbol_name: None,
            target_start_line: None,
            target_end_line: None,
            exact: 0.0,
            symbol: 0.0,
            reference: 0.0,
            bm25: 0.0,
            path_score: 0.0,
            lexical_frequency_penalty: 0.0,
            size_score: 0.0,
            focus_boost: 0.0,
            import_boost: 0.0,
            change_boost: 0.0,
        }
    }

    pub fn match_kind(mut self, kind: impl Into<String>) -> Self {
        self.match_kinds.push(kind.into());
        self
    }

    pub(crate) fn facet(mut self, kind: &str, fusion_key: &str) -> Self {
        self.push_metadata(format!("{FACET_PREFIX}{kind}:{fusion_key}"));
        self
    }

    pub(crate) fn channel(mut self, channel: &str, rank: usize) -> Self {
        self.push_metadata(format!("{CHANNEL_PREFIX}{channel}:{rank}"));
        self
    }

    pub(in crate::ranking) fn push_metadata(&mut self, value: String) {
        if !self.match_kinds.contains(&value) {
            self.match_kinds.push(value);
        }
    }

    /// Associate this evidence with an independently extracted task concept.
    pub fn concept(mut self, concept: impl Into<String>, weight: f64) -> Self {
        let concept = concept.into();
        if !concept.is_empty() && !self.concepts.contains(&concept) {
            self.concepts.push(concept);
        }
        self.concept_weight = self.concept_weight.max(weight.clamp(0.0, 2.0));
        self
    }

    pub fn representation(mut self, representation: impl Into<String>) -> Self {
        self.representation = representation.into();
        self
    }

    pub fn symbol_name(mut self, name: impl Into<String>) -> Self {
        self.symbol_name = Some(name.into());
        self
    }

    pub fn target_range(mut self, start_line: usize, end_line: usize) -> Self {
        self.target_start_line = Some(start_line);
        self.target_end_line = Some(end_line);
        self
    }

    pub(in crate::ranking) fn target_truncated(&self) -> bool {
        self.target_start_line
            .zip(self.target_end_line)
            .is_some_and(|(start, end)| self.start_line > start || self.end_line < end)
    }

    pub fn exact(mut self, value: f64) -> Self {
        self.exact = value;
        self
    }

    pub fn symbol(mut self, value: f64) -> Self {
        self.symbol = value;
        self
    }

    pub fn reference(mut self, value: f64) -> Self {
        self.reference = value;
        self
    }

    pub fn bm25(mut self, value: f64) -> Self {
        self.bm25 = value;
        self
    }

    pub fn path_score(mut self, value: f64) -> Self {
        self.path_score = value;
        self
    }

    pub fn lexical_frequency_penalty(mut self, value: f64) -> Self {
        self.lexical_frequency_penalty = value;
        self
    }

    pub fn size_score(mut self, value: f64) -> Self {
        self.size_score = value;
        self
    }

    pub fn focus_boost(mut self, value: f64) -> Self {
        self.focus_boost = value;
        self
    }

    pub fn import_boost(mut self, value: f64) -> Self {
        self.import_boost = value;
        self
    }

    pub fn change_boost(mut self, value: f64) -> Self {
        self.change_boost = value;
        self
    }

    /// BLAKE3 fingerprint of the candidate content.
    #[must_use]
    pub fn content_hash(&self) -> String {
        crate::text::hash(&self.content)
    }

    /// Exact token count using LeanToken's default tokenizer.
    #[must_use]
    pub fn token_count(&self) -> usize {
        tokens::count(&self.content)
    }

    /// Count this candidate with an explicit tokenizer.
    #[must_use]
    pub fn token_count_with(&self, tokenizer: tokens::Tokenizer) -> usize {
        tokenizer.count(&self.content)
    }

    /// Number of lines covered by the candidate range.
    #[must_use]
    pub fn line_count(&self) -> usize {
        if self.end_line >= self.start_line {
            self.end_line - self.start_line + 1
        } else {
            0
        }
    }

    /// Combined ranking score using the supplied weights and pre-computed
    /// token count.  Deterministic and side-effect free.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn score(&self, weights: &Weights, token_count: usize) -> f64 {
        // BM25 is normalized so a raw score of 0 maps to 0 and very large raw
        // scores saturate near 1.
        let bm25_norm = self.bm25 / (1.0 + self.bm25);

        // If an explicit size score was supplied, use it; otherwise larger
        // fragments receive a small penalty.
        let size = if self.size_score == 0.0 {
            1.0 / (1.0 + token_count as f64 / 150.0)
        } else {
            self.size_score
        };

        let base = self.exact * weights.exact
            + self.symbol * weights.symbol
            + self.reference * weights.reference
            + bm25_norm * weights.bm25
            + self.path_score * weights.path
            + size * weights.size;

        // God-file penalty: files that mention a term everywhere are down-weighted.
        let penalty = self.lexical_frequency_penalty * weights.lexical_frequency_penalty;

        // Focus/import/change boosts are additive.
        let boost = self.focus_boost * weights.focus
            + self.import_boost * weights.import
            + self.change_boost * weights.change;

        (base + boost - penalty).max(0.0)
    }

    /// Short human-readable reason for why the candidate was selected.
    #[must_use]
    pub fn reason(&self) -> String {
        let mut parts: Vec<&str> = self
            .match_kinds
            .iter()
            .map(String::as_str)
            .filter(|kind| !is_internal_metadata(kind))
            .collect();
        if self.focus_boost > 0.0 && !parts.contains(&"focus") {
            parts.push("focus");
        }
        if self.import_boost > 0.0 && !parts.contains(&"import") {
            parts.push("import");
        }
        if self.change_boost > 0.0 && !parts.contains(&"changed") {
            parts.push("changed");
        }
        if parts.is_empty() {
            "context".to_string()
        } else {
            parts.join("; ")
        }
    }
}

pub(in crate::ranking) fn is_internal_metadata(kind: &str) -> bool {
    kind.starts_with(FACET_PREFIX) || kind.starts_with(CHANNEL_PREFIX)
}
