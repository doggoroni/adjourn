//! Locate the WASM modules the bundle embeds, and fail loudly if they are absent.
//!
//! `include_bytes!` on a missing path reports only the path, which sends the
//! reader looking for a file that is *supposed* to be absent from a clean
//! checkout. The modules are build artifacts, deliberately not committed.
//!
//! The paths are resolved HERE and handed to `src/lib.rs` through
//! `cargo:rustc-env`, rather than hardcoded at both ends. A literal
//! `../../target/...` in `lib.rs` is wrong for anyone with `CARGO_TARGET_DIR`
//! set: `scripts/build-contract.sh` would put the artifact where cargo was
//! told to, and the build would then report it missing from a directory that
//! is not being used.

use std::path::PathBuf;

fn main() {
    // Everything is resolved absolutely, off `CARGO_MANIFEST_DIR` (`ui/`).
    // `include_bytes!` resolves a relative path against the including FILE
    // rather than the package root, so an absolute path is the only one both
    // ends can agree on -- and `std::fs::canonicalize` is avoided on purpose,
    // since on Windows it returns a `\\?\` verbatim path.
    let workspace = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets this"))
        .parent()
        .expect("the ui package is a workspace member, so it has a parent")
        .to_path_buf();
    // `CARGO_TARGET_DIR` may be relative -- to the workspace root -- or absolute.
    let target_dir = match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => {
            let dir = PathBuf::from(dir);
            if dir.is_absolute() {
                dir
            } else {
                workspace.join(dir)
            }
        }
        None => workspace.join("target"),
    };
    println!("cargo:rerun-if-env-changed=CARGO_TARGET_DIR");

    for (what, artifact, var) in [
        ("contract", "adjourn_contract.wasm", "ADJOURN_CONTRACT_WASM"),
        ("delegate", "adjourn_delegate.wasm", "ADJOURN_DELEGATE_WASM"),
    ] {
        let path = target_dir
            .join("wasm32-unknown-unknown")
            .join("release")
            .join(artifact);
        let display = path.display();
        println!("cargo:rerun-if-changed={display}");
        if !path.exists() {
            panic!(
                "the {what} WASM is missing at {display}.\n\
                 The UI bundle embeds it, so build it first:\n\
                 \n    ./scripts/build-{what}.sh\n\n\
                 Do NOT use a bare `cargo build --release` -- it embeds \
                 home-directory paths and produces a different, unshippable key."
            );
        }
        println!("cargo:rustc-env={var}={display}");
    }
}
