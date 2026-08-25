//! The game flows, independent of transport.
//!
//! Everything here is generic over [`node::NodeClient`], so the same code runs
//! against a real node over a WebSocket, against a browser's WebSocket, or
//! against [`fake::FakeNode`] in a test. That is not code reuse for its own
//! sake: both players must derive byte-identical `GameParams`, or they land on
//! different contract ids and each sees a game the other never joins, with no
//! error anywhere.

/// Test-facing only: runs the real contract and delegate code in memory so
/// integration tests can exercise the flows without a live Freenet node.
#[cfg(feature = "fake")]
#[doc(hidden)]
pub mod fake;
pub mod invite;
pub mod node;
pub mod session;
