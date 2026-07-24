# Agent evaluation fixtures

This directory owns frozen model-in-the-loop evaluation specifications and
publishable, source-free trajectory fixtures. Correctness gates efficiency:
token, call, reread, and latency measurements are compared only among runs that
meet each evaluation's semantic threshold.

The fixtures separate four evidence classes:

- candidate-visible task inputs and tool contracts;
- private oracle evidence and randomized holdouts;
- publishable trajectory receipts containing hashes, paths, ranges, outcomes,
  and validation classifications but no source, prompts, queries, commands, or
  session identifiers;
- reviewer handoff bundles containing only evidence declared by the candidate.

`sanitize_codex_trajectory` creates the publishable trajectory receipt for the
evolving-worktree and version-aware critique evaluations:

```bash
cargo run --release --example sanitize_codex_trajectory -- \
  --input PRIVATE_ROLLOUT.jsonl \
  --output target/evolving-worktree-trajectory-v1.json \
  --cutoff 2026-07-24T04:00:00Z \
  --repository morluto/jacobian \
  --base-revision ecb1e56831d1842643acfeb52e94172ff2141a91 \
  --checkpoint implementation=2d2a990e9e04b4440dbb4db5dd143bc91580c8b3 \
  --checkpoint documentation=fc51e5d75742dd8d36233190b9cf57cabfb9446e
```

The cutoff is part of the frozen control. It excludes the later audit that
inspected agent-evaluation documentation, leaving the 361-call implementation
trajectory described by issue #201. The sanitizer reads the private rollout
once, hashes the exact input, refuses to overwrite output, and never copies its
path or raw content into the receipt.

## Evaluation ownership

- `#198` evaluates whether agents distinguish repository-source evidence from
  generated artifacts, subprocesses, runtime behavior, and remote CI before
  any response metadata proposed by `#196` is accepted.
- `#201` adds the long evolving-worktree trajectory. Public Jacobian revisions
  freeze the base, implementation, and documentation phases; the private raw
  session remains outside version control.
- `#202` reuses the sanitized trajectory but changes the learning objective to
  version-aware tool/skill critique. Candidate conditions must not receive the
  prior audit conclusion or current-defect labels.

No product or schema change follows from one observed trajectory. Each runnable
evaluation still needs a private versioned oracle, randomized holdout variants,
known-good and known-bad calibration runs, repeated trials, and a reviewer-only
handoff condition.
