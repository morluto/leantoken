# Version-aware critique fixture seed

This directory contains source-free public inputs for the tool-critique
evaluation. It deliberately separates npm publication, GitHub release, source
revision, and observed runtime behavior.

The `0.1.12` release exists on GitHub with native binaries and a bundled npm
tarball, but the `leantoken@0.1.12` package is not present in the npm registry.
That distinction matters: a candidate must not infer npm availability from a
GitHub tag or infer current behavior from an older npm-installed runtime.

Runtime replay records, randomized variants, and the oracle remain private.
Candidate runs must have network disabled and receive only the artifacts named
by their condition.
