//! Bounded, read-only normalization of redacted episode-analysis artifacts.
//!
//! The auditor consumes reports produced by the repository's existing offline
//! analyzers. It never imports raw prompts, source, tool arguments, or tool
//! outputs, and it preserves unavailable measurements as `null`.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::ser::SerializeStruct;
use serde_json::Value;

/// Failure while reading or normalizing an offline experiment artifact.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The artifact could not be read from disk.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// The artifact violates the selected adapter's bounded schema.
    #[error("invalid episode audit request: {0}")]
    InvalidRequest(String),
}

impl Error {
    /// Stable category for command adapters and tests.
    #[must_use]
    pub const fn public_category(&self) -> &'static str {
        match self {
            Self::Io(_) => "io",
            Self::InvalidRequest(_) => "invalid_request",
        }
    }
}

/// Result returned by offline experiment analysis.
pub type Result<T> = std::result::Result<T, Error>;

/// Episode-audit report schema emitted by this module.
pub const EPISODE_AUDIT_SCHEMA_V1: u32 = 1;
/// Maximum bytes accepted from one already-redacted analyzer report.
pub const MAX_EPISODE_INPUT_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum episode records accepted from one aggregate report.
pub const MAX_EPISODES: usize = 10_000;
/// Maximum tool calls accepted from one report.
pub const MAX_EPISODE_TOOL_CALLS: usize = 100_000;
/// Maximum protocol or trajectory events accepted from one report.
pub const MAX_EPISODE_EVENTS: usize = 100_000;
/// Maximum evidence ranges accepted from one report.
pub const MAX_EPISODE_RANGES: usize = 100_000;
/// Maximum distinct artifact bindings copied into a normalized report.
pub const MAX_ARTIFACT_BINDINGS: usize = 4_096;

/// Versioned input adapter for an existing redacted analyzer report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpisodeAdapter {
    /// Aggregate produced by `codex_multi_agent_suite`.
    MultiAgentSuiteV1,
    /// Classification produced by `model_ab_trajectory`.
    ModelAbTrajectoryV1,
    /// Version-two report produced by `mcp_wire_analyze`.
    McpWireReportV2,
    /// Publishable receipt produced by `codex_host_receipt`.
    CodexHostReceiptV1,
    /// Classification produced by `context_utilization`.
    ContextUtilizationV1,
}

impl EpisodeAdapter {
    fn name(self) -> &'static str {
        match self {
            Self::MultiAgentSuiteV1 => "multi_agent_suite",
            Self::ModelAbTrajectoryV1 => "model_ab_trajectory",
            Self::McpWireReportV2 => "mcp_wire_report",
            Self::CodexHostReceiptV1 => "codex_host_receipt",
            Self::ContextUtilizationV1 => "context_utilization",
        }
    }

    const fn version(self) -> u32 {
        match self {
            Self::McpWireReportV2 => 2,
            Self::MultiAgentSuiteV1
            | Self::ModelAbTrajectoryV1
            | Self::CodexHostReceiptV1
            | Self::ContextUtilizationV1 => 1,
        }
    }
}

/// Request to normalize one existing analyzer report.
#[derive(Debug, Clone)]
pub struct EpisodeAuditRequest {
    /// Explicit adapter and version expected for the input.
    pub adapter: EpisodeAdapter,
    /// Path to an already-redacted analyzer report.
    pub input: PathBuf,
}

/// Coverage of one normalized measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricCoverage {
    /// The adapter's report accounts for the full imported episode.
    Complete,
    /// The source report exposes only a named or implied subset.
    ReportedSubset,
    /// The source report does not expose this measurement.
    Unavailable,
}

/// Count whose availability and evidence coverage cannot contradict each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditCount {
    /// Count covering the complete imported episode.
    Complete(u64),
    /// Count covering only the subset exposed by the source analyzer.
    ReportedSubset(u64),
    /// The source analyzer did not expose this measurement.
    Unavailable,
}

impl AuditCount {
    const fn complete(value: u64) -> Self {
        Self::Complete(value)
    }

    const fn subset(value: u64) -> Self {
        Self::ReportedSubset(value)
    }

    const fn unavailable() -> Self {
        Self::Unavailable
    }

    /// Return the observed count, if the source exposed it.
    #[must_use]
    pub const fn value(self) -> Option<u64> {
        match self {
            Self::Complete(value) | Self::ReportedSubset(value) => Some(value),
            Self::Unavailable => None,
        }
    }

    /// Return the evidence boundary carried by this count.
    #[must_use]
    pub const fn coverage(self) -> MetricCoverage {
        match self {
            Self::Complete(_) => MetricCoverage::Complete,
            Self::ReportedSubset(_) => MetricCoverage::ReportedSubset,
            Self::Unavailable => MetricCoverage::Unavailable,
        }
    }
}

impl Serialize for AuditCount {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("AuditCount", 2)?;
        state.serialize_field("value", &self.value())?;
        state.serialize_field("coverage", &self.coverage())?;
        state.end()
    }
}

/// Hash binding retained from an imported, already-redacted report.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ArtifactBinding {
    /// Stable role of the bound artifact.
    pub kind: String,
    /// Digest algorithm (`blake3` or `git_sha1`).
    pub algorithm: &'static str,
    /// Lowercase hexadecimal digest.
    pub digest: String,
}

/// Version identity of the selected adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdapterReceipt {
    /// Stable adapter family.
    pub name: &'static str,
    /// Adapter contract version.
    pub version: u32,
    /// Schema version declared by the imported report.
    pub input_schema_version: u32,
}

/// Source identity of the imported report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditSource {
    /// BLAKE3 of the complete imported report bytes.
    pub input_blake3: String,
    /// Hashes already published by the source analyzer.
    pub artifact_bindings: Vec<ArtifactBinding>,
}

/// Bounds enforced before or while normalizing a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct EpisodeAuditBounds {
    /// Maximum input bytes.
    pub max_input_bytes: u64,
    /// Maximum episodes.
    pub max_episodes: usize,
    /// Maximum tool calls.
    pub max_tool_calls: usize,
    /// Maximum events.
    pub max_events: usize,
    /// Maximum evidence ranges.
    pub max_ranges: usize,
    /// Maximum artifact bindings.
    pub max_artifact_bindings: usize,
}

/// Adapter-neutral episode measurements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EpisodeAuditSummary {
    /// Imported episode count.
    pub episodes: u64,
    /// Episodes with a positive task outcome.
    pub successful_episodes: AuditCount,
    /// Provider/model requests.
    pub model_calls: AuditCount,
    /// Tool calls visible to the source analyzer.
    pub tool_calls: AuditCount,
    /// Provider-native input tokens. Local tokenizer estimates never populate this field.
    pub provider_input_tokens: AuditCount,
    /// Serialized MCP result bytes.
    pub mcp_result_bytes: AuditCount,
    /// Source tokens emitted by MCP retrievals.
    pub mcp_source_tokens: AuditCount,
    /// Locally tokenized tool-result output retained by a source receipt.
    pub tool_output_tokens: AuditCount,
    /// Exact serialized JSON tokens observed at the MCP boundary.
    pub mcp_wire_tokens: AuditCount,
    /// Tokens duplicated across text and structured result representations.
    pub duplicated_result_tokens: AuditCount,
    /// Evidence ranges represented by the imported analyzer.
    pub evidence_ranges: AuditCount,
    /// Repository-generation changes.
    pub generation_changes: AuditCount,
    /// Receipt or known-hash reuse events.
    pub receipt_events: AuditCount,
    /// Retry-like events exposed by the source analyzer.
    pub retry_events: AuditCount,
    /// Failed calls or failed discovery events.
    pub failure_events: AuditCount,
    /// Host compaction events.
    pub compactions: AuditCount,
    /// Ranges with at least one observed downstream-use proxy.
    pub downstream_signal_ranges: AuditCount,
    /// Ranges without an observed downstream-use proxy.
    pub no_observed_downstream_signal_ranges: AuditCount,
}

/// Strength of the evidence behind one classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingEvidence {
    /// Directly counted by a source analyzer.
    Exact,
    /// Recomputed from complete redacted run samples.
    AggregateExact,
    /// A conservative proxy that does not establish causality.
    Proxy,
}

/// Evidence availability for one avoidable-event classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassifierEvidence {
    /// The adapter exposes exact identities or counts for this classifier.
    Exact,
    /// The adapter exposes only a conservative proxy.
    Proxy,
    /// The adapter cannot evaluate this classifier without inventing evidence.
    Unavailable,
}

