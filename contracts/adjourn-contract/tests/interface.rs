//! The contract interface, exercised directly — no Freenet node required.
//!
//! The algebra is tested in `adjourn-core`. What is tested here is the adapter:
//! byte encodings, the empty-state cases, and that the contract's validity
//! predicate is the structural one and not a chess one.

#![cfg(not(target_arch = "wasm32"))]

use adjourn_contract::Contract;
use adjourn_core::state::{Delta, Summary};
use adjourn_core::{make_move, project, Body, GameParams, GameState, Record};
use ciborium::{de::from_reader, ser::into_writer};
use ed25519_dalek::SigningKey;
use freenet_stdlib::prelude::*;

fn encode<T: serde::Serialize>(value: &T) -> Vec<u8> {
    let mut buf = Vec::new();
    into_writer(value, &mut buf).expect("cbor encode");
    buf
}

fn keys() -> (SigningKey, SigningKey, GameParams) {
    let w = SigningKey::from_bytes(&[1u8; 32]);
    let b = SigningKey::from_bytes(&[2u8; 32]);
    let params = GameParams {
        white: w.verifying_key().to_bytes(),
        black: b.verifying_key().to_bytes(),
        nonce: [7u8; 16],
    };
    (w, b, params)
}

fn play(moves: &[&str]) -> (GameState, GameParams, SigningKey, SigningKey) {
    let (w, b, params) = keys();
    let mut state = GameState::empty();
    for (i, uci) in moves.iter().enumerate() {
        let key = if i % 2 == 0 { &w } else { &b };
        let rec = make_move(&state, &params, key, uci).expect("legal move");
        assert!(state.insert_verified(&rec, &params));
    }
    (state, params, w, b)
}

fn params_bytes(params: &GameParams) -> Parameters<'static> {
    Parameters::from(encode(params))
}

fn state_bytes(state: &GameState) -> State<'static> {
    State::from(state.encode())
}

fn validate(params: &GameParams, state: &State<'static>) -> ValidateResult {
    Contract::validate_state(
        params_bytes(params),
        state.clone(),
        RelatedContracts::default(),
    )
    .expect("validate_state")
}

// ---------------------------------------------------------------------------
// validate_state
// ---------------------------------------------------------------------------

/// A contract is PUT before either player has moved. If empty bytes were a
/// decode error, a fresh game would be unreachable.
#[test]
fn empty_state_is_valid() {
    let (_, _, params) = keys();
    assert_eq!(
        validate(&params, &State::from(Vec::new())),
        ValidateResult::Valid
    );
}

#[test]
fn a_played_game_is_valid() {
    let (state, params, _, _) = play(&["e2e4", "e7e5"]);
    assert_eq!(
        validate(&params, &state_bytes(&state)),
        ValidateResult::Valid
    );
}

/// INVARIANT 1. An illegal move is a well-formed signed statement; the
/// projection ignores it. If it made the whole state invalid, either player
/// could destroy the game by signing one garbage move.
#[test]
fn an_illegal_move_does_not_make_the_state_invalid() {
    let (state, params, w, _) = play(&["e2e4", "e7e5"]);
    let head = *project(&state, &params).chain.last().unwrap();

    let mut s = state.clone();
    let garbage = Record::sign(
        &w,
        &params,
        Body::Move {
            ply: 3,
            parent: head,
            uci: "a1a8".into(), // straight through three of its own pieces
        },
    );
    assert!(s.insert_verified(&garbage, &params));

    assert_eq!(
        validate(&params, &state_bytes(&s)),
        ValidateResult::Valid,
        "chess legality must not be a validity condition"
    );
    assert_eq!(
        project(&s, &params).ply,
        2,
        "and the projection must ignore it"
    );
}

/// A record not signed by either player is not a statement about this game.
#[test]
fn a_state_carrying_a_forgery_is_invalid() {
    let (state, params, _, _) = play(&["e2e4"]);
    let good = state.records.values().next().unwrap().clone();

    let mut s = state.clone();
    s.absorb_for_test(&Record {
        body: good.body.clone(),
        signer: good.signer,
        sig: vec![0u8; 64],
    });

    assert_eq!(validate(&params, &state_bytes(&s)), ValidateResult::Invalid);
}

