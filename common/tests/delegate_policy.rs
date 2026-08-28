use adjourn_core::delegate_api::{EntropyQuality, GameSummary, Refusal, Request, Response, Side};
use adjourn_core::delegate_policy::{
    classify_host_entropy, decide_bind, decide_rebind, decide_sign, derive_seed, migrate_record,
    BindDecision, GameRecord, HostEntropy, RebindDecision, SignDecision, GAME_RECORD_FORMAT,
};
use adjourn_core::{Body, GameParams, MAX_PLY};
use ed25519_dalek::SigningKey;

#[test]
fn requests_round_trip_through_cbor() {
    let req = Request::Sign {
        game_id: [7u8; 32],
        body: Body::Move {
            ply: 3,
            parent: [9u8; 32],
            uci: "e2e4".into(),
        },
    };
    let back = Request::decode(&req.encode()).expect("decode");
    assert_eq!(back, req);
}

#[test]
fn refusals_round_trip_through_cbor() {
    let resp = Response::Refused(Refusal::WrongSide {
        ours: Side::White,
        ply_needs: Side::Black,
    });
    let back = Response::decode(&resp.encode()).expect("decode");
    assert_eq!(back, resp);
}

#[test]
fn malformed_bytes_decode_to_a_refusal_not_a_panic() {
    assert!(matches!(
        Request::decode(&[0xff, 0xff, 0xff]),
        Err(Refusal::Malformed(_))
    ));
}

#[test]
fn all_zero_host_entropy_is_dead() {
    // This is exactly what the off-wasm stub returns.
    assert!(matches!(
        classify_host_entropy([0u8; 32], [0u8; 32]),
        HostEntropy::Dead
    ));
}

#[test]
fn two_identical_draws_mean_the_source_is_dead() {
    // A live CSPRNG repeats 32 bytes with negligible probability.
    assert!(matches!(
        classify_host_entropy([5u8; 32], [5u8; 32]),
        HostEntropy::Dead
    ));
}

#[test]
fn two_different_draws_are_live() {
    let mut second = [5u8; 32];
    second[0] = 6;
    assert!(matches!(
        classify_host_entropy([5u8; 32], second),
        HostEntropy::Live(_)
    ));
}

#[test]
fn dead_host_and_no_caller_entropy_fails_closed() {
    assert_eq!(
        derive_seed(HostEntropy::Dead, None, "g1").unwrap_err(),
        Refusal::NoEntropy
    );
}

#[test]
fn all_zero_caller_entropy_counts_as_absent() {
    assert_eq!(
        derive_seed(HostEntropy::Dead, Some([0u8; 32]), "g1").unwrap_err(),
        Refusal::NoEntropy
    );
}

#[test]
fn dead_host_with_caller_entropy_is_degraded_not_fatal() {
    let (seed, quality) = derive_seed(HostEntropy::Dead, Some([1u8; 32]), "g1").expect("seed");
    assert_eq!(quality, EntropyQuality::Degraded);
    assert_ne!(seed, [0u8; 32]);
}

#[test]
fn live_host_is_host_backed() {
    let (_, quality) = derive_seed(HostEntropy::Live([2u8; 32]), None, "g1").expect("seed");
    assert_eq!(quality, EntropyQuality::HostBacked);
}

#[test]
fn seeds_are_deterministic_and_label_separated() {
    let a = derive_seed(HostEntropy::Live([2u8; 32]), Some([1u8; 32]), "g1").unwrap();
    let b = derive_seed(HostEntropy::Live([2u8; 32]), Some([1u8; 32]), "g1").unwrap();
    let c = derive_seed(HostEntropy::Live([2u8; 32]), Some([1u8; 32]), "g2").unwrap();
    assert_eq!(a.0, b.0, "derivation must be deterministic in its inputs");
    assert_ne!(a.0, c.0, "a different label must give a different key");
}

#[test]
fn caller_entropy_changes_the_seed_even_with_the_same_host_draw() {
    let a = derive_seed(HostEntropy::Live([2u8; 32]), Some([1u8; 32]), "g1").unwrap();
    let b = derive_seed(HostEntropy::Live([2u8; 32]), Some([9u8; 32]), "g1").unwrap();
    assert_ne!(a.0, b.0, "caller entropy must be mixed in, not ignored");
}

