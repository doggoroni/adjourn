//! freenet-chess `common` crate: the state algebra.
//!
//! No Freenet dependencies — the consistency model is testable standalone.

pub mod delegate_api;
pub mod delegate_policy;
pub mod project;
pub mod state;
pub mod types;

pub use project::{legal_moves, make_move, project, Decision, Reason, Status};
pub use state::{Delta, GameState, SigDigest, Summary};
pub use types::{color_at_ply, Body, GameParams, KeyBytes, Record, RecordId};