#[test]
fn a_state_signed_by_a_stranger_is_invalid() {
    let (state, params, _, _) = play(&["e2e4"]);
    let stranger = SigningKey::from_bytes(&[9u8; 32]);

    let mut s = state.clone();
    s.absorb_for_test(&Record::sign(&stranger, &params, Body::Resign));

    assert_eq!(validate(&params, &state_bytes(&s)), ValidateResult::Invalid);
}

// ---------------------------------------------------------------------------
// update_state
// ---------------------------------------------------------------------------

fn update(
    params: &GameParams,
    state: &GameState,
    data: Vec<UpdateData<'static>>,
) -> Result<GameState, ContractError> {
    let out = Contract::update_state(params_bytes(params), state_bytes(state), data)?;
    let bytes = out.unwrap_valid();
    Ok(GameState::decode(bytes.as_ref()).expect("contract emitted decodable state"))
}

#[test]
fn a_delta_advances_the_state() {
    let (state, params, _, b) = play(&["e2e4"]);
    let reply = make_move(&state, &params, &b, "e7e5").expect("legal");
    let delta: Delta = vec![reply];

    let after = update(
        &params,
        &state,
        vec![UpdateData::Delta(StateDelta::from(encode(&delta)))],
    )
    .expect("update");

    assert_eq!(after.len(), 2);
    assert_eq!(project(&after, &params).ply, 2);
}

#[test]
fn a_whole_state_merges() {
    let (mine, params, _, _) = play(&["e2e4", "e7e5"]);
    let (theirs, _, _, _) = play(&["e2e4", "e7e5", "g1f3", "b8c6"]);

    let after = update(
        &params,
        &mine,
        vec![UpdateData::State(state_bytes(&theirs))],
    )
    .expect("update");

    assert_eq!(project(&after, &params).ply, 4);
}

#[test]
fn an_empty_delta_is_a_no_op() {
    let (state, params, _, _) = play(&["e2e4", "e7e5"]);
    let after = update(
        &params,
        &state,
        vec![UpdateData::Delta(StateDelta::from(Vec::new()))],
    )
    .expect("update");
    assert_eq!(after, state);
}

#[test]
fn applying_the_same_delta_twice_changes_nothing() {
    let (state, params, _, b) = play(&["e2e4"]);
    let reply = make_move(&state, &params, &b, "e7e5").expect("legal");
    let delta = UpdateData::Delta(StateDelta::from(encode(&vec![reply])));

    let once = update(&params, &state, vec![delta.clone()]).expect("update");
    let twice = update(&params, &once, vec![delta]).expect("update");
    assert_eq!(once, twice, "update_state is not idempotent");
}

#[test]
fn a_delta_carrying_a_forgery_is_rejected() {
    let (state, params, _, _) = play(&["e2e4"]);
    let good = state.records.values().next().unwrap().clone();
    let forged = Record {
        body: good.body.clone(),
        signer: good.signer,
        sig: vec![0u8; 64],
    };

    let err = update(
        &params,
        &state,
        vec![UpdateData::Delta(StateDelta::from(encode(&vec![forged])))],
    )
    .expect_err("a forged record must not be accepted");
    assert!(matches!(err, ContractError::InvalidUpdateWithInfo { .. }));
}

// ---------------------------------------------------------------------------
// summarize_state / get_state_delta
// ---------------------------------------------------------------------------

fn summarize(params: &GameParams, state: &GameState) -> StateSummary<'static> {
    Contract::summarize_state(params_bytes(params), state_bytes(state)).expect("summarize")
}

fn state_delta(
    params: &GameParams,
    state: &GameState,
    summary: StateSummary<'static>,
) -> StateDelta<'static> {
    Contract::get_state_delta(params_bytes(params), state_bytes(state), summary)
        .expect("get_state_delta")
}