const ORIGIN: [u8; 32] = [3u8; 32];
const CONTRACT: [u8; 32] = [5u8; 32];

fn game() -> (SigningKey, SigningKey, GameParams) {
    let w = SigningKey::from_bytes(&[1u8; 32]);
    let b = SigningKey::from_bytes(&[2u8; 32]);
    let params = GameParams {
        white: w.verifying_key().to_bytes(),
        black: b.verifying_key().to_bytes(),
        nonce: [7u8; 16],
    };
    (w, b, params)
}

/// A CLI is not a web app, so the runtime gives it no MessageOrigin. Binding
/// with `None` must therefore SUCCEED and record `None`.
#[test]
fn a_game_can_be_bound_with_no_origin_at_all() {
    let (w, _b, params) = game();
    let BindDecision::Bind { record } = decide_bind(
        None,
        "g1",
        w.verifying_key().to_bytes(),
        &params,
        CONTRACT,
        EntropyQuality::HostBacked,
        None,
    ) else {
        panic!("a CLI-bound game must be allowed");
    };
    assert_eq!(record.origin, None);
}

#[test]
fn binding_a_key_that_is_not_a_player_is_refused() {
    let (_w, _b, params) = game();
    let stranger = SigningKey::from_bytes(&[9u8; 32]);
    assert!(matches!(
        decide_bind(
            None,
            "g1",
            stranger.verifying_key().to_bytes(),
            &params,
            CONTRACT,
            EntropyQuality::HostBacked,
            Some(ORIGIN)
        ),
        BindDecision::Refuse(Refusal::KeyNotInParams)
    ));
}

#[test]
fn binding_records_the_side_and_starts_the_ply_counter_at_zero() {
    let (w, _b, params) = game();
    let BindDecision::Bind { record } = decide_bind(
        None,
        "g1",
        w.verifying_key().to_bytes(),
        &params,
        CONTRACT,
        EntropyQuality::HostBacked,
        Some(ORIGIN),
    ) else {
        panic!("expected a bind");
    };
    assert_eq!(record.side, Side::White);
    assert_eq!(record.origin, Some(ORIGIN));
    assert_eq!(record.last_signed_ply, 0);
    assert_eq!(record.label, "g1");
}

#[test]
fn rebinding_a_label_to_a_different_game_is_refused() {
    // Rebinding would orphan the ply counter, which is the whole protection.
    let (w, _b, params) = game();
    let BindDecision::Bind { record } = decide_bind(
        None,
        "g1",
        w.verifying_key().to_bytes(),
        &params,
        CONTRACT,
        EntropyQuality::HostBacked,
        Some(ORIGIN),
    ) else {
        panic!("expected a bind");
    };

    let mut other = params.clone();
    other.nonce = [8u8; 16]; // a different game between the same two players
    assert!(matches!(
        decide_bind(
            Some(&record),
            "g1",
            w.verifying_key().to_bytes(),
            &other,
            CONTRACT,
            EntropyQuality::HostBacked,
            Some(ORIGIN)
        ),
        BindDecision::Refuse(Refusal::AlreadyBound { .. })
    ));
}

#[test]
fn rebinding_the_same_label_to_the_same_game_is_idempotent() {
    // A dropped response must not wedge setup.
    let (w, _b, params) = game();
    let BindDecision::Bind { record } = decide_bind(
        None,
        "g1",
        w.verifying_key().to_bytes(),
        &params,
        CONTRACT,
        EntropyQuality::HostBacked,
        Some(ORIGIN),
    ) else {
        panic!("expected a bind");
    };
    let BindDecision::Bind { record: again } = decide_bind(
        Some(&record),
        "g1",
        w.verifying_key().to_bytes(),
        &params,
        CONTRACT,
        EntropyQuality::HostBacked,
        Some(ORIGIN),
    ) else {
        panic!("expected an idempotent re-bind");
    };
    assert_eq!(record, again);
}

const OTHER_ORIGIN: [u8; 32] = [4u8; 32];

