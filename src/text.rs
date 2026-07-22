/// Classification of a byte sequence as text or binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    Text,
    Binary,
}

/// A canonical, non-overlapping, bounded chunk of source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
    pub hash: String,
}

/// A text file after binary detection and chunking.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedText {
    pub kind: TextKind,
    pub content: String,
    pub chunks: Vec<Chunk>,
    pub line_count: usize,
}

impl PreparedText {
    /// Decode `bytes` as UTF-8 when possible and safe, otherwise mark as binary.
    ///
    /// # Panics
    ///
    /// Panics if `max_chunk_lines` or `max_chunk_bytes` is zero.
    #[must_use]
    pub fn from_bytes(bytes: &[u8], max_chunk_lines: usize, max_chunk_bytes: usize) -> Self {
        assert!(max_chunk_lines > 0, "max_chunk_lines must be positive");
        assert!(max_chunk_bytes > 0, "max_chunk_bytes must be positive");

        let kind = detect_kind(bytes);
        if kind == TextKind::Binary {
            return Self {
                kind,
                content: String::new(),
                chunks: Vec::new(),
                line_count: 0,
            };
        }

        // `detect_kind` already verified valid UTF-8.
        let content = std::str::from_utf8(bytes)
            .expect("utf8 verified")
            .to_string();
        let chunks = chunk_text(&content, max_chunk_lines, max_chunk_bytes);
        let line_count = line_starts(&content).len();

        Self {
            kind,
            content,
            chunks,
            line_count,
        }
    }
}

/// Hex characters retained from BLAKE3 content digests.
///
/// A 128-bit fingerprint is ample for local change detection and duplicate
/// suppression while materially reducing repeated MCP metadata.
pub const CONTENT_FINGERPRINT_HEX_LEN: usize = 32;

/// Return a 128-bit BLAKE3 hex fingerprint of a UTF-8 string.
#[must_use]
pub fn hash(text: &str) -> String {
    hash_bytes(text.as_bytes())
}

/// Return a 128-bit BLAKE3 hex fingerprint of a byte slice.
#[must_use]
pub fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex()[..CONTENT_FINGERPRINT_HEX_LEN].to_string()
}

/// Decide whether `bytes` are textual or binary.
///
/// A sequence is binary if it contains a NUL byte, is not valid UTF-8, or has
/// too high a ratio of control characters (< 0x20, excluding tab/line feed/
/// carriage return, plus 0x7F) relative to its total length.
#[must_use]
pub fn detect_kind(bytes: &[u8]) -> TextKind {
    if bytes.contains(&0) {
        return TextKind::Binary;
    }

    let Ok(text) = std::str::from_utf8(bytes) else {
        return TextKind::Binary;
    };

    let control = text
        .bytes()
        .filter(|&b| (b < 0x20 && b != b'\t' && b != b'\n' && b != b'\r') || b == 0x7F)
        .count();

    // Treat more than 30% control bytes as binary.
    if control * 10 > text.len() * 3 {
        TextKind::Binary
    } else {
        TextKind::Text
    }
}

/// Return the byte offset of the first character of every 1-based line.
///
/// The returned vector is empty for an empty string. Trailing newlines do not
/// create an extra empty line.
#[must_use]
pub fn line_starts(text: &str) -> Vec<usize> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut starts = vec![0];
    for (i, byte) in text.bytes().enumerate() {
        if byte == b'\n' && i + 1 < text.len() {
            starts.push(i + 1);
        }
    }
    starts
}

/// Convert a 0-based byte offset into a 1-based line number.
#[must_use]
pub fn byte_to_line(line_starts: &[usize], text_len: usize, byte_offset: usize) -> usize {
    let byte_offset = byte_offset.min(text_len);
    if line_starts.is_empty() {
        return 1;
    }
    match line_starts.binary_search(&byte_offset) {
        Ok(idx) => idx + 1,
        Err(idx) => idx,
    }
}

/// Convert a 0-based byte range into a 1-based inclusive line range.
#[must_use]
pub fn byte_range_to_line_range(text: &str, start_byte: usize, end_byte: usize) -> (usize, usize) {
    let text_len = text.len();
    let mut start_byte = start_byte.min(text_len);
    let mut end_byte = end_byte.min(text_len);
    if end_byte < start_byte {
        std::mem::swap(&mut start_byte, &mut end_byte);
    }

    let starts = line_starts(text);
    let start_line = byte_to_line(&starts, text_len, start_byte);
    let end_line = if end_byte == 0 || end_byte == start_byte {
        start_line
    } else {
        byte_to_line(&starts, text_len, end_byte - 1)
    };

    (start_line, end_line)
}

