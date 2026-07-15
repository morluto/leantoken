pub fn count(text: &str) -> usize {
    tiktoken_rs::cl100k_base_singleton()
        .encode_ordinary(text)
        .len()
}

pub fn truncate(text: &str, max_tokens: usize) -> (&str, usize) {
    let total = count(text);
    if total <= max_tokens {
        return (text, total);
    }
    if max_tokens == 0 {
        return ("", 0);
    }

    let mut boundaries = text
        .char_indices()
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    boundaries.push(text.len());

    let mut low = 0;
    let mut high = boundaries.len() - 1;
    while low < high {
        let midpoint = low + (high - low).div_ceil(2);
        let boundary = boundaries[midpoint];
        if count(&text[..boundary]) <= max_tokens {
            low = midpoint;
        } else {
            high = midpoint - 1;
        }
    }
    let boundary = boundaries[low];
    let prefix = &text[..boundary];
    (prefix, count(prefix))
}
