//! Adversarial review: attacks on convergence, projection order-independence,
//! and the chess edges. Each test documents CURRENT behaviour; the ones marked
//! BUG assert the broken behaviour so that a fix flips them.

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

const SCHOLARS: &[&str] = &["e2e4", "e7e5", "f1c4", "b8c6", "d1h5", "g8f6", "h5f7"];

/// A record with the same (signer, body) as `rec`, hence the same id, but an
/// all-zero signature: both invalid and lexicographically minimal.
fn poison(rec: &Record) -> Record {
    Record {
        body: rec.body.clone(),
        signer: rec.signer,
        sig: vec![0u8; 64],
    }
}

// ---------------------------------------------------------------------------
// 1. Convergence
// ---------------------------------------------------------------------------

/// An all-zero signature is lexicographically minimal, so under a naive
/// min(sig) tiebreak it evicts the honest record. The tiebreak must only ever
/// run between records that VERIFY, so the forgery is refused on every route
/// into state -- verified delta and raw state merge alike.
#[test]
fn forged_signature_cannot_evict_the_valid_record() {
    let (state, params, _, _) = play(&["e2e4"]);
    let good = state.records.values().next().unwrap().clone();
    let bad = poison(&good);
    assert_eq!(good.id(), bad.id(), "poison must collide by id");
    assert!(good.verify(&params) && !bad.verify(&params));
    assert!(bad.sig < good.sig, "poison must win a naive byte tiebreak");

    // Peer A admits records through the verifying delta path.
    let mut a = state.clone();
    a.apply_delta(&vec![bad.clone()], &params);

    // Peer B receives a whole state (UpdateData::State) and merges it.
    let mut b_incoming = GameState::empty();
    b_incoming.absorb_for_test(&bad);
    let mut b = state.clone();
    b.merge(&b_incoming, &params);

    assert_eq!(a, state, "delta path admitted a forgery");
    assert_eq!(b, state, "merge path admitted a forgery");
    assert!(a.all_valid(&params) && b.all_valid(&params));
}

/// Merge and validation must commute, or two peers that validate at different
/// points in the pipeline reach different states.
#[test]
fn merge_and_filter_commute() {
    let (state, params, _, _) = play(&["e2e4"]);
    let good = state.records.values().next().unwrap().clone();

    let mut attacker = GameState::empty();
    attacker.absorb_for_test(&poison(&good));
    let honest = state.clone();

    let filter_then_merge = attacker
        .filter_valid(&params)
        .merged(&honest.filter_valid(&params), &params);
    let merge_then_filter = attacker.merged(&honest, &params).filter_valid(&params);

    assert_eq!(filter_then_merge, merge_then_filter);
    assert_eq!(filter_then_merge.len(), 1, "the honest move must survive");
    assert_eq!(filter_then_merge, state);
}

/// A strict superset moves the projection DOWN: a decided game (mate at ply 7)
/// becomes an undecided-looking position at ply 2 with the opposite winner.
/// Deterministic, so peers still agree, but the result is not monotone and the
/// reported fen/ply rewind to the fork point.
#[test]
fn superset_reverses_the_outcome_and_rewinds_the_board() {
    let (state, params, w, _) = play(SCHOLARS);
    let before = project(&state, &params);
    assert_eq!(before.ply, 7);
    assert_eq!(before.decision.unwrap().winner, Some(Color::White));

    // White signs a second, different, legal move at ply 3.
    let fork = Record::sign(
        &w,
        &params,
        Body::Move {
            ply: 3,
            parent: before.chain[1],
            uci: "g1f3".into(),
        },
    );
    let mut bigger = state.clone();
    assert!(bigger.insert_verified(&fork, &params));

    let after = project(&bigger, &params);
    assert_eq!(after.decision.unwrap().reason, Reason::DoubleSignForfeit);
    assert_eq!(after.decision.unwrap().winner, Some(Color::Black));
    assert_eq!(after.ply, 2, "chain truncated to the fork point");
    assert_ne!(after.fen, before.fen, "reported position rewound");
}

// ---------------------------------------------------------------------------
// 2. Outcome precedence
// ---------------------------------------------------------------------------

