use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use tiktoken_rs::bpe_for_tokenizer;

/// Configured tokenizer used for all source and protocol token accounting.
///
/// Exact variants are backed by `tiktoken-rs`, the maintained Rust port of
/// OpenAI's BPE tokenizers. The `Estimate` variant is a fast, conservative
/// approximation that does not load a BPE vocabulary; responses that use it
/// set `token_count_exact` to `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
#[repr(u8)]
pub enum Tokenizer {
    /// OpenAI `cl100k_base` (GPT-4, GPT-3.5-turbo, text-embedding-ada-002, ...).
    #[default]
    Cl100kBase,
    /// OpenAI `o200k_base` (GPT-4o, o1/o3/o4, codex-*, ...).
    O200kBase,
    /// OpenAI `p50k_base` (code models, text-davinci-002/003).
    P50kBase,
    /// OpenAI `r50k_base` / GPT-2.
    R50kBase,
    /// GPT-2 (alias for `r50k_base`).
    Gpt2,
    /// OpenAI `p50k_edit`.
    P50kEdit,
    /// Fast estimate: one token per four characters plus whitespace-split words.
    ///
    /// This is not a BPE tokenizer; it is a stand-in for cases where no exact
    /// vocabulary is needed or available. Counts are always inexact.
    Estimate,
}

impl Tokenizer {
    /// Return the snake_case identifier used in CLI and report output.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Cl100kBase => "cl100k_base",
            Self::O200kBase => "o200k_base",
            Self::P50kBase => "p50k_base",
            Self::R50kBase => "r50k_base",
            Self::Gpt2 => "gpt2",
            Self::P50kEdit => "p50k_edit",
            Self::Estimate => "estimate",
        }
    }

    /// Whether this tokenizer produces exact token counts.
    ///
    /// `Estimate` is always `false`; all BPE-backed variants are `true`.
    #[must_use]
    pub fn is_exact(&self) -> bool {
        !matches!(self, Self::Estimate)
    }

    /// Map to the underlying `tiktoken-rs` tokenizer, if any.
    #[must_use]
    pub fn as_tiktoken(&self) -> Option<tiktoken_rs::tokenizer::Tokenizer> {
        use tiktoken_rs::tokenizer::Tokenizer as T;
        match self {
            Self::Cl100kBase => Some(T::Cl100kBase),
            Self::O200kBase => Some(T::O200kBase),
            Self::P50kBase => Some(T::P50kBase),
            Self::R50kBase => Some(T::R50kBase),
            Self::Gpt2 => Some(T::Gpt2),
            Self::P50kEdit => Some(T::P50kEdit),
            Self::Estimate => None,
        }
    }

    /// Count tokens in `text` using this tokenizer.
    #[must_use]
    pub fn count(&self, text: &str) -> usize {
        if let Some(tiktoken) = self.as_tiktoken() {
            bpe_for_tokenizer(tiktoken).map_or(0, |bpe| bpe.count_ordinary(text))
        } else {
            estimate_count(text)
        }
    }

    /// Return a UTF-8 prefix of `text` that contains at most `max_tokens` tokens.
    #[must_use]
    pub fn truncate<'a>(&self, text: &'a str, max_tokens: usize) -> (&'a str, usize) {
        if text.is_empty() || max_tokens == 0 {
            return ("", 0);
        }
        let total = self.count(text);
        if total <= max_tokens {
            return (text, total);
        }

        let mut boundaries: Vec<usize> = text.char_indices().map(|(index, _)| index).collect();
        boundaries.push(text.len());

        let mut low = 0;
        let mut high = boundaries.len() - 1;
        while low < high {
            let midpoint = low + (high - low).div_ceil(2);
            let boundary = boundaries[midpoint];
            if self.count(&text[..boundary]) <= max_tokens {
                low = midpoint;
            } else {
                high = midpoint - 1;
            }
        }
        let boundary = boundaries[low];
        let prefix = &text[..boundary];
        (prefix, self.count(prefix))
    }
}

impl fmt::Display for Tokenizer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.to_possible_value()
            .expect("no tokenizer variants are skipped")
            .get_name()
            .fmt(f)
    }
}

/// Fast, conservative token estimate.
///
/// The heuristic combines the common rule-of-thumb (one token per four
/// characters) with a small contribution from whitespace-delimited words so
/// that code with many short tokens is not systematically undercounted.
#[must_use]
fn estimate_count(text: &str) -> usize {
    let chars = text.chars().count() / 4;
    let words = text.split_whitespace().count();
    chars + words.saturating_sub(chars) / 4
}