fn white_record() -> GameRecord {
    let (w, _b, params) = game();
    let BindDecision::Bind { record } = decide_bind(
        None,
        "g1",
        w.verifying_key().to_bytes(),
        &params,
        CONTRACT,
        EntropyQuality::HostBacked,
        Some(ORIGIN),
    ) else {
        panic!("expected a bind");
    };
    record
}

/// A game bound the way the CLI binds one: no origin at all.
fn cli_record() -> GameRecord {
    let (w, _b, params) = game();
    let BindDecision::Bind { record } = decide_bind(
        None,
        "cli",
        w.verifying_key().to_bytes(),
        &params,
        CONTRACT,
        EntropyQuality::HostBacked,
        None,
    ) else {
        panic!("expected a bind");
    };
    record
}

fn black_record() -> GameRecord {
    let (_w, b, params) = game();
    let BindDecision::Bind { record } = decide_bind(
        None,
        "g1",
        b.verifying_key().to_bytes(),
        &params,
        CONTRACT,
        EntropyQuality::HostBacked,
        Some(ORIGIN),
    ) else {
        panic!("expected a bind");
    };
    record
}

fn mv(ply: u16, uci: &str) -> Body {
    Body::Move {
        ply,
        parent: [9u8; 32],
        uci: uci.into(),
    }
}

fn sign(record: &GameRecord, body: &Body) -> GameRecord {
    match decide_sign(record, body, Some(ORIGIN)) {
        SignDecision::Sign { updated } => updated,
        other => panic!("expected a signature, got {other:?}"),
    }
}

/// The delegate's secrets OUTLIVE the delegate. `RegisterDelegate` carries a
/// `predecessors` list and the node copies LOCAL-scope secrets forward into a
/// new delegate's namespace, so a future delegate will read records this one
/// wrote.
///
/// The danger is not a decode error -- it is a decode SUCCESS. Add a
/// `#[serde(default)]` field in some later version and serde will happily
/// deserialize an old record with `last_signed_ply` defaulted to 0, silently
/// resetting the double-sign guard on a real in-progress game. So the format
/// is checked explicitly, before any other field is trusted.
#[test]
fn a_record_from_another_format_cannot_be_signed_against() {
    let mut record = white_record();
    record.format = GAME_RECORD_FORMAT + 1;

    match decide_sign(&record, &mv(1, "e2e4"), Some(ORIGIN)) {
        SignDecision::Refuse(Refusal::StaleRecordFormat { found, expected }) => {
            assert_eq!(found, GAME_RECORD_FORMAT + 1);
            assert_eq!(expected, GAME_RECORD_FORMAT);
        }
        other => panic!("expected a format refusal, got {other:?}"),
    }
}

/// The format check must run BEFORE the origin check: if we cannot trust the
/// record's layout we cannot trust the origin field inside it either.
#[test]
fn the_format_check_precedes_every_other_check() {
    let mut record = white_record();
    record.format = GAME_RECORD_FORMAT + 1;

    // No origin at all, and a wrong-side ply. Both would normally refuse with
    // their own reason; format must win.
    assert!(matches!(
        decide_sign(&record, &mv(2, "e7e5"), None),
        SignDecision::Refuse(Refusal::StaleRecordFormat { .. })
    ));
}

#[test]
fn a_record_from_another_format_cannot_be_rebound() {
    let (w, _b, params) = game();
    let mut existing = white_record();
    existing.format = GAME_RECORD_FORMAT + 1;

    assert!(matches!(
        decide_bind(
            Some(&existing),
            "g1",
            w.verifying_key().to_bytes(),
            &params,
            CONTRACT,
            EntropyQuality::HostBacked,
            Some(ORIGIN)
        ),
        BindDecision::Refuse(Refusal::StaleRecordFormat { .. })
    ));
}

#[test]
fn binding_stamps_the_current_format() {
    assert_eq!(white_record().format, GAME_RECORD_FORMAT);
}