/// `Resign` is unanchored and unconditional: once said it cannot be taken back,
/// so no later board result may override it. Otherwise a player who resigns and
/// then delivers mate is awarded the win.
#[test]
fn resignation_outranks_a_later_board_result() {
    let (state, params, w, _) = play(SCHOLARS);
    assert_eq!(
        project(&state, &params).decision.unwrap().reason,
        Reason::Checkmate,
        "precondition: this line is mate for White"
    );

    let mut s = state.clone();
    assert!(s.insert_verified(&Record::sign(&w, &params, Body::Resign), &params));

    let d = project(&s, &params).decision.unwrap();
    assert_eq!(d.reason, Reason::Resignation);
    assert_eq!(d.winner, Some(Color::Black), "White resigned; White loses");
}

/// A draw offer is anchored to a head, and expires implicitly once the game
/// moves on. Black offers at ply 2 as a courtesy; White plays on, dislikes the
/// position, and tries to cash the stale offer at ply 6.
#[test]
fn stale_draw_offer_is_ignored() {
    let (state, params, w, b) = play(&["e2e4", "e7e5", "g1f3", "b8c6", "f1c4", "g8f6"]);
    let chain = project(&state, &params).chain;

    let offer = Record::sign(&b, &params, Body::DrawOffer { at: chain[1] });
    let accept = Record::sign(&w, &params, Body::DrawAccept { offer: offer.id() });

    let mut s = state.clone();
    assert!(s.insert_verified(&offer, &params));
    assert!(s.insert_verified(&accept, &params));

    let st = project(&s, &params);
    assert_eq!(st.ply, 6, "game is four plies past the offer");
    assert_eq!(
        st.decision, None,
        "an offer from ply 2 must not bind at ply 6"
    );
}

/// The other half of the rule: an offer anchored to the CURRENT head is live,
/// and the opponent can accept it.
#[test]
fn draw_offer_at_the_current_head_is_accepted() {
    let (state, params, w, b) = play(&["e2e4", "e7e5", "g1f3", "b8c6", "f1c4", "g8f6"]);
    let head = *project(&state, &params).chain.last().unwrap();

    let offer = Record::sign(&b, &params, Body::DrawOffer { at: head });
    let accept = Record::sign(&w, &params, Body::DrawAccept { offer: offer.id() });

    let mut s = state.clone();
    assert!(s.insert_verified(&offer, &params));
    assert!(s.insert_verified(&accept, &params));

    assert_eq!(
        project(&s, &params).decision.unwrap().reason,
        Reason::DrawAgreement
    );
}

/// Only the acceptor can advance the head past a live offer — they are the one
/// to move — so an acceptance can be voided only by the acceptor's own move,
/// never by the offerer. That is what makes head-binding safe rather than a
/// race.
///
/// When the acceptor both accepts and moves at the same head, the move wins.
/// The two orderings ("accepted, then reneged" and "moved, then cashed a stale
/// offer") produce *identical record sets*, so no pure function of the set can
/// tell them apart and the precedence has to be fixed by fiat. Letting the move
/// win is the safe half: it never ends a game someone is still playing.
///
/// `make_move` refuses to build this record — the projection says drawn — so an
/// attacker has to sign it directly, which is exactly what this simulates.
#[test]
fn accepting_and_then_moving_voids_your_own_acceptance() {
    let (state, params, w, b) = play(&["e2e4", "e7e5", "g1f3", "b8c6", "f1c4", "g8f6"]);
    let head = *project(&state, &params).chain.last().unwrap();

    let offer = Record::sign(&b, &params, Body::DrawOffer { at: head });
    let accept = Record::sign(&w, &params, Body::DrawAccept { offer: offer.id() });

    let mut s = state.clone();
    assert!(s.insert_verified(&offer, &params));
    assert!(s.insert_verified(&accept, &params));
    assert_eq!(
        project(&s, &params).decision.unwrap().reason,
        Reason::DrawAgreement,
        "precondition: the offer is live and accepted"
    );
    assert!(
        make_move(&s, &params, &w, "c4f7").is_none(),
        "an honest client must not be able to renege"
    );

    let reneged = Record::sign(
        &w,
        &params,
        Body::Move {
            ply: 7,
            parent: head,
            uci: "c4f7".into(),
        },
    );
    assert!(s.insert_verified(&reneged, &params));

    let st = project(&s, &params);
    assert_eq!(st.ply, 7);
    assert_eq!(st.decision, None, "the move supersedes the acceptance");
}

