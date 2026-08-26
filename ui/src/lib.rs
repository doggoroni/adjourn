//! The adjourn web UI.
//!
//! Split lib/bin on purpose: everything with logic lives in the library so it
//! can be tested natively, and the binary is only the Dioxus entry point. The
//! board in particular is a pure function of a projected `Status`, so square
//! colours, orientation and legal-target marking are all testable with no
//! browser and no framework.

pub mod board;
pub mod node;

/// The compiled contract, embedded because a browser cannot read it off disk.
///
/// This pins the contract key into the bundle: rebuilding the contract means
/// rebuilding the UI. See `ui/build.rs` for the guard that says so when the
/// artifact is missing.
pub const CONTRACT_WASM: &[u8] =
    include_bytes!("../../target/wasm32-unknown-unknown/release/adjourn_contract.wasm");

/// The compiled delegate, embedded for the same reason. The UI registers it on
/// first run -- the browser's equivalent of `adjourn init`.
pub const DELEGATE_WASM: &[u8] =
    include_bytes!("../../target/wasm32-unknown-unknown/release/adjourn_delegate.wasm");
