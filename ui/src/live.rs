//! Talking to a real node.
//!
//! This is the first code in the repo that exercises [`BrowserClient`] against
//! anything. Its two Critical defects — every node-reported error discarded,
//! and a socket death being undetectable — were both found by reading the
//! source, not by running it, and it still has no automated test.
//!
//! Split out of `main.rs` because it is wasm-only: `BrowserClient` is gated
//! `cfg(target_arch = "wasm32")`, since `freenet-stdlib`'s browser `WebApi`
//! exists only for `target_family = "wasm"`. The native stub keeps
//! `cargo check --all-targets` compiling, which is what lets the crate's
//! tests run at all.

/// What a connection attempt produced, rendered as plain lines.
///
/// Deliberately a `Result<Vec<String>, String>` rather than something
/// structured: the point of the bring-up is to see what the node actually says,
/// including the failures, not to model it.
pub type Probe = Result<Vec<String>, String>;

#[cfg(target_arch = "wasm32")]
pub async fn probe(url: &str) -> Probe {
    use adjourn_client::node::{delegate_container, NodeClient};
    use adjourn_core::delegate_api::{Request, Response};

    let mut out = Vec::new();

    let mut client = crate::node::BrowserClient::connect(url)
        .await
        .map_err(|e| format!("connect: {e:#}"))?;
    out.push(format!("connected to {url}"));

    // The delegate key is a pure function of its code, so registering is
    // idempotent: the browser equivalent of `adjourn init`.
    let (container, key) = delegate_container(crate::DELEGATE_WASM.to_vec());
    out.push(format!("delegate key {key}"));

    client
        .register_delegate(container)
        .await
        .map_err(|e| format!("register_delegate: {e:#}"))?;
    out.push("delegate registered".into());

    // A round trip through the delegate. An empty list is the expected answer
    // for a fresh browser origin -- the node does not populate `MessageOrigin`
    // for a CLI caller, so CLI-created games are invisible here by design.
    match client
        .delegate(Request::ListGames)
        .await
        .map_err(|e| format!("ListGames: {e:#}"))?
    {
        Response::Games(games) => {
            out.push(format!("ListGames returned {} game(s)", games.len()));
            for g in games {
                out.push(format!(
                    "  {} · {:?} · last signed ply {}",
                    g.label, g.side, g.last_signed_ply
                ));
            }
        }
        other => out.push(format!("unexpected reply: {other:?}")),
    }

    Ok(out)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn probe(_url: &str) -> Probe {
    Err("the browser transport is wasm32-only".into())
}
