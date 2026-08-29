mod common;

use adjourn_client::fake::{shared_world, FakeNode};
use adjourn_client::node::NodeClient;
use adjourn_client::session::{
    game_bind, invite_accept, invite_new, play_move, show_label, watch_label,
};
use adjourn_core::delegate_api::Side;
use adjourn_core::state::Delta;
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
    let invite = invite_new(&mut alice, "alice", Side::White, ALICE_ENTROPY, NONCE)
        .await
        .unwrap();
    let offer = invite_accept(&mut bob, "bob", &invite, wasm.clone(), BOB_ENTROPY)
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
    //
    // The payload is a DELTA, not a state: `sign_and_submit` submits a delta
    // and a real node broadcasts that delta to subscribers, so this is the
    // arm `watch_label` actually runs in production. Asserting the variant is
    // part of the test -- a fake that quietly switched to `State` would take
    // the live arm back out of coverage.
    let UpdateData::Delta(bytes) = update else {
        panic!("an update-originated notification carries the submitted Delta");
    };
    let delta: Delta = ciborium::from_reader(bytes.as_ref()).expect("payload decodes as a Delta");
    let params = params_of(&mut bob, "bob").await;
    let mut carried = GameState::empty();
    carried.apply_delta(&delta, &params);
    let status = adjourn_core::project(&carried, &params);
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
    let invite = invite_new(&mut alice, "alice", Side::White, ALICE_ENTROPY, NONCE)
        .await
        .unwrap();
    let offer = invite_accept(&mut bob, "bob", &invite, wasm.clone(), BOB_ENTROPY)
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

/// `watch_label` end to end: bob watches, alice moves, bob's callback fires.
///
/// This is the only test that drives `watch_label` itself -- the id filter,
/// the merge arms and the termination check. It terminates because
/// `FakeNode::next_update` returns `Ok(None)` once the log is drained and
/// `watch_label` returns on `None`; a real `WsClient` blocks instead and
/// never returns `None`, so the loop there runs until the game ends.
#[tokio::test]
async fn watch_label_reports_the_opponents_move() {
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
    game_bind(&mut alice, "alice", &offer, wasm.clone())
        .await
        .unwrap();

    // Subscribe BEFORE alice moves, so her move lands in the log after bob's
    // subscribe point and is genuinely delivered as a notification.
    // `watch_label`'s own subscribing GET will not move that point forward
    // (`FakeNode` uses `or_insert`), so the loop below really does run the
    // notification path rather than only the opening GET.
    let contract = contract_bytes_of(&mut bob, "bob").await;
    bob.get(ContractInstanceId::new(contract), true)
        .await
        .unwrap();

    play_move(&mut alice, "alice", "e2e4", wasm.clone())
        .await
        .unwrap();

    let mut seen = Vec::new();
    // The callback gets the merged `GameState` alongside the `Status` on
    // every call, not just the status. A UI that only kept the status (as
    // this one used to) can advance the board and the status line while the
    // move history -- driven by resolving `status.chain` against
    // `state.records` in `moves_in_order` -- stays frozen forever.
    //
    // The assertion below checks the pair is coherent: every id in
    // `status.chain` resolves against the `state` handed to the SAME call.
    // Be honest about its reach -- mutation testing says it does NOT
    // currently discriminate. Handing the callback a deliberately stale
    // state still passes, for the same structural reason recorded in
    // CLAUDE.md about the subscribing-GET merge: both fakes share one
    // `World`, so `alice`'s move is already in the contract state that
    // `watch_label`'s own opening GET returns, and there is no snapshot for
    // a later one to drift from. Constructing that drift needs `FakeNode` to
    // model per-node state divergence, which is a larger change than the
    // property it would pin.
    //
    // It is kept, rather than deleted, for two reasons: it costs nothing,
    // and it becomes load-bearing the moment such a fake exists. What
    // actually guarantees the pair moves together today is `watch_label`
    // itself -- it projects `status` from the very `state` it passes, on
    // every call -- not this test.
    watch_label(
        &mut bob,
        "bob",
        wasm,
        |state, status| {
            for id in &status.chain {
                assert!(
                    state.records.contains_key(id),
                    "chain id {id:?} at ply {} is missing from the state passed \
                     alongside it -- the move history would silently drop it",
                    status.ply
                );
            }
            seen.push(status.ply);
        },
        // This game never migrated, so `previous` is `None` and no skew
        // check ever runs -- nothing here should ever fire.
        |msg| panic!("unexpected skew signal on a game that never migrated: {msg}"),
    )
    .await
    .expect("watch runs to the end of the log");

    assert!(
        seen.len() >= 2,
        "the opening projection plus at least one notification, got {seen:?}"
    );
    assert_eq!(
        seen.last().copied(),
        Some(1),
        "bob's last reported position includes alice's move, got {seen:?}"
    );
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
