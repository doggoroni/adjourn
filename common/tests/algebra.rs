//! The tests that matter: the monoid laws, projection determinism under
//! arbitrary delivery order, and the adversarial cases.

use adjourn_core::types::signing_payload;
use adjourn_core::*;
use ed25519_dalek::SigningKey;
use shakmaty::Color;

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

/// Play a sequence of UCI moves, returning the resulting state.
fn play(moves: &[&str]) -> (GameState, GameParams, SigningKey, SigningKey) {
    let (w, b, params) = keys();
    let mut state = GameState::empty();
    for (i, uci) in moves.iter().enumerate() {
        let key = if i % 2 == 0 { &w } else { &b };
        let rec = make_move(&state, &params, key, uci)
            .unwrap_or_else(|| panic!("move {} ({}) rejected", i + 1, uci));
        assert!(state.insert_verified(&rec, &params));
    }
    (state, params, w, b)
}

/// A second, different, but fully VALID signature over the same body.
///
/// ed25519 verification does not pin the nonce, so re-signing with a different
/// nonce prefix yields another signature that verifies just as well. This is
/// what a player running their own signer can do, and what the id-excludes-the
/// -signature rule has to survive.
fn second_valid_signature(key: &SigningKey, params: &GameParams, body: &Body) -> Record {
    use ed25519_dalek::hazmat::{raw_sign, ExpandedSecretKey};
    use sha2::Sha512;

    let payload = signing_payload(&params.game_id(), body);
    let mut esk = ExpandedSecretKey::from(&key.to_bytes());
    esk.hash_prefix[0] ^= 0x01;
    Record {
        body: body.clone(),
        signer: key.verifying_key().to_bytes(),
        sig: raw_sign::<Sha512>(&esk, &payload, &key.verifying_key())
            .to_bytes()
            .to_vec(),
    }
}

/// Deterministic xorshift so shuffles are reproducible.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const SCHOLARS: &[&str] = &["e2e4", "e7e5", "f1c4", "b8c6", "d1h5", "g8f6", "h5f7"];

#[test]
fn scholars_mate_is_detected() {
    let (state, params, _, _) = play(SCHOLARS);
    let st = project(&state, &params);
    assert_eq!(st.ply, 7);
    assert_eq!(
        st.decision,
        Some(Decision {
            winner: Some(Color::White),
            reason: Reason::Checkmate
        })
    );
}

#[test]
fn merge_is_a_monoid() {
    let (full, params, _, _) = play(SCHOLARS);
    let recs: Vec<Record> = full.records.values().cloned().collect();
    let mut rng = Rng(0xDEADBEEF);

    for _ in 0..200 {
        // Split records into three arbitrary buckets.
        let (mut a, mut b, mut c) = (GameState::empty(), GameState::empty(), GameState::empty());
        for r in &recs {
            match rng.below(3) {
                0 => a.insert_verified(r, &params),
                1 => b.insert_verified(r, &params),
                _ => c.insert_verified(r, &params),
            };
        }

        // Commutative
        assert_eq!(
            a.merged(&b, &params),
            b.merged(&a, &params),
            "merge not commutative"
        );

        // Associative
        assert_eq!(
            a.merged(&b, &params).merged(&c, &params),
            a.merged(&b.merged(&c, &params), &params),
            "merge not associative"
        );

        // Idempotent
        assert_eq!(a.merged(&a, &params), a, "merge not idempotent");
        let ab = a.merged(&b, &params);
        assert_eq!(
            ab.merged(&b, &params),
            ab,
            "re-merging a subset changed state"
        );

        // Identity
        assert_eq!(
            a.merged(&GameState::empty(), &params),
            a,
            "empty is not identity"
        );
    }
}

#[test]
fn projection_is_order_independent() {
    let (full, params, _, _) = play(SCHOLARS);
    let expected = project(&full, &params);
    let recs: Vec<Record> = full.records.values().cloned().collect();
    let mut rng = Rng(0x1234_5678);

    for _ in 0..500 {
        // Deliver records in a random order, with random duplicates.
        let mut shuffled = recs.clone();
        for i in (1..shuffled.len()).rev() {
            shuffled.swap(i, rng.below(i + 1));
        }
        let mut s = GameState::empty();
        for r in &shuffled {
            s.insert_verified(r, &params);
            if rng.below(4) == 0 {
                s.insert_verified(r, &params); // duplicate delivery
            }
        }
        let got = project(&s, &params);
        assert_eq!(got.fen, expected.fen, "divergent position");
        assert_eq!(got.decision, expected.decision, "divergent outcome");
        assert_eq!(got.chain, expected.chain, "divergent chain");
    }
}

