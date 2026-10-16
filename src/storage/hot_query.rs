//! Private ownership seam for SQL whose query plan is a reviewed contract.
//!
//! This is a declaration, not an execution abstraction: callers continue using
//! rusqlite directly, and production and diagnostics prepare the same `sql`.

pub(super) struct HotQuery {
    pub(super) sql: &'static str,
    #[cfg(test)]
    pub(super) contract: QueryContract,
}

#[cfg(test)]
pub(super) struct QueryContract {
    pub(super) operation: &'static str,
    pub(super) parameters: &'static [&'static str],
    pub(super) result: &'static str,
    pub(super) indexes: &'static [&'static str],
}