/// Convert a 1-based inclusive line range into a 0-based byte range.
#[must_use]
pub fn line_range_to_byte_range(
    line_starts: &[usize],
    text_len: usize,
    mut start_line: usize,
    mut end_line: usize,
) -> (usize, usize) {
    if line_starts.is_empty() {
        return (0, text_len);
    }

    let count = line_starts.len();
    if start_line == 0 {
        start_line = 1;
    }
    if end_line < start_line {
        std::mem::swap(&mut start_line, &mut end_line);
    }
    if start_line > count {
        start_line = count;
    }
    if end_line > count {
        end_line = count;
    }

    let start_byte = line_starts[start_line - 1];
    let end_byte = if end_line < count {
        line_starts[end_line]
    } else {
        text_len
    };

    (start_byte, end_byte)
}

/// Split `text` into canonical, non-overlapping chunks bounded by line and
/// byte limits. Each chunk records its 0-based byte range, 1-based inclusive
/// line range, full content, and BLAKE3 fingerprint.
///
/// # Panics
///
/// Panics if `max_lines` or `max_bytes` is zero.
#[must_use]
pub fn chunk_text(text: &str, max_lines: usize, max_bytes: usize) -> Vec<Chunk> {
    assert!(max_lines > 0, "max_lines must be positive");
    assert!(max_bytes > 0, "max_bytes must be positive");

    if text.is_empty() {
        return Vec::new();
    }

    let starts = line_starts(text);
    let mut chunks = Vec::new();
    let mut i = 0;

    let mut chunk_start_byte = 0;
    let mut chunk_start_line = 1;
    let mut chunk_lines = 0;
    let mut chunk_bytes = 0;

    while i < starts.len() {
        let line_start = starts[i];
        let line_end = starts.get(i + 1).copied().unwrap_or(text.len());
        let line_len = line_end - line_start;
        let line_no = i + 1;

        // Close the current chunk before adding this line if a bound would be exceeded.
        if chunk_lines > 0 && (chunk_lines + 1 > max_lines || chunk_bytes + line_len > max_bytes) {
            push_chunk(
                &mut chunks,
                text,
                chunk_start_byte,
                line_start,
                chunk_start_line,
                line_no - 1,
            );
            chunk_lines = 0;
            chunk_bytes = 0;
            continue;
        }

        // A single line that is too long is split into byte-bounded pieces.
        if chunk_lines == 0 && line_len > max_bytes {
            let mut offset = line_start;
            while offset < line_end {
                let end_byte = if offset + max_bytes >= line_end {
                    line_end
                } else {
                    let mut boundary = floor_char_boundary(text, offset + max_bytes);
                    if boundary <= offset {
                        boundary = next_char_boundary(text, offset + 1);
                    }
                    boundary.min(line_end)
                };
                push_chunk(&mut chunks, text, offset, end_byte, line_no, line_no);
                offset = end_byte;
            }
            i += 1;
            continue;
        }

        if chunk_lines == 0 {
            chunk_start_byte = line_start;
            chunk_start_line = line_no;
        }
        chunk_lines += 1;
        chunk_bytes += line_len;
        i += 1;
    }

    if chunk_lines > 0 {
        push_chunk(
            &mut chunks,
            text,
            chunk_start_byte,
            text.len(),
            chunk_start_line,
            starts.len(),
        );
    }

    chunks
}

/// Return the source lines from `start_line` to `end_line` (1-based, inclusive).
#[must_use]
pub fn excerpt(text: &str, start_line: usize, end_line: usize) -> String {
    let starts = line_starts(text);
    let (start_byte, end_byte) =
        line_range_to_byte_range(&starts, text.len(), start_line, end_line);
    text[start_byte..end_byte].to_string()
}

