//! Tests that need a real browser.
//!
//! Run with a node listening on 7509:
//!   wasm-pack test --headless --firefox ui
//! These are NOT part of `cargo test --workspace`; they need a browser and a
//! node, and CI has neither.

#![cfg(target_arch = "wasm32")]

use adjourn_ui::node::BrowserClient;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

const NODE_URL: &str = "ws://127.0.0.1:7509/v1/contract/command?encodingProtocol=native";
const DEAD_URL: &str = "ws://127.0.0.1:7599/v1/contract/command?encodingProtocol=native";

/// The failure that presented as an endless spinner. It must resolve.
#[wasm_bindgen_test]
async fn connecting_to_a_dead_port_fails_rather_than_hanging() {
    let result = BrowserClient::connect(DEAD_URL).await;
    assert!(
        result.is_err(),
        "a refused connection must surface as an error, not hang"
    );
}

/// The success path, end to end against a live node.
#[wasm_bindgen_test]
async fn a_live_node_registers_the_delegate_and_lists_games() {
    use adjourn_client::node::delegate_container;
    use adjourn_client::node::NodeClient;
    use adjourn_core::delegate_api::{Request, Response};

    let mut client = BrowserClient::connect(NODE_URL)
        .await
        .expect("a node must be listening on 7509 for this test");
    let (container, _key) = delegate_container(adjourn_ui::DELEGATE_WASM.to_vec());
    client
        .register_delegate(container)
        .await
        .expect("register_delegate");
    match client
        .delegate(Request::ListGames)
        .await
        .expect("ListGames")
    {
        Response::Games(_) => {}
        other => panic!("unexpected reply: {other:?}"),
    }
}
