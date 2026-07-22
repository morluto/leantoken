use std::fmt;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// Tokenizer used for source and protocol token accounting.
///
/// Exact variants are backed by `tiktoken-rs`, the maintained Rust port of
/// OpenAI's BPE tokenizers. The `Estimate` variant is a fast, inexact
/// approximation that does not load a BPE vocabulary; responses that use it
/// set `token_count_exact` to `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub enum Tokenizer {
    /// OpenAI `cl100k_base` (GPT-4, GPT-3.5-turbo, text-embedding-ada-002, ...).
    #[default]
    Cl100kBase,
    /// OpenAI `o200k_base` (GPT-4o, o1/o3/o4, codex-*, ...).
    O200kBase,
    /// OpenAI `o200k_harmony`.
    O200kHarmony,
    /// OpenAI `p50k_base` (code models, text-davinci-002/003).
    P50kBase,
    /// OpenAI `r50k_base` / GPT-2.
    R50kBase,
    /// GPT-2 (alias for `r50k_base`).
    Gpt2,
    /// OpenAI `p50k_edit`.
    P50kEdit,
    /// Fast estimate: the larger of one token per four characters and the
    /// whitespace-split word count.
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
            Self::O200kHarmony => "o200k_harmony",
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
            Self::O200kHarmony => Some(T::O200kHarmony),
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
        match self {
            Self::Cl100kBase => tiktoken_rs::cl100k_base_singleton().count_ordinary(text),
            Self::O200kBase => tiktoken_rs::o200k_base_singleton().count_ordinary(text),
            Self::O200kHarmony => tiktoken_rs::o200k_harmony_singleton().count_ordinary(text),
            Self::P50kBase => tiktoken_rs::p50k_base_singleton().count_ordinary(text),
            Self::P50kEdit => tiktoken_rs::p50k_edit_singleton().count_ordinary(text),
            Self::R50kBase | Self::Gpt2 => tiktoken_rs::r50k_base_singleton().count_ordinary(text),
            Self::Estimate => estimate_count(text),
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

        // Tokenize once and find the byte offset of the token boundary at
        // `max_tokens`. This avoids the O(N log N) binary search that
        // re-tokenizes the same prefixes repeatedly.
        let prefix_end = self.token_boundary_byte_offset(text, max_tokens);
        let prefix = &text[..prefix_end];
        (prefix, self.count(prefix))
    }

    /// Find the byte offset in `text` where the first `max_tokens` tokens end.
    ///
    /// Uses `split_by_token_ordinary_iter` to decode and split the text into
    /// token-sized substrings in a single pass, then sums their byte lengths
    /// until the budget is exhausted.
    fn token_boundary_byte_offset(&self, text: &str, max_tokens: usize) -> usize {
        match self {
            Self::Cl100kBase => {
                self.bpe_boundary(tiktoken_rs::cl100k_base_singleton(), text, max_tokens)
            }
            Self::O200kBase => {
                self.bpe_boundary(tiktoken_rs::o200k_base_singleton(), text, max_tokens)
            }
            Self::O200kHarmony => {
                self.bpe_boundary(tiktoken_rs::o200k_harmony_singleton(), text, max_tokens)
            }
            Self::P50kBase => {
                self.bpe_boundary(tiktoken_rs::p50k_base_singleton(), text, max_tokens)
            }
            Self::P50kEdit => {
                self.bpe_boundary(tiktoken_rs::p50k_edit_singleton(), text, max_tokens)
            }
            Self::R50kBase | Self::Gpt2 => {
                self.bpe_boundary(tiktoken_rs::r50k_base_singleton(), text, max_tokens)
            }
            Self::Estimate => {
                // Keep the boundary consistent with `estimate_count`, whose
                // budget is the larger of words and one token per four chars.
                let mut offset = 0usize;
                let mut chars = 0usize;
                let mut words = 0usize;
                let mut in_word = false;
                for (start, character) in text.char_indices() {
                    let next_chars = chars + 1;
                    let next_in_word = !character.is_whitespace();
                    let next_words = words + usize::from(next_in_word && !in_word);
                    if next_chars.div_ceil(4).max(next_words) > max_tokens {
                        break;
                    }
                    chars = next_chars;
                    words = next_words;
                    in_word = next_in_word;
                    offset = start + character.len_utf8();
                }
                offset
            }
        }
    }

    fn bpe_boundary(&self, bpe: &tiktoken_rs::CoreBPE, text: &str, max_tokens: usize) -> usize {
        let mut offset = 0usize;
        for (_, token) in bpe
            .split_by_token_ordinary_iter(text)
            .enumerate()
            .take(max_tokens)
        {
            if let Ok(token) = token {
                offset += token.len();
            }
        }
        offset.min(text.len())
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

/// Fast, inexact token estimate.
///
/// The heuristic takes the larger of the common one-token-per-four-characters
/// rule and the whitespace-delimited word count. The latter keeps code made of
/// many short identifiers from being systematically undercounted.
#[must_use]
fn estimate_count(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let chars = text.chars().count().div_ceil(4);
    let words = text.split_whitespace().count();
    chars.max(words).max(1)
}

/// Count tokens using the default `cl100k_base` tokenizer.
#[must_use]
pub fn count(text: &str) -> usize {
    Tokenizer::default().count(text)
}

/// Return a UTF-8 prefix bounded with the default `cl100k_base` tokenizer.
#[must_use]
pub fn truncate(text: &str, max_tokens: usize) -> (&str, usize) {
    Tokenizer::default().truncate(text, max_tokens)
}

/// Count tokens with an explicit tokenizer.
#[must_use]
pub fn count_with(text: &str, tokenizer: Tokenizer) -> usize {
    tokenizer.count(text)
}

/// Truncate with an explicit tokenizer.
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
        assert_eq!(Tokenizer::O200kHarmony.name(), "o200k_harmony");
        assert_eq!(Tokenizer::Estimate.name(), "estimate");
    }

    #[test]
    fn exact_variants_report_exact() {
        for tokenizer in [
            Tokenizer::Cl100kBase,
            Tokenizer::O200kBase,
            Tokenizer::O200kHarmony,
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
            Tokenizer::O200kHarmony,
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
        assert_eq!(Tokenizer::Estimate.count("a b c d"), 4);
        assert_eq!(Tokenizer::Estimate.count("x"), 1);
        assert_eq!(Tokenizer::Estimate.count(""), 0);
    }

    #[test]
    fn truncate_respects_budget_for_each_tokenizer() {
        let source = "fn main() { println!(\"hello\"); }\n".repeat(20);
        for tokenizer in [
            Tokenizer::Cl100kBase,
            Tokenizer::O200kBase,
            Tokenizer::O200kHarmony,
            Tokenizer::Estimate,
        ] {
            let (prefix, tokens) = tokenizer.truncate(&source, 12);
            assert!(source.starts_with(prefix));
            assert!(tokens <= 12);
            assert!(std::str::from_utf8(prefix.as_bytes()).is_ok());
        }
    }

    #[test]
    fn estimate_truncate_tracks_sequential_words_and_character_budget() {
        let repeated = "a a a";
        let (prefix, tokens) = Tokenizer::Estimate.truncate(repeated, 2);
        assert_eq!(prefix, "a a ");
        assert_eq!(tokens, 2);

        let long_word = "abcdefghijklmnop";
        let (prefix, tokens) = Tokenizer::Estimate.truncate(long_word, 2);
        assert_eq!(prefix, "abcdefgh");
        assert_eq!(tokens, 2);
    }
}