/// Coverage of one fixed v1 avoidable-event classifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClassifierCoverage {
    /// Stable machine-readable classifier code.
    pub code: &'static str,
    /// Evidence available through the selected adapter.
    pub evidence: ClassifierEvidence,
}

/// One deterministic episode-audit classification.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EpisodeFinding {
    /// Stable machine-readable finding code.
    pub code: &'static str,
    /// Number of observed occurrences.
    pub occurrences: u64,
    /// Optional scalar associated with the finding.
    pub value: Option<f64>,
    /// Unit for `value`, or `null` when the count is sufficient.
    pub unit: Option<&'static str>,
    /// Evidence strength.
    pub evidence: FindingEvidence,
    /// Fixed, non-sensitive interpretation.
    pub detail: &'static str,
}

/// Stable normalized episode audit.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EpisodeAuditReport {
    /// Normalized report schema version.
    pub schema_version: u32,
    /// Stable report discriminator.
    pub report_kind: &'static str,
    /// The report is diagnostic and does not claim causal attribution.
    pub diagnostic_only: bool,
    /// Input adapter identity.
    pub adapter: AdapterReceipt,
    /// Hash-only source receipt.
    pub source: AuditSource,
    /// Enforced resource bounds.
    pub bounds: EpisodeAuditBounds,
    /// Adapter-neutral measurements.
    pub summary: EpisodeAuditSummary,
    /// Explicit coverage of every v1 avoidable-event classifier.
    pub classifier_coverage: Vec<ClassifierCoverage>,
    /// Exact or explicitly proxied findings.
    pub findings: Vec<EpisodeFinding>,
    /// Fixed limitations of this adapter normalization.
    pub limitations: Vec<&'static str>,
}

impl EpisodeAuditReport {
    /// Render a deterministic Markdown projection of this normalized report.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut output = String::new();
        output.push_str("# LeanToken episode audit\n\n");
        output.push_str(&format!(
            "- Adapter: `{}` v{}\n- Input schema: v{}\n- Input BLAKE3: `{}`\n- Diagnostic only: yes\n\n",
            self.adapter.name,
            self.adapter.version,
            self.adapter.input_schema_version,
            self.source.input_blake3
        ));
        output.push_str("## Summary\n\n");
        output.push_str("| Metric | Value | Coverage |\n| --- | ---: | --- |\n");
        output.push_str(&format!(
            "| episodes | {} | complete |\n",
            self.summary.episodes
        ));
        for (name, metric) in self.summary_rows() {
            let value = metric
                .value()
                .map_or_else(|| "null".to_owned(), |value| value.to_string());
            output.push_str(&format!(
                "| {name} | {value} | {} |\n",
                coverage_name(metric.coverage())
            ));
        }

        output.push_str("\n## Findings\n\n");
        if self.findings.is_empty() {
            output.push_str("No avoidable or contract-level event had sufficient evidence.\n");
        } else {
            output.push_str("| Code | Occurrences | Value | Evidence | Detail |\n");
            output.push_str("| --- | ---: | ---: | --- | --- |\n");
            for finding in &self.findings {
                let value = finding.value.map_or_else(
                    || "null".to_owned(),
                    |value| match finding.unit {
                        Some("fraction") => format!("{:.2}%", value * 100.0),
                        Some(unit) => format!("{value:.6} {unit}"),
                        None => format!("{value:.6}"),
                    },
                );
                output.push_str(&format!(
                    "| `{}` | {} | {} | {} | {} |\n",
                    finding.code,
                    finding.occurrences,
                    value,
                    evidence_name(finding.evidence),
                    finding.detail
                ));
            }
        }

        output.push_str("\n## Classifier coverage\n\n");
        output.push_str("| Classifier | Evidence |\n| --- | --- |\n");
        for classifier in &self.classifier_coverage {
            output.push_str(&format!(
                "| `{}` | {} |\n",
                classifier.code,
                classifier_evidence_name(classifier.evidence)
            ));
        }

        output.push_str("\n## Artifact bindings\n\n");
        if self.source.artifact_bindings.is_empty() {
            output.push_str("No upstream artifact binding was available.\n");
        } else {
            output.push_str("| Kind | Algorithm | Digest |\n| --- | --- | --- |\n");
            for binding in &self.source.artifact_bindings {
                output.push_str(&format!(
                    "| `{}` | {} | `{}` |\n",
                    binding.kind, binding.algorithm, binding.digest
                ));
            }
        }

        output.push_str("\n## Limitations\n\n");
        for limitation in &self.limitations {
            output.push_str(&format!("- {limitation}\n"));
        }
        output
    }

    fn summary_rows(&self) -> [(&'static str, AuditCount); 17] {
        [
            ("successful_episodes", self.summary.successful_episodes),
            ("model_calls", self.summary.model_calls),
            ("tool_calls", self.summary.tool_calls),
            ("provider_input_tokens", self.summary.provider_input_tokens),
            ("mcp_result_bytes", self.summary.mcp_result_bytes),
            ("mcp_source_tokens", self.summary.mcp_source_tokens),
            ("tool_output_tokens", self.summary.tool_output_tokens),
            ("mcp_wire_tokens", self.summary.mcp_wire_tokens),
            (
                "duplicated_result_tokens",
                self.summary.duplicated_result_tokens,
            ),
            ("evidence_ranges", self.summary.evidence_ranges),
            ("generation_changes", self.summary.generation_changes),
            ("receipt_events", self.summary.receipt_events),
            ("retry_events", self.summary.retry_events),
            ("failure_events", self.summary.failure_events),
            ("compactions", self.summary.compactions),
            (
                "downstream_signal_ranges",
                self.summary.downstream_signal_ranges,
            ),
            (
                "no_observed_downstream_signal_ranges",
                self.summary.no_observed_downstream_signal_ranges,
            ),
        ]
    }
}

/// Normalize one existing, redacted analyzer report.
///
/// # Errors
///
/// Returns an error if the file exceeds the byte bound, has the wrong adapter
/// schema, omits a required binding, violates a count bound, or is internally
/// inconsistent.
pub fn audit_episode(request: &EpisodeAuditRequest) -> Result<EpisodeAuditReport> {
    let bytes = read_bounded(&request.input)?;
    audit_episode_bytes(request.adapter, &bytes)
}

fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    read_bounded_with_limit(path, MAX_EPISODE_INPUT_BYTES)
}

fn read_bounded_with_limit(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return invalid(format!("episode input exceeds byte bound of {limit}"));
    }
    Ok(bytes)
}

fn audit_episode_bytes(adapter: EpisodeAdapter, bytes: &[u8]) -> Result<EpisodeAuditReport> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_EPISODE_INPUT_BYTES {
        return invalid(format!(
            "episode input exceeds byte bound of {MAX_EPISODE_INPUT_BYTES}"
        ));
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|error| {
        Error::InvalidRequest(format!(
            "invalid episode JSON at line {}, column {}",
            error.line(),
            error.column()
        ))
    })?;
    let schema_version = required_u64(&value, "/schema_version")?;
    if schema_version != u64::from(adapter.version()) {
        return invalid(format!(
            "adapter {} v{} requires input schema {}, found {schema_version}",
            adapter.name(),
            adapter.version(),
            adapter.version()
        ));
    }
    let mut report = match adapter {
        EpisodeAdapter::MultiAgentSuiteV1 => normalize_suite(&value)?,
        EpisodeAdapter::ModelAbTrajectoryV1 => normalize_trajectory(&value)?,
        EpisodeAdapter::McpWireReportV2 => normalize_wire(&value)?,
        EpisodeAdapter::CodexHostReceiptV1 => normalize_host_receipt(&value)?,
        EpisodeAdapter::ContextUtilizationV1 => normalize_context_utilization(&value)?,
    };
    report.adapter = AdapterReceipt {
        name: adapter.name(),
        version: adapter.version(),
        input_schema_version: u32::try_from(schema_version)
            .map_err(|_| Error::InvalidRequest("input schema version does not fit u32".into()))?,
    };
    report.source.input_blake3 = blake3::hash(bytes).to_hex().to_string();
    report.source.artifact_bindings.sort();
    report.source.artifact_bindings.dedup();
    if report.source.artifact_bindings.len() > MAX_ARTIFACT_BINDINGS {
        return invalid(format!(
            "artifact bindings exceed bound: {} > {MAX_ARTIFACT_BINDINGS}",
            report.source.artifact_bindings.len()
        ));
    }
    report.findings.sort_by_key(|finding| finding.code);
    Ok(report)
}

