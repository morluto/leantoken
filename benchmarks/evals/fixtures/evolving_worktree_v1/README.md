# Evolving-worktree fixture seed

This directory freezes the public, non-sensitive part of the Jacobian M3/M4
scenario from `morluto/jacobian#13`.

`scenario.json` records the source checkpoints, task boundary, observed
LeanToken call aggregate, and authoritative validation commands. The original
Codex rollout is private and is not a fixture input.

To finish the fixture, run `sanitize_codex_trajectory` against the private
rollout, verify that its aggregate matches `expected_trace_summary`, and store
the resulting sanitized JSONL plus its manifest in evaluator-controlled
storage. The candidate must receive only the sanitized records and frozen
source checkpoints. Prompts, unrelated commands, local absolute paths, raw
tool responses, issue conclusions, and hidden oracle labels must remain
private.

The public pull request is provenance for the scenario, not candidate evidence.
Disable network access during evaluation so later commits, the pull-request
body, and this benchmark issue cannot leak the answer.