#[test]
fn a_second_different_move_at_a_signed_ply_is_refused() {
    // The one self-inflicted loss in the protocol, made unreachable.
    let record = sign(&white_record(), &mv(1, "e2e4"));
    assert_eq!(record.last_signed_ply, 1);

    assert!(matches!(
        decide_sign(&record, &mv(1, "d2d4"), Some(ORIGIN)),
        SignDecision::Refuse(Refusal::PlyAlreadySigned { ply: 1 })
    ));
}

#[test]
fn an_identical_move_at_a_signed_ply_is_signed_again() {
    // A dropped response must not wedge the game. ed25519 signing is
    // deterministic, so the record the UI gets back is byte-identical.
    let record = sign(&white_record(), &mv(1, "e2e4"));
    let again = sign(&record, &mv(1, "e2e4"));
    assert_eq!(record, again, "an identical retry must not change state");
}

#[test]
fn signing_one_body_twice_produces_byte_identical_records() {
    // The whole retry story rests on this: ed25519-dalek signing is
    // deterministic, so re-signing an identical body returns the same record
    // and the peer sees no new state. If this ever stopped holding, an
    // idempotent retry would start splitting into two records.
    use adjourn_core::Record;
    let (w, _b, params) = game();
    let body = mv(1, "e2e4");
    let a = Record::sign(&w, &params, body.clone());
    let b = Record::sign(&w, &params, body);
    assert_eq!(a, b);
}

#[test]
fn a_move_at_a_lower_ply_than_one_already_signed_is_refused() {
    let mut record = sign(&white_record(), &mv(1, "e2e4"));
    record = sign(&record, &mv(3, "g1f3"));
    assert!(matches!(
        decide_sign(&record, &mv(1, "e2e4"), Some(ORIGIN)),
        SignDecision::Refuse(Refusal::PlyAlreadySigned { ply: 1 })
    ));
}

#[test]
fn signing_for_the_wrong_side_is_refused() {
    // Ply 2 is Black's; this record holds White's key.
    assert!(matches!(
        decide_sign(&white_record(), &mv(2, "e7e5"), Some(ORIGIN)),
        SignDecision::Refuse(Refusal::WrongSide {
            ours: Side::White,
            ply_needs: Side::Black
        })
    ));
}

/// ...and then only a caller with the same (absent) origin may sign.
#[test]
fn a_cli_bound_game_is_signable_with_no_origin() {
    let record = cli_record();
    assert!(matches!(
        decide_sign(&record, &mv(1, "e2e4"), None),
        SignDecision::Sign { .. }
    ));
}

/// A web app cannot hijack a CLI-bound game by supplying an origin.
#[test]
fn a_web_app_cannot_sign_a_cli_bound_game() {
    let record = cli_record();
    assert!(matches!(
        decide_sign(&record, &mv(1, "e2e4"), Some(ORIGIN)),
        SignDecision::Refuse(Refusal::WrongOrigin)
    ));
}

/// And the reverse: a CLI cannot sign a game a web app bound.
#[test]
fn a_cli_cannot_sign_a_web_app_bound_game() {
    let record = white_record(); // bound with Some(ORIGIN)
    assert!(matches!(
        decide_sign(&record, &mv(1, "e2e4"), None),
        SignDecision::Refuse(Refusal::WrongOrigin)
    ));
}

#[test]
fn a_different_web_app_is_still_refused() {
    assert!(matches!(
        decide_sign(&white_record(), &mv(1, "e2e4"), Some(OTHER_ORIGIN)),
        SignDecision::Refuse(Refusal::WrongOrigin)
    ));
}

#[test]
fn rebinding_from_a_different_origin_is_refused() {
    let (w, _b, params) = game();
    let existing = white_record();
    assert!(matches!(
        decide_bind(
            Some(&existing),
            "g1",
            w.verifying_key().to_bytes(),
            &params,
            CONTRACT,
            EntropyQuality::HostBacked,
            Some(OTHER_ORIGIN)
        ),
        BindDecision::Refuse(Refusal::WrongOrigin)
    ));
}

#[test]
fn the_record_format_is_now_three() {
    assert_eq!(GAME_RECORD_FORMAT, 3);
}

