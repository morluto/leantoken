use super::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy)]
pub(in crate::mcp) struct McpLimitPolicy {
    pub(in crate::mcp) max_results: usize,
    pub(in crate::mcp) max_output_tokens: usize,
    pub(in crate::mcp) max_response_tokens: usize,
    pub(in crate::mcp) max_context_lines: usize,
    pub(in crate::mcp) default_context_tokens: usize,
}

impl McpLimitPolicy {
    pub(in crate::mcp) const DEFAULT: Self = Self {
        max_results: MAX_RESULTS,
        max_output_tokens: MAX_OUTPUT_TOKENS,
        max_response_tokens: MAX_OUTPUT_TOKENS,
        max_context_lines: MAX_CONTEXT_LINES,
        default_context_tokens: DEFAULT_CONTEXT_TOKENS,
    };

    pub(in crate::mcp) fn from_config(config: &Config) -> crate::Result<Self> {
        config.validate()?;
        Ok(Self {
            max_results: config.max_results,
            max_output_tokens: config.max_output_tokens,
            max_response_tokens: MAX_OUTPUT_TOKENS,
            max_context_lines: MAX_CONTEXT_LINES,
            default_context_tokens: config.default_context_tokens,
        })
    }
}

#[derive(Debug, Clone)]
pub(in crate::mcp) enum McpServiceState {
    Starting(McpLimitPolicy),
    Ready {
        services: Arc<Services>,
        limits: McpLimitPolicy,
    },
    Failed {
        limits: McpLimitPolicy,
        failure: StartupFailure,
    },
}

