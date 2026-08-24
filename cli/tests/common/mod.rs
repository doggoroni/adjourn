use std::path::PathBuf;

/// The contract WASM the CLI derives ids from. Produced by
/// `./scripts/build-contract.sh`; tests that need it skip loudly rather than
/// failing obscurely when it is absent.
pub fn contract_wasm() -> Option<Vec<u8>> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target/wasm32-unknown-unknown/release/adjourn_contract.wasm");
    std::fs::read(p).ok()
}