#[test]
fn resign_and_draw_bodies_sign_without_touching_the_ply_counter() {
    // These are idempotent by record id (INVARIANT 2), so there is nothing to
    // guard: signing the same statement twice collapses to one slot on merge.
    let record = sign(&white_record(), &mv(1, "e2e4"));
    for body in [
        Body::Resign,
        Body::DrawOffer {
            ply: 1,
            at: [1u8; 32],
        },
        Body::DrawAccept {
            ply: 2,
            offer: [2u8; 32],
        },
    ] {
        let after = sign(&record, &body);
        assert_eq!(after.last_signed_ply, record.last_signed_ply);
        assert_eq!(after.last_move_body_hash, record.last_move_body_hash);
    }
}

/// A ply past `MAX_PLY` can never appear in a record that `Record::verify`
/// accepts, so signing one produces an unusable signature -- and worse,
/// permanently advances `last_signed_ply` past every ply the game can still
/// legitimately reach, locking the key out of its own game.
#[test]
fn a_move_past_max_ply_is_refused() {
    // MAX_PLY is even, so MAX_PLY + 1 is White's ply: `WrongSide` cannot be
    // what refuses it.
    let past = MAX_PLY + 1;
    assert!(matches!(
        decide_sign(&white_record(), &mv(past, "e2e4"), Some(ORIGIN)),
        SignDecision::Refuse(Refusal::PlyOutOfRange { ply, max })
            if ply == past && max == MAX_PLY
    ));

    // The cap itself is still signable, and the refusal does not touch the
    // counter on the way past.
    assert!(matches!(
        decide_sign(&black_record(), &mv(MAX_PLY, "e7e5"), Some(ORIGIN)),
        SignDecision::Sign { .. }
    ));
    let record = white_record();
    assert!(matches!(
        decide_sign(&record, &mv(past, "e2e4"), Some(ORIGIN)),
        SignDecision::Refuse(_)
    ));
    assert_eq!(
        record.last_signed_ply, 0,
        "a refusal never advances the ply"
    );
}

#[test]
fn ply_zero_is_refused_as_already_signed_not_mistaken_for_real() {
    // `last_signed_ply` starts at 0, and the sentinel guard
    // `record.last_signed_ply != 0` is what stops ply 0 being treated as a
    // real signed ply that a retry could match. `color_at_ply(0)` is Black,
    // so use a Black record to exercise the ply branch rather than
    // `WrongSide`.
    assert!(matches!(
        decide_sign(&black_record(), &mv(0, "e7e5"), Some(ORIGIN)),
        SignDecision::Refuse(Refusal::PlyAlreadySigned { ply: 0 })
    ));
}

#[test]
fn binding_records_the_entropy_quality() {
    let (w, _b, params) = game();
    let BindDecision::Bind { record } = decide_bind(
        None,
        "g1",
        w.verifying_key().to_bytes(),
        &params,
        CONTRACT,
        EntropyQuality::Degraded,
        Some(ORIGIN),
    ) else {
        panic!("expected a bind");
    };
    assert_eq!(record.entropy, EntropyQuality::Degraded);
}

#[test]
fn advancing_plies_updates_the_counter() {
    let mut record = white_record();
    for ply in [1u16, 3, 5, 7] {
        record = sign(&record, &mv(ply, "e2e4"));
        assert_eq!(record.last_signed_ply, ply);
    }
}

/// GameRecord is the delegate's persistence format: it lives in the secret
/// store and carries last_signed_ply, the double-sign guard. Nothing else
/// tests that it survives a round trip, and `origin` just changed shape.
#[test]
fn a_game_record_round_trips_through_cbor() {
    for record in [white_record(), cli_record()] {
        let mut buf = Vec::new();
        ciborium::into_writer(&record, &mut buf).expect("encode");
        let back: GameRecord = ciborium::from_reader(buf.as_slice()).expect("decode");
        assert_eq!(back, record, "GameRecord did not survive CBOR");
        assert_eq!(back.origin, record.origin, "origin lost its value");
        assert_eq!(
            back.last_signed_ply, record.last_signed_ply,
            "the double-sign guard did not survive"
        );
    }
}