// ---------------------------------------------------------------------------
// 3. Chess edges
// ---------------------------------------------------------------------------

const PROMO_LINE: &[&str] = &[
    "e2e4", "d7d5", "e4d5", "c7c6", "d5c6", "g8f6", "c6b7", "e7e6",
];

#[test]
fn queen_promotion_and_underpromotion_round_trip() {
    let (state, params, w, _) = play(PROMO_LINE);
    for uci in ["b7a8q", "b7a8n", "b7c8q", "b7c8r"] {
        let rec = make_move(&state, &params, &w, uci)
            .unwrap_or_else(|| panic!("promotion {uci} rejected"));
        let mut s = state.clone();
        assert!(s.insert_verified(&rec, &params));
        assert_eq!(
            project(&s, &params).ply,
            9,
            "promotion {uci} did not project"
        );
    }
    let moves = legal_moves(&state, &params);
    assert!(moves.contains(&"b7a8q".to_string()));
    assert!(moves.contains(&"b7a8n".to_string()));
}

#[test]
fn promotion_without_a_piece_suffix_is_rejected() {
    let (state, params, w, _) = play(PROMO_LINE);
    assert!(make_move(&state, &params, &w, "b7a8").is_none());
}

const CASTLE_LINE: &[&str] = &["e2e4", "e7e5", "g1f3", "b8c6", "f1c4", "f8c5"];

#[test]
fn kingside_castling_uses_king_target_notation() {
    let (state, params, w, _) = play(CASTLE_LINE);
    let moves = legal_moves(&state, &params);
    assert!(
        moves.contains(&"e1g1".to_string()),
        "expected e1g1, got {moves:?}"
    );

    let rec = make_move(&state, &params, &w, "e1g1").expect("castling rejected");
    let mut s = state.clone();
    assert!(s.insert_verified(&rec, &params));
    assert!(
        project(&s, &params).fen.contains("RK1"),
        "rook did not move: {}",
        project(&s, &params).fen
    );
}

/// shakmaty accepts both `e1g1` (king target) and `e1h1` (rook target, the
/// Chess960 spelling) for the same castling move. Two spellings would be two
/// bodies with two ids for one move, so `make_move` must canonicalise: one
/// move, one record, whatever the caller typed.
#[test]
fn make_move_canonicalises_castling_notation() {
    let (state, params, w, _) = play(CASTLE_LINE);
    let king_target = make_move(&state, &params, &w, "e1g1").expect("e1g1");
    let rook_target = make_move(&state, &params, &w, "e1h1").expect("e1h1");

    assert_eq!(
        king_target.id(),
        rook_target.id(),
        "the two spellings must produce one record"
    );
    assert!(
        matches!(&king_target.body, Body::Move { uci, .. } if uci == "e1g1"),
        "canonical form should be the king-target spelling"
    );
}

/// And a hostile client that signs both spellings directly, bypassing
/// `make_move`, must not be read as a double-sign: they are one move.
#[test]
fn two_spellings_of_one_castling_move_do_not_forfeit() {
    let (state, params, w, _) = play(CASTLE_LINE);
    let head = *project(&state, &params).chain.last().unwrap();

    let mut s = state.clone();
    for uci in ["e1g1", "e1h1"] {
        let rec = Record::sign(
            &w,
            &params,
            Body::Move {
                ply: 7,
                parent: head,
                uci: uci.into(),
            },
        );
        assert!(s.insert_verified(&rec, &params));
    }
    assert_eq!(s.len(), state.len() + 2, "two distinct bodies are in state");

    let st = project(&s, &params);
    assert_eq!(st.decision, None, "one move must not read as a double-sign");
    assert_eq!(st.ply, 7, "and the move should still be played");
}

