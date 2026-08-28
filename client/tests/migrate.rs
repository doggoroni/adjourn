mod common;

use adjourn_client::fake::{shared_world, FakeNode};
use adjourn_client::node::NodeClient;
use adjourn_client::session::{
    game_bind, invite_accept, invite_new, migrate_label, open_game_view, play_move, MigrateOutcome,
};
use adjourn_core::delegate_api::{Request, Response, Side};

/// Pins the PUT-before-Rebind ordering. `FakeNode`'s `put` and `delegate`
/// both otherwise succeed unconditionally, so nothing else in this file (or
/// `migrate_label` itself) can tell a correctly-ordered implementation apart
/// from one that Rebinds before it PUTs -- both would pass the two tests
/// above identically. Arming `fail_next_put` makes the PUT fail, and the
/// assertion that follows is exactly what "a failed PUT leaves the game
/// exactly where it was" means: still bound to the OLD contract id, and still
/// readable there.
///
/// The binding is read through a raw `Request::ListGames` round trip
/// (matching `delegates/adjourn-delegate/tests/adapter.rs`'s pattern),
/// deliberately NOT through `open_game_view`/`bound_game`: those derive a
/// contract id from `contract_wasm` and would raise their own build-mismatch
/// error the moment the recorded id and the wasm-derived id disagree -- which
/// is exactly the state a wrongly-ordered `migrate_label` can leave behind
/// after a bad Rebind, so going through them would mask the property under
/// an unrelated panic instead of observing it.
#[tokio::test]
async fn a_failed_put_leaves_the_game_bound_to_the_old_contract() {
    let Some((mut alice, mut bob, wasm)) = setup().await else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };
    let mut variant = wasm.clone();
    variant.push(0);
    variant.push(0);
    variant.extend_from_slice(b"variant");

    play_move(&mut alice, "alice", "e2e4", wasm.clone())
        .await
        .unwrap();
    let before = open_game_view(&mut bob, "bob", wasm.clone()).await.unwrap();

    bob.fail_next_put();
    let err = migrate_label(&mut bob, "bob", variant)
        .await
        .expect_err("a failed PUT must fail the migration");
    assert!(
        format!("{err:#}").to_lowercase().contains("put"),
        "got: {err:#}"
    );

    // Still bound to the OLD contract -- read straight off the delegate's
    // ListGames response, which names the recorded contract id without
    // deriving one from any wasm build.
    let bound_contract = bound_contract_of(&mut bob, "bob").await;
    assert_eq!(
        bound_contract,
        Some(before.contract),
        "the delegate must still point at the OLD contract after a failed PUT"
    );
}

/// The contract id the delegate has recorded for `label`, read via a raw
/// `Request::ListGames` round trip -- no contract id is derived here, so this
/// cannot itself hit a build-mismatch path.
async fn bound_contract_of<N: NodeClient>(node: &mut N, label: &str) -> Option<[u8; 32]> {
    let Response::Games(games) = node.delegate(Request::ListGames).await.unwrap() else {
        panic!("expected Response::Games");
    };
    games
        .into_iter()
        .find(|g| g.label == label)
        .and_then(|g| g.contract)
}

/// Same shape as `client/tests/moves.rs::setup`: invite, accept, bind, and
/// hand back both players plus the contract WASM bytes so a test can play
/// moves and derive ids.
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

/// Two different WASM byte strings give two contract ids, which is all the
/// migration path needs to exercise. `FakeNode` runs the real contract code
/// regardless of the bytes, so this models the id change without needing a
/// second real build.
#[tokio::test]
async fn migrating_moves_a_game_to_the_new_contract_id() {
    let Some((mut alice, mut bob, wasm)) = setup().await else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };
    let mut variant = wasm.clone();
    variant.push(0);
    variant.push(0);
    variant.extend_from_slice(b"variant");

    play_move(&mut alice, "alice", "e2e4", wasm.clone())
        .await
        .unwrap();

    let before = open_game_view(&mut bob, "bob", wasm.clone()).await.unwrap();

    let outcome = migrate_label(&mut bob, "bob", variant.clone())
        .await
        .unwrap();
    let MigrateOutcome::Migrated { from, to, records } = outcome else {
        panic!("expected Migrated, got {outcome:?}");
    };
    assert_eq!(from, before.contract);
    assert_ne!(to, before.contract, "the id must actually move");
    assert_eq!(
        records,
        before.state.records.len(),
        "every record must come across"
    );

    // The game is playable at the new address under the new build.
    let after = open_game_view(&mut bob, "bob", variant).await.unwrap();
    assert_eq!(after.contract, to);
    assert_eq!(after.status.ply, before.status.ply);
}

#[tokio::test]
async fn migrating_twice_is_a_no_op_the_second_time() {
    let Some((mut alice, mut bob, wasm)) = setup().await else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };
    let mut variant = wasm.clone();
    variant.push(0);
    variant.push(0);
    variant.extend_from_slice(b"variant");

    play_move(&mut alice, "alice", "e2e4", wasm.clone())
        .await
        .unwrap();

    migrate_label(&mut bob, "bob", variant.clone())
        .await
        .unwrap();
    let second = migrate_label(&mut bob, "bob", variant).await.unwrap();
    assert!(
        matches!(second, MigrateOutcome::AlreadyCurrent { .. }),
        "a second migrate must be a no-op, got {second:?}"
    );
}

const ALICE_ENTROPY: [u8; 32] = [0xa1; 32];
const BOB_ENTROPY: [u8; 32] = [0xb0; 32];
const NONCE: [u8; 16] = [0x42; 16];
