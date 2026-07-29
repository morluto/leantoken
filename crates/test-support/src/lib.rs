//! Small, capability-oriented primitives for LeanToken tests.
//!
//! The support crate deliberately does not depend on the product crate. This
//! keeps test setup independent from product behavior and makes the dependency
//! direction machine-checkable.

mod deadline;
mod fixtures;
mod git;
mod normalize;
mod process;
mod repo;
mod sandbox;

pub use deadline::{Deadline, DeadlineError};
pub use fixtures::{FixtureCase, FixtureError};
pub use git::{GitError, GitRepository};
pub use normalize::Normalizer;
pub use process::{ProcessError, ProcessHarness, ProcessOutput};
pub use repo::{RepoBuilder, RepoError};
pub use sandbox::{Sandbox, SandboxError};
