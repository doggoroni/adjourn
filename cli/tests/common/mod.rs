use std::path::PathBuf;

/// The contract WASM the CLI derives ids from. Produced by
/// `./scripts/build-contract.sh`.
///
/// Missing WASM means "skip" locally, but a hard failure in CI: a skipped
/// test reports `ok`, so silently skipping in CI is indistinguishable from
/// passing.
pub fn contract_wasm() -> Option<Vec<u8>> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target/wasm32-unknown-unknown/release/adjourn_contract.wasm");
    match std::fs::read(&p) {
        Ok(bytes) => Some(bytes),
        Err(_) if std::env::var_os("CI").is_some() => {
            panic!("contract WASM missing at {p:?}; CI must build it before running tests")
        }
        Err(_) => None,
    }
}
