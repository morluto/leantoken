use super::*;

#[test]
fn retry_backoff_is_exponential_and_capped() {
    let mut backoff = RetryBackoff::new(Duration::from_millis(10), Duration::from_millis(25));
    assert_eq!(backoff.failure_delay(), Duration::from_millis(10));
    assert_eq!(backoff.failure_delay(), Duration::from_millis(20));
    assert_eq!(backoff.failure_delay(), Duration::from_millis(25));
    assert_eq!(backoff.failure_delay(), Duration::from_millis(25));
    backoff.reset();
    assert_eq!(backoff.failure_delay(), Duration::from_millis(10));
}

#[test]
fn configuration_and_safety_errors_are_terminal_but_io_is_retryable() {
    let terminal = [
        leantoken::Error::RootNotFound(PathBuf::from("missing")),
        leantoken::Error::UnsafeRepositoryRoot(PathBuf::from("broad")),
        leantoken::Error::IndexLimitExceeded {
            kind: IndexLimitKind::Files,
            observed: 2,
            limit: 1,
        },
        leantoken::Error::InvalidConfiguration("invalid".into()),
        leantoken::Error::RuntimeCapabilityUnavailable {
            capability: "fts5",
            source: None,
        },
    ];
    assert!(terminal.iter().all(is_terminal_index_error));
    assert!(!is_terminal_index_error(&leantoken::Error::Io(
        std::io::Error::other("transient")
    )));
}
