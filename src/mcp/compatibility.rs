use std::sync::{
    Arc, RwLock,
    atomic::{AtomicU8, Ordering},
};

use rmcp::model::{InitializeRequestParams, ProtocolVersion};
use serde::Serialize;

use super::{McpResultMode, mcp_schema_fingerprint};

const CURRENT_VERIFIED_CATALOG_DIGEST: &str = "ffd5662e0731d265f561a5c10a891097";

#[derive(Debug, Clone, Copy)]
struct CompatibilityRow {
    client_name: &'static str,
    client_version: &'static str,
    protocol_version: &'static str,
    result_mode: McpResultMode,
    catalog_digest: &'static str,
    observed_on: &'static str,
    evidence_blake3: &'static str,
}

const COMPATIBILITY_ROWS: &[CompatibilityRow] = &[CompatibilityRow {
    client_name: "codex-mcp-client",
    client_version: "0.144.1",
    protocol_version: "2025-06-18",
    result_mode: McpResultMode::Structured,
    catalog_digest: CURRENT_VERIFIED_CATALOG_DIGEST,
    observed_on: "2026-07-20",
    evidence_blake3: "37f18e8f28565320e91d72c6454b814d6ab14f2d56b0e69880ecfffa925119fd",
}];

/// Why an MCP result-mode request resolved to its effective wire projection.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpResultModeResolutionReason {
    /// The caller explicitly selected `dual`, `text`, or `structured`.
    ExplicitOverride,
    /// Auto mode has not observed a complete initialize request.
    InitializeIncomplete,
    /// Every compatibility-registry key matched exactly.
    ExactRegistryMatch,
    /// No reviewed row exists for the exact client name.
    ClientNameMiss,
    /// The client name matched, but its exact version did not.
    ClientVersionMiss,
    /// The client and version matched, but the negotiated protocol did not.
    ProtocolVersionMiss,
    /// The host tuple matched, but the current tool catalog was not verified.
    CatalogDigestMiss,
}

impl McpResultModeResolutionReason {
    /// Stable diagnostic name used by doctor and JSON reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitOverride => "explicit_override",
            Self::InitializeIncomplete => "initialize_incomplete",
            Self::ExactRegistryMatch => "exact_registry_match",
            Self::ClientNameMiss => "client_name_miss",
            Self::ClientVersionMiss => "client_version_miss",
            Self::ProtocolVersionMiss => "protocol_version_miss",
            Self::CatalogDigestMiss => "catalog_digest_miss",
        }
    }
}

/// Auditable resolution of a requested MCP result mode.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct McpResultModeResolution {
    /// Command-line mode requested by the caller.
    pub requested_mode: McpResultMode,
    /// Effective successful-result wire projection.
    pub resolved_mode: McpResultMode,
    /// Exact-match or fail-closed reason.
    pub reason: McpResultModeResolutionReason,
    /// Whether the compiled compatibility row matches the runtime tool catalog.
    pub registry_schema_current: bool,
    /// Observation date of the exact reviewed row, when matched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry_observed_on: Option<&'static str>,
    /// Frozen real-host evidence artifact hash, when matched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_blake3: Option<&'static str>,
}

