mod common;

use adjourn_client::fake::{shared_world, FakeNode};
use adjourn_client::node::NodeClient;
use adjourn_client::session::{
    game_bind, invite_accept, invite_new, migrate_label, open_game_view,
    opponent_moved_on_previous, play_move, watch_label, MigrateOutcome,
};
use adjourn_core::delegate_api::{Request, Response, Side};
use adjourn_core::{Body, GameState, KeyBytes, Record};
use ed25519_dalek::SigningKey;

/// The old contract legitimately holds the whole pre-migration history, so
/// "the opponent has records there" is ALWAYS true and is not the signal. The
/// signal is a set difference: records on the old id that are absent from the
/// new one and were signed by the opponent.
#[test]
fn skew_is_a_set_difference_not_the_presence_of_opponent_records() {
    let (ours, theirs) = fixture_two_keys();
    let shared = state_with(&[(ours, 1), (theirs, 2)]);

    // Identical sets: migration complete, nothing to report.
    assert!(!opponent_moved_on_previous(&shared, &shared, &ours));

    // Opponent moved on the OLD contract after we migrated.
    let old_ahead = state_with(&[(ours, 1), (theirs, 2), (theirs, 4)]);
    assert!(opponent_moved_on_previous(&shared, &old_ahead, &ours));

    // WE are ahead on the old one. Not skew -- it is our own record, and
    // reporting it would cry wolf on every migration.
    let ours_ahead = state_with(&[(ours, 1), (theirs, 2), (ours, 3)]);
    assert!(!opponent_moved_on_previous(&shared, &ours_ahead, &ours));
}

/// Two distinct public keys, playing the role of "us" and "the opponent" in
/// `opponent_moved_on_previous`'s tests. Follows `common/tests/adversarial.rs`'s
/// `keys()` fixture style -- fixed seed bytes, derived verifying keys -- but
/// hands back raw `KeyBytes` since the predicate under test never verifies a
/// signature, only compares signer bytes.
fn fixture_two_keys() -> (KeyBytes, KeyBytes) {
    let ours = SigningKey::from_bytes(&[11u8; 32]);
    let theirs = SigningKey::from_bytes(&[22u8; 32]);
    (
        ours.verifying_key().to_bytes(),
        theirs.verifying_key().to_bytes(),
    )
}

/// A `GameState` holding one unsigned `Move` record per `(signer, ply)` pair,
/// inserted via `absorb_for_test` -- no signature verification, no chain, no
/// eviction. `opponent_moved_on_previous` is a pure set difference over
/// `state.records`, so none of that machinery is needed to exercise it: only
/// distinct record ids (one per ply here) and the `signer` field matter.
fn state_with(entries: &[(KeyBytes, u16)]) -> GameState {
    let mut state = GameState::empty();
    for (signer, ply) in entries {
        let rec = Record {
            body: Body::Move {
                ply: *ply,
                parent: [0u8; 32],
                uci: "e2e4".to_string(),
            },
            signer: *signer,
            sig: vec![0u8; 64],
        };
        state.absorb_for_test(&rec);
    }
    state
}

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

/// Bob migrates; alice never does and keeps playing on the OLD contract.
/// `watch_label` must still run to completion (not panic, not hang, not
/// error) and must not stop reflecting bob's own (new-contract) game just
/// because the opponent is stuck behind -- the game stays watchable, and the
/// skew is reported (via `eprintln!`, not observable from here) rather than
/// torn down. This test cannot see the printed warning; it pins the
/// behaviour around it: the watch survives the skew and still reports the
/// correct position on the contract bob actually migrated to.
#[tokio::test]
async fn watch_label_survives_skew_against_an_unmigrated_opponent() {
    let Some((mut alice, mut bob, wasm)) = setup().await else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };
    let mut variant = wasm.clone();
    variant.push(0);
    variant.push(0);
    variant.extend_from_slice(b"variant");

    // Two moves on the OLD contract, shared by both players before anyone
    // migrates.
    play_move(&mut alice, "alice", "e2e4", wasm.clone())
        .await
        .unwrap();
    play_move(&mut bob, "bob", "e7e5", wasm.clone())
        .await
        .unwrap();

    migrate_label(&mut bob, "bob", variant.clone())
        .await
        .unwrap();

    // Alice never migrated -- her label is still bound to the OLD contract.
    // She keeps playing there (it is legitimately her turn, ply 3), and bob
    // -- now over on the new contract -- will never see this move.
    play_move(&mut alice, "alice", "g1f3", wasm.clone())
        .await
        .unwrap();

    let mut seen = Vec::new();
    watch_label(&mut bob, "bob", variant.clone(), |_state, status| {
        seen.push(status.ply);
    })
    .await
    .expect("a lagging opponent must not turn watch_label into an error");

    assert_eq!(
        seen,
        vec![2],
        "bob's own contract only ever saw the first two moves"
    );

    // The game is still fully readable afterward -- skew is a report, not a
    // teardown.
    let after = open_game_view(&mut bob, "bob", variant).await.unwrap();
    assert_eq!(after.status.ply, 2);
}

const ALICE_ENTROPY: [u8; 32] = [0xa1; 32];
const BOB_ENTROPY: [u8; 32] = [0xb0; 32];
const NONCE: [u8; 16] = [0x42; 16];
