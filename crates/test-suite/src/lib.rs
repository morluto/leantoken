//! Private integration-suite ownership map.
//!
//! The existing root integration executable remains the product package's
//! process-test seam. Cross-component tests belong in this package; the
//! package boundary prevents support code from leaking into production.

#[cfg(test)]
mod domains;