/// `FakeNode::delegate` (in `cli/src/fake.rs`) returns the `Response` value
/// directly, so `Response::encode`/`decode` -- the real wire path a live node
/// uses -- is never exercised by the CLI's own test suite. `params` and
/// `contract` on `GameSummary` are wire format that `session::bound_game`
/// depends on to recover `GameParams` and the contract id from a label; a
/// silent CBOR round-trip bug in either field would only surface against a
/// real Freenet node. Cover it here instead.
#[test]
fn a_game_summary_with_params_and_contract_round_trips_through_cbor() {
    let (_w, _b, params) = game();
    let summary = GameSummary {
        label: "g1".into(),
        public_key: [1u8; 32],
        game_id: Some([2u8; 32]),
        side: Some(Side::White),
        last_signed_ply: 3,
        entropy: Some(EntropyQuality::HostBacked),
        params: Some(params.clone()),
        contract: Some(CONTRACT),
        previous: None,
    };
    let resp = Response::Games(vec![summary.clone()]);

    let back = Response::decode(&resp.encode()).expect("decode");
    assert_eq!(back, resp, "GameSummary did not survive CBOR");

    let Response::Games(games) = back else {
        panic!("expected Games");
    };
    assert_eq!(
        games[0].params.as_ref().map(|p| &p.white),
        Some(&params.white),
        "params must survive specifically, not just round-trip by luck"
    );
    assert_eq!(games[0].contract, Some(CONTRACT));
}

#[test]
fn the_new_and_changed_bodies_round_trip_through_cbor() {
    let (w, _b, params) = game();
    for body in [
        Body::DrawOffer {
            ply: 7,
            at: [3u8; 32],
        },
        Body::DrawAccept {
            ply: 8,
            offer: [4u8; 32],
        },
        Body::DrawClaim {
            ply: 9,
            at: [5u8; 32],
        },
    ] {
        let rec = adjourn_core::Record::sign(&w, &params, body.clone());
        let mut buf = Vec::new();
        ciborium::into_writer(&rec, &mut buf).expect("encode");
        let back: adjourn_core::Record = ciborium::from_reader(&buf[..]).expect("decode");
        assert_eq!(rec, back);
        assert_eq!(back.body, body);
        assert!(
            back.verify(&params),
            "round-tripped record must still verify"
        );
    }
}

fn sample_record() -> GameRecord {
    let (_w, _b, params) = game();
    GameRecord {
        format: GAME_RECORD_FORMAT,
        label: "alice".into(),
        params,
        side: Side::White,
        origin: None,
        contract: [3u8; 32],
        previous: None,
        entropy: EntropyQuality::HostBacked,
        last_signed_ply: 0,
        last_move_body_hash: [0u8; 32],
    }
}

#[test]
fn rebind_updates_the_contract_and_records_the_previous_one() {
    let mut rec = sample_record();
    rec.contract = [1u8; 32];
    rec.last_signed_ply = 5;
    // Distinctive and non-zero: sample_record()'s default is [0u8; 32], which a
    // dropped field would silently preserve by accident. Only a distinct value
    // proves the double-sign guard's body hash actually survives a rebind.
    rec.last_move_body_hash = [7u8; 32];

    match decide_rebind(Some(&rec), "alice", [2u8; 32], None) {
        RebindDecision::Rebind { record } => {
            assert_eq!(record.contract, [2u8; 32]);
            assert_eq!(record.previous, Some([1u8; 32]));
            assert_eq!(
                record.last_signed_ply, 5,
                "the ply counter must survive a rebind"
            );
            assert_eq!(
                record.last_move_body_hash, [7u8; 32],
                "the double-sign guard's body hash must survive a rebind"
            );
            assert_eq!(record.params, rec.params);
            assert_eq!(record.side, rec.side);
            assert_eq!(record.origin, rec.origin);
        }
        other => panic!("expected Rebind, got {other:?}"),
    }
}