#[test]
fn summary_round_trips_through_cbor() {
    let (state, params, _, _) = play(&["e2e4", "e7e5"]);
    let bytes = summarize(&params, &state);
    let decoded: Summary = from_reader(bytes.as_ref()).expect("decode summary");
    assert_eq!(decoded, state.summarize());
}

/// An empty summary means "I have nothing", not "I am up to date".
#[test]
fn an_empty_summary_asks_for_everything() {
    let (state, params, _, _) = play(&["e2e4", "e7e5"]);
    let d = state_delta(&params, &state, StateSummary::from(Vec::new()));
    let decoded: Delta = from_reader(d.as_ref()).expect("decode delta");
    assert_eq!(decoded.len(), state.len(), "peer holding nothing needs all");
}

/// The network decides whether to skip a broadcast by looking at the ENCODED
/// delta length. An empty `Vec<Record>` CBOR-encodes to one byte (`0x80`),
/// which is not empty -- so a peer that is already up to date still receives a
/// delta, freenet-core's "empty delta -> skip" path never fires, and two peers
/// can re-offer to each other indefinitely. That is the failure that drove
/// River's 2026-07-25 bandwidth incident, where the room contract reached
/// 63.7% of all byte-weighted broadcast work network-wide.
///
/// `fdev conformance` reports this as the `self_delta_empty` diagnostic. Note
/// that asserting on the DECODED delta cannot catch it: the decoded vec really
/// is empty. Only the wire length tells the truth.
#[test]
fn a_delta_against_our_own_summary_is_zero_bytes() {
    let (state, params, _, _) = play(&["e2e4", "e7e5"]);
    let d = state_delta(&params, &state, summarize(&params, &state));
    assert!(
        d.as_ref().is_empty(),
        "an up-to-date peer must get zero bytes, got {}",
        d.as_ref().len()
    );
}

/// The whole point, end to end: two peers that have seen different halves of a
/// game reconcile in one round through the contract interface.
#[test]
fn two_peers_converge_in_one_round() {
    let (full, params, _, _) = play(&["e2e4", "e7e5", "g1f3", "b8c6"]);
    let records: Vec<Record> = full.records.values().cloned().collect();

    let mut a = GameState::empty();
    let mut b = GameState::empty();
    for (i, rec) in records.iter().enumerate() {
        if i % 2 == 0 {
            a.insert_verified(rec, &params);
        } else {
            b.insert_verified(rec, &params);
        }
    }
    assert_ne!(a, b);

    // A summarises; B answers with what A is missing; A applies it.
    let a_after = {
        let d = state_delta(&params, &b, summarize(&params, &a));
        update(&params, &a, vec![UpdateData::Delta(d)]).expect("update")
    };
    let b_after = {
        let d = state_delta(&params, &a, summarize(&params, &b));
        update(&params, &b, vec![UpdateData::Delta(d)]).expect("update")
    };

    assert_eq!(a_after, b_after, "peers did not converge");
    assert_eq!(a_after, full);
    assert_eq!(
        a_after.encode(),
        b_after.encode(),
        "converged states must be byte-identical"
    );

    // And a second round is empty in both directions -- checked on the WIRE
    // length, because that is what freenet-core's broadcast-skip path reads.
    // Zero bytes is not decodable CBOR, which is the point: there is nothing
    // to decode.
    for (from, to) in [(&b_after, &a_after), (&a_after, &b_after)] {
        let again = state_delta(&params, from, summarize(&params, to));
        assert!(
            again.as_ref().is_empty(),
            "sync did not settle in one round: {} bytes still offered",
            again.as_ref().len()
        );
    }
}

#[test]
fn an_over_k_state_comes_back_normalized_through_update() {
    let (w, _b, params) = keys();

    // Crafted bytes: 30 records from one signer in one (signer, kind, ply)
    // group. `absorb_for_test` bypasses eviction, which is exactly what a
    // hostile peer's encoder would do.
    let mut spam = GameState::empty();
    for i in 0..30u64 {
        let mut parent = [0u8; 32];
        parent[..8].copy_from_slice(&i.to_le_bytes());
        spam.absorb_for_test(&Record::sign(
            &w,
            &params,
            Body::Move {
                ply: 1,
                parent,
                uci: "e2e4".into(),
            },
        ));
    }
    assert_eq!(spam.len(), 30, "the crafted state really is over-K");

    let out = update(
        &params,
        &GameState::empty(),
        vec![UpdateData::State(state_bytes(&spam))],
    )
    .expect("update_state");

    assert_eq!(
        out.len(),
        2,
        "the contract normalizes an over-K state to K=2"
    );
}

