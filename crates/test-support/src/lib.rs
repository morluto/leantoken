//! Small, capability-oriented primitives for LeanToken tests.
//!
//! The support crate deliberately does not depend on the product crate. This
//! keeps test setup independent from product behavior and makes the dependency
//! direction machine-checkable.

mod sandbox;

pub use sandbox::{Sandbox, SandboxError};