#[test]
fn en_passant_capture_projects() {
    let (state, params, _, _) = play(&["e2e4", "a7a6", "e4e5", "d7d5", "e5d6"]);
    let st = project(&state, &params);
    assert_eq!(st.ply, 5);
    assert!(
        st.fen.starts_with("rnbqkbnr/1pp1pppp/p2P4"),
        "en passant capture wrong: {}",
        st.fen
    );
}

/// One full knight shuffle returns to the starting position.
fn shuffle(cycles: usize) -> Vec<&'static str> {
    let mut line = Vec::new();
    for _ in 0..cycles {
        line.extend_from_slice(&["g1f3", "b8c6", "f3g1", "c6b8"]);
    }
    line
}

/// `shakmaty::Chess` tracks no history, so repetition has to be counted while
/// walking the chain. Without it there is no automatic draw at all — and with
/// no timers either, a shuffling game would be immortal.
///
/// Fivefold is the FIDE rule that fires by itself (9.6.1); threefold is a
/// *claim*, so it is reported in `Status` rather than forced here.
#[test]
fn fivefold_repetition_draws_automatically() {
    // The initial position counts as the first occurrence, so one cycle per
    // further occurrence: cycles 1..=3 give four, and the fourth gives five.
    let (state, params, _, _) = play(&shuffle(3));
    let st = project(&state, &params);
    assert_eq!(st.ply, 12);
    assert_eq!(st.repetitions, 4, "start position seen four times");
    assert_eq!(st.decision, None, "four occurrences is not yet five");

    let (state, params, _, _) = play(&shuffle(4));
    let st = project(&state, &params);
    assert_eq!(st.ply, 16);
    assert_eq!(st.repetitions, 5);
    assert_eq!(
        st.decision,
        Some(Decision {
            winner: None,
            reason: Reason::AutomaticDraw
        })
    );
}

/// The draw is terminal: moves signed after it are ignored, so a player cannot
/// play on out of a drawn game.
#[test]
fn moves_after_an_automatic_draw_are_ignored() {
    let (state, params, w, _) = play(&shuffle(4));
    let head = *project(&state, &params).chain.last().unwrap();

    let after = Record::sign(
        &w,
        &params,
        Body::Move {
            ply: 17,
            parent: head,
            uci: "g1f3".into(),
        },
    );
    let mut s = state.clone();
    assert!(s.insert_verified(&after, &params));

    let st = project(&s, &params);
    assert_eq!(st.ply, 16, "the chain must not extend past the draw");
    assert_eq!(st.decision.unwrap().reason, Reason::AutomaticDraw);
}

/// Threefold is claimable, not automatic: the projection reports it and plays
/// on, matching FIDE 9.2.
#[test]
fn threefold_repetition_is_reported_but_not_forced() {
    let (state, params, _, _) = play(&shuffle(2));
    let st = project(&state, &params);
    assert_eq!(st.repetitions, 3);
    assert_eq!(
        st.decision, None,
        "threefold must not end the game by itself"
    );
}

#[test]
fn halfmove_clock_tracks_quiet_and_resetting_moves() {
    let (state, params, _, _) = play(&["g1f3", "b8c6"]);
    assert_eq!(
        project(&state, &params).halfmove_clock,
        2,
        "two quiet moves"
    );

    let (state, params, _, _) = play(&["e2e4", "e7e5", "g1f3"]);
    assert_eq!(
        project(&state, &params).halfmove_clock,
        1,
        "pawn moves reset the clock"
    );
}