impl McpResultModeResolution {
    const fn initial(requested_mode: McpResultMode) -> Self {
        if matches!(requested_mode, McpResultMode::Auto) {
            Self {
                requested_mode,
                resolved_mode: McpResultMode::Dual,
                reason: McpResultModeResolutionReason::InitializeIncomplete,
                registry_schema_current: false,
                registry_observed_on: None,
                evidence_blake3: None,
            }
        } else {
            Self {
                requested_mode,
                resolved_mode: requested_mode,
                reason: McpResultModeResolutionReason::ExplicitOverride,
                registry_schema_current: false,
                registry_observed_on: None,
                evidence_blake3: None,
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(in crate::mcp) struct McpResultModeState {
    requested_mode: McpResultMode,
    resolved_mode: Arc<AtomicU8>,
    resolution: Arc<RwLock<McpResultModeResolution>>,
}

impl McpResultModeState {
    pub(in crate::mcp) fn new(requested_mode: McpResultMode) -> Self {
        let resolution = McpResultModeResolution::initial(requested_mode);
        Self {
            requested_mode,
            resolved_mode: Arc::new(AtomicU8::new(mode_code(resolution.resolved_mode))),
            resolution: Arc::new(RwLock::new(resolution)),
        }
    }

    pub(in crate::mcp) fn resolved_mode(&self) -> McpResultMode {
        mode_from_code(self.resolved_mode.load(Ordering::Acquire))
    }

    pub(in crate::mcp) fn resolution(&self) -> McpResultModeResolution {
        *self
            .resolution
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(in crate::mcp) fn resolve_initialize(
        &self,
        request: &InitializeRequestParams,
        negotiated_protocol: &str,
    ) {
        if self.requested_mode != McpResultMode::Auto {
            return;
        }
        let catalog_digest = mcp_schema_fingerprint();
        let resolution = resolve_auto_result_mode(
            &request.client_info.name,
            &request.client_info.version,
            negotiated_protocol,
            &catalog_digest,
        );
        self.resolved_mode
            .store(mode_code(resolution.resolved_mode), Ordering::Release);
        *self
            .resolution
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = resolution;
    }
}

impl From<McpResultMode> for McpResultModeState {
    fn from(mode: McpResultMode) -> Self {
        Self::new(mode)
    }
}

pub(crate) fn resolve_auto_result_mode(
    client_name: &str,
    client_version: &str,
    protocol_version: &str,
    catalog_digest: &str,
) -> McpResultModeResolution {
    let registry_schema_current = catalog_digest == CURRENT_VERIFIED_CATALOG_DIGEST;
    let name_rows = COMPATIBILITY_ROWS
        .iter()
        .filter(|row| row.client_name == client_name)
        .collect::<Vec<_>>();
    if name_rows.is_empty() {
        return auto_miss(
            McpResultModeResolutionReason::ClientNameMiss,
            registry_schema_current,
        );
    }
    let version_rows = name_rows
        .into_iter()
        .filter(|row| row.client_version == client_version)
        .collect::<Vec<_>>();
    if version_rows.is_empty() {
        return auto_miss(
            McpResultModeResolutionReason::ClientVersionMiss,
            registry_schema_current,
        );
    }
    if !ProtocolVersion::KNOWN_VERSIONS
        .iter()
        .any(|known| known.as_str() == protocol_version)
    {
        return auto_miss(
            McpResultModeResolutionReason::ProtocolVersionMiss,
            registry_schema_current,
        );
    }
    let protocol_rows = version_rows
        .into_iter()
        .filter(|row| row.protocol_version == protocol_version)
        .collect::<Vec<_>>();
    if protocol_rows.is_empty() {
        return auto_miss(
            McpResultModeResolutionReason::ProtocolVersionMiss,
            registry_schema_current,
        );
    }
    let Some(row) = protocol_rows
        .into_iter()
        .find(|row| row.catalog_digest == catalog_digest)
    else {
        return auto_miss(
            McpResultModeResolutionReason::CatalogDigestMiss,
            registry_schema_current,
        );
    };
    McpResultModeResolution {
        requested_mode: McpResultMode::Auto,
        resolved_mode: row.result_mode,
        reason: McpResultModeResolutionReason::ExactRegistryMatch,
        registry_schema_current,
        registry_observed_on: Some(row.observed_on),
        evidence_blake3: Some(row.evidence_blake3),
    }
}

const fn auto_miss(
    reason: McpResultModeResolutionReason,
    registry_schema_current: bool,
) -> McpResultModeResolution {
    McpResultModeResolution {
        requested_mode: McpResultMode::Auto,
        resolved_mode: McpResultMode::Dual,
        reason,
        registry_schema_current,
        registry_observed_on: None,
        evidence_blake3: None,
    }
}

const fn mode_code(mode: McpResultMode) -> u8 {
    match mode {
        McpResultMode::Dual | McpResultMode::Auto => 0,
        McpResultMode::Text => 1,
        McpResultMode::Structured => 2,
    }
}

const fn mode_from_code(code: u8) -> McpResultMode {
    match code {
        1 => McpResultMode::Text,
        2 => McpResultMode::Structured,
        _ => McpResultMode::Dual,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_registry_tuple_selects_verified_structured_mode() {
        let resolution = resolve_auto_result_mode(
            "codex-mcp-client",
            "0.144.1",
            "2025-06-18",
            CURRENT_VERIFIED_CATALOG_DIGEST,
        );
        assert_eq!(resolution.resolved_mode, McpResultMode::Structured);
        assert_eq!(
            resolution.reason,
            McpResultModeResolutionReason::ExactRegistryMatch
        );
        assert!(resolution.registry_schema_current);
        assert!(resolution.evidence_blake3.is_some());
    }

    #[test]
    fn every_registry_key_fails_closed_to_dual() {
        for (name, version, protocol, digest, reason) in [
            (
                "codex",
                "0.144.1",
                "2025-06-18",
                CURRENT_VERIFIED_CATALOG_DIGEST,
                McpResultModeResolutionReason::ClientNameMiss,
            ),
            (
                "codex-mcp-client",
                "0.144.2",
                "2025-06-18",
                CURRENT_VERIFIED_CATALOG_DIGEST,
                McpResultModeResolutionReason::ClientVersionMiss,
            ),
            (
                "codex-mcp-client",
                "0.144.1",
                "2025-11-25",
                CURRENT_VERIFIED_CATALOG_DIGEST,
                McpResultModeResolutionReason::ProtocolVersionMiss,
            ),
            (
                "codex-mcp-client",
                "0.144.1",
                "2099-01-01",
                CURRENT_VERIFIED_CATALOG_DIGEST,
                McpResultModeResolutionReason::ProtocolVersionMiss,
            ),
            (
                "codex-mcp-client",
                "0.144.1",
                "2025-06-18",
                "stale-catalog",
                McpResultModeResolutionReason::CatalogDigestMiss,
            ),
        ] {
            let resolution = resolve_auto_result_mode(name, version, protocol, digest);
            assert_eq!(resolution.resolved_mode, McpResultMode::Dual);
            assert_eq!(resolution.reason, reason);
        }
    }

    #[test]
    fn reviewed_registry_digest_matches_the_runtime_catalog() {
        assert_eq!(
            mcp_schema_fingerprint(),
            CURRENT_VERIFIED_CATALOG_DIGEST,
            "tool catalog changes invalidate reviewed auto-mode rows"
        );
    }

    #[test]
    fn checked_registry_receipt_matches_runtime_row_and_frozen_evidence() {
        let receipt: serde_json::Value = serde_json::from_str(include_str!(
            "../../benchmarks/reports/mcp-result-mode-registry-v1.json"
        ))
        .expect("registry receipt");
        let row = &receipt["rows"][0];
        let runtime = COMPATIBILITY_ROWS[0];
        assert_eq!(row["client_name"], runtime.client_name);
        assert_eq!(row["client_version"], runtime.client_version);
        assert_eq!(row["protocol_version"], runtime.protocol_version);
        assert_eq!(row["tool_catalog_digest"], runtime.catalog_digest);
        assert_eq!(row["observed_on"], runtime.observed_on);
        assert_eq!(row["evidence"]["blake3"], runtime.evidence_blake3);

        let evidence = include_bytes!(
            "../../benchmarks/reports/multi-agent-context-pilot-codex-0.144.1-thin-leantoken-structured-owner.json"
        );
        assert_eq!(
            blake3::hash(evidence).to_hex().as_str(),
            runtime.evidence_blake3
        );
    }

    #[test]
    fn auto_state_is_dual_until_initialize_and_explicit_modes_never_change() {
        let auto = McpResultModeState::new(McpResultMode::Auto);
        assert_eq!(auto.resolved_mode(), McpResultMode::Dual);
        assert_eq!(
            auto.resolution().reason,
            McpResultModeResolutionReason::InitializeIncomplete
        );

        for mode in [
            McpResultMode::Dual,
            McpResultMode::Text,
            McpResultMode::Structured,
        ] {
            let state = McpResultModeState::new(mode);
            assert_eq!(state.resolved_mode(), mode);
            assert_eq!(
                state.resolution().reason,
                McpResultModeResolutionReason::ExplicitOverride
            );
        }
    }
}
