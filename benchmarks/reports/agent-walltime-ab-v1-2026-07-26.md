# Agent wall-time A/B

- Status: `passed_accuracy_gates`
- Source: `5e2b31da5d4be52c900e6c291d09544a19b5d20b`
- Host: `linux/x86_64`
- Iterations: 30 exact, 10 context

Exact search and read are parity-gated. Discovery and context have different
semantics and their timing ratio is diagnostic only.

| Corpus | Cold index | Exact search rg | Exact search MCP | Exact read native | Exact read MCP | Discovery rg | Context MCP |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| flask-validation | 858.1 ms | 12.84 ms | 25.85 ms | 2.41 ms | 5.87 ms | 51.53 ms | 92.42 ms |
| gin-validation | 655.1 ms | 7.97 ms | 45.04 ms | 2.35 ms | 5.00 ms | 31.19 ms | 94.42 ms |
| express-validation | 532.1 ms | 12.57 ms | 45.02 ms | 2.53 ms | 6.02 ms | 50.92 ms | 77.29 ms |
| tokio-validation | 3521.4 ms | 27.49 ms | 47.98 ms | 2.54 ms | 6.76 ms | 115.79 ms | 198.81 ms |

## Accuracy

- Exhaustive exact-search coordinate parity: pass
- Exact line-read parity: pass
- Warm context determinism and token budgets: pass
- Native discovery relevant-file recall: 11/11 (100.0%)
- Context relevant-file recall: 7/11 (63.6%)
- Context line-anchor recall: 19/38 (50.0%)

## Suite Diagnostic

These are sums of the four corpus medians, not pooled latency samples.

| Operation | Native | LeanToken | Absolute delta | Relative delta |
| --- | ---: | ---: | ---: | ---: |
| Exhaustive exact search | 60.86 ms | 163.87 ms | +103.01 ms | +169.3% |
| Exact read | 9.83 ms | 23.65 ms | +13.82 ms | +140.6% |
| Discovery / context diagnostic | 249.43 ms | 462.94 ms | +213.51 ms | +85.6% |

## Limits

- This is a local retrieval microbenchmark, not an end-to-end agent task-time result.
- Exact search and exact read are observable-parity comparisons; multi-query ripgrep discovery and ranked context are not semantically equivalent.
- Wall time depends on this host, filesystem cache, process scheduler, and pinned corpus sizes.
- Cold index cost is paid once per repository generation and must be amortized over a real session.
- CPU time, peak RSS, provider latency, model turns, patch quality, and task success are outside this report.
- The prospective validation tasks are consumed development evidence rather than a blind holdout.