impl McpServiceState {
    pub(in crate::mcp) const fn limits(&self) -> McpLimitPolicy {
        match self {
            Self::Starting(limits) | Self::Ready { limits, .. } | Self::Failed { limits, .. } => {
                *limits
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(in crate::mcp) struct StartupFailure {
    pub(in crate::mcp) reason: &'static str,
    pub(in crate::mcp) message: &'static str,
}

impl StartupFailure {
    pub(in crate::mcp) fn from_error(error: &crate::Error) -> Self {
        match error {
            crate::Error::UnsafeRepositoryRoot(_) => Self {
                reason: "unsafe_repository_root",
                message: "repository index is unavailable because the root is too broad; start LeanToken from a repository root or explicitly allow the broad root",
            },
            crate::Error::RootNotFound(_) => Self {
                reason: "repository_root_not_found",
                message: "repository index is unavailable because the root does not exist; start LeanToken from an existing repository root",
            },
            crate::Error::IndexLimitExceeded { .. } => Self {
                reason: "repository_index_limit",
                message: "repository index is unavailable because a discovery limit was exceeded; narrow the root or adjust the configured discovery limits",
            },
            crate::Error::InvalidConfiguration(_) => Self {
                reason: "invalid_configuration",
                message: "repository index is unavailable because its configuration is invalid; review the server configuration",
            },
            _ => Self {
                reason: "index_startup_failed",
                message: "repository index is unavailable because startup failed; check bounded server diagnostics and retry",
            },
        }
    }
}

/// Shared readiness handle used by handshake-first MCP startup.
#[derive(Debug, Clone)]
pub struct McpServices {
    pub(in crate::mcp) state: Arc<RwLock<McpServiceState>>,
    pub(in crate::mcp) state_changed: Arc<tokio::sync::Notify>,
    pub(in crate::mcp) protocol_initialized: Arc<AtomicBool>,
    pub(in crate::mcp) initialized: Arc<tokio::sync::Notify>,
}

/// Names the bounded set of repository runtimes approved for one MCP server.
#[derive(Debug, Clone)]
pub struct McpContextRegistry {
    contexts: Arc<RwLock<BTreeMap<String, McpServices>>>,
}

impl McpContextRegistry {
    pub(in crate::mcp) fn primary(primary: McpServices) -> Self {
        let mut contexts = BTreeMap::new();
        contexts.insert("default".into(), primary);
        Self {
            contexts: Arc::new(RwLock::new(contexts)),
        }
    }

    pub fn register(&self, name: String, services: McpServices) -> crate::Result<()> {
        if name.is_empty() || name == "default" || name.len() > 64 || name.contains(['/', '\\']) {
            return Err(crate::Error::InvalidInput {
                field: "repository_context",
                reason: "must be a non-empty approved context name",
            });
        }
        let mut contexts = self
            .contexts
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let approved_contexts = contexts.len().saturating_sub(1);
        if !contexts.contains_key(&name) && approved_contexts >= MAX_REPOSITORY_CONTEXTS {
            return Err(crate::Error::RequestLimitExceeded {
                field: "repository_contexts",
                requested: approved_contexts.saturating_add(1),
                limit: MAX_REPOSITORY_CONTEXTS,
            });
        }
        contexts.insert(name, services);
        Ok(())
    }

    pub(in crate::mcp) fn resolve(&self, name: Option<&str>) -> crate::Result<McpServices> {
        let name = match name {
            None => "default",
            Some(name) if !name.trim().is_empty() => name.trim(),
            Some(_) => {
                return Err(crate::Error::InvalidInput {
                    field: "repository_context",
                    reason: "must be a non-empty approved context name",
                });
            }
        };
        if name.len() > 64 || name.contains(['/', '\\']) {
            return Err(crate::Error::InvalidInput {
                field: "repository_context",
                reason: "must be a bounded approved context name",
            });
        }
        self.contexts
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(name)
            .cloned()
            .ok_or(crate::Error::InvalidInput {
                field: "repository_context",
                reason: "must name an approved repository context",
            })
    }

    pub(in crate::mcp) fn all(&self) -> Vec<(String, McpServices)> {
        self.contexts
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|(name, services)| (name.clone(), services.clone()))
            .collect()
    }
}

impl McpServices {
    pub fn starting_default() -> Self {
        Self::starting(McpLimitPolicy::DEFAULT)
    }
    pub(in crate::mcp) fn starting(limits: McpLimitPolicy) -> Self {
        Self {
            state: Arc::new(RwLock::new(McpServiceState::Starting(limits))),
            state_changed: Arc::new(tokio::sync::Notify::new()),
            protocol_initialized: Arc::new(AtomicBool::new(false)),
            initialized: Arc::new(tokio::sync::Notify::new()),
        }
    }

    pub(in crate::mcp) fn ready(services: Arc<Services>) -> Self {
        let limits = McpLimitPolicy::from_config(services.config())
            .expect("Services always contains a validated configuration");
        Self {
            state: Arc::new(RwLock::new(McpServiceState::Ready { services, limits })),
            state_changed: Arc::new(tokio::sync::Notify::new()),
            protocol_initialized: Arc::new(AtomicBool::new(false)),
            initialized: Arc::new(tokio::sync::Notify::new()),
        }
    }

    pub(in crate::mcp) fn get(&self) -> McpServiceState {
        self.state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(in crate::mcp) async fn wait_for_services(
        &self,
        initial_state: McpServiceState,
        cancellation: CancellationToken,
        deadline: tokio::time::Instant,
    ) -> crate::Result<McpServiceState> {
        if !matches!(initial_state, McpServiceState::Starting(_)) {
            return Ok(initial_state);
        }
        let started = Instant::now();
        loop {
            let state_changed = self.state_changed.notified();
            tokio::pin!(state_changed);
            state_changed.as_mut().enable();
            let state = self.get();
            if !matches!(state, McpServiceState::Starting(_)) {
                tracing::debug!(
                    waited_ms = started.elapsed().as_millis(),
                    ready = matches!(state, McpServiceState::Ready { .. }),
                    "MCP retrieval waited for repository services"
                );
                return Ok(state);
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                tracing::debug!(
                    waited_ms = started.elapsed().as_millis(),
                    ready = false,
                    "MCP retrieval waited for repository services"
                );
                return Ok(state);
            }
            tokio::select! {
                _ = cancellation.cancelled() => return Err(crate::Error::Cancelled),
                _ = tokio::time::sleep(remaining) => {},
                _ = &mut state_changed => {}
            }
        }
    }

    /// Make initialized retrieval services visible to MCP tool handlers.
    pub fn set_ready(&self, services: Arc<Services>) {
        let limits = McpLimitPolicy::from_config(services.config())
            .expect("Services always contains a validated configuration");
        *self
            .state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            McpServiceState::Ready { services, limits };
        self.state_changed.notify_waiters();
    }

    /// Apply validated configured request limits before retrieval services are ready.
    ///
    /// # Errors
    ///
    /// Returns an error when `config` contains invalid runtime limits.
    pub fn configure_limits(&self, config: &Config) -> crate::Result<()> {
        let limits = McpLimitPolicy::from_config(config)?;
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &mut *state {
            McpServiceState::Starting(current)
            | McpServiceState::Failed {
                limits: current, ..
            } => {
                *current = limits;
            }
            McpServiceState::Ready { .. } => {}
        }
        Ok(())
    }

    /// Mark startup as failed while retaining only an allowlisted client-safe reason.
    pub fn set_failed(&self, error: &crate::Error) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *state = McpServiceState::Failed {
            limits: state.limits(),
            failure: StartupFailure::from_error(error),
        };
        drop(state);
        self.state_changed.notify_waiters();
    }

    pub(in crate::mcp) fn mark_protocol_initialized(&self) {
        self.protocol_initialized.store(true, Ordering::Release);
        self.initialized.notify_waiters();
    }

    /// Wait until the client completes the MCP initialization phase.
    pub async fn wait_initialized(&self) {
        loop {
            let notified = self.initialized.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.protocol_initialized.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}
