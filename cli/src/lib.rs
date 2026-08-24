/// Test-facing only: runs the real contract and delegate code in memory so
/// integration tests can exercise `session.rs` without a live Freenet node.
/// Not part of the CLI's public surface.
#[doc(hidden)]
pub mod fake;
pub mod invite;
pub mod node;
pub mod session;