fn base_report(
    episodes: u64,
    summary: EpisodeAuditSummary,
    bindings: Vec<ArtifactBinding>,
    classifier_coverage: Vec<ClassifierCoverage>,
    findings: Vec<EpisodeFinding>,
    limitations: Vec<&'static str>,
) -> EpisodeAuditReport {
    EpisodeAuditReport {
        schema_version: EPISODE_AUDIT_SCHEMA_V1,
        report_kind: "episode_audit",
        diagnostic_only: true,
        adapter: AdapterReceipt {
            name: "",
            version: 0,
            input_schema_version: 0,
        },
        source: AuditSource {
            input_blake3: String::new(),
            artifact_bindings: bindings,
        },
        bounds: EpisodeAuditBounds {
            max_input_bytes: MAX_EPISODE_INPUT_BYTES,
            max_episodes: MAX_EPISODES,
            max_tool_calls: MAX_EPISODE_TOOL_CALLS,
            max_events: MAX_EPISODE_EVENTS,
            max_ranges: MAX_EPISODE_RANGES,
            max_artifact_bindings: MAX_ARTIFACT_BINDINGS,
        },
        summary: EpisodeAuditSummary {
            episodes,
            ..summary
        },
        classifier_coverage,
        findings,
        limitations,
    }
}

fn empty_summary() -> EpisodeAuditSummary {
    EpisodeAuditSummary {
        episodes: 0,
        successful_episodes: AuditCount::unavailable(),
        model_calls: AuditCount::unavailable(),
        tool_calls: AuditCount::unavailable(),
        provider_input_tokens: AuditCount::unavailable(),
        mcp_result_bytes: AuditCount::unavailable(),
        mcp_source_tokens: AuditCount::unavailable(),
        tool_output_tokens: AuditCount::unavailable(),
        mcp_wire_tokens: AuditCount::unavailable(),
        duplicated_result_tokens: AuditCount::unavailable(),
        evidence_ranges: AuditCount::unavailable(),
        generation_changes: AuditCount::unavailable(),
        receipt_events: AuditCount::unavailable(),
        retry_events: AuditCount::unavailable(),
        failure_events: AuditCount::unavailable(),
        compactions: AuditCount::unavailable(),
        downstream_signal_ranges: AuditCount::unavailable(),
        no_observed_downstream_signal_ranges: AuditCount::unavailable(),
    }
}

#[derive(Debug)]
struct SuiteRun {
    task_id: String,
    repetition: u64,
    arm: String,
    success: bool,
    family_input: u64,
    child_requests: u64,
    child_mcp_calls: u64,
    child_failed_mcp_calls: u64,
    child_mcp_result_bytes: u64,
    child_mcp_source_tokens: u64,
    child_shell_calls: u64,
}

