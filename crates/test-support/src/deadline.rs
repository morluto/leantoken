use std::fmt;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub struct Deadline {
    end: Instant,
}

#[derive(Debug, Clone)]
pub struct DeadlineError {
    pub label: String,
    pub last_observed: String,
}

impl fmt::Display for DeadlineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "deadline exceeded while waiting for {} (last observed: {})",
            self.label, self.last_observed
        )
    }
}
impl std::error::Error for DeadlineError {}

impl Deadline {
    pub fn new(duration: Duration) -> Self {
        Self {
            end: Instant::now() + duration,
        }
    }
    pub fn expired(&self) -> bool {
        Instant::now() >= self.end
    }
    pub fn remaining(&self) -> Duration {
        self.end.saturating_duration_since(Instant::now())
    }

    /// Poll only an external boundary. The callback returns the current
    /// observable state, so timeout failures explain what the system did last.
    pub fn wait_for<T, F>(&self, label: impl Into<String>, mut poll: F) -> Result<T, DeadlineError>
    where
        F: FnMut() -> (Option<T>, String),
    {
        let label = label.into();
        loop {
            let (value, observed) = poll();
            if let Some(value) = value {
                return Ok(value);
            }
            if self.expired() {
                return Err(DeadlineError {
                    label,
                    last_observed: format!("{}; remaining={:?}", observed, self.remaining()),
                });
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Deadline;
    use std::time::Duration;

    #[test]
    fn timeout_reports_the_last_external_observation() {
        let deadline = Deadline::new(Duration::from_millis(1));
        let error = deadline
            .wait_for::<(), _>("child readiness", || (None, "child exited: 7".to_owned()))
            .expect_err("deadline should expire");
        assert!(error.last_observed.contains("child exited: 7"));
    }
}
