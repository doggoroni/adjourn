mod common;

use adjourn_cli::fake::{shared_world, FakeNode};
use adjourn_cli::session::{
    game_bind, invite_accept, invite_new, play_move, show_label, sign_move_at_ply,
};
use adjourn_core::delegate_api::Side;

async fn setup() -> Option<(FakeNode, FakeNode, Vec<u8>)> {
    let wasm = common::contract_wasm()?;
    let world = shared_world();
    let (mut alice, mut bob) = (FakeNode::new(world.clone()), FakeNode::new(world));
    let invite = invite_new(&mut alice, "alice", Side::White).await.unwrap();
    let offer = invite_accept(&mut bob, "bob", &invite, wasm.clone())
        .await
        .unwrap();
    game_bind(&mut alice, "alice", &offer, wasm.clone())
        .await
        .unwrap();
    Some((alice, bob, wasm))
}

#[tokio::test]
async fn a_move_is_visible_to_both_players() {
    let Some((mut alice, mut bob, wasm)) = setup().await else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };

    let st = play_move(&mut alice, "alice", "e2e4", wasm.clone())
        .await
        .unwrap();
    assert_eq!(st.ply, 1);

    let seen = show_label(&mut bob, "bob", wasm.clone()).await.unwrap();
    assert_eq!(seen.ply, 1, "black cannot see white's move");

    let st = play_move(&mut bob, "bob", "e7e5", wasm).await.unwrap();
    assert_eq!(st.ply, 2);
}

/// Caught locally, before the delegate is bothered: a good error beats a
/// refusal for something the client already knows.
#[tokio::test]
async fn moving_out_of_turn_fails_before_signing() {
    let Some((mut alice, _bob, wasm)) = setup().await else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };
    let err = play_move(&mut alice, "alice", "e7e5", wasm)
        .await
        .expect_err("that is not white's move");
    let text = format!("{err:#}").to_lowercase();
    assert!(
        text.contains("turn") || text.contains("legal"),
        "got: {err:#}"
    );
}

/// The guarantee, through the whole stack: a second DIFFERENT move at a ply
/// already signed is refused by the DELEGATE, not by the client. Uses the
/// bypass helper so the client's own pre-checks cannot mask it.
#[tokio::test]
async fn a_double_sign_attempt_is_refused_by_the_delegate() {
    let Some((mut alice, _bob, wasm)) = setup().await else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };
    play_move(&mut alice, "alice", "e2e4", wasm.clone())
        .await
        .unwrap();

    let err = sign_move_at_ply(&mut alice, "alice", 1, "d2d4", wasm)
        .await
        .expect_err("the delegate must refuse a second move at ply 1");
    assert!(format!("{err:#}").contains("ply 1"), "got: {err:#}");
}
