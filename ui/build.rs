//! Fail loudly if the WASM modules the bundle embeds have not been built.
//!
//! `include_bytes!` on a missing path reports only the path, which sends the
//! reader looking for a file that is *supposed* to be absent from a clean
//! checkout. The modules are build artifacts, deliberately not committed.

fn main() {
    for (what, path) in [
        (
            "contract",
            "../target/wasm32-unknown-unknown/release/adjourn_contract.wasm",
        ),
        (
            "delegate",
            "../target/wasm32-unknown-unknown/release/adjourn_delegate.wasm",
        ),
    ] {
        println!("cargo:rerun-if-changed={path}");
        if !std::path::Path::new(path).exists() {
            panic!(
                "the {what} WASM is missing at {path}.\n\
                 The UI bundle embeds it, so build it first:\n\
                 \n    ./scripts/build-{what}.sh\n\n\
                 Do NOT use a bare `cargo build --release` -- it embeds \
                 home-directory paths and produces a different, unshippable key."
            );
        }
    }
}
