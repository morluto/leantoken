use std::fmt;

use clap::ValueEnum;
use rmcp::model::{CallToolResult, ContentBlock, ProtocolVersion, Resource, ServerResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpResponseMode {
    Dual,
    Text,
    Structured,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpProtocolShape {
    Legacy,
    Modern,
}

impl McpProtocolShape {
    pub(crate) fn negotiated(protocol: Option<&ProtocolVersion>) -> Self {
        if protocol.is_some_and(|version| version >= &ProtocolVersion::V_2026_07_28) {
            Self::Modern
        } else {
            Self::Legacy
        }
    }
}

#[must_use]
pub(crate) fn supports_mcp_private_resource_metadata(protocol: Option<&ProtocolVersion>) -> bool {
    McpProtocolShape::negotiated(protocol) == McpProtocolShape::Modern
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct McpResponseShape {
    pub(crate) mode: McpResponseMode,
    pub(crate) protocol: McpProtocolShape,
}

pub(crate) fn build_mcp_tool_result(value: Value, mode: McpResponseMode) -> CallToolResult {
    match mode {
        McpResponseMode::Dual => CallToolResult::structured(value),
        McpResponseMode::Text => {
            CallToolResult::success(vec![ContentBlock::text(value.to_string())])
        }
        McpResponseMode::Structured => {
            let mut result = CallToolResult::success(Vec::new());
            result.structured_content = Some(value);
            result
        }
    }
}

pub(crate) fn model_visible_mcp_result(value: Value, shape: McpResponseShape) -> ServerResult {
    let mut result = ServerResult::CallToolResult(model_visible_mcp_tool_result(value, shape.mode));
    if shape.protocol == McpProtocolShape::Legacy {
        result.strip_result_type_for_legacy_peer();
    }
    result
}

pub(crate) fn model_visible_mcp_tool_result(
    mut value: Value,
    mode: McpResponseMode,
) -> CallToolResult {
    let receipt_id = value
        .pointer("/meta/receipt_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(object) = value.as_object_mut() {
        object.remove("receipt_resource");
    }

    let mut result = build_mcp_tool_result(value, mode);
    if let Some(receipt_id) = receipt_id {
        result
            .content
            .push(ContentBlock::ResourceLink(Resource::new(
                crate::mcp::receipt_uri(&receipt_id),
                "retrieval_receipt",
            )));
    }
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResponseTokenAccounting {
    pub(crate) protocol_tokens: usize,
    pub(crate) path_and_metadata_tokens: usize,
    pub(crate) total_response_tokens: usize,
}

/// Monotonic prefix fitting primitive for an exact serialized-response budget.
pub(crate) struct ResponseBudget {
    max_tokens: usize,
}

impl ResponseBudget {
    pub(crate) const fn new(max_tokens: usize) -> Self {
        Self { max_tokens }
    }

    /// Find the largest prefix accepted by a monotonic serialized-size callback.
    pub(crate) fn largest_fitting_prefix(
        &self,
        item_count: usize,
        mut count_prefix: impl FnMut(usize) -> Result<usize>,
    ) -> Result<Option<usize>> {
        let mut lower = 0usize;
        let mut upper = item_count;
        let mut best = None;
        while lower <= upper {
            let middle = lower + (upper - lower) / 2;
            if count_prefix(middle)? <= self.max_tokens {
                best = Some(middle);
                lower = middle.saturating_add(1);
            } else if middle == 0 {
                break;
            } else {
                upper = middle - 1;
            }
        }
        Ok(best)
    }
}

pub(crate) fn response_token_accounting<T: Serialize>(
    response: &T,
    source_tokens: usize,
    tokenizer: &Tokenizer,
) -> serde_json::Result<ResponseTokenAccounting> {
    let payload = serde_json::to_string(response)?;
    let total_response_tokens = tokenizer.count(&payload);
    let mut protocol_skeleton = serde_json::to_value(response)?;
    strip_response_values(&mut protocol_skeleton);
    let available_overhead = total_response_tokens.saturating_sub(source_tokens);
    let protocol_tokens = tokenizer
        .count(&protocol_skeleton.to_string())
        .min(available_overhead);

    Ok(ResponseTokenAccounting {
        protocol_tokens,
        path_and_metadata_tokens: available_overhead.saturating_sub(protocol_tokens),
        total_response_tokens,
    })
}

fn strip_response_values(value: &mut Value) {
    match value {
        Value::Null => {}
        Value::Bool(value) => *value = false,
        Value::Number(value) => *value = 0.into(),
        Value::String(value) => value.clear(),
        Value::Array(values) => values.clear(),
        Value::Object(values) => {
            for value in values.values_mut() {
                strip_response_values(value);
            }
        }
    }
}

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
        match self {
            Self::Cl100kBase => {
                self.truncate_bpe(tiktoken_rs::cl100k_base_singleton(), text, max_tokens)
            }
            Self::O200kBase => {
                self.truncate_bpe(tiktoken_rs::o200k_base_singleton(), text, max_tokens)
            }
            Self::O200kHarmony => {
                self.truncate_bpe(tiktoken_rs::o200k_harmony_singleton(), text, max_tokens)
            }
            Self::P50kBase => {
                self.truncate_bpe(tiktoken_rs::p50k_base_singleton(), text, max_tokens)
            }
            Self::P50kEdit => {
                self.truncate_bpe(tiktoken_rs::p50k_edit_singleton(), text, max_tokens)
            }
            Self::R50kBase | Self::Gpt2 => {
                self.truncate_bpe(tiktoken_rs::r50k_base_singleton(), text, max_tokens)
            }
            Self::Estimate => {
                let total = estimate_count(text);
                if total <= max_tokens {
                    return (text, total);
                }
                let prefix = &text[..estimate_boundary(text, max_tokens)];
                (prefix, estimate_count(prefix))
            }
        }
    }

    /// Truncate a read page and report its smallest byte-progressing source budget.
    pub(crate) fn truncate_for_read<'a>(
        &self,
        text: &'a str,
        max_tokens: usize,
    ) -> (&'a str, usize, Option<usize>) {
        if text.is_empty() {
            return ("", 0, None);
        }
        match self {
            Self::Cl100kBase => {
                self.truncate_bpe_for_read(tiktoken_rs::cl100k_base_singleton(), text, max_tokens)
            }
            Self::O200kBase => {
                self.truncate_bpe_for_read(tiktoken_rs::o200k_base_singleton(), text, max_tokens)
            }
            Self::O200kHarmony => {
                self.truncate_bpe_for_read(tiktoken_rs::o200k_harmony_singleton(), text, max_tokens)
            }
            Self::P50kBase => {
                self.truncate_bpe_for_read(tiktoken_rs::p50k_base_singleton(), text, max_tokens)
            }
            Self::P50kEdit => {
                self.truncate_bpe_for_read(tiktoken_rs::p50k_edit_singleton(), text, max_tokens)
            }
            Self::R50kBase | Self::Gpt2 => {
                self.truncate_bpe_for_read(tiktoken_rs::r50k_base_singleton(), text, max_tokens)
            }
            Self::Estimate => {
                let (prefix, tokens) = self.truncate(text, max_tokens);
                (prefix, tokens, Some(1))
            }
        }
    }

    fn truncate_bpe<'a>(
        &self,
        bpe: &tiktoken_rs::CoreBPE,
        text: &'a str,
        max_tokens: usize,
    ) -> (&'a str, usize) {
        let tokens = bpe.encode_ordinary(text);
        truncate_bpe_tokens(bpe, text, &tokens, max_tokens)
    }

    fn truncate_bpe_for_read<'a>(
        &self,
        bpe: &tiktoken_rs::CoreBPE,
        text: &'a str,
        max_tokens: usize,
    ) -> (&'a str, usize, Option<usize>) {
        let tokens = bpe.encode_ordinary(text);
        let minimum_progress_tokens = minimum_bpe_progress_tokens(bpe, text, &tokens);
        let (prefix, emitted_tokens) = truncate_bpe_tokens(bpe, text, &tokens, max_tokens);
        (prefix, emitted_tokens, minimum_progress_tokens)
    }
}

