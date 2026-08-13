# Measurement policy

LeanToken keeps machine-readable benchmark inputs and reports in `benchmarks/`.
This page defines how to interpret them; it is not an experiment journal.

Use release builds, pinned repository revisions, identical manifests, and the
same tokenizer for comparisons. Record the command, platform, LeanToken commit,
input digests, configuration, sample count, and raw output. Write exploratory
results under `target/`. Commit a report only when it is a durable regression
fixture or reviewed promotion receipt used by code or tests.

Retrieval changes are evaluated in this order:

1. correctness and task-relevant retrieval quality;
2. exact source and complete-response token cost;
3. avoided downstream reads and tool calls;
4. warm latency, cold indexing, peak memory, database size, and write cost.

Do not promote a faster candidate that weakens snapshot consistency,
determinism, boundedness, cancellation, path safety, or false-negative
behavior. A development-set improvement is not evidence of model task success.
Paired agent results must use the same tasks and report failures, retries, and
provider input rather than substituting retrieval-only proxies.

Machine-readable historical results preserve what was measured. Git history
preserves the accompanying dated narrative. Current commands and promotion
rules live in [the benchmark guide](../benchmarks/README.md).
