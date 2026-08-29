mod common;

use adjourn_client::fake::{shared_world, FakeNode};
use adjourn_client::node::NodeClient;
use adjourn_client::session::{
    draw_offer, game_bind, invite_accept, invite_new, migrate_label, open_game_view,
    opponent_moved_on_previous, play_move, watch_label, MigrateOutcome,
};
use adjourn_core::delegate_api::{Request, Response, Side};
use adjourn_core::{Body, GameState, KeyBytes, Record};
use ed25519_dalek::SigningKey;
use freenet_stdlib::prelude::ContractInstanceId;
use std::cell::RefCell;
use std::rc::Rc;

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
/// error), must not stop reflecting bob's own (new-contract) game just
/// because the opponent is stuck behind, and must report the skew through
/// `on_skew` exactly once -- the game stays watchable; this is a report, not
/// a teardown.
///
/// This exercises the PRE-LOOP check: alice's stray move already exists by
/// the time `watch_label` is called (ordinary, single-threaded test
/// ordering), so the one-shot check `watch_label` runs before entering its
/// loop is what catches it here. See
/// `watch_label_detects_skew_delivered_as_a_later_notification` below for the
/// IN-LOOP check, which needs a very different construction.
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
    let mut skew_messages = Vec::new();
    watch_label(
        &mut bob,
        "bob",
        variant.clone(),
        |_state, status| {
            seen.push(status.ply);
        },
        |msg: &str| skew_messages.push(msg.to_string()),
    )
    .await
    .expect("a lagging opponent must not turn watch_label into an error");

    assert_eq!(
        seen,
        vec![2],
        "bob's own contract only ever saw the first two moves"
    );
    assert_eq!(
        skew_messages.len(),
        1,
        "the skew must be reported exactly once, got {skew_messages:?}"
    );

    // The game is still fully readable afterward -- skew is a report, not a
    // teardown.
    let after = open_game_view(&mut bob, "bob", variant).await.unwrap();
    assert_eq!(after.status.ply, 2);
}

/// Exercises the IN-LOOP skew check specifically -- the branch in
/// `watch_label` that fires when a wake names the OLD contract, as opposed to
/// the one-shot check `watch_label` runs before it ever enters its loop.
///
/// This is deliberately NOT constructed the same way as the test above.
/// `FakeNode` never actually suspends -- none of its methods ever return
/// `Poll::Pending` -- so there is no way to interleave a second, independent
/// task's writes with an in-flight `watch_label` call using ordinary
/// concurrency: a task sharing this single-threaded executor is never
/// scheduled until `watch_label`'s own poll call yields control, and it never
/// does until it returns. Writing alice's stray move BEFORE calling
/// `watch_label` (as the test above does) is therefore always caught by the
/// PRE-LOOP check -- there is no ordinary way to make it land only late
/// enough for the in-loop check to be the one that finds it.
///
/// The way out: inject alice's move from INSIDE the `on_status` callback,
/// which `watch_label` calls synchronously partway through its own loop --
/// strictly after the pre-loop check has already run and found nothing.
/// `futures::executor::block_on` runs her `play_move` call to completion
/// there; it never actually blocks because `FakeNode` never returns Pending,
/// so this is deterministic, not a race. This makes the ordering exact
/// without needing real concurrency: alice's move enters `FakeNode`'s shared
/// `World` only once `watch_label`'s loop is already running, at a point
/// the pre-loop check can no longer reach.
#[tokio::test]
async fn watch_label_detects_skew_delivered_as_a_later_notification() {
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
    play_move(&mut bob, "bob", "e7e5", wasm.clone())
        .await
        .unwrap();

    let outcome = migrate_label(&mut bob, "bob", variant.clone())
        .await
        .unwrap();
    let MigrateOutcome::Migrated { to, .. } = outcome else {
        panic!("expected Migrated, got {outcome:?}");
    };

    // Pre-subscribe bob to the CURRENT contract only, exactly like
    // `updates.rs::watch_label_reports_the_opponents_move` does -- `FakeNode`
    // only redelivers log entries landing at or after a subscribe point, and
    // this needs one before the draw offer below or it would never be
    // redelivered as a notification at all.
    //
    // Deliberately NOT also pre-subscribing to `old` here: doing so would let
    // this test pass even if `watch_label`'s OWN subscribing call on `old`
    // (`check_previous_skew`'s pre-loop call, `subscribe = true`) were
    // deleted or changed to `subscribe = false` -- `FakeNode`'s subscription
    // map is `or_insert`, so an earlier, test-driven subscribe would silently
    // absorb that mutation and this test would keep passing for the wrong
    // reason. Leaving `old` unsubscribed until `watch_label` itself
    // subscribes is what makes the mutation in the report below actually be
    // caught here.
    bob.get(ContractInstanceId::new(to), true).await.unwrap();

    // A record on bob's OWN (current, migrated-to) contract, so the watch
    // loop has something to process before it ever wakes for the old
    // contract. `draw_offer` has no turn check -- `Body::DrawOffer` signs
    // unconditionally in `decide_sign`, unlike `Body::Move` -- so this does
    // not depend on whose move it legitimately is, and it does not end the
    // game the way `resign` would (which would return from `watch_label`
    // before it ever looked at a second notification).
    draw_offer(&mut bob, "bob", variant.clone()).await.unwrap();

    let status_calls = Rc::new(RefCell::new(0usize));
    let skew_after = Rc::new(RefCell::new(None::<usize>));
    let mut injected = false;

    {
        let status_calls_for_status = status_calls.clone();
        let status_calls_for_skew = status_calls.clone();
        let skew_after_for_skew = skew_after.clone();

        watch_label(
            &mut bob,
            "bob",
            variant,
            move |_state, _status| {
                *status_calls_for_status.borrow_mut() += 1;
                let n = *status_calls_for_status.borrow();
                // Call 1 is `watch_label`'s own opening projection, before it
                // has subscribed to anything -- injecting there would still
                // land ahead of the pre-loop check and prove nothing new.
                // Call 2 is the draw-offer notification processed inside the
                // loop; injecting here happens strictly after the pre-loop
                // check already ran (and found nothing).
                if n == 2 && !injected {
                    injected = true;
                    futures::executor::block_on(play_move(
                        &mut alice,
                        "alice",
                        "g1f3",
                        wasm.clone(),
                    ))
                    .expect("alice can still play on the old contract");
                }
            },
            move |_msg: &str| {
                let n = *status_calls_for_skew.borrow();
                skew_after_for_skew.borrow_mut().get_or_insert(n);
            },
        )
        .await
        .expect("a lagging opponent must not turn watch_label into an error");
    }

    assert_eq!(
        *skew_after.borrow(),
        Some(2),
        "the skew must be reported only after the loop processed a real \
         notification (status call 2), proving the IN-LOOP check fired -- \
         the one-shot pre-loop check would show Some(1) or never fire at all, \
         since alice had not moved a third time when it ran"
    );
}

const ALICE_ENTROPY: [u8; 32] = [0xa1; 32];
const BOB_ENTROPY: [u8; 32] = [0xb0; 32];
const NONCE: [u8; 16] = [0x42; 16];