fn truncate_bpe_tokens<'a>(
    bpe: &tiktoken_rs::CoreBPE,
    text: &'a str,
    tokens: &[u32],
    max_tokens: usize,
) -> (&'a str, usize) {
    if tokens.len() <= max_tokens {
        return (text, tokens.len());
    }
    let selected = &tokens[..max_tokens];
    let Some(offset) = decoded_prefix_offset(bpe, text, selected) else {
        return ("", 0);
    };
    let prefix = &text[..offset];
    (prefix, bpe.count_ordinary(prefix))
}

fn minimum_bpe_progress_tokens(
    bpe: &tiktoken_rs::CoreBPE,
    text: &str,
    tokens: &[u32],
) -> Option<usize> {
    let first_scalar_bytes = text.chars().next()?.len_utf8();
    // Ordinary BPE tokens each decode at least one byte, so completing the
    // first scalar cannot require more tokens than that scalar has UTF-8 bytes.
    (1..=tokens.len().min(first_scalar_bytes)).find(|token_count| {
        decoded_prefix_offset(bpe, text, &tokens[..*token_count]).is_some_and(|offset| offset > 0)
    })
}

fn decoded_prefix_offset(bpe: &tiktoken_rs::CoreBPE, text: &str, tokens: &[u32]) -> Option<usize> {
    let bytes = bpe.decode_bytes(tokens).ok()?;
    let mut offset = bytes.len().min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    Some(offset)
}

