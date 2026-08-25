mod common;

use adjourn_client::fake::{shared_world, FakeNode};
use adjourn_client::node::NodeClient;
use adjourn_client::session::{game_bind, invite_accept, invite_new, play_move, show_label};
use adjourn_core::delegate_api::Side;

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

    // Bob reads the game once, so he is subscribed and holds a baseline.
    let before = show_label(&mut bob, "bob", wasm.clone()).await.unwrap();
    assert_eq!(before.ply, 0);

    play_move(&mut alice, "alice", "e2e4", wasm.clone())
        .await
        .unwrap();

    let update = bob
        .next_update()
        .await
        .expect("next_update failed")
        .expect("expected an update after the opponent moved");
    assert_eq!(
        *update.0,
        contract_bytes_of(&mut bob, "bob").await,
        "the update names our game"
    );

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

    assert!(
        bob.next_update().await.unwrap().is_none(),
        "no write happened, so there is nothing to report"
    );
}