fn normalize_suite(value: &Value) -> Result<EpisodeAuditReport> {
    if !required_bool(value, "/consistency/complete_schedule")? {
        return invalid("suite schedule is incomplete");
    }
    let run_count = bounded_count(required_u64(value, "/run_count")?, MAX_EPISODES, "episodes")?;
    let samples = required_array(value, "/run_samples")?;
    if samples.len() != run_count {
        return invalid("run_count does not match run_samples length");
    }
    let arms = required_array(value, "/arms")?
        .iter()
        .enumerate()
        .map(|(index, arm)| {
            arm.as_str()
                .filter(|arm| !arm.trim().is_empty())
                .map(str::to_owned)
                .ok_or_else(|| Error::InvalidRequest(format!("invalid arm at /arms/{index}")))
        })
        .collect::<Result<BTreeSet<_>>>()?;
    if arms.len() > 64 {
        return invalid("suite arm count exceeds bound of 64");
    }
    let candidate_arms = arms
        .iter()
        .filter(|arm| arm.contains("leantoken"))
        .cloned()
        .collect::<Vec<_>>();
    if candidate_arms.len() != 1 {
        return invalid("suite adapter requires exactly one LeanToken candidate arm");
    }
    let candidate_arm = &candidate_arms[0];
    let baseline_arms = arms
        .iter()
        .filter(|arm| arm.as_str() == "thin-native")
        .cloned()
        .collect::<Vec<_>>();
    if baseline_arms.len() != 1 {
        return invalid("suite adapter requires the thin-native retrieval baseline");
    }
    let baseline_arm = &baseline_arms[0];

    let mut schedule_indexes = BTreeSet::new();
    let mut run_keys = BTreeSet::new();
    let mut runs = Vec::with_capacity(run_count);
    let mut bindings = standard_bindings(
        value,
        &[
            ("suite_manifest", "/suite_manifest_blake3"),
            ("aggregate_binary", "/aggregate_binary_blake3"),
            ("receipt_set", "/receipt_set_blake3"),
        ],
    )?;
    let receipt_binary_bindings = required_array(value, "/consistency/receipt_binary_blake3")?;
    if receipt_binary_bindings.len() > 64 {
        return invalid("suite receipt-binary bindings exceed bound of 64");
    }
    if run_count
        .saturating_add(bindings.len())
        .saturating_add(receipt_binary_bindings.len())
        > MAX_ARTIFACT_BINDINGS
    {
        return invalid("suite artifact bindings exceed the normalized report bound");
    }
    for (index, digest) in receipt_binary_bindings.iter().enumerate() {
        push_blake3_binding(
            &mut bindings,
            format!("receipt_binary_{index}"),
            digest.as_str().ok_or_else(|| {
                Error::InvalidRequest(format!(
                    "invalid digest at /consistency/receipt_binary_blake3/{index}"
                ))
            })?,
        )?;
    }
    for (index, sample) in samples.iter().enumerate() {
        let prefix = format!("/run_samples/{index}");
        let schedule_index = required_u64(sample, "/schedule_index")?;
        let repetition = required_u64(sample, "/repetition")?;
        let task_id = required_str(sample, "/task_id")?.to_owned();
        let arm = required_str(sample, "/arm")?.to_owned();
        if !arms.contains(&arm) {
            return invalid(format!("{prefix}/arm is not declared by the suite"));
        }
        if !schedule_indexes.insert(schedule_index) {
            return invalid("suite contains duplicate schedule_index values");
        }
        if !run_keys.insert((task_id.clone(), repetition, arm.clone())) {
            return invalid("suite contains duplicate task/repetition/arm records");
        }
        push_blake3_binding(
            &mut bindings,
            format!("source_family_{schedule_index}"),
            required_str(sample, "/source_family_blake3")?,
        )?;
        runs.push(SuiteRun {
            task_id,
            repetition,
            arm,
            success: required_bool(sample, "/success")?,
            family_input: required_u64(sample, "/family_usage/total_input_tokens")?,
            child_requests: required_u64(sample, "/child_provider_requests")?,
            child_mcp_calls: required_u64(sample, "/child_mcp_calls")?,
            child_failed_mcp_calls: required_u64(sample, "/child_failed_mcp_calls")?,
            child_mcp_result_bytes: required_u64(sample, "/child_mcp_result_bytes")?,
            child_mcp_source_tokens: required_u64(sample, "/child_mcp_source_tokens")?,
            child_shell_calls: required_u64(sample, "/child_shell_calls")?,
        });
    }
    let task_count = bounded_count(required_u64(value, "/task_count")?, MAX_EPISODES, "tasks")?;
    let repetitions = bounded_count(
        required_u64(value, "/repetitions")?,
        MAX_EPISODES,
        "repetitions",
    )?;
    let expected_runs = task_count
        .checked_mul(repetitions)
        .and_then(|count| count.checked_mul(arms.len()))
        .ok_or_else(|| Error::InvalidRequest("suite schedule cardinality overflow".into()))?;
    if expected_runs != run_count {
        return invalid("suite task/arm/repetition cardinality does not match run_count");
    }
    let expected_schedule_indexes = (1..=u64::try_from(run_count)
        .map_err(|_| Error::InvalidRequest("schedule count overflow".into()))?)
        .collect::<BTreeSet<_>>();
    if schedule_indexes != expected_schedule_indexes {
        return invalid("suite schedule indexes must be contiguous and start at one");
    }

    let total_model_calls = checked_sum(runs.iter().map(|run| run.child_requests), "model calls")?;
    bounded_count(total_model_calls, MAX_EPISODE_TOOL_CALLS, "model calls")?;
    let total_tool_calls = checked_sum(
        runs.iter()
            .map(|run| {
                run.child_mcp_calls
                    .checked_add(run.child_shell_calls)
                    .ok_or_else(|| Error::InvalidRequest("tool calls overflow".into()))
            })
            .collect::<Result<Vec<_>>>()?,
        "tool calls",
    )?;
    bounded_count(total_tool_calls, MAX_EPISODE_TOOL_CALLS, "tool calls")?;
    let total_provider_input =
        checked_sum(runs.iter().map(|run| run.family_input), "provider input")?;
    let total_result_bytes = checked_sum(
        runs.iter().map(|run| run.child_mcp_result_bytes),
        "MCP result bytes",
    )?;
    let total_source_tokens = checked_sum(
        runs.iter().map(|run| run.child_mcp_source_tokens),
        "MCP source tokens",
    )?;
    let total_failures = checked_sum(
        runs.iter().map(|run| run.child_failed_mcp_calls),
        "failed MCP calls",
    )?;
    let successes = u64::try_from(runs.iter().filter(|run| run.success).count())
        .map_err(|_| Error::InvalidRequest("success count overflow".into()))?;

    let candidate_runs = runs
        .iter()
        .filter(|run| &run.arm == candidate_arm)
        .collect::<Vec<_>>();
    let baseline_runs = runs
        .iter()
        .filter(|run| &run.arm == baseline_arm)
        .collect::<Vec<_>>();
    if candidate_runs.is_empty() || candidate_runs.len() != baseline_runs.len() {
        return invalid("suite candidate and retrieval baseline are not equally populated");
    }
    let candidate_keys = candidate_runs
        .iter()
        .map(|run| (&run.task_id, run.repetition))
        .collect::<BTreeSet<_>>();
    let baseline_keys = baseline_runs
        .iter()
        .map(|run| (&run.task_id, run.repetition))
        .collect::<BTreeSet<_>>();
    if candidate_keys != baseline_keys {
        return invalid("suite candidate and retrieval baseline samples are not paired");
    }
    let candidate_requests = checked_sum(
        candidate_runs.iter().map(|run| run.child_requests),
        "candidate model calls",
    )?;
    let candidate_input = checked_sum(
        candidate_runs.iter().map(|run| run.family_input),
        "candidate input",
    )?;
    let baseline_input = checked_sum(
        baseline_runs.iter().map(|run| run.family_input),
        "baseline input",
    )?;
    if baseline_input == 0 {
        return invalid("suite retrieval baseline input must be positive");
    }
    let request_mean = exact_f64(candidate_requests, "candidate model calls")?
        / exact_f64(
            u64::try_from(candidate_runs.len())
                .map_err(|_| Error::InvalidRequest("candidate count overflow".into()))?,
            "candidate episode count",
        )?;
    let baseline_input_f64 = exact_f64(baseline_input, "baseline provider input")?;
    let candidate_input_f64 = exact_f64(candidate_input, "candidate provider input")?;
    let savings = (baseline_input_f64 - candidate_input_f64) / baseline_input_f64;

    let arm_summaries = required_array(value, "/arm_summaries")?;
    let declared_candidate = arm_summaries
        .iter()
        .find(|summary| optional_str(summary, "/arm") == Some(candidate_arm.as_str()))
        .ok_or_else(|| Error::InvalidRequest("candidate arm summary is missing".into()))?;
    verify_u64(
        declared_candidate,
        "/runs",
        u64::try_from(candidate_runs.len())
            .map_err(|_| Error::InvalidRequest("candidate count overflow".into()))?,
    )?;
    verify_u64(
        declared_candidate,
        "/successes",
        u64::try_from(candidate_runs.iter().filter(|run| run.success).count())
            .map_err(|_| Error::InvalidRequest("candidate success count overflow".into()))?,
    )?;
    verify_u64(
        declared_candidate,
        "/child_provider_requests/sum",
        candidate_requests,
    )?;
    verify_f64(
        declared_candidate,
        "/child_provider_requests/mean",
        request_mean,
    )?;
    verify_u64(
        declared_candidate,
        "/family_total_input/sum",
        candidate_input,
    )?;
    let candidate_contract_violations = required_u64(declared_candidate, "/contract_violations")?;
    let declared_all_violations = required_u64(value, "/consistency/contract_violation_count")?;
    let summed_violations = checked_sum(
        arm_summaries
            .iter()
            .map(|summary| required_u64(summary, "/contract_violations"))
            .collect::<Result<Vec<_>>>()?,
        "contract violations",
    )?;
    if summed_violations != declared_all_violations {
        return invalid("suite contract violation summaries are inconsistent");
    }

    let comparisons = required_array(value, "/comparisons")?;
    let declared_comparison = comparisons
        .iter()
        .find(|comparison| {
            optional_str(comparison, "/baseline") == Some(baseline_arm.as_str())
                && optional_str(comparison, "/candidate") == Some(candidate_arm.as_str())
        })
        .ok_or_else(|| {
            Error::InvalidRequest(
                "candidate versus retrieval-baseline comparison is missing".into(),
            )
        })?;
    verify_f64(
        declared_comparison,
        "/weighted_total_input_savings_fraction",
        savings,
    )?;
    verify_u64(
        declared_comparison,
        "/paired_runs",
        u64::try_from(candidate_runs.len())
            .map_err(|_| Error::InvalidRequest("candidate count overflow".into()))?,
    )?;

    let mut findings = vec![EpisodeFinding {
        code: if candidate_arm.contains("context-bundle") {
            "one_context_optional_search_contract"
        } else {
            "iterative_retrieval_contract"
        },
        occurrences: u64::try_from(candidate_runs.len())
            .map_err(|_| Error::InvalidRequest("candidate count overflow".into()))?,
        value: Some(request_mean),
        unit: Some("mean_model_calls"),
        evidence: FindingEvidence::AggregateExact,
        detail: if candidate_arm.contains("context-bundle") {
            "The frozen aggregate identifies the one-context plus optional-search candidate; contract violations are reported separately."
        } else {
            "The frozen aggregate identifies an iterative LeanToken retrieval candidate; the value is its mean child provider-request count."
        },
    }];
    if savings < 0.0 {
        findings.push(EpisodeFinding {
            code: "provider_input_regression",
            occurrences: u64::try_from(candidate_runs.len())
                .map_err(|_| Error::InvalidRequest("candidate count overflow".into()))?,
            value: Some(-savings),
            unit: Some("fraction"),
            evidence: FindingEvidence::AggregateExact,
            detail: "Candidate family input exceeded its paired retrieval baseline in the complete redacted aggregate.",
        });
    }
    if candidate_contract_violations > 0 {
        findings.push(EpisodeFinding {
            code: "retrieval_contract_violation",
            occurrences: candidate_contract_violations,
            value: None,
            unit: None,
            evidence: FindingEvidence::AggregateExact,
            detail: "The frozen suite classifier recorded calls outside the candidate retrieval contract.",
        });
    }

    let mut summary = empty_summary();
    summary.successful_episodes = AuditCount::complete(successes);
    summary.model_calls = AuditCount::subset(total_model_calls);
    summary.tool_calls = AuditCount::subset(total_tool_calls);
    summary.provider_input_tokens = AuditCount::complete(total_provider_input);
    summary.mcp_result_bytes = AuditCount::subset(total_result_bytes);
    summary.mcp_source_tokens = AuditCount::subset(total_source_tokens);
    summary.failure_events = AuditCount::subset(total_failures);
    Ok(base_report(
        u64::try_from(run_count)
            .map_err(|_| Error::InvalidRequest("episode count overflow".into()))?,
        summary,
        bindings,
        classifier_coverage(&[], &[]),
        findings,
        vec![
            "Model and tool counts cover child threads exposed by the aggregate, not every root-thread action.",
            "Provider input is aggregate context volume; provider cache state is not controlled.",
            "Task success uses the source suite's redacted path-set validator and does not establish patch correctness.",
            "Contract labels come from the frozen suite arm and its aggregate violation accounting; this adapter does not reconstruct private tool arguments.",
        ],
    ))
}