fn to_u8(tokenizer: Tokenizer) -> u8 {
    tokenizer as u8
}

fn from_u8(value: u8) -> Option<Tokenizer> {
    match value {
        0 => Some(Tokenizer::Cl100kBase),
        1 => Some(Tokenizer::O200kBase),
        2 => Some(Tokenizer::P50kBase),
        3 => Some(Tokenizer::R50kBase),
        4 => Some(Tokenizer::Gpt2),
        5 => Some(Tokenizer::P50kEdit),
        6 => Some(Tokenizer::Estimate),
        _ => None,
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(Tokenizer::Cl100kBase as u8);

/// Set the process-wide tokenizer used by [`count`] and [`truncate`].
///
/// `Config::discover` and the CLI call this on startup; tests that need an
/// explicit tokenizer should use [`Tokenizer::count`] and
/// [`Tokenizer::truncate`] directly.
pub fn set_current(tokenizer: Tokenizer) {
    CURRENT.store(to_u8(tokenizer), Ordering::Release);
}

/// Return the current process-wide tokenizer.
#[must_use]
pub fn current() -> Tokenizer {
    from_u8(CURRENT.load(Ordering::Acquire)).unwrap_or_default()
}

/// Whether the current tokenizer produces exact counts.
#[must_use]
pub fn is_exact() -> bool {
    current().is_exact()
}

/// Count tokens using the current process-wide tokenizer.
#[must_use]
pub fn count(text: &str) -> usize {
    current().count(text)
}

/// Return a UTF-8 prefix of `text` bounded to `max_tokens` using the current
/// tokenizer.
#[must_use]
pub fn truncate(text: &str, max_tokens: usize) -> (&str, usize) {
    current().truncate(text, max_tokens)
}

/// Count tokens with an explicit tokenizer, ignoring the process-wide default.
#[must_use]
pub fn count_with(text: &str, tokenizer: Tokenizer) -> usize {
    tokenizer.count(text)
}

/// Truncate with an explicit tokenizer, ignoring the process-wide default.
#[must_use]
pub fn truncate_with(text: &str, max_tokens: usize, tokenizer: Tokenizer) -> (&str, usize) {
    tokenizer.truncate(text, max_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_names_are_snake_case() {
        assert_eq!(Tokenizer::Cl100kBase.name(), "cl100k_base");
        assert_eq!(Tokenizer::O200kBase.name(), "o200k_base");
        assert_eq!(Tokenizer::Estimate.name(), "estimate");
    }

    #[test]
    fn exact_variants_report_exact() {
        for tokenizer in [
            Tokenizer::Cl100kBase,
            Tokenizer::O200kBase,
            Tokenizer::P50kBase,
            Tokenizer::R50kBase,
            Tokenizer::Gpt2,
            Tokenizer::P50kEdit,
        ] {
            assert!(tokenizer.is_exact(), "{tokenizer:?} should be exact");
        }
        assert!(!Tokenizer::Estimate.is_exact());
    }

    #[test]
    fn bpe_tokenizers_count_source() {
        let source = "fn main() { println!(\"hello\"); }\n";
        for tokenizer in [
            Tokenizer::Cl100kBase,
            Tokenizer::O200kBase,
            Tokenizer::P50kBase,
            Tokenizer::R50kBase,
            Tokenizer::Gpt2,
            Tokenizer::P50kEdit,
        ] {
            assert!(tokenizer.count(source) > 0, "{tokenizer:?} returned zero");
        }
    }

    #[test]
    fn estimate_is_inexact_and_bounded() {
        let source = "fn main() { println!(\"hello\"); }\n";
        let exact = Tokenizer::Cl100kBase.count(source);
        let approx = Tokenizer::Estimate.count(source);
        assert!(approx > 0);
        assert!(!Tokenizer::Estimate.is_exact());
        // Estimate should be within a factor of two of the exact count for this
        // short English/code source; it is intentionally not identical.
        assert!(approx <= exact.max(1) * 2);
    }

    #[test]
    fn truncate_respects_budget_for_each_tokenizer() {
        let source = "fn main() { println!(\"hello\"); }\n".repeat(20);
        for tokenizer in [
            Tokenizer::Cl100kBase,
            Tokenizer::O200kBase,
            Tokenizer::Estimate,
        ] {
            let (prefix, tokens) = tokenizer.truncate(&source, 12);
            assert!(source.starts_with(prefix));
            assert!(tokens <= 12);
            assert!(std::str::from_utf8(prefix.as_bytes()).is_ok());
        }
    }
}