#[test]
fn partial_state_projects_to_a_prefix() {
    // A peer that has only the first N records must see exactly the first N
    // plies, never a gap-skipping position.
    let (full, params, _, _) = play(SCHOLARS);
    let full_status = project(&full, &params);

    for n in 0..=SCHOLARS.len() {
        let mut s = GameState::empty();
        for id in full_status.chain.iter().take(n) {
            s.insert_verified(full.records.get(id).unwrap(), &params);
        }
        assert_eq!(project(&s, &params).ply, n as u16);
    }

    // Records delivered out of order with a hole: ply 1,2 present, 3 missing,
    // 4..7 present. Chain must stop at 2.
    let mut s = GameState::empty();
    for (i, id) in full_status.chain.iter().enumerate() {
        if i == 2 {
            continue;
        }
        s.insert_verified(full.records.get(id).unwrap(), &params);
    }
    assert_eq!(project(&s, &params).ply, 2);
}

#[test]
fn double_signing_forfeits() {
    let (state, params, _w, b) = play(&["e2e4"]);
    let mut forked = state.clone();

    // Black signs two different legal replies to the same parent.
    for uci in ["e7e5", "c7c5"] {
        let rec = make_move(&state, &params, &b, uci).unwrap();
        assert!(forked.insert_verified(&rec, &params));
    }

    let st = project(&forked, &params);
    assert_eq!(
        st.decision,
        Some(Decision {
            winner: Some(Color::White),
            reason: Reason::DoubleSignForfeit
        })
    );
    // And it is order-independent, like everything else.
    let mut rebuilt = GameState::empty();
    for r in forked.records.values().rev() {
        rebuilt.insert_verified(r, &params);
    }
    assert_eq!(project(&rebuilt, &params).decision, st.decision);
}

#[test]
fn illegal_move_is_ignored_not_fatal() {
    let (state, params, w, b) = play(&["e2e4"]);
    let mut poisoned = state.clone();

    // Black signs a structurally valid but chess-illegal move.
    let junk = Record::sign(
        &b,
        &params,
        Body::Move {
            ply: 2,
            parent: project(&state, &params).chain[0],
            uci: "a1a8".into(),
        },
    );
    assert!(poisoned.insert_verified(&junk, &params));

    // State stays valid; the game simply hasn't advanced.
    assert!(poisoned.all_valid(&params));
    assert_eq!(project(&poisoned, &params).ply, 1);

    // Black can still play a real move afterwards, and it is NOT a forfeit
    // because only one candidate is legal.
    let real = make_move(&poisoned, &params, &b, "e7e5").unwrap();
    poisoned.insert_verified(&real, &params);
    let st = project(&poisoned, &params);
    assert_eq!(st.ply, 2);
    assert_eq!(st.decision, None);
    assert_eq!(st.ignored, 1);

    // White is unaffected and play continues.
    let nxt = make_move(&poisoned, &params, &w, "g1f3").unwrap();
    poisoned.insert_verified(&nxt, &params);
    assert_eq!(project(&poisoned, &params).ply, 3);
}

#[test]
fn wrong_turn_and_forgery_are_rejected() {
    let (state, params, w, _b) = play(&["e2e4"]);

    // White tries to move twice in a row.
    assert!(make_move(&state, &params, &w, "d2d4").is_none());

    // A third party signs a move.
    let outsider = SigningKey::from_bytes(&[9u8; 32]);
    let rec = Record::sign(
        &outsider,
        &params,
        Body::Move {
            ply: 2,
            parent: params.genesis(),
            uci: "e7e5".into(),
        },
    );
    let mut s = state.clone();
    assert!(!s.insert_verified(&rec, &params));
    assert_eq!(s.len(), state.len());

    // A tampered signature.
    let mut tampered = state.records.values().next().unwrap().clone();
    tampered.sig[0] ^= 0xFF;
    assert!(!tampered.verify(&params));
}

#[test]
fn moves_do_not_replay_across_games() {
    let (w, b, params) = keys();
    let other = GameParams {
        nonce: [8u8; 16],
        ..params.clone()
    };
    assert_ne!(params.game_id(), other.game_id());
    assert_ne!(params.genesis(), other.genesis());

    let mut s = GameState::empty();
    let rec = make_move(&s, &params, &w, "e2e4").unwrap();
    s.insert_verified(&rec, &params);

    // The same signed record offered to the sibling game fails verification.
    let mut s2 = GameState::empty();
    assert!(!s2.insert_verified(&rec, &other));
    assert!(s2.is_empty());
    let _ = b;
}

#[test]
fn sync_soundness_two_step() {
    // Property 1 from the whitepaper: applying B's delta to A brings A at
    // least as far up the lattice as merging B in full would have.
    let (full, params, _, _) = play(SCHOLARS);
    let recs: Vec<Record> = full.records.values().cloned().collect();
    let mut rng = Rng(0xC0FFEE);

    for _ in 0..300 {
        let (mut a, mut b) = (GameState::empty(), GameState::empty());
        for r in &recs {
            if rng.below(2) == 0 {
                a.insert_verified(r, &params);
            }
            if rng.below(2) == 0 {
                b.insert_verified(r, &params);
            }
        }

        let summary = a.summarize();
        let delta = b.delta_against(&summary);
        let mut a_after = a.clone();
        a_after.apply_delta(&delta, &params);

        assert_eq!(
            a_after.merged(&b, &params),
            a_after,
            "delta did not dominate the peer's state"
        );
        assert_eq!(a_after, a.merged(&b, &params), "delta sync != full merge");

        // A second round is a no-op: replicas have converged.
        let d2 = b.delta_against(&a_after.summarize());
        assert!(d2.is_empty(), "sync did not converge in one round");
    }
}

