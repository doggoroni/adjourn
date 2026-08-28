mod common;

use adjourn_client::fake::{shared_world, FakeNode};
use adjourn_client::session::{
    game_bind, invite_accept, invite_new, moves_in_order, open_game_view, play_move,
};
use adjourn_core::delegate_api::Side;

#[tokio::test]
async fn the_view_carries_the_moves_in_order() {
    let Some(wasm) = common::contract_wasm() else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };
    let world = shared_world();
    let (mut alice, mut bob) = (FakeNode::new(world.clone()), FakeNode::new(world));
    let invite = invite_new(&mut alice, "alice", Side::White, [0xa1; 32], [0x11; 16])
        .await
        .unwrap();
    let offer = invite_accept(&mut bob, "bob", &invite, wasm.clone(), [0xb0; 32])
        .await
        .unwrap();
    game_bind(&mut alice, "alice", &offer, wasm.clone())
        .await
        .unwrap();

    for (is_alice, label, uci) in [
        (true, "alice", "e2e4"),
        (false, "bob", "e7e5"),
        (true, "alice", "g1f3"),
    ] {
        let node = if is_alice { &mut alice } else { &mut bob };
        play_move(node, label, uci, wasm.clone()).await.unwrap();
    }

    let view = open_game_view(&mut alice, "alice", wasm).await.unwrap();
    assert_eq!(view.status.ply, 3);
    assert_eq!(view.side, Side::White);
    assert_eq!(
        moves_in_order(&view),
        vec!["e2e4", "e7e5", "g1f3"],
        "the history is the chain in order, not the record set's id order"
    );
}

/// The chain is ordered; the record map is not. A history built by iterating
/// the map would come out in id order, which is effectively random.
#[tokio::test]
async fn the_move_order_is_the_chain_not_the_map() {
    let Some(wasm) = common::contract_wasm() else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };
    let world = shared_world();
    let (mut alice, mut bob) = (FakeNode::new(world.clone()), FakeNode::new(world));
    let invite = invite_new(&mut alice, "alice", Side::White, [0xa1; 32], [0x11; 16])
        .await
        .unwrap();
    let offer = invite_accept(&mut bob, "bob", &invite, wasm.clone(), [0xb0; 32])
        .await
        .unwrap();
    game_bind(&mut alice, "alice", &offer, wasm.clone())
        .await
        .unwrap();
    for (is_alice, label, uci) in [
        (true, "alice", "e2e4"),
        (false, "bob", "e7e5"),
        (true, "alice", "g1f3"),
        (false, "bob", "b8c6"),
    ] {
        let node = if is_alice { &mut alice } else { &mut bob };
        play_move(node, label, uci, wasm.clone()).await.unwrap();
    }
    let view = open_game_view(&mut alice, "alice", wasm).await.unwrap();

    let by_map: Vec<String> = view
        .state
        .records
        .values()
        .filter_map(|r| match &r.body {
            adjourn_core::Body::Move { uci, .. } => Some(uci.clone()),
            _ => None,
        })
        .collect();
    let by_chain = moves_in_order(&view);

    assert_eq!(by_chain, vec!["e2e4", "e7e5", "g1f3", "b8c6"]);
    assert_ne!(
        by_map, by_chain,
        "if these ever match, this fixture stopped proving anything -- pick \
         moves whose record ids sort differently from their play order"
    );
}
