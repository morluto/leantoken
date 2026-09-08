//! Small, capability-oriented primitives for LeanToken tests.
//!
//! The support crate deliberately does not depend on the product crate. This
//! keeps test setup independent from product behavior and makes the dependency
//! direction machine-checkable.

mod git;
mod sandbox;

pub use git::GitFixture;
pub use sandbox::{Sandbox, SandboxError};