fn estimate_boundary(text: &str, max_tokens: usize) -> usize {
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
    use serde_json::json;

    #[test]
    fn response_accounting_separates_protocol_from_values_and_items() {
        let tokenizer = Tokenizer::Cl100kBase;
        let response = json!({
            "entries": [{"path": "src/lib.rs", "kind": "file"}],
            "meta": {"source_tokens": 0, "freshness": "current"}
        });
        let payload = response.to_string();

        let accounting =
            response_token_accounting(&response, 0, &tokenizer).expect("response accounting");

        assert!(accounting.protocol_tokens > 0);
        assert!(accounting.path_and_metadata_tokens > 0);
        assert_eq!(
            accounting.total_response_tokens,
            accounting.protocol_tokens + accounting.path_and_metadata_tokens
        );
        assert_eq!(accounting.total_response_tokens, tokenizer.count(&payload));
    }

    #[test]
    fn response_budget_finds_the_largest_monotonic_prefix() {
        let tokenizer = Tokenizer::Cl100kBase;
        let budget = ResponseBudget::new(34);
        assert_eq!(
            budget
                .largest_fitting_prefix(8, |prefix| Ok(prefix * 10 + 5))
                .expect("prefix fit"),
            Some(2)
        );
        assert_eq!(
            ResponseBudget::new(4)
                .largest_fitting_prefix(8, |prefix| Ok(prefix * 10 + 5))
                .expect("empty prefix does not fit"),
            None
        );
        let response = json!({"message": "bounded"});
        let payload = serde_json::to_string(&response).expect("serialized response");
        assert!(tokenizer.count(&payload) <= 34);
    }

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
            assert_eq!(tokens, tokenizer.count(prefix));
            assert!(std::str::from_utf8(prefix.as_bytes()).is_ok());
        }
    }

    #[test]
    fn exact_truncation_preserves_utf8_boundaries() {
        for tokenizer in [
            Tokenizer::Cl100kBase,
            Tokenizer::O200kBase,
            Tokenizer::O200kHarmony,
            Tokenizer::P50kBase,
            Tokenizer::R50kBase,
            Tokenizer::Gpt2,
            Tokenizer::P50kEdit,
        ] {
            for source in ["Ā", "𐀀", "aĀb𐀀c"] {
                for budget in 1..=tokenizer.count(source) {
                    let (prefix, count) = tokenizer.truncate(source, budget);
                    assert!(source.starts_with(prefix), "{tokenizer:?}: {source:?}");
                    assert!(source.is_char_boundary(prefix.len()));
                    assert_eq!(count, tokenizer.count(prefix));
                    assert!(count <= budget, "{tokenizer:?}: {source:?}");
                }
            }
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
