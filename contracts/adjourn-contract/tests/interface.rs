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

    // And a second round is empty in both directions.
    let again: Delta =
        from_reader(state_delta(&params, &b_after, summarize(&params, &a_after)).as_ref())
            .expect("decode");
    assert!(again.is_empty(), "sync did not settle in one round");
}
