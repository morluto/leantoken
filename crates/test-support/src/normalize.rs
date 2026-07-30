use std::path::Path;

#[derive(Debug, Clone)]
pub struct Normalizer {
    from: String,
    to: String,
}

impl Normalizer {
    pub fn path(root: impl AsRef<Path>) -> Self {
        Self::new(root.as_ref().display().to_string(), "<sandbox>")
    }
    pub fn time(value: impl Into<String>) -> Self {
        Self::new(value.into(), "<time>")
    }
    pub fn protocol(request_id: impl Into<String>) -> Self {
        Self::new(request_id.into(), "<request-id>")
    }
    pub fn platform(platform_error: impl Into<String>) -> Self {
        Self::new(platform_error.into(), "<platform-error>")
    }
    pub fn normalize(&self, input: &str) -> String {
        input.replace(&self.from, &self.to)
    }
    pub fn assert_semantic_field_unchanged(&self, before: &str, after: &str, field: &str) -> bool {
        extract_field(before, field) == extract_field(after, field)
    }
    fn new(from: String, to: &str) -> Self {
        Self {
            from,
            to: to.to_owned(),
        }
    }
}

fn extract_field(input: &str, field: &str) -> Option<String> {
    let marker = format!("\"{field}\":");
    input
        .split_once(&marker)
        .map(|(_, value)| value.split(',').next().unwrap_or(value).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::Normalizer;

    #[test]
    fn path_normalization_does_not_change_counts() {
        let normalizer = Normalizer::path("/tmp/test");
        let before = r#"{"path":"/tmp/test/src/lib.rs","count":3}"#;
        let after = normalizer.normalize(before);
        assert_eq!(after, r#"{"path":"<sandbox>/src/lib.rs","count":3}"#);
        assert!(normalizer.assert_semantic_field_unchanged(before, &after, "count"));
    }

    #[test]
    fn normalizer_does_not_hide_a_changed_semantic_value() {
        let normalizer = Normalizer::time("2026-07-29T00:00:00Z");
        let before = r#"{"status":"ready","count":3}"#;
        let after = r#"{"status":"failed","count":3}"#;
        assert!(!normalizer.assert_semantic_field_unchanged(before, after, "status"));
    }

    #[test]
    fn protocol_and_platform_normalizers_replace_only_declared_values() {
        let protocol = Normalizer::protocol("request-7");
        assert_eq!(
            protocol.normalize(r#"{"id":"request-7","count":2}"#),
            r#"{"id":"<request-id>","count":2}"#
        );

        let platform = Normalizer::platform("permission denied");
        let before = r#"{"error":"permission denied","status":"ready"}"#;
        let after = platform.normalize(before);
        assert_eq!(after, r#"{"error":"<platform-error>","status":"ready"}"#);
        assert!(platform.assert_semantic_field_unchanged(before, &after, "status"));
        assert!(!platform.assert_semantic_field_unchanged(before, &after, "error"));
    }
}
