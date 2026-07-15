use leantoken::tokens::{count, truncate};

#[test]
fn count_is_nonzero_for_source() {
    let text = "fn main() { println!(\"hello\"); }\n";
    let n = count(text);
    assert!(n > 0);
}

#[test]
fn truncate_respects_budget_and_valid_utf8() {
    let source = "fn café() { println!(\"hello\"); }\n".repeat(20);
    let (prefix, tokens) = truncate(&source, 12);
    assert!(source.starts_with(prefix));
    assert!(tokens <= 12);
    assert!(std::str::from_utf8(prefix.as_bytes()).is_ok());
}

#[test]
fn truncate_zero_budget_returns_empty() {
    let (prefix, tokens) = truncate("hello world", 0);
    assert_eq!(prefix, "");
    assert_eq!(tokens, 0);
}

#[test]
fn truncate_short_text_passes_through() {
    let text = "fn a() {}";
    let (prefix, tokens) = truncate(text, 100);
    assert_eq!(prefix, text);
    assert_eq!(tokens, count(text));
}

#[test]
fn truncate_never_exceeds_content_length() {
    let text = "αβγ";
    let (prefix, tokens) = truncate(text, 1);
    assert!(prefix.len() <= text.len());
    assert!(tokens <= count(text));
    assert!(std::str::from_utf8(prefix.as_bytes()).is_ok());
}