/// I3: the BASE state of `update_state` must be normalized too, not just the
/// incoming one. `summarize_state` and `get_state_delta` both normalize on
/// read; if `update_state` did not, the same stored bytes would produce a
/// different record set depending on which entry point read them.
#[test]
fn an_over_k_base_state_is_normalized_before_the_update_is_applied() {
    let (w, _b, params) = keys();

    let mut spam = GameState::empty();
    for i in 0..30u64 {
        let mut parent = [0u8; 32];
        parent[..8].copy_from_slice(&i.to_le_bytes());
        spam.absorb_for_test(&Record::sign(
            &w,
            &params,
            Body::Move {
                ply: 1,
                parent,
                uci: "e2e4".into(),
            },
        ));
    }
    assert_eq!(spam.len(), 30, "the crafted base state really is over-K");

    // An empty delta changes nothing, so anything shrinking here is the base
    // normalization and nothing else.
    let out = update(
        &params,
        &spam,
        vec![UpdateData::Delta(StateDelta::from(Vec::new()))],
    )
    .expect("update_state");

    assert_eq!(
        out.len(),
        2,
        "update_state must normalize its base state, exactly as summarize/delta do"
    );
}

/// Finding 1: a PUT'd over-K state must not summarize or re-offer forever.
///
/// `validate_state` is permissive by design (it does not check chess
/// legality, and it does not evict either -- eviction needs `params`, which
/// `validate_state` has, but rejecting an over-K state would be a content
/// judgment this codebase forbids). So a hostile peer can PUT 30 records in
/// one `(signer, kind, ply)` group. Without normalizing in `summarize_state`
/// and `get_state_delta`, that node's summary would report all 30 records
/// forever, and its delta against an honest peer's summary would offer the
/// same 28 evicted-away records every round -- the same never-settles shape
/// as `self_delta_empty`.
#[test]
fn an_over_k_state_summarizes_and_diffs_as_normalized() {
    let (w, _b, params) = keys();

    let mut spam = GameState::empty();
    for i in 0..30u64 {
        let mut parent = [0u8; 32];
        parent[..8].copy_from_slice(&i.to_le_bytes());
        spam.absorb_for_test(&Record::sign(
            &w,
            &params,
            Body::Move {
                ply: 1,
                parent,
                uci: "e2e4".into(),
            },
        ));
    }
    assert_eq!(spam.len(), 30, "the crafted state really is over-K");

    // The summary must be bounded to K=2, not 30.
    let summary_bytes = summarize(&params, &spam);
    let decoded: Summary = from_reader(summary_bytes.as_ref()).expect("decode summary");
    assert_eq!(
        decoded.len(),
        2,
        "summarize_state must normalize before summarizing"
    );

    // A peer holding nothing asks with an empty summary; the delta offered
    // back must be exactly the normalized K=2, not all 30.
    let delta_bytes = state_delta(&params, &spam, StateSummary::from(Vec::new()));
    let delta: Delta = from_reader(delta_bytes.as_ref()).expect("decode delta");
    assert_eq!(
        delta.len(),
        2,
        "get_state_delta must normalize before diffing"
    );

    // And once the peer holds the normalized K=2, a second round offers
    // nothing further -- it must not keep re-offering the evicted 28.
    let mut normalized = GameState::empty();
    for rec in &delta {
        normalized.insert_verified(rec, &params);
    }
    let again = state_delta(&params, &spam, summarize(&params, &normalized));
    assert!(
        again.as_ref().is_empty(),
        "an already-normalized peer must not be re-offered evicted-away records"
    );
}
