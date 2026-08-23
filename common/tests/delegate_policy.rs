use chess_core::delegate_api::{EntropyQuality, Refusal, Request, Response, Side};
use chess_core::delegate_policy::{
    classify_host_entropy, decide_bind, derive_seed, BindDecision, HostEntropy,
};
use chess_core::{Body, GameParams};
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

#[test]
fn binding_without_an_origin_is_refused() {
    let (w, _b, params) = game();
    assert!(matches!(
        decide_bind(
            None,
            "g1",
            w.verifying_key().to_bytes(),
            &params,
            CONTRACT,
            None
        ),
        BindDecision::Refuse(Refusal::MissingOrigin)
    ));
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
        Some(ORIGIN),
    ) else {
        panic!("expected a bind");
    };
    assert_eq!(record.side, Side::White);
    assert_eq!(record.origin, ORIGIN);
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
        Some(ORIGIN),
    ) else {
        panic!("expected an idempotent re-bind");
    };
    assert_eq!(record, again);
}