/// Resolve a bounded inclusive line window that always contains a required span.
///
/// `desired_start` through `desired_end` describes all useful surrounding
/// context. `required_start` through `required_end` is the evidence that cannot
/// be removed by `max_lines`. A zero maximum leaves the desired span uncapped.
#[must_use]
pub fn anchored_line_window(
    desired_start: usize,
    desired_end: usize,
    required_start: usize,
    required_end: usize,
    max_lines: usize,
) -> (usize, usize) {
    let required_start = required_start.max(1);
    let required_end = required_end.max(required_start);
    let desired_start = desired_start.max(1).min(required_start);
    let desired_end = desired_end.max(required_end);
    let desired_lines = desired_end.saturating_sub(desired_start).saturating_add(1);
    if max_lines == 0 || desired_lines <= max_lines {
        return (desired_start, desired_end);
    }

    let required_lines = required_end
        .saturating_sub(required_start)
        .saturating_add(1);
    if required_lines >= max_lines {
        return (required_start, required_end);
    }

    let available_before = required_start.saturating_sub(desired_start);
    let available_after = desired_end.saturating_sub(required_end);
    let extra_lines = max_lines - required_lines;
    let mut before = available_before.min(extra_lines.div_ceil(2));
    let mut after = available_after.min(extra_lines - before);
    let mut remaining = extra_lines - before - after;

    let additional_before = available_before.saturating_sub(before).min(remaining);
    before += additional_before;
    remaining -= additional_before;
    after += available_after.saturating_sub(after).min(remaining);

    (required_start - before, required_end.saturating_add(after))
}

/// Return lines around `focus_start_line`..`focus_end_line` extended by
/// `context_lines`, optionally capped to `max_lines` (0 means no cap).
#[must_use]
pub fn excerpt_with_context(
    text: &str,
    focus_start_line: usize,
    focus_end_line: usize,
    context_lines: usize,
    max_lines: usize,
) -> String {
    let starts = line_starts(text);
    let count = starts.len();
    if count == 0 {
        return String::new();
    }

    let required_start = focus_start_line.max(1).min(count);
    let required_end = focus_end_line.max(required_start).min(count);
    let desired_start = required_start.saturating_sub(context_lines).max(1);
    let desired_end = required_end.saturating_add(context_lines).min(count);
    let (first, last) = anchored_line_window(
        desired_start,
        desired_end,
        required_start,
        required_end,
        max_lines,
    );

    excerpt(text, first, last)
}

/// Return an excerpt covering `start_byte`..`end_byte` extended by
/// `context_lines`.
#[must_use]
pub fn excerpt_around(
    text: &str,
    start_byte: usize,
    end_byte: usize,
    context_lines: usize,
) -> String {
    let (start_line, end_line) = byte_range_to_line_range(text, start_byte, end_byte);
    excerpt_with_context(text, start_line, end_line, context_lines, 0)
}

/// Split an identifier into its natural sub-words (e.g. `fooBar` -> `foo`, `Bar`).
#[must_use]
pub fn identifier_words(identifier: &str) -> Vec<String> {
    let mut words = Vec::new();
    for piece in identifier.split('_') {
        if piece.is_empty() {
            continue;
        }
        let mut current = String::new();
        let chars: Vec<char> = piece.chars().collect();
        for (i, c) in chars.iter().enumerate() {
            let prev = current.chars().last();
            let next = chars.get(i + 1).copied();

            let start_new = match (prev, c, next) {
                (Some(prev), c, _) if prev.is_lowercase() && c.is_uppercase() => true,
                (Some(prev), c, Some(next))
                    if prev.is_uppercase() && c.is_uppercase() && next.is_lowercase() =>
                {
                    true
                }
                (Some(prev), c, _) if prev.is_alphabetic() && c.is_ascii_digit() => true,
                (Some(prev), c, _) if prev.is_ascii_digit() && c.is_alphabetic() => true,
                _ => false,
            };

            if start_new && !current.is_empty() {
                words.push(current);
                current = String::new();
            }
            current.push(*c);
        }
        if !current.is_empty() {
            words.push(current);
        }
    }
    words
}

/// Expand an identifier into searchable terms, including normalized,
/// `snake_case`, lowercased, and original-case variants.
#[must_use]
pub fn expand_identifier(identifier: &str) -> Vec<String> {
    if identifier.is_empty() {
        return Vec::new();
    }

    let mut terms = Vec::new();

    let normalized: String = identifier
        .chars()
        .filter(|&c| c != '_')
        .collect::<String>()
        .to_lowercase();
    push_unique(&mut terms, normalized);

    let words = identifier_words(identifier);
    let snake = words
        .iter()
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .join("_");
    push_unique(&mut terms, snake);

    for word in &words {
        push_unique(&mut terms, word.to_lowercase());
        push_unique(&mut terms, word.clone());
    }

    terms
}