#[test]
fn resignation_and_draw_agreement() {
    let (state, params, _w, b) = play(&["e2e4", "e7e5"]);

    let mut resigned = state.clone();
    resigned.insert_verified(&Record::sign(&b, &params, Body::Resign), &params);
    assert_eq!(
        project(&resigned, &params).decision,
        Some(Decision {
            winner: Some(Color::White),
            reason: Reason::Resignation
        })
    );

    // Draw: offer anchored at the head, accepted by the opponent.
    let (state, params, w, b) = play(&["e2e4", "e7e5"]);
    let head = project(&state, &params).chain.last().copied().unwrap();
    let offer = Record::sign(&w, &params, Body::DrawOffer { at: head });
    let accept = Record::sign(&b, &params, Body::DrawAccept { offer: offer.id() });
    let mut drawn = state.clone();
    drawn.insert_verified(&offer, &params);
    drawn.insert_verified(&accept, &params);
    assert_eq!(
        project(&drawn, &params).decision,
        Some(Decision {
            winner: None,
            reason: Reason::DrawAgreement
        })
    );

    // Self-accepting your own offer does nothing.
    let mut sneaky = state.clone();
    let offer2 = Record::sign(&w, &params, Body::DrawOffer { at: head });
    let self_accept = Record::sign(&w, &params, Body::DrawAccept { offer: offer2.id() });
    sneaky.insert_verified(&offer2, &params);
    sneaky.insert_verified(&self_accept, &params);
    assert_eq!(project(&sneaky, &params).decision, None);
}

#[test]
fn signature_malleability_does_not_split_records() {
    // Two records with the same statement but different signature bytes must
    // collapse to one entry, and merge must pick the same one either way.
    let (w, _b, params) = keys();
    let body = Body::Move {
        ply: 1,
        parent: params.genesis(),
        uci: "e2e4".into(),
    };
    let a = Record::sign(&w, &params, body.clone());
    let mut mangled = a.clone();
    mangled.sig[63] ^= 0x01; // same id, different (invalid) signature bytes

    assert_eq!(a.id(), mangled.id(), "id must not depend on signature");

    // A mangled signature does not verify, so it never displaces the real one,
    // in either delivery order.
    for order in [[&a, &mangled], [&mangled, &a]] {
        let mut s = GameState::empty();
        for rec in order {
            s.insert_verified(rec, &params);
        }
        assert_eq!(s.len(), 1, "one statement, one slot");
        assert_eq!(s.records.values().next().unwrap(), &a, "forgery won");
    }

    // Two genuinely VALID signatures over one body also collapse to one slot,
    // and to the same one regardless of order.
    let alt = second_valid_signature(&w, &params, &body);
    assert!(alt.verify(&params) && alt.sig != a.sig);
    assert_eq!(a.id(), alt.id());

    let mut s1 = GameState::empty();
    s1.insert_verified(&a, &params);
    s1.insert_verified(&alt, &params);
    let mut s2 = GameState::empty();
    s2.insert_verified(&alt, &params);
    s2.insert_verified(&a, &params);
    assert_eq!(s1, s2, "collision tiebreak is order-dependent");
    assert_eq!(s1.len(), 1);

    // ...and the summary distinguishes them, so a peer holding the loser is
    // told to catch up rather than being told it is already in sync.
    let mut loser = GameState::empty();
    loser.insert_verified(if a.sig < alt.sig { &alt } else { &a }, &params);
    assert_ne!(loser.summarize(), s1.summarize());
    assert_eq!(s1.delta_against(&loser.summarize()).len(), 1);

    // Sanity: the payload really is what we think it is.
    let payload = signing_payload(&params.game_id(), &body);
    assert!(payload.starts_with(b"freenet-chess-v1/sig"));
}

#[test]
fn round_trips_through_cbor() {
    let (full, params, _, _) = play(SCHOLARS);
    let bytes = full.encode();
    let back = GameState::decode(&bytes).expect("decode");
    assert_eq!(back, full);
    assert_eq!(project(&back, &params).fen, project(&full, &params).fen);

    // Encoding is canonical: same logical state -> identical bytes.
    let mut rebuilt = GameState::empty();
    for r in full.records.values().rev() {
        rebuilt.insert_verified(r, &params);
    }
    assert_eq!(rebuilt.encode(), bytes, "encoding is not canonical");
    println!(
        "full-game state: {} bytes, {} records",
        bytes.len(),
        full.len()
    );
}