fn normalize_trajectory(value: &Value) -> Result<EpisodeAuditReport> {
    if required_str(value, "/report_kind")? != "model_ab_trajectory_classification" {
        return invalid("trajectory adapter received an unexpected report_kind");
    }
    let runs = required_array(value, "/runs")?;
    bounded_count(
        u64::try_from(runs.len()).unwrap_or(u64::MAX),
        MAX_EPISODES,
        "episodes",
    )?;
    verify_u64(
        value,
        "/controls/runs",
        u64::try_from(runs.len())
            .map_err(|_| Error::InvalidRequest("episode count overflow".into()))?,
    )?;
    let bindings = standard_bindings(
        value,
        &[
            ("classifier_binary", "/source/classifier_binary_blake3"),
            ("classifier_source", "/source/classifier_source_blake3"),
            ("classifier_manifest", "/source/classifier_manifest_blake3"),
            ("raw_report", "/source/raw_report_blake3"),
            ("model_ab_manifest", "/source/model_ab_manifest_blake3"),
            ("dataset", "/source/dataset_blake3"),
        ],
    )?;
    bounded_count(
        required_u64(value, "/source/verified_artifacts")?,
        MAX_ARTIFACT_BINDINGS,
        "verified artifacts",
    )?;

    let successes = count_true(runs, "/official_success")?;
    let tool_calls = sum_field(runs, "/retrieval_calls", "retrieval calls")?;
    bounded_count(tool_calls, MAX_EPISODE_TOOL_CALLS, "tool calls")?;
    let observed_events = checked_sum(
        runs.iter()
            .enumerate()
            .map(|(index, run)| {
                required_array(run, "/discovery_order").and_then(|events| {
                    u64::try_from(events.len()).map_err(|_| {
                        Error::InvalidRequest(format!(
                            "discovery event count overflow at run {index}"
                        ))
                    })
                })
            })
            .collect::<Result<Vec<_>>>()?,
        "trajectory events",
    )?;
    bounded_count(observed_events, MAX_EPISODE_EVENTS, "trajectory events")?;
    let exact_rereads = sum_field(runs, "/exact_rereads", "exact rereads")?;
    let overlap_rereads = sum_field(runs, "/overlap_rereads", "overlap rereads")?;
    bounded_count(
        exact_rereads
            .checked_add(overlap_rereads)
            .ok_or_else(|| Error::InvalidRequest("reread range count overflow".into()))?,
        MAX_EPISODE_RANGES,
        "reread ranges",
    )?;
    let generation_changes =
        sum_field(runs, "/repository_generation_changes", "generation changes")?;
    let receipt_events = checked_sum(
        [
            sum_field(runs, "/known_hash_reuses", "known-hash reuses")?,
            sum_field(runs, "/known_hash_resends", "known-hash resends")?,
        ],
        "receipt events",
    )?;
    let retries = sum_field(runs, "/retryable_results", "retryable results")?;
    let failures = sum_field(runs, "/failed_discovery_calls", "failed discovery calls")?;
    let mut findings = Vec::new();
    push_repeat_findings(&mut findings, exact_rereads, overlap_rereads);

    let mut summary = empty_summary();
    summary.successful_episodes = AuditCount::complete(successes);
    summary.tool_calls = AuditCount::complete(tool_calls);
    summary.generation_changes = AuditCount::complete(generation_changes);
    summary.receipt_events = AuditCount::complete(receipt_events);
    summary.retry_events = AuditCount::complete(retries);
    summary.failure_events = AuditCount::complete(failures);
    Ok(base_report(
        u64::try_from(runs.len())
            .map_err(|_| Error::InvalidRequest("episode count overflow".into()))?,
        summary,
        bindings,
        classifier_coverage(&["repeated_exact_evidence", "overlapping_reread"], &[]),
        findings,
        vec![
            "Provider requests and provider-native input are unavailable in the trajectory classification.",
            "Native command parsing is conservative; unattributed native reads remain outside exact range counts.",
            "Reread findings identify repeated retrieval pressure and do not prove that the first evidence was sufficient.",
        ],
    ))
}

