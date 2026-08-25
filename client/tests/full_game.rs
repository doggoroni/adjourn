mod common;

use adjourn_client::fake::{shared_world, FakeNode};
use adjourn_client::node::NodeClient;
use adjourn_client::session::{game_bind, invite_accept, invite_new, play_move, show_label};
use adjourn_core::delegate_api::Side;
use adjourn_core::{GameState, Reason};
use shakmaty::Color;

/// Scholar's Mate, played alternately through two `FakeNode`s sharing one
/// contract world but holding separate delegate secrets -- exactly what
/// distinguishes "the two players independently converge through the real
/// contract" from "one side talking to itself". After each move both nodes
/// must project the same ply, and at the end both must project the same
/// checkmate decision from byte-identical state.
#[tokio::test]
async fn scholars_mate_end_to_end() {
    let Some(wasm) = common::contract_wasm() else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };

    let world = shared_world();
    let (mut alice, mut bob) = (FakeNode::new(world.clone()), FakeNode::new(world));

    let invite = invite_new(&mut alice, "alice", Side::White, ALICE_ENTROPY, NONCE)
        .await
        .unwrap();
    let offer = invite_accept(&mut bob, "bob", &invite, wasm.clone(), BOB_ENTROPY)
        .await
        .unwrap();
    let contract_id = game_bind(&mut alice, "alice", &offer, wasm.clone())
        .await
        .unwrap();

    // (label, node-is-white) pairs in ply order: alice is white, bob is black.
    let moves = [
        ("e2e4", true),
        ("e7e5", false),
        ("f1c4", true),
        ("b8c6", false),
        ("d1h5", true),
        ("g8f6", false),
        ("h5f7", true),
    ];

    let mut last_status = None;
    for (ply, (uci, white_to_move)) in moves.iter().enumerate() {
        let ply = ply as u16 + 1;
        let status = if *white_to_move {
            play_move(&mut alice, "alice", uci, wasm.clone())
                .await
                .unwrap()
        } else {
            play_move(&mut bob, "bob", uci, wasm.clone()).await.unwrap()
        };
        assert_eq!(status.ply, ply, "mover did not land on the expected ply");

        // The opponent must project the same ply from the shared contract --
        // proof the move flowed through the real contract, not a local echo.
        let opponent_status = if *white_to_move {
            show_label(&mut bob, "bob", wasm.clone()).await.unwrap()
        } else {
            show_label(&mut alice, "alice", wasm.clone()).await.unwrap()
        };
        assert_eq!(
            opponent_status.ply, status.ply,
            "opponent did not see the move that was just signed"
        );

        last_status = Some(status);
    }

    let final_status = last_status.expect("moves is non-empty");
    let decision = final_status.decision.expect("Scholar's Mate ends the game");
    assert_eq!(decision.reason, Reason::Checkmate);
    assert_eq!(decision.winner, Some(Color::White));

    let alice_status = show_label(&mut alice, "alice", wasm.clone()).await.unwrap();
    let bob_status = show_label(&mut bob, "bob", wasm.clone()).await.unwrap();
    let alice_decision = alice_status.decision.expect("alice sees the mate");
    let bob_decision = bob_status.decision.expect("bob sees the mate");
    assert_eq!(alice_decision.reason, Reason::Checkmate);
    assert_eq!(alice_decision.winner, Some(Color::White));
    assert_eq!(bob_decision.reason, Reason::Checkmate);
    assert_eq!(bob_decision.winner, Some(Color::White));

    // Both nodes must have converged on byte-identical contract state, not
    // merely agree in their projections -- re-decoding and re-encoding each
    // side's raw bytes rules out two states that happen to project the same
    // status while differing in, say, an ignored record.
    let alice_bytes = alice.get(contract_id, false).await.unwrap().unwrap();
    let bob_bytes = bob.get(contract_id, false).await.unwrap().unwrap();
    assert_eq!(
        alice_bytes, bob_bytes,
        "the two nodes did not converge on byte-identical contract state"
    );
    let alice_state = GameState::decode(&alice_bytes).expect("alice's state decodes");
    let bob_state = GameState::decode(&bob_bytes).expect("bob's state decodes");
    assert_eq!(alice_state.encode(), bob_state.encode());
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
