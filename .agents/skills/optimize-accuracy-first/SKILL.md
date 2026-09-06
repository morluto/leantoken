---
name: optimize-accuracy-first
description: Investigate performance bottlenecks and validate optimizations without weakening retrieval quality or correctness.
---

# Optimize Accuracy First

Use this skill for a performance investigation or an optimization whose benefit
needs measurement. Ordinary bug fixes do not need a performance study.

Locate the owner of the work and state the expected improvement before changing
it. Preserve the applicable contracts: deterministic results, exact token
budgets, Unicode matching, truthful coverage, bounded work, atomic generations,
and snapshot consistency. An intentional contract change should be described
and tested as such.

Choose evidence that answers the question. Behavioral regressions and
differential checks establish correctness; candidate counts can explain work;
representative release measurements support latency, CPU, memory, or storage
claims. A smaller local timer does not justify worse evidence or more retries.
Do not invent agent-success or provider-cost results from retrieval proxies.

Prefer the smallest ownership change supported by the evidence. Remove repeated
work or reuse an existing indexed primitive before adding caching, concurrency,
or a new abstraction. Keep a simpler design when a proposed optimization has
no demonstrated benefit.

## Measurement references

Consult only the material relevant to the investigation:

- [Measurement matrix](references/measurement-matrix.md): workload and metric
  choices for search, context, reads, indexing, or caching experiments.
- [Repository measurement tools](../../../docs/measurement.md): existing
  profilers and retrieval datasets.
- [Benchmark commands](../../../benchmarks/README.md): frozen comparisons and
  optional paired-agent research.
- [Development checks](../../../docs/development.md): test ownership and CI.

Report the observed result and its limits. Run checks appropriate to the change;
broaden them when a failure, risk, or unresolved question warrants it. Missing
optional research data limits performance claims, not permission to finish a
validated correctness fix.
