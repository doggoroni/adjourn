mod common;

use adjourn_client::fake::{shared_world, FakeNode};
use adjourn_client::node::NodeClient;
use adjourn_client::session::{game_bind, invite_accept, invite_new, play_move, show_label};
use adjourn_core::delegate_api::Side;
use adjourn_core::GameState;
use freenet_stdlib::prelude::{ContractInstanceId, UpdateData};

/// Alice moves; Bob learns about it from a notification rather than a GET.
#[tokio::test]
async fn a_move_reaches_the_other_peer_as_an_update() {
    let Some(wasm) = common::contract_wasm() else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };
    let world = shared_world();
    let (mut alice, mut bob) = (FakeNode::new(world.clone()), FakeNode::new(world));
    let invite = invite_new(&mut alice, "alice", Side::White).await.unwrap();
    let offer = invite_accept(&mut bob, "bob", &invite, wasm.clone())
        .await
        .unwrap();
    game_bind(&mut alice, "alice", &offer, wasm.clone())
        .await
        .unwrap();

    let before = show_label(&mut bob, "bob", wasm.clone()).await.unwrap();
    assert_eq!(before.ply, 0);

    // Subscribing is what makes updates arrive at all; `show_label` does not.
    let contract = contract_bytes_of(&mut bob, "bob").await;
    bob.get(ContractInstanceId::new(contract), true)
        .await
        .unwrap();

    // The setup PUTs the contract, so the log is not empty. Drain it, so what
    // follows observes only what the test itself causes.
    while bob.next_update().await.unwrap().is_some() {}
    assert!(
        bob.next_update().await.unwrap().is_none(),
        "quiescent before alice moves"
    );

    play_move(&mut alice, "alice", "e2e4", wasm.clone())
        .await
        .unwrap();

    let (id, update) = bob
        .next_update()
        .await
        .expect("next_update failed")
        .expect("alice's move must produce a notification");
    assert_eq!(
        *id,
        contract_bytes_of(&mut bob, "bob").await,
        "names our game"
    );

    // Checking the id proves a notification exists; it does not prove the
    // notification carried the move. Decode and project it.
    let UpdateData::State(bytes) = update else {
        panic!("FakeNode emits State payloads");
    };
    let carried = GameState::decode(bytes.as_ref()).expect("payload decodes");
    let status = adjourn_core::project(&carried, &params_of(&mut bob, "bob").await);
    assert_eq!(status.ply, 1, "the notification carries alice's move");

    let after = show_label(&mut bob, "bob", wasm).await.unwrap();
    assert_eq!(after.ply, 1, "bob sees the move");
}

/// The contract id the delegate recorded for `label` at bind time.
///
/// Compared as raw bytes rather than as a `ContractInstanceId`, because that is
/// what `GameSummary.contract` stores and what the id derefs to.
async fn contract_bytes_of(node: &mut FakeNode, label: &str) -> [u8; 32] {
    use adjourn_core::delegate_api::{Request, Response};
    let Response::Games(games) = node.delegate(Request::ListGames).await.unwrap() else {
        panic!("expected a games list");
    };
    games
        .into_iter()
        .find(|g| g.label == label)
        .expect("label is bound")
        .contract
        .expect("a bound game carries its contract id")
}

/// The `GameParams` the delegate recorded for `label` at bind time.
async fn params_of(node: &mut FakeNode, label: &str) -> adjourn_core::GameParams {
    use adjourn_core::delegate_api::{Request, Response};
    let Response::Games(games) = node.delegate(Request::ListGames).await.unwrap() else {
        panic!("expected a games list");
    };
    games
        .into_iter()
        .find(|g| g.label == label)
        .expect("label is bound")
        .params
        .expect("a bound game carries its params")
}

/// With nothing happening, `next_update` must not invent one.
#[tokio::test]
async fn next_update_is_empty_when_nothing_changed() {
    let Some(wasm) = common::contract_wasm() else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };
    let world = shared_world();
    let (mut alice, mut bob) = (FakeNode::new(world.clone()), FakeNode::new(world));
    let invite = invite_new(&mut alice, "alice", Side::White).await.unwrap();
    let offer = invite_accept(&mut bob, "bob", &invite, wasm.clone())
        .await
        .unwrap();
    game_bind(&mut alice, "alice", &offer, wasm.clone())
        .await
        .unwrap();
    let _ = show_label(&mut bob, "bob", wasm).await.unwrap();

    // The setup PUTs the contract, so the log is not empty. Drain it, so what
    // follows observes only what the test itself causes.
    while bob.next_update().await.unwrap().is_some() {}

    assert!(
        bob.next_update().await.unwrap().is_none(),
        "no write happened, so there is nothing to report"
    );
}