fn normalize_wire(value: &Value) -> Result<EpisodeAuditReport> {
    let events = bounded_count(
        required_u64(value, "/event_count")?,
        MAX_EPISODE_EVENTS,
        "wire events",
    )?;
    let bindings = standard_bindings(
        value,
        &[
            ("trace_file", "/trace_file_blake3"),
            ("trace_content", "/trace_content_blake3"),
        ],
    )?;
    let modes = required_object(value, "/tool_result_modes")?;
    let tool_calls = checked_sum(
        modes
            .values()
            .map(|count| {
                count.as_u64().ok_or_else(|| {
                    Error::InvalidRequest(
                        "tool_result_modes values must be non-negative integers".into(),
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?,
        "wire tool calls",
    )?;
    bounded_count(tool_calls, MAX_EPISODE_TOOL_CALLS, "tool calls")?;
    let wire_tokens = required_u64(value, "/total_serialized_json_tokens")?;
    let duplicated_tokens = required_u64(value, "/duplicated_result_tokens")?;
    let ranges = bounded_count(
        required_u64(value, "/range_identity_count")?,
        MAX_EPISODE_RANGES,
        "evidence ranges",
    )?;
    let exact_duplicates = required_u64(value, "/same_range_duplicate_ranges")?;
    let rereads = required_u64(value, "/reread_ranges")?;
    let provider_input = optional_u64(value, "/provider_total_input_tokens")?;
    let source_tokens = optional_u64(value, "/total_source_tokens")?;
    let mut findings = Vec::new();
    push_repeat_findings(&mut findings, exact_duplicates, rereads);
    if duplicated_tokens > 0 {
        findings.push(EpisodeFinding {
            code: "dual_result_duplication",
            occurrences: required_u64(value, "/exact_text_structured_duplicates")?,
            value: None,
            unit: None,
            evidence: FindingEvidence::Exact,
            detail: "Exact text and structured MCP result representations duplicated the same payload.",
        });
    }

    let mut summary = empty_summary();
    summary.tool_calls = AuditCount::complete(tool_calls);
    summary.provider_input_tokens =
        provider_input.map_or_else(AuditCount::unavailable, AuditCount::complete);
    summary.mcp_source_tokens =
        source_tokens.map_or_else(AuditCount::unavailable, AuditCount::complete);
    summary.mcp_wire_tokens = AuditCount::complete(wire_tokens);
    summary.duplicated_result_tokens = AuditCount::complete(duplicated_tokens);
    summary.evidence_ranges = AuditCount::complete(
        u64::try_from(ranges).map_err(|_| Error::InvalidRequest("range count overflow".into()))?,
    );
    Ok(base_report(
        1,
        summary,
        bindings,
        classifier_coverage(
            &[
                "repeated_exact_evidence",
                "overlapping_reread",
                "dual_result_duplication",
            ],
            &[],
        ),
        findings,
        vec![
            "MCP wire tokens are exact for the declared local tokenizer, not provider billing tokens.",
            "Provider input remains null unless the wire trace exported an authoritative provider total.",
            "Protocol framing outside serialized JSON messages is not represented.",
            if events == 0 {
                "The imported wire report contained no events."
            } else {
                "Result-mode and range findings are limited to events visible at the captured MCP boundary."
            },
        ],
    ))
}

fn normalize_host_receipt(value: &Value) -> Result<EpisodeAuditReport> {
    validate_publishable_privacy(value)?;
    let tool_calls = required_array(value, "/tool_calls")?;
    bounded_count(
        u64::try_from(tool_calls.len()).unwrap_or(u64::MAX),
        MAX_EPISODE_TOOL_CALLS,
        "tool calls",
    )?;
    let compactions = required_array(value, "/compactions")?;
    bounded_count(
        u64::try_from(compactions.len()).unwrap_or(u64::MAX),
        MAX_EPISODE_EVENTS,
        "compactions",
    )?;
    let mut bindings = standard_bindings(
        value,
        &[
            ("source_rollout", "/source_rollout_blake3"),
            ("source_mcp_trace", "/source_mcp_trace_blake3"),
            ("source_mcp_content", "/source_mcp_content_blake3"),
            ("host_binary", "/host_binary_blake3"),
            ("runtime_binary", "/runtime_binary_blake3"),
            ("capture_binary", "/capture_binary_blake3"),
            ("receipt_binary", "/receipt_binary_blake3"),
        ],
    )?;
    push_git_binding(
        &mut bindings,
        "harness_revision",
        required_str(value, "/harness_revision")?,
    )?;
    push_git_binding(
        &mut bindings,
        "runtime_revision",
        required_str(value, "/runtime_revision")?,
    )?;
    let failures = count_true(tool_calls, "/result_is_error")?;
    verify_u64(
        value,
        "/mcp_correlation/rollout_tool_calls",
        u64::try_from(tool_calls.len())
            .map_err(|_| Error::InvalidRequest("tool call count overflow".into()))?,
    )?;
    let receipt_events = checked_sum(
        [
            required_u64(value, "/mcp_correlation/known_hash_followups")?,
            required_u64(value, "/mcp_correlation/not_modified_results")?,
        ],
        "receipt events",
    )?;
    let output_tokens = sum_field(tool_calls, "/output_tokens", "tool output tokens")?;
    let provider_input = required_u64(value, "/total_input_tokens")?;
    let successful = if required_u64(value, "/completed_turns")? > 0
        && required_u64(value, "/aborted_turns")? == 0
    {
        1
    } else {
        0
    };
    let mut findings = Vec::new();
    if failures > 0 {
        findings.push(EpisodeFinding {
            code: "tool_failure",
            occurrences: failures,
            value: None,
            unit: None,
            evidence: FindingEvidence::Exact,
            detail: "The redacted host receipt contains tool results marked as errors; their private payloads were not imported.",
        });
    }

    let mut summary = empty_summary();
    summary.successful_episodes = AuditCount::subset(successful);
    summary.tool_calls = AuditCount::complete(
        u64::try_from(tool_calls.len())
            .map_err(|_| Error::InvalidRequest("tool call count overflow".into()))?,
    );
    summary.provider_input_tokens = AuditCount::complete(provider_input);
    summary.tool_output_tokens = AuditCount::complete(output_tokens);
    summary.receipt_events = AuditCount::complete(receipt_events);
    summary.failure_events = AuditCount::complete(failures);
    summary.compactions = AuditCount::complete(
        u64::try_from(compactions.len())
            .map_err(|_| Error::InvalidRequest("compaction count overflow".into()))?,
    );
    Ok(base_report(
        1,
        summary,
        bindings,
        classifier_coverage(&[], &[]),
        findings,
        vec![
            "Provider usage is native session accounting, but the receipt does not expose provider request framing.",
            "Tool-output tokens use the receipt's selected local tokenizer and are kept separate from provider input.",
            "Turn completion is only a host-level outcome proxy and does not establish task success.",
            "The privacy declaration is validated before any publishable normalized report is emitted.",
        ],
    ))
}

fn normalize_context_utilization(value: &Value) -> Result<EpisodeAuditReport> {
    if required_str(value, "/report_kind")? != "context_utilization_trajectory" {
        return invalid("context-utilization adapter received an unexpected report_kind");
    }
    if !required_bool(value, "/diagnostic_only")? {
        return invalid("context-utilization report must remain diagnostic_only");
    }
    let ranges = required_array(value, "/ranges")?;
    bounded_count(
        u64::try_from(ranges.len()).unwrap_or(u64::MAX),
        MAX_EPISODE_RANGES,
        "evidence ranges",
    )?;
    let context_ranges = required_u64(value, "/summary/context_ranges/ranges")?;
    if context_ranges
        != u64::try_from(ranges.len())
            .map_err(|_| Error::InvalidRequest("range count overflow".into()))?
    {
        return invalid("context range summary does not match ranges length");
    }
    let calls = required_u64(value, "/summary/context_calls")?;
    bounded_count(calls, MAX_EPISODE_TOOL_CALLS, "tool calls")?;
    let failures = required_u64(value, "/summary/failed_context_calls")?;
    let exact_rereads = required_u64(value, "/summary/exact_reread_later/ranges")?;
    let overlap_rereads = required_u64(value, "/summary/overlap_reread_later/ranges")?;
    let no_signal = required_u64(value, "/summary/no_observed_downstream_signal/ranges")?;
    if no_signal > context_ranges {
        return invalid("no-signal range count exceeds context range count");
    }
    let downstream = context_ranges - no_signal;
    let receipt_events = required_u64(value, "/summary/receipt_follow_up_calls")?;
    let mut bindings = standard_bindings(
        value,
        &[
            ("classifier_source", "/source/classifier_source_blake3"),
            ("tool_trace", "/source/tool_trace_blake3"),
            ("trajectory", "/source/trajectory_blake3"),
            ("manifest", "/source/binding/manifest_blake3"),
        ],
    )?;
    let experiment = required_str(value, "/source/binding/experiment_id")?;
    let task = required_str(value, "/source/binding/task_id")?;
    let arm = required_str(value, "/source/binding/arm")?;
    let repetition = required_u64(value, "/source/binding/repetition")?;
    let binding_identity = format!("{experiment}\0{task}\0{repetition}\0{arm}");
    let binding_digest = blake3::hash(binding_identity.as_bytes()).to_hex();
    push_blake3_binding(&mut bindings, "run_binding", binding_digest.as_str())?;
    let success = match required_str(value, "/outcome")? {
        "success" => AuditCount::complete(1),
        "failure" => AuditCount::complete(0),
        "unknown" => AuditCount::unavailable(),
        _ => return invalid("context-utilization outcome is unsupported"),
    };
    let mut findings = Vec::new();
    push_repeat_findings(&mut findings, exact_rereads, overlap_rereads);
    if no_signal > 0 {
        findings.push(EpisodeFinding {
            code: "no_observed_downstream_signal",
            occurrences: no_signal,
            value: None,
            unit: None,
            evidence: FindingEvidence::Proxy,
            detail: "No supported downstream-use proxy was observed; this is not proof that the evidence was unused.",
        });
    }

    let mut summary = empty_summary();
    summary.successful_episodes = success;
    summary.tool_calls = AuditCount::subset(calls);
    summary.evidence_ranges = AuditCount::complete(context_ranges);
    summary.receipt_events = AuditCount::complete(receipt_events);
    summary.failure_events = AuditCount::subset(failures);
    summary.downstream_signal_ranges = AuditCount::complete(downstream);
    summary.no_observed_downstream_signal_ranges = AuditCount::complete(no_signal);
    Ok(base_report(
        1,
        summary,
        bindings,
        classifier_coverage(&["repeated_exact_evidence", "overlapping_reread"], &[]),
        findings,
        vec![
            "Relevant-path, hash-input, and reread signals are downstream-use proxies and do not establish model reasoning.",
            "No-observed-signal is not proof that returned evidence was unused.",
            "Provider requests and provider-native usage are unavailable in this classifier report.",
            "Paths, prompts, commands, and source content from the underlying artifacts are not copied into the audit.",
        ],
    ))
}

fn validate_publishable_privacy(value: &Value) -> Result<()> {
    for pointer in [
        "/privacy/raw_rollout_retained",
        "/privacy/raw_mcp_messages_retained",
        "/privacy/prompts_retained",
        "/privacy/tool_arguments_retained",
        "/privacy/tool_outputs_retained",
        "/privacy/credentials_retained",
        "/privacy/absolute_paths_retained",
    ] {
        if required_bool(value, pointer)? {
            return invalid(format!(
                "host receipt is not publishable because {pointer} is true"
            ));
        }
    }
    if !required_bool(value, "/privacy/session_and_call_ids_hashed_or_omitted")? {
        return invalid("host receipt does not hash or omit session and call identifiers");
    }
    Ok(())
}

fn push_repeat_findings(findings: &mut Vec<EpisodeFinding>, exact: u64, overlap: u64) {
    if exact > 0 {
        findings.push(EpisodeFinding {
            code: "repeated_exact_evidence",
            occurrences: exact,
            value: None,
            unit: None,
            evidence: FindingEvidence::Exact,
            detail: "The same evidence identity was retrieved again.",
        });
    }
    if overlap > 0 {
        findings.push(EpisodeFinding {
            code: "overlapping_reread",
            occurrences: overlap,
            value: None,
            unit: None,
            evidence: FindingEvidence::Exact,
            detail: "A later retrieval overlapped an earlier evidence range.",
        });
    }
}

fn classifier_coverage(exact: &[&'static str], proxy: &[&'static str]) -> Vec<ClassifierCoverage> {
    const CLASSIFIERS: [&str; 8] = [
        "repeated_exact_evidence",
        "overlapping_reread",
        "repeated_or_subsumed_exact_query",
        "fixed_plan_materialize_pair",
        "index_retry_without_new_progress",
        "dual_result_duplication",
        "tool_validation_retry",
        "unchanged_diagnostic_replay",
    ];
    CLASSIFIERS
        .into_iter()
        .map(|code| ClassifierCoverage {
            code,
            evidence: if exact.contains(&code) {
                ClassifierEvidence::Exact
            } else if proxy.contains(&code) {
                ClassifierEvidence::Proxy
            } else {
                ClassifierEvidence::Unavailable
            },
        })
        .collect()
}

fn standard_bindings(value: &Value, fields: &[(&str, &str)]) -> Result<Vec<ArtifactBinding>> {
    let mut bindings = Vec::with_capacity(fields.len());
    for (kind, pointer) in fields {
        push_blake3_binding(&mut bindings, *kind, required_str(value, pointer)?)?;
    }
    Ok(bindings)
}

fn push_blake3_binding(
    bindings: &mut Vec<ArtifactBinding>,
    kind: impl Into<String>,
    digest: &str,
) -> Result<()> {
    validate_lower_hex(digest, 64, "BLAKE3 binding")?;
    bindings.push(ArtifactBinding {
        kind: kind.into(),
        algorithm: "blake3",
        digest: digest.to_owned(),
    });
    Ok(())
}

fn push_git_binding(
    bindings: &mut Vec<ArtifactBinding>,
    kind: impl Into<String>,
    digest: &str,
) -> Result<()> {
    validate_lower_hex(digest, 40, "Git revision binding")?;
    bindings.push(ArtifactBinding {
        kind: kind.into(),
        algorithm: "git_sha1",
        digest: digest.to_owned(),
    });
    Ok(())
}

fn validate_lower_hex(value: &str, length: usize, label: &str) -> Result<()> {
    if value.len() != length
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return invalid(format!(
            "{label} must be {length} lowercase hexadecimal bytes"
        ));
    }
    Ok(())
}

fn exact_f64(value: u64, label: &str) -> Result<f64> {
    const MAX_EXACT_F64_INTEGER: u64 = 1 << 53;
    if value > MAX_EXACT_F64_INTEGER {
        return invalid(format!("{label} exceeds exact normalized-number range"));
    }
    Ok(value as f64)
}

fn bounded_count(value: u64, limit: usize, label: &str) -> Result<usize> {
    let count = usize::try_from(value)
        .map_err(|_| Error::InvalidRequest(format!("{label} does not fit usize")))?;
    if count > limit {
        return invalid(format!("{label} exceed bound: {count} > {limit}"));
    }
    Ok(count)
}

fn checked_sum(values: impl IntoIterator<Item = u64>, label: &'static str) -> Result<u64> {
    values.into_iter().try_fold(0u64, |total, value| {
        total
            .checked_add(value)
            .ok_or_else(|| Error::InvalidRequest(format!("{label} overflow")))
    })
}

fn sum_field(values: &[Value], pointer: &str, label: &'static str) -> Result<u64> {
    checked_sum(
        values
            .iter()
            .map(|value| required_u64(value, pointer))
            .collect::<Result<Vec<_>>>()?,
        label,
    )
}

fn count_true(values: &[Value], pointer: &str) -> Result<u64> {
    u64::try_from(
        values
            .iter()
            .map(|value| required_bool(value, pointer))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|value| *value)
            .count(),
    )
    .map_err(|_| Error::InvalidRequest("boolean count overflow".into()))
}

fn required_array<'a>(value: &'a Value, pointer: &str) -> Result<&'a [Value]> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| Error::InvalidRequest(format!("missing or invalid array at {pointer}")))
}

fn required_object<'a>(
    value: &'a Value,
    pointer: &str,
) -> Result<&'a serde_json::Map<String, Value>> {
    value
        .pointer(pointer)
        .and_then(Value::as_object)
        .ok_or_else(|| Error::InvalidRequest(format!("missing or invalid object at {pointer}")))
}