/// Idempotent: rebinding to the id already recorded changes nothing, and in
/// particular must NOT set `previous` to the current id and start watching
/// an address that is the same as the live one.
#[test]
fn rebinding_to_the_same_contract_is_a_no_op() {
    let mut rec = sample_record();
    rec.contract = [1u8; 32];
    rec.previous = None;

    match decide_rebind(Some(&rec), "alice", [1u8; 32], None) {
        RebindDecision::Rebind { record } => {
            assert_eq!(record.contract, [1u8; 32]);
            assert_eq!(
                record.previous, None,
                "a no-op rebind must not invent a previous id"
            );
        }
        other => panic!("expected a no-op Rebind, got {other:?}"),
    }
}

#[test]
fn rebind_refuses_a_label_with_no_bound_game() {
    assert!(matches!(
        decide_rebind(None, "alice", [2u8; 32], None),
        RebindDecision::Refuse(Refusal::NotBound)
    ));
}

/// Same origin rule as every other call: a web-app game keeps full protection,
/// and a `None` game refuses any caller that presents an origin.
#[test]
fn rebind_refuses_a_different_origin() {
    let mut rec = sample_record();
    rec.origin = Some([9u8; 32]);
    assert!(matches!(
        decide_rebind(Some(&rec), "alice", [2u8; 32], Some([8u8; 32])),
        RebindDecision::Refuse(Refusal::WrongOrigin)
    ));
}

/// A record whose layout is not ours cannot be trusted field by field.
///
/// The record ALSO carries an origin that differs from the caller's (`None`
/// here, `Some([8u8; 32])` on the record), so this only passes if the format
/// check runs BEFORE the origin check. With both `None`, a version that
/// checked origin first would also return `StaleRecordFormat` by accident
/// (there'd be nothing to disagree about) and this test would not catch the
/// reordering. Do not "simplify" this back to a bare `None`/`None` pair.
#[test]
fn rebind_refuses_an_unmigratable_format() {
    let mut rec = sample_record();
    rec.format = 200;
    rec.origin = Some([8u8; 32]);
    assert!(matches!(
        decide_rebind(Some(&rec), "alice", [2u8; 32], None),
        RebindDecision::Refuse(Refusal::StaleRecordFormat { .. })
    ));
}

/// A format-2 record must be UPGRADED, not rejected. Refusing it would break
/// every game already bound — the games this feature exists to save.
#[test]
fn a_format_2_record_migrates_to_the_current_format() {
    let mut old = sample_record();
    old.format = 2;
    old.previous = None;
    old.last_signed_ply = 7;

    let migrated = migrate_record(old.clone()).expect("format 2 must migrate");
    assert_eq!(migrated.format, GAME_RECORD_FORMAT);
    assert_eq!(
        migrated.previous, None,
        "a format-2 record has no previous id"
    );
}

/// The whole point of the format field. If migration silently zeroed this,
/// the double-sign guard would be disarmed on a live game.
#[test]
fn migration_preserves_last_signed_ply_and_every_other_field() {
    let mut old = sample_record();
    old.format = 2;
    old.last_signed_ply = 9;
    old.last_move_body_hash = [7u8; 32];

    let migrated = migrate_record(old.clone()).expect("format 2 must migrate");
    assert_eq!(
        migrated.last_signed_ply, 9,
        "ply counter must survive migration"
    );
    assert_eq!(migrated.last_move_body_hash, [7u8; 32]);
    assert_eq!(migrated.label, old.label);
    assert_eq!(migrated.params, old.params);
    assert_eq!(migrated.side, old.side);
    assert_eq!(migrated.origin, old.origin);
    assert_eq!(migrated.contract, old.contract);
    assert_eq!(migrated.entropy, old.entropy);
}

/// Migrate the shapes we know; refuse the rest. Never widen the check.
#[test]
fn an_unknown_format_does_not_migrate() {
    for bad in [0u8, 1, 4, 255] {
        let mut rec = sample_record();
        rec.format = bad;
        assert!(
            migrate_record(rec).is_none(),
            "format {bad} must not migrate — widening this check is how a \
             future layout gets silently misread"
        );
    }
}

#[test]
fn a_current_format_record_passes_through_unchanged() {
    let rec = sample_record();
    assert_eq!(rec.format, GAME_RECORD_FORMAT);
    assert_eq!(migrate_record(rec.clone()), Some(rec));
}
