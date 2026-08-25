mod common;

use adjourn_client::fake::{shared_world, FakeNode};
use adjourn_client::session::{
    draw_claim, game_bind, invite_accept, invite_new, play_move, show_label, sign_move_at_ply,
};
use adjourn_core::delegate_api::Side;

async fn setup() -> Option<(FakeNode, FakeNode, Vec<u8>)> {
    let wasm = common::contract_wasm()?;
    let world = shared_world();
    let (mut alice, mut bob) = (FakeNode::new(world.clone()), FakeNode::new(world));
    let invite = invite_new(&mut alice, "alice", Side::White, ALICE_ENTROPY, NONCE)
        .await
        .unwrap();
    let offer = invite_accept(&mut bob, "bob", &invite, wasm.clone(), BOB_ENTROPY)
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

/// A groundless claim is refused locally. A claim with no valid ground is
/// ignored at projection anyway, so signing one would only add a dead record
/// to state.
#[tokio::test]
async fn draw_claim_refuses_when_there_is_no_ground_to_claim() {
    let Some((mut alice, _bob, wasm)) = setup().await else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };

    let err = draw_claim(&mut alice, "alice", wasm)
        .await
        .expect_err("a fresh game has nothing to claim");
    let text = format!("{err:#}").to_lowercase();
    assert!(text.contains("no draw to claim"), "got: {err:#}");
}

/// Fixed test entropy. `adjourn-client` takes randomness as a parameter (it
/// must compile for wasm32, where `rand` does not), so tests supply constants
/// -- deterministic keys and a deterministic contract id, which is strictly
/// better for a test than a fresh random one every run. The two sides differ
/// so they never derive the same signing key.
const ALICE_ENTROPY: [u8; 32] = [0xa1; 32];
const BOB_ENTROPY: [u8; 32] = [0xb0; 32];
/// The `GameParams` nonce the inviter authors.
const NONCE: [u8; 16] = [0x5e; 16];