fn required_str<'a>(value: &'a Value, pointer: &str) -> Result<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::InvalidRequest(format!("missing or invalid string at {pointer}")))
}

fn optional_str<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value.pointer(pointer).and_then(Value::as_str)
}

fn required_u64(value: &Value, pointer: &str) -> Result<u64> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::InvalidRequest(format!("missing or invalid integer at {pointer}")))
}

fn optional_u64(value: &Value, pointer: &str) -> Result<Option<u64>> {
    match value.pointer(pointer) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| Error::InvalidRequest(format!("invalid optional integer at {pointer}"))),
    }
}

fn required_bool(value: &Value, pointer: &str) -> Result<bool> {
    value
        .pointer(pointer)
        .and_then(Value::as_bool)
        .ok_or_else(|| Error::InvalidRequest(format!("missing or invalid boolean at {pointer}")))
}

fn verify_u64(value: &Value, pointer: &str, expected: u64) -> Result<()> {
    let actual = required_u64(value, pointer)?;
    if actual != expected {
        return invalid(format!(
            "aggregate mismatch at {pointer}: declared {actual}, recomputed {expected}"
        ));
    }
    Ok(())
}

fn verify_f64(value: &Value, pointer: &str, expected: f64) -> Result<()> {
    let actual = value
        .pointer(pointer)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .ok_or_else(|| Error::InvalidRequest(format!("missing or invalid number at {pointer}")))?;
    let tolerance = f64::EPSILON * 64.0 * expected.abs().max(1.0);
    if (actual - expected).abs() > tolerance {
        return invalid(format!(
            "aggregate mismatch at {pointer}: declared {actual}, recomputed {expected}"
        ));
    }
    Ok(())
}

fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::InvalidRequest(message.into()))
}

fn coverage_name(coverage: MetricCoverage) -> &'static str {
    match coverage {
        MetricCoverage::Complete => "complete",
        MetricCoverage::ReportedSubset => "reported_subset",
        MetricCoverage::Unavailable => "unavailable",
    }
}

fn evidence_name(evidence: FindingEvidence) -> &'static str {
    match evidence {
        FindingEvidence::Exact => "exact",
        FindingEvidence::AggregateExact => "aggregate_exact",
        FindingEvidence::Proxy => "proxy",
    }
}

