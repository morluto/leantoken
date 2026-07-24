# Evidence-boundary fixture

This fixture freezes a real packaged-runtime failure from
`morluto/rea#432`. It is intentionally split into candidate-visible evidence
and a hidden oracle.

Candidate-visible inputs:

- `task.json`
- `failure.json`
- a source checkout of `morluto/rea` at revision
  `9c3aec81317cd6caf627f13ebb0c6dc0f6735516`

The evaluator must disable network access and must not expose later revisions,
the pull-request discussion, successful runs, or the hidden oracle. Otherwise
the task becomes a history lookup rather than an evidence-boundary test.

The hidden oracle records the later source change and successful macOS package
run. It belongs in evaluator-private storage, not this repository. Its claims
are used only for scoring diagnosis quality and final validation choices.

The fixture's failure strings are short excerpts from GitHub Actions job
`89393398416` in run `30064737090`. They are included because the candidate
must distinguish observed packaged-runtime behavior from what source
inspection alone can prove.