/// The harder version of the tiebreak problem, and the one that survives any
/// "prefer the valid signature" fix.
///
/// ed25519 verification does not pin the nonce, so a player running their own
/// signer can emit TWO VALID signatures over the same body. Both records are
/// valid and share an id, and merge keeps min(sig) -- so the summary must
/// distinguish them, or a peer holding the other one is told it is already in
/// sync and the states differ byte-for-byte forever.
#[test]
fn two_valid_signatures_on_one_body_converge_in_one_round() {
    use ed25519_dalek::hazmat::{raw_sign, ExpandedSecretKey};
    use sha2::Sha512;

    let (w, _b, params) = keys();
    let body = Body::Move {
        ply: 1,
        parent: params.genesis(),
        uci: "e2e4".into(),
    };
    let payload = adjourn_core::types::signing_payload(&params.game_id(), &body);

    let canonical = Record::sign(&w, &params, body.clone());

    // Same secret scalar, different nonce prefix => a different, valid signature.
    let mut esk = ExpandedSecretKey::from(&w.to_bytes());
    esk.hash_prefix[0] ^= 0x01;
    let alt_sig = raw_sign::<Sha512>(&esk, &payload, &w.verifying_key());
    let alt = Record {
        body,
        signer: w.verifying_key().to_bytes(),
        sig: alt_sig.to_bytes().to_vec(),
    };

    assert!(alt.verify(&params), "second signature must also be valid");
    assert_ne!(canonical.sig, alt.sig, "signatures must differ");
    assert_eq!(canonical.id(), alt.id(), "same statement, same id");

    // Two honest peers, each having seen one of the two.
    let mut a = GameState::empty();
    assert!(a.insert_verified(&canonical, &params));
    let mut b = GameState::empty();
    assert!(b.insert_verified(&alt, &params));

    assert!(
        a.all_valid(&params) && b.all_valid(&params),
        "both are valid"
    );
    assert_ne!(
        a.summarize(),
        b.summarize(),
        "the summary must distinguish two signatures over one body"
    );

    // One exchange in each direction, and the two peers agree byte for byte.
    let to_b = a.delta_against(&b.summarize());
    let to_a = b.delta_against(&a.summarize());
    assert!(!to_b.is_empty() && !to_a.is_empty(), "neither side offered");
    a.apply_delta(&to_a, &params);
    b.apply_delta(&to_b, &params);

    assert_eq!(a, b, "peers did not converge");
    assert_eq!(a.encode(), b.encode(), "encodings differ after convergence");
    assert_eq!(a.len(), 1, "the two records must collapse to one slot");

    // And a second round is a no-op.
    assert!(a.delta_against(&b.summarize()).is_empty());
    assert!(b.delta_against(&a.summarize()).is_empty());
}

/// The same case stated against the whitepaper's own Property 1 (sync
/// soundness, §4.4):
///
///   for all s_A, s_B:  let s = summarize(s_B), d = getDelta(s_A, s);
///                      then applyDelta(s_B, d) JOIN s_A == applyDelta(s_B, d)
///
/// `sync_soundness_two_step` in the main suite exercises this only over record
/// sets with no id collision. This is the collision case.
#[test]
fn property_1_sync_soundness_holds_on_a_signature_collision() {
    use ed25519_dalek::hazmat::{raw_sign, ExpandedSecretKey};
    use sha2::Sha512;

    let (w, _b, params) = keys();
    let body = Body::Move {
        ply: 1,
        parent: params.genesis(),
        uci: "e2e4".into(),
    };
    let payload = adjourn_core::types::signing_payload(&params.game_id(), &body);

    let canonical = Record::sign(&w, &params, body.clone());
    let mut esk = ExpandedSecretKey::from(&w.to_bytes());
    esk.hash_prefix[0] ^= 0x01;
    let alt = Record {
        body,
        signer: w.verifying_key().to_bytes(),
        sig: raw_sign::<Sha512>(&esk, &payload, &w.verifying_key())
            .to_bytes()
            .to_vec(),
    };

    // Orient the pair so that A holds the record that WINS the min(sig)
    // tiebreak; then joining A must move B, while the delta cannot.
    let (lo, hi) = if canonical.sig < alt.sig {
        (canonical, alt)
    } else {
        (alt, canonical)
    };
    let mut a = GameState::empty();
    assert!(a.insert_verified(&lo, &params));
    let mut b = GameState::empty();
    assert!(b.insert_verified(&hi, &params));

    let delta = a.delta_against(&b.summarize());
    let mut b_after = b.clone();
    b_after.apply_delta(&delta, &params);

    assert_eq!(
        b_after.merged(&a, &params),
        b_after,
        "Property 1 violated: the delta did not dominate the join"
    );
}