fn classifier_evidence_name(evidence: ClassifierEvidence) -> &'static str {
    match evidence {
        ClassifierEvidence::Exact => "exact",
        ClassifierEvidence::Proxy => "proxy",
        ClassifierEvidence::Unavailable => "unavailable",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUITE_V1: &[u8] = include_bytes!(
        "../../../benchmarks/reports/multi-agent-context-suite-v1-codex-0.144.1.json"
    );
    const SUITE_V2: &[u8] = include_bytes!(
        "../../../benchmarks/reports/multi-agent-context-suite-v2-codex-0.144.1.json"
    );
    const TRAJECTORY: &[u8] =
        include_bytes!("../../../benchmarks/reports/model-ab-trajectory-v1.json");
    const WIRE: &[u8] = include_bytes!("../../../benchmarks/reports/wire-trace-synthetic-v2.json");
    const HOST: &[u8] =
        include_bytes!("../../../benchmarks/reports/codex-host-receipt-0.144.1.json");

    #[test]
    fn suite_v1_recomputes_negative_episode_signals() {
        let report =
            audit_episode_bytes(EpisodeAdapter::MultiAgentSuiteV1, SUITE_V1).expect("audit");
        assert_eq!(report.summary.episodes, 60);
        let contract = finding(&report, "iterative_retrieval_contract");
        assert_eq!(contract.occurrences, 20);
        assert!((contract.value.expect("mean") - 8.2).abs() < 1e-12);
        let regression = finding(&report, "provider_input_regression");
        assert!((regression.value.expect("regression") - 0.509_299_914_852_593_3).abs() < 1e-12);
        assert_eq!(
            finding(&report, "retrieval_contract_violation").occurrences,
            13
        );
    }

    #[test]
    fn suite_v2_identifies_one_context_contract_without_regression() {
        let report =
            audit_episode_bytes(EpisodeAdapter::MultiAgentSuiteV1, SUITE_V2).expect("audit");
        assert_eq!(report.summary.episodes, 60);
        assert_eq!(
            finding(&report, "one_context_optional_search_contract").occurrences,
            20
        );
        assert!(
            report
                .findings
                .iter()
                .all(|finding| finding.code != "provider_input_regression")
        );
        assert!(
            report
                .findings
                .iter()
                .all(|finding| finding.code != "retrieval_contract_violation")
        );
    }

    #[test]
    fn normalized_json_and_markdown_are_deterministic() {
        let first =
            audit_episode_bytes(EpisodeAdapter::ModelAbTrajectoryV1, TRAJECTORY).expect("first");
        let second =
            audit_episode_bytes(EpisodeAdapter::ModelAbTrajectoryV1, TRAJECTORY).expect("second");
        assert_eq!(
            serde_json::to_vec_pretty(&first).expect("first json"),
            serde_json::to_vec_pretty(&second).expect("second json")
        );
        assert_eq!(first.to_markdown(), second.to_markdown());
        assert!(
            first
                .source
                .artifact_bindings
                .windows(2)
                .all(|pair| pair[0] <= pair[1])
        );
    }

    #[test]
    fn wire_and_host_receipts_share_normalized_shape_without_inventing_usage() {
        let wire = audit_episode_bytes(EpisodeAdapter::McpWireReportV2, WIRE).expect("wire");
        let host = audit_episode_bytes(EpisodeAdapter::CodexHostReceiptV1, HOST).expect("host");
        assert_eq!(wire.schema_version, host.schema_version);
        assert_eq!(wire.report_kind, host.report_kind);
        assert_eq!(wire.summary.provider_input_tokens.value(), None);
        assert_eq!(wire.summary.mcp_source_tokens.value(), Some(21));
        assert_eq!(host.summary.provider_input_tokens.value(), Some(70_904));
        assert_eq!(finding(&wire, "dual_result_duplication").occurrences, 1);
        assert_eq!(host.summary.tool_calls.value(), Some(3));
        assert_eq!(
            classifier(&wire, "dual_result_duplication").evidence,
            ClassifierEvidence::Exact
        );
        assert_eq!(
            classifier(&host, "tool_validation_retry").evidence,
            ClassifierEvidence::Unavailable
        );
    }

    #[test]
    fn missing_or_corrupted_binding_fails_closed() {
        let mut input: Value = serde_json::from_slice(WIRE).expect("wire json");
        input
            .as_object_mut()
            .expect("object")
            .remove("trace_content_blake3");
        let error = audit_episode_bytes(
            EpisodeAdapter::McpWireReportV2,
            &serde_json::to_vec(&input).expect("serialize"),
        )
        .expect_err("missing binding");
        assert!(error.to_string().contains("trace_content_blake3"));

        let mut input: Value = serde_json::from_slice(HOST).expect("host json");
        input["source_rollout_blake3"] = Value::String("not-a-hash".into());
        let error = audit_episode_bytes(
            EpisodeAdapter::CodexHostReceiptV1,
            &serde_json::to_vec(&input).expect("serialize"),
        )
        .expect_err("corrupt binding");
        assert!(error.to_string().contains("BLAKE3 binding"));
    }

    #[test]
    fn adapter_version_mismatch_fails_loudly() {
        let error = audit_episode_bytes(EpisodeAdapter::McpWireReportV2, SUITE_V1)
            .expect_err("schema mismatch");
        assert!(error.to_string().contains("requires input schema 2"));
    }

    #[test]
    fn malformed_json_is_a_caller_error_without_echoing_input() {
        let error = audit_episode_bytes(EpisodeAdapter::MultiAgentSuiteV1, br#"{"private":"#)
            .expect_err("malformed input");
        assert_eq!(error.public_category(), "invalid_request");
        assert!(!error.to_string().contains("private"));
        assert!(error.to_string().contains("line 1"));
    }

    #[test]
    fn aggregate_tampering_and_resource_overflow_fail_closed() {
        let mut suite: Value = serde_json::from_slice(SUITE_V1).expect("suite json");
        suite["arm_summaries"][2]["child_provider_requests"]["sum"] = Value::from(999_u64);
        let error = audit_episode_bytes(
            EpisodeAdapter::MultiAgentSuiteV1,
            &serde_json::to_vec(&suite).expect("serialize"),
        )
        .expect_err("tampered aggregate");
        assert!(error.to_string().contains("aggregate mismatch"));

        for (pointer, value, label) in [
            (
                "/event_count",
                (MAX_EPISODE_EVENTS as u64) + 1,
                "wire events",
            ),
            (
                "/range_identity_count",
                (MAX_EPISODE_RANGES as u64) + 1,
                "evidence ranges",
            ),
        ] {
            let mut wire: Value = serde_json::from_slice(WIRE).expect("wire json");
            *wire.pointer_mut(pointer).expect("bounded field") = Value::from(value);
            let error = audit_episode_bytes(
                EpisodeAdapter::McpWireReportV2,
                &serde_json::to_vec(&wire).expect("serialize"),
            )
            .expect_err("bound violation");
            assert!(error.to_string().contains(label));
        }

        let mut wire: Value = serde_json::from_slice(WIRE).expect("wire json");
        wire["tool_result_modes"]["dual"] = Value::from((MAX_EPISODE_TOOL_CALLS as u64) + 1);
        let error = audit_episode_bytes(
            EpisodeAdapter::McpWireReportV2,
            &serde_json::to_vec(&wire).expect("serialize"),
        )
        .expect_err("tool-call bound");
        assert!(error.to_string().contains("tool calls"));
    }

    #[test]
    fn host_privacy_declaration_blocks_private_material() {
        let mut input: Value = serde_json::from_slice(HOST).expect("host json");
        input["privacy"]["prompts_retained"] = Value::Bool(true);
        let error = audit_episode_bytes(
            EpisodeAdapter::CodexHostReceiptV1,
            &serde_json::to_vec(&input).expect("serialize"),
        )
        .expect_err("private receipt");
        assert!(error.to_string().contains("prompts_retained"));
    }

    #[test]
    fn bounded_reader_stops_after_the_limit() {
        let input = tempfile::NamedTempFile::new().expect("temporary input");
        std::fs::write(input.path(), b"1234").expect("write input");
        let error = read_bounded_with_limit(input.path(), 3).expect_err("byte bound");
        assert!(error.to_string().contains("byte bound of 3"));
    }

    #[test]
    fn context_utilization_preserves_proxy_boundaries() {
        let digest = "a".repeat(64);
        let input = serde_json::json!({
            "schema_version": 1,
            "report_kind": "context_utilization_trajectory",
            "diagnostic_only": true,
            "outcome": "unknown",
            "source": {
                "classifier_source_blake3": digest,
                "tool_trace_blake3": "b".repeat(64),
                "trajectory_blake3": "c".repeat(64),
                "artifact_schema_version": 1,
                "binding": {
                    "experiment_id": "experiment",
                    "manifest_blake3": "d".repeat(64),
                    "task_id": "task",
                    "repetition": 1,
                    "arm": "candidate"
                }
            },
            "bounds": {},
            "summary": {
                "context_calls": 1,
                "successful_context_calls": 1,
                "failed_context_calls": 0,
                "context_ranges": {"ranges": 2, "source_tokens": 20, "source_tokens_complete": true},
                "relevant_path_proxy": {"ranges": 1, "source_tokens": 10, "source_tokens_complete": true},
                "explicit_hash_input_later": {"ranges": 0, "source_tokens": 0, "source_tokens_complete": true},
                "exact_reread_later": {"ranges": 1, "source_tokens": 10, "source_tokens_complete": true},
                "overlap_reread_later": {"ranges": 1, "source_tokens": 10, "source_tokens_complete": true},
                "no_observed_downstream_signal": {"ranges": 1, "source_tokens": 10, "source_tokens_complete": true},
                "receipt_follow_up_calls": 0,
                "follow_up_retrieval_calls": 1
            },
            "ranges": [{}, {}],
            "limitations": []
        });
        let report = audit_episode_bytes(
            EpisodeAdapter::ContextUtilizationV1,
            &serde_json::to_vec(&input).expect("serialize"),
        )
        .expect("audit");
        assert_eq!(report.summary.successful_episodes.value(), None);
        assert_eq!(report.summary.downstream_signal_ranges.value(), Some(1));
        assert_eq!(
            finding(&report, "no_observed_downstream_signal").evidence,
            FindingEvidence::Proxy
        );
    }

    fn finding<'a>(report: &'a EpisodeAuditReport, code: &str) -> &'a EpisodeFinding {
        report
            .findings
            .iter()
            .find(|finding| finding.code == code)
            .unwrap_or_else(|| panic!("missing finding {code}"))
    }

    fn classifier<'a>(report: &'a EpisodeAuditReport, code: &str) -> &'a ClassifierCoverage {
        report
            .classifier_coverage
            .iter()
            .find(|classifier| classifier.code == code)
            .unwrap_or_else(|| panic!("missing classifier {code}"))
    }
}
