# Host wire compatibility matrix

Date: 2026-07-20

Experiment: `host-wire-compatibility-v1`

Machine-readable matrix:
[`host-wire-compatibility-v1.json`](host-wire-compatibility-v1.json)

Validator: `cargo run --release --example host_wire_compatibility -- --matrix
benchmarks/reports/host-wire-compatibility-v1.json --repository-root .`

## Scope and evidence rule

The audit covers the requested Codex CLI, Claude Code, Cursor, Gemini CLI, and
OpenCode host families. It classifies three result modes separately for every
host observation: `dual`, `text`, and `structured`. Four evidence boundaries
remain distinct:

1. complete JSON-RPC transport;
2. a later model response after the result;
3. provider-native cumulative token accounting;
4. the provider-visible serialized request frame.

A lower boundary does not imply a higher one. In particular, local JSON-RPC
duplication is not a provider-input savings claim. Synthetic fixtures and SDK
integration tests are excluded from real-host compatibility classifications.

The validator checks each source artifact's BLAKE3 identity, complete lifecycle
categories, mode counts, local token fields, host/result correlations, task
success, provider totals, and privacy assertions. It also rejects missing modes,
unknown evidence references, and any zero substituted for an unavailable
measurement.

## Availability

| Host | Version | Audit access | Consequence |
| --- | --- | --- | --- |
| Codex CLI | 0.144.1 | executable available; sanitized frozen evidence available | dual lifecycle and structured task consumption can be classified |
| Codex CLI | 0.144.5 | historical sanitized wire capture only | dual transport can be classified |
| Claude Code | unknown | executable unavailable | all result-mode fields remain unknown and null |
| Cursor | unknown | executable unavailable | all result-mode fields remain unknown and null |
| Gemini CLI | unknown | executable unavailable | all result-mode fields remain unknown and null |
| OpenCode | unknown | executable unavailable | all result-mode fields remain unknown and null |

Unavailable means only that the executable was not installed in this audit
environment. It is not an incompatibility result.

## Compatibility result

| Host/version | Mode | Complete wire | Model consumption | Provider usage | Provider frame |
| --- | --- | --- | --- | --- | --- |
| Codex CLI 0.144.1 | `dual` | proven | proven | partial | unknown |
| Codex CLI 0.144.1 | `structured` | tool results only | proven for one frozen task | partial | unknown |
| Codex CLI 0.144.1 | `text` | unknown | unknown | unknown | unknown |
| Codex CLI 0.144.5 | `dual` | proven | unknown | unknown | unknown |
| Codex CLI 0.144.5 | `text` | unknown | unknown | unknown | unknown |
| Codex CLI 0.144.5 | `structured` | unknown | unknown | unknown | unknown |
| Other requested hosts | all | unknown | unknown | unknown | unknown |

Codex CLI 0.144.1 dual transport contains 4,483 exact local JSON-RPC tokens.
Three results contain 854 text tokens and 776 structured-content tokens; all
three duplicate their structured payload in text, accounting for 776 local
tokens. The matching host receipt correlates all three MCP results with later
model responses and later provider-usage events. Its final cumulative usage is
70,904 input tokens: 7,672 uncached and 63,232 cache-read. Cache-creation input
is unavailable. The provider request body was not exported.

The separate Codex CLI 0.144.1 structured-only owner task returned all four
expected exact path/symbol labels. Its child consumed six successful MCP
results containing 37,821 structured-content bytes, zero text-content bytes,
and 5,265 emitted source tokens. Child cumulative provider usage is 83,923
input tokens: 17,619 uncached and 66,304 cache-read. This proves model
consumption for that task and version, but the receipt does not contain a full
wire lifecycle or provider request frame.

Codex CLI 0.144.5 has a complete nine-event dual wire with 2,896 exact local
tokens and two tool results. It has no matching model-lifecycle or provider
receipt, so those fields remain unknown.

## Decision

Keep `dual` as the global default. The matrix contains one scoped
structured-only success, no text-only real-host success, and no evidence for
four requested host families. It therefore cannot support a cross-host default
change or provider-savings claim.

A future update must add sanitized evidence from an actually available host and
version. Unknown fields remain null until then; they must never be inferred from
fixture serialization or entered as zero.
