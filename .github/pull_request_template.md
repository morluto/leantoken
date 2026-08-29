<!--
PR title: type(optional-scope): imperative outcome
Use a Conventional Commit title; link the issue with "Fixes #123" when applicable.
-->

## Summary

<!-- What user, agent, or maintainer problem does this solve? Keep prior
behavior, expected contract, and new behavior understandable without private context. -->

## Problem and expected behavior

<!-- For fixes, state the smallest trigger and violated invariant. For features,
state the use case and observable outcome. -->

## Change and scope

<!-- Explain the approach, why it fits LeanToken's bounded retrieval architecture,
and what is intentionally not included. -->

## Evidence and regression coverage

<!--
Describe the retrieval or protocol behavior being proved. Distinguish observed
output, derived accounting, and inference. For performance claims, include the
workload, baseline/candidate, metric, units, platform, and method.
-->

- Tests or fixtures added/updated:
- Retrieval correctness, coverage, or receipt evidence:
- Token/resource budget evidence:
- User-visible CLI/MCP output (if applicable):
- Remaining proof gaps:

## Contract and boundary impact

<!-- Complete the applicable lines; use "none" or "not applicable" explicitly. -->

- Semantic owner and earliest changed stage:
- MCP schema, resource, or serialized result contract:
- CLI or setup/runtime/upgrade contract:
- Repository admission, isolation, cache, revision, or storage contract:
- Token accounting, retrieval completeness, or receipt/hash semantics:
- Concurrency, process, security, privacy, or containment impact:

## Validation performed

<!-- List only commands that actually ran, with observed results. Include focused
commands and relevant product/contract lanes. -->

- `command` — result

## Retrieval promotion

<!-- Complete when candidate generation, ranking, allocation, context defaults,
or token accounting changes. A benchmark receipt is evidence, not a substitute
for correctness and contract tests. -->

- Retrieval behavior: unchanged / changed
- Promotion receipt or measurement artifact:
- Correctness and completeness gate:

## Compatibility and safety

<!-- Call out breaking changes, migration steps, platform/client effects, and
intentionally unchanged behavior. -->

- Breaking changes or migration steps:
- Supported platform/client changes:
- Runtime, process, repository, or security impact:
- Documentation or generated-output impact:

## Review checklist

- [ ] The PR has one focused outcome and the title follows `type(scope): outcome`.
- [ ] Related issue is linked, or the reason for not linking one is stated above.
- [ ] Tests cover changed observable behavior and meaningful failure paths.
- [ ] MCP schema changes include the required snapshot/contract evidence.
- [ ] Retrieval changes preserve truthful coverage, bounds, and token accounting.
- [ ] Local repository containment and no-unapproved-execution boundaries remain intact.
- [ ] I checked the final diff for secrets, unrelated cleanup, and unsupported claims.