/// Extract identifier tokens from a free-form query and expand each one.
#[must_use]
pub fn expand_terms(query: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();

    for c in query.chars() {
        if c.is_alphanumeric() || c == '_' {
            current.push(c);
        } else if !current.is_empty() {
            for term in expand_identifier(&current) {
                push_unique(&mut result, term);
            }
            current.clear();
        }
    }

    if !current.is_empty() {
        for term in expand_identifier(&current) {
            push_unique(&mut result, term);
        }
    }

    result
}

fn push_unique(vec: &mut Vec<String>, value: String) {
    if !value.is_empty() && !vec.contains(&value) {
        vec.push(value);
    }
}

fn push_chunk(
    chunks: &mut Vec<Chunk>,
    text: &str,
    start_byte: usize,
    end_byte: usize,
    start_line: usize,
    end_line: usize,
) {
    let content = &text[start_byte..end_byte];
    chunks.push(Chunk {
        start_byte,
        end_byte,
        start_line,
        end_line,
        content: content.to_string(),
        hash: hash(content),
    });
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn next_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_binary_by_nul_byte() {
        assert_eq!(detect_kind(b"hello\x00world"), TextKind::Binary);
    }

    #[test]
    fn detects_binary_by_invalid_utf8() {
        assert_eq!(detect_kind(&[0xc0, 0x80]), TextKind::Binary);
    }

    #[test]
    fn detects_binary_by_control_ratio() {
        let bytes = vec![0x01; 100];
        assert_eq!(detect_kind(&bytes), TextKind::Binary);
    }

    #[test]
    fn treats_plain_text_as_text() {
        assert_eq!(detect_kind(b"fn main() {}\n"), TextKind::Text);
    }

    #[test]
    fn empty_input_is_text() {
        assert_eq!(detect_kind(b""), TextKind::Text);
    }

    #[test]
    fn line_starts_handles_trailing_newline() {
        assert_eq!(line_starts("a\nb\n"), vec![0, 2]);
    }

    #[test]
    fn line_starts_handles_no_trailing_newline() {
        assert_eq!(line_starts("a\nb"), vec![0, 2]);
    }

    #[test]
    fn byte_to_line_and_range_conversions() {
        let text = "a\nbb\nccc\n";
        let starts = line_starts(text);
        assert_eq!(byte_to_line(&starts, text.len(), 0), 1);
        assert_eq!(byte_to_line(&starts, text.len(), 2), 2); // start of line 2
        assert_eq!(byte_to_line(&starts, text.len(), 3), 2);
        assert_eq!(byte_to_line(&starts, text.len(), text.len()), 3);

        assert_eq!(byte_range_to_line_range(text, 0, 1), (1, 1)); // "a"
        assert_eq!(byte_range_to_line_range(text, 0, 2), (1, 1)); // "a\n"
        assert_eq!(byte_range_to_line_range(text, 3, 5), (2, 2)); // "bb\n"
        assert_eq!(byte_range_to_line_range(text, 2, 5), (2, 2)); // "bb\n"
        assert_eq!(byte_range_to_line_range(text, 0, text.len()), (1, 3));
    }

    #[test]
    fn line_range_to_byte_range_clamps() {
        let text = "a\nbb\nccc";
        let starts = line_starts(text);
        assert_eq!(line_range_to_byte_range(&starts, text.len(), 1, 1), (0, 2));
        assert_eq!(
            line_range_to_byte_range(&starts, text.len(), 2, 3),
            (2, text.len())
        );
        assert_eq!(
            line_range_to_byte_range(&starts, text.len(), 0, 5),
            (0, text.len())
        );
        assert_eq!(
            line_range_to_byte_range(&starts, text.len(), 3, 1),
            (0, text.len())
        );
    }

    #[test]
    fn chunks_are_non_overlapping_and_cover_text() {
        let text = "line1\nline2\nline3\nline4\n";
        let chunks = chunk_text(text, 2, 1_000);
        let mut combined = String::new();
        for chunk in &chunks {
            combined.push_str(&chunk.content);
        }
        assert_eq!(combined, text);

        for i in 1..chunks.len() {
            assert_eq!(chunks[i].start_byte, chunks[i - 1].end_byte);
            assert!(chunks[i].start_line >= chunks[i - 1].end_line);
        }
    }

    #[test]
    fn chunks_respect_byte_and_line_limits() {
        let text = "123\n4567\n890\n";
        let chunks = chunk_text(text, 2, 5);
        for chunk in &chunks {
            assert!(chunk.end_line - chunk.start_line < 2);
            assert!(chunk.content.len() <= 5 || chunk.content.len() <= 8); // UTF-8 char may force one extra
        }
    }

    #[test]
    fn chunks_hash_content() {
        let chunks = chunk_text("hello\nworld\n", 1, 100);
        for chunk in &chunks {
            assert_eq!(chunk.hash, hash(&chunk.content));
        }
    }

    #[test]
    fn long_line_is_split_at_char_boundaries() {
        let text = "café123456789"; // multi-byte 'é' at bytes 3-4
        let chunks = chunk_text(text, 1, 4);
        assert!(!chunks.is_empty());
        let mut combined = String::new();
        for chunk in &chunks {
            combined.push_str(&chunk.content);
            assert!(chunk.start_line == chunk.end_line);
        }
        assert_eq!(combined, text);
    }

    #[test]
    fn excerpt_returns_lines() {
        let text = "one\ntwo\nthree\n";
        assert_eq!(excerpt(text, 1, 1), "one\n");
        assert_eq!(excerpt(text, 2, 3), "two\nthree\n");
        assert_eq!(excerpt(text, 5, 10), "three\n");
    }

    #[test]
    fn excerpt_with_context_bounds() {
        let text = "1\n2\n3\n4\n5\n";
        assert_eq!(excerpt_with_context(text, 3, 3, 1, 0), "2\n3\n4\n");
        assert_eq!(excerpt_with_context(text, 3, 3, 1, 2), "2\n3\n");
    }

    #[test]
    fn anchored_windows_keep_required_lines_and_rebalance_at_boundaries() {
        assert_eq!(anchored_line_window(10, 50, 30, 30, 20), (20, 39));
        assert_eq!(anchored_line_window(1, 22, 2, 2, 20), (1, 20));
        assert_eq!(anchored_line_window(39, 60, 59, 59, 20), (41, 60));
        assert_eq!(anchored_line_window(1, 40, 6, 31, 20), (6, 31));
        assert_eq!(anchored_line_window(2, 4, 3, 3, 0), (2, 4));
    }

    #[test]
    fn excerpt_around_byte_range() {
        let text = "alpha\nbeta\ngamma\n";
        assert_eq!(excerpt_around(text, 7, 11, 1), "alpha\nbeta\ngamma\n");
    }

    #[test]
    fn identifier_words_split_camel_and_snake() {
        assert_eq!(identifier_words("fooBar_baz"), vec!["foo", "Bar", "baz"]);
        assert_eq!(identifier_words("XMLParser"), vec!["XML", "Parser"]);
        assert_eq!(identifier_words("foo123bar"), vec!["foo", "123", "bar"]);
    }

    #[test]
    fn expand_identifier_produces_terms() {
        let terms = expand_identifier("fooBar_baz");
        assert!(terms.contains(&"foo".to_string()));
        assert!(terms.contains(&"bar".to_string()));
        assert!(terms.contains(&"baz".to_string()));
        assert!(terms.contains(&"foo_bar_baz".to_string()));
        assert!(terms.contains(&"foobarbaz".to_string()));
    }

    #[test]
    fn expand_terms_handles_free_form_query() {
        let terms = expand_terms("find FooBar in XMLParser");
        assert!(terms.contains(&"foo".to_string()));
        assert!(terms.contains(&"bar".to_string()));
        assert!(terms.contains(&"xml".to_string()));
        assert!(terms.contains(&"parser".to_string()));
        assert!(terms.contains(&"find".to_string()));
    }

    #[test]
    fn prepared_text_chunks_and_counts_lines() {
        let prepared = PreparedText::from_bytes(b"a\nbb\nccc\n", 2, 100);
        assert_eq!(prepared.kind, TextKind::Text);
        assert_eq!(prepared.line_count, 3);
        assert_eq!(prepared.chunks.len(), 2);
    }

    #[test]
    fn prepared_text_marks_binary() {
        let prepared = PreparedText::from_bytes(b"bin\x00ary", 2, 100);
        assert_eq!(prepared.kind, TextKind::Binary);
        assert!(prepared.chunks.is_empty());
    }

    #[test]
    fn hash_is_stable_hex() {
        let h = hash("hello");
        assert_eq!(h.len(), CONTENT_FINGERPRINT_HEX_LEN);
        assert_eq!(h, hash("hello"));
        assert_ne!(h, hash("world"));
    }
}
