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

/// A state holding exactly these records, with NO eviction applied.
fn raw_state(records: &[Record]) -> GameState {
    let mut s = GameState::empty();
    for r in records {
        s.absorb_for_test(r);
    }
    s
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
    let (state, params, w, _) = play(&["e2e4"]);
    let good = state.records.values().next().unwrap().clone();

    // A single record can't distinguish filter-then-merge from
    // merge-then-filter under eviction -- with K=2 either order keeps it. Add
    // a group with more than K records, split across the two peers, so
    // top-K's distributivity is actually exercised: each side alone would
    // keep a different two records than the union does.
    let mut spam = Vec::new();
    for i in 0..6u64 {
        let mut parent = [0u8; 32];
        parent[..8].copy_from_slice(&i.to_le_bytes());
        spam.push(Record::sign(
            &w,
            &params,
            Body::Move {
                ply: 5,
                parent,
                uci: "g1f3".into(),
            },
        ));
    }

    let mut attacker = GameState::empty();
    attacker.absorb_for_test(&poison(&good));
    for r in &spam[..3] {
        attacker.absorb_for_test(r);
    }

    let mut honest = state.clone();
    for r in &spam[3..] {
        honest.absorb_for_test(r);
    }

    let filter_then_merge = attacker
        .filter_valid(&params)
        .merged(&honest.filter_valid(&params), &params);
    let merge_then_filter = attacker.merged(&honest, &params).filter_valid(&params);

    assert_eq!(filter_then_merge, merge_then_filter);
    assert_eq!(
        filter_then_merge.len(),
        3,
        "the ply-1 move plus top-K=2 of the ply-5 spam group survive"
    );
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

    let offer = Record::sign(
        &b,
        &params,
        Body::DrawOffer {
            ply: 2,
            at: chain[1],
        },
    );
    let accept = Record::sign(
        &w,
        &params,
        Body::DrawAccept {
            ply: 2,
            offer: offer.id(),
        },
    );

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
    let chain_len = project(&state, &params).chain.len() as u16;
    let head = *project(&state, &params).chain.last().unwrap();

    let offer = Record::sign(
        &b,
        &params,
        Body::DrawOffer {
            ply: chain_len,
            at: head,
        },
    );
    let accept = Record::sign(
        &w,
        &params,
        Body::DrawAccept {
            ply: chain_len,
            offer: offer.id(),
        },
    );

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
    let chain_len = project(&state, &params).chain.len() as u16;
    let head = *project(&state, &params).chain.last().unwrap();

    let offer = Record::sign(
        &b,
        &params,
        Body::DrawOffer {
            ply: chain_len,
            at: head,
        },
    );
    let accept = Record::sign(
        &w,
        &params,
        Body::DrawAccept {
            ply: chain_len,
            offer: offer.id(),
        },
    );

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

/// Invariant 8, REOPENED and inverted -- deliberately.
///
/// The double-sign forfeit is now STRUCTURAL: two `Move` records from one
/// signer at one ply forfeit, counted without ever consulting the position.
/// That is what makes the fraud proof survive blind eviction, and it is what
/// closes retroactive move substitution. The price is that a position-free
/// rule cannot tell `e1g1` and `e1h1` apart: they are two bodies, two ids, two
/// records, so signing both forfeits even though they spell ONE castling move.
///
/// The stock stack cannot reach this. `make_move` signs only the canonical
/// spelling (see `make_move_canonicalises_castling_notation`), and the
/// delegate refuses a second signature at an already-signed ply. Only a
/// third-party client that signs raw bodies itself could do it, and it would
/// forfeit its own user over notation.
///
/// The trade was taken knowingly: unlimited takeback is a game-integrity
/// break, a self-inflicted notation forfeit in a non-stock client is not.
#[test]
fn two_spellings_of_one_castling_move_now_forfeit() {
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
    assert_eq!(
        st.decision.map(|d| d.reason),
        Some(Reason::DoubleSignForfeit),
        "two Move records at one ply forfeit, whatever they spell"
    );
    assert_eq!(
        st.decision.and_then(|d| d.winner),
        Some(Color::Black),
        "the signer forfeits, so their opponent wins"
    );
    assert_eq!(st.ply, 6, "the chain stops one ply short of the fraud");
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

#[test]
fn a_record_beyond_max_ply_is_not_valid() {
    let (w, _b, params) = keys();
    let ok = Record::sign(
        &w,
        &params,
        Body::Move {
            ply: adjourn_core::types::MAX_PLY,
            parent: params.genesis(),
            uci: "e2e4".into(),
        },
    );
    assert!(
        ok.verify(&params),
        "a record at exactly MAX_PLY must stay valid"
    );

    let too_far = Record::sign(
        &w,
        &params,
        Body::Move {
            ply: adjourn_core::types::MAX_PLY + 1,
            parent: params.genesis(),
            uci: "e2e4".into(),
        },
    );
    assert!(
        !too_far.verify(&params),
        "ply > MAX_PLY must be structurally invalid"
    );

    // The cap is structural, so it must also refuse a state built from such a
    // record -- not merely ignore it at projection.
    let mut state = GameState::empty();
    state.insert_verified(&too_far, &params);
    assert!(
        state.is_empty(),
        "an over-MAX_PLY record must never enter state"
    );
}

#[test]
fn the_ply_cap_applies_to_draw_records_too() {
    let (w, _b, params) = keys();
    let rec = Record::sign(
        &w,
        &params,
        Body::DrawOffer {
            ply: adjourn_core::types::MAX_PLY + 1,
            at: params.genesis(),
        },
    );
    assert!(
        !rec.verify(&params),
        "draw records carry a ply and are capped too"
    );
}

#[test]
fn resign_has_no_ply_and_one_possible_id() {
    let (w, _b, params) = keys();
    let a = Record::sign(&w, &params, Body::Resign);
    let b = Record::sign(&w, &params, Body::Resign);
    assert_eq!(
        a.body.ply(),
        None,
        "Resign is a unit variant: no ply to group on"
    );
    assert_eq!(
        a.id(),
        b.id(),
        "one signer has exactly one possible Resign id"
    );
}

#[test]
fn flooding_draw_offers_cannot_evict_a_move_at_the_same_ply() {
    let (state, params, w, _b) = play(&["e2e4"]);
    let white_move = *state.records.keys().next().expect("one move");

    let offers: Vec<Record> = (0..50u64)
        .map(|i| {
            let mut at = [0u8; 32];
            at[..8].copy_from_slice(&i.to_le_bytes());
            Record::sign(&w, &params, Body::DrawOffer { ply: 1, at })
        })
        .collect();

    let mut flooded = state.clone();
    let mut raw_offers = GameState::empty();
    for o in &offers {
        raw_offers.absorb_for_test(o);
    }
    flooded.merge(&raw_offers, &params);

    assert!(
        flooded.records.contains_key(&white_move),
        "kind is part of the group key, so offers never compete with moves"
    );
}

/// Burial no longer works.
///
/// Eviction must sort blind by id, so a cheater CAN still evict both halves of
/// a legality-based fraud proof under lower-id junk. What they cannot do is
/// leave the group clean: eviction FLOORS `(signer, Move, ply)` at K=2 rather
/// than emptying it, so whatever survives is still two `Move` records at one
/// ply -- which is the structural forfeit. The junk used to hide the fraud is
/// itself the fraud proof.
#[test]
fn a_buried_double_sign_still_forfeits() {
    let (state, params, w, _b) = play(&["e2e4", "e7e5"]);
    let head = project(&state, &params)
        .chain
        .last()
        .copied()
        .expect("head");

    // Two genuinely different legal moves at ply 3: the fraud.
    let a = Record::sign(
        &w,
        &params,
        Body::Move {
            ply: 3,
            parent: head,
            uci: "g1f3".into(),
        },
    );
    let b = Record::sign(
        &w,
        &params,
        Body::Move {
            ply: 3,
            parent: head,
            uci: "b1c3".into(),
        },
    );

    let mut fraud = state.clone();
    fraud.merge(&raw_state(&[a.clone(), b.clone()]), &params);
    let caught = project(&fraud, &params);
    assert_eq!(
        caught.decision.map(|d| d.reason),
        Some(Reason::DoubleSignForfeit),
        "unburied, a double-sign forfeits"
    );

    // Now bury both under lower-id records in the same group.
    let mut buried = state.clone();
    let mut junk: Vec<Record> = vec![a.clone(), b.clone()];
    for i in 0..64u64 {
        let mut parent = [0u8; 32];
        parent[..8].copy_from_slice(&i.to_le_bytes());
        junk.push(Record::sign(
            &w,
            &params,
            Body::Move {
                ply: 3,
                parent,
                uci: "e2e4".into(),
            },
        ));
    }
    buried.merge(&raw_state(&junk), &params);

    assert!(
        !buried.records.contains_key(&a.id()) || !buried.records.contains_key(&b.id()),
        "precondition: burial really did evict at least one half of the fraud"
    );
    assert_eq!(
        buried
            .records
            .values()
            .filter(|r| matches!(&r.body, Body::Move { ply: 3, .. }) && r.signer == a.signer)
            .count(),
        2,
        "eviction floors the group at K=2 -- it never empties it"
    );

    let st = project(&buried, &params);
    assert_eq!(
        st.decision.map(|d| d.reason),
        Some(Reason::DoubleSignForfeit),
        "buried, it STILL forfeits: the rule counts records, not legality"
    );
    assert_eq!(
        st.decision.and_then(|d| d.winner),
        Some(Color::Black),
        "the burier forfeits, not their opponent"
    );
    assert_eq!(st.ply, 2, "the chain stops one ply short of the fraud");
}

/// Two `Move` records at one ply forfeit even when NEITHER is a legal
/// continuation, and even at a ply the chain never reaches. The rule is a
/// property of the record set, not of any position.
#[test]
fn two_move_records_at_one_ply_forfeit_regardless_of_legality() {
    let (state, params, _w, b) = play(&["e2e4"]);

    let mut s = state.clone();
    for uci in ["a1a8", "h8h1"] {
        // Both illegal, both wrong-parent, at a ply the chain cannot reach.
        let rec = Record::sign(
            &b,
            &params,
            Body::Move {
                ply: 40,
                parent: [3u8; 32],
                uci: uci.into(),
            },
        );
        assert!(s.insert_verified(&rec, &params));
    }

    let st = project(&s, &params);
    assert_eq!(
        st.decision,
        Some(Decision {
            winner: Some(Color::White),
            reason: Reason::DoubleSignForfeit
        }),
        "two Move records in one group forfeit, position-free"
    );
}

/// Both players double-signing has no principled winner, so it is a draw --
/// exactly as for mutual resignation. Deterministic either way, which is all
/// convergence requires.
#[test]
fn a_mutual_double_sign_is_a_draw() {
    let (state, params, w, b) = play(&["e2e4"]);

    let mut s = state.clone();
    for (key, ply) in [(&w, 41u16), (&b, 40u16)] {
        for uci in ["a1a8", "h8h1"] {
            let rec = Record::sign(
                key,
                &params,
                Body::Move {
                    ply,
                    parent: [3u8; 32],
                    uci: uci.into(),
                },
            );
            assert!(s.insert_verified(&rec, &params));
        }
    }

    assert_eq!(
        project(&s, &params).decision,
        Some(Decision {
            winner: None,
            reason: Reason::DoubleSignForfeit
        })
    );
}

/// Grind `parent` until the record's id sorts below `below`.
fn junk_below(
    key: &SigningKey,
    params: &GameParams,
    ply: u16,
    uci: &str,
    below: RecordId,
) -> Option<Record> {
    for i in 0..200_000u64 {
        let mut parent = [0u8; 32];
        parent[..8].copy_from_slice(&i.to_le_bytes());
        let rec = Record::sign(
            key,
            params,
            Body::Move {
                ply,
                parent,
                uci: uci.into(),
            },
        );
        if rec.id() < below {
            return Some(rec);
        }
    }
    None
}

/// THE ATTACK the structural forfeit exists to close: retroactive move
/// substitution.
///
/// White plays a real move, sees Black's reply, then publishes TWO lower-id
/// records at its own ply -- one junk record with a wrong parent (never a walk
/// candidate) and one genuinely different LEGAL move. Eviction drops the real
/// move, the parent check filters the junk, and under a legality-based rule
/// exactly one candidate survives: the walk continues on the substitute, and
/// White has taken its move back using Black as a search oracle.
///
/// Structurally it is two `Move` records in one group. Forfeit.
#[test]
fn retroactive_move_substitution_forfeits() {
    let (state, params, w, _b) = play(&["e2e4", "e7e5", "g1f3", "b8c6"]);
    let before = project(&state, &params);
    assert_eq!(before.ply, 4);
    let head = before.chain.last().copied().expect("head");

    // The real ply-5 move, published and answered.
    let real = make_move(&state, &params, &w, "f1b5").expect("Bb5 is legal");
    let mut s = state.clone();
    assert!(s.insert_verified(&real, &params));
    assert_eq!(project(&s, &params).ply, 5, "the real move is on the chain");

    // A different LEGAL ply-5 move whose id sorts below the real one.
    let alt = [
        "h2h3", "a2a3", "d2d3", "b1c3", "f3g5", "f1c4", "f1e2", "f1d3",
    ]
    .iter()
    .map(|uci| {
        Record::sign(
            &w,
            &params,
            Body::Move {
                ply: 5,
                parent: head,
                uci: (*uci).into(),
            },
        )
    })
    .find(|rec| rec.id() < real.id())
    .expect("some legal alternative sorts below the real move");

    // ...and a wrong-parent junk record, also below it, to fill the group.
    let junk = junk_below(&w, &params, 5, "e2e4", real.id().min(alt.id()))
        .expect("a wrong-parent record below both");

    let mut attacked = s.clone();
    attacked.merge(&raw_state(&[alt.clone(), junk.clone()]), &params);

    assert!(
        !attacked.records.contains_key(&real.id()),
        "precondition: the real move really was evicted"
    );
    assert!(attacked.records.contains_key(&alt.id()));
    assert!(attacked.records.contains_key(&junk.id()));

    let st = project(&attacked, &params);
    assert_eq!(
        st.decision.map(|d| d.reason),
        Some(Reason::DoubleSignForfeit),
        "the substitution is a double-sign, and is caught structurally"
    );
    assert_eq!(
        st.decision.and_then(|d| d.winner),
        Some(Color::Black),
        "the substituting player loses, rather than getting a free takeback"
    );
    assert_eq!(st.ply, 4, "the chain stops before the rewritten ply");
    assert_eq!(
        st.fen, before.fen,
        "and the board is the last agreed position"
    );
}

/// The same rewind, aimed at a draw record instead of the board: pull the head
/// back to a position where an opponent's `DrawOffer` was still live, then cash
/// it. Under a legality-based forfeit this converted a lost game into a
/// recorded draw. Structurally the rewind still needs two `Move` records in one
/// group, so it forfeits.
#[test]
fn reviving_an_expired_draw_offer_by_rewinding_forfeits() {
    let (state, params, w, b) = play(&["e2e4", "e7e5", "g1f3"]);
    let head_at_3 = project(&state, &params)
        .chain
        .last()
        .copied()
        .expect("head");

    // Black offers a draw at ply 3; it is Black to answer, so the offer is live
    // only while ply 3 is the head.
    let offer = Record::sign(
        &b,
        &params,
        Body::DrawOffer {
            ply: 3,
            at: head_at_3,
        },
    );
    let mut s = state.clone();
    assert!(s.insert_verified(&offer, &params));

    // Black plays on, and the offer expires.
    let reply = make_move(&s, &params, &b, "b8c6").expect("legal");
    assert!(s.insert_verified(&reply, &params));
    let real = make_move(&s, &params, &w, "f1b5").expect("legal");
    assert!(s.insert_verified(&real, &params));

    // White, now wanting out, accepts the STALE offer and rewinds ply 5 so
    // that ply 3 is the head again.
    let accept = Record::sign(
        &w,
        &params,
        Body::DrawAccept {
            ply: 5,
            offer: offer.id(),
        },
    );
    assert!(s.insert_verified(&accept, &params));
    assert_eq!(
        project(&s, &params).decision,
        None,
        "precondition: the stale offer alone does nothing"
    );

    let junk_a = junk_below(&w, &params, 5, "e2e4", real.id()).expect("junk below");
    let junk_b =
        junk_below(&w, &params, 5, "d2d4", real.id().min(junk_a.id())).expect("second junk below");
    let mut attacked = s.clone();
    attacked.merge(&raw_state(&[junk_a, junk_b]), &params);
    assert!(
        !attacked.records.contains_key(&real.id()),
        "precondition: the rewind really did evict White's ply-5 move"
    );

    let st = project(&attacked, &params);
    assert_eq!(
        st.decision.map(|d| d.reason),
        Some(Reason::DoubleSignForfeit),
        "the rewind forfeits instead of cashing the expired offer"
    );
    assert_eq!(
        st.decision.and_then(|d| d.winner),
        Some(Color::Black),
        "and it is the rewinder who loses"
    );
}

// ---------------------------------------------------------------------------
// 4. Draw claims (FIDE 9.2, 9.3)
// ---------------------------------------------------------------------------

/// Threefold needs the SAME position three times. Knights out and back does it.
const THREEFOLD_LINE: &[&str] = &[
    "g1f3", "g8f6", "f3g1", "f6g8", // position 2
    "g1f3", "g8f6", "f3g1", "f6g8", // position 3
];

#[test]
fn a_threefold_claim_at_the_head_draws_the_game() {
    let (state, params, w, _b) = play(THREEFOLD_LINE);
    let status = project(&state, &params);
    assert!(status.repetitions >= 3, "the line must actually repeat");
    assert!(
        status.decision.is_none(),
        "threefold alone does not end the game"
    );

    // White is to move at the head, so White is the one who may claim.
    assert_eq!(
        status.turn,
        Color::White,
        "this line ends with white to move"
    );
    let head = status.chain.last().copied().expect("head");
    let claim = Record::sign(
        &w,
        &params,
        Body::DrawClaim {
            ply: status.ply,
            at: head,
        },
    );

    let mut claimed = state.clone();
    assert!(claimed.insert_verified(&claim, &params));
    let after = project(&claimed, &params);
    assert_eq!(
        after.decision.map(|d| d.reason),
        Some(Reason::ThreefoldClaim),
        "a valid claim at the head draws"
    );
    assert_eq!(
        after.decision.and_then(|d| d.winner),
        None,
        "a claim is a draw"
    );
}

#[test]
fn a_claim_with_no_valid_ground_is_ignored_not_fatal() {
    let (state, params, w, _b) = play(&["e2e4", "e7e5"]);
    let status = project(&state, &params);
    let head = status.chain.last().copied().expect("head");
    let claim = Record::sign(
        &w,
        &params,
        Body::DrawClaim {
            ply: status.ply,
            at: head,
        },
    );

    let mut claimed = state.clone();
    assert!(claimed.insert_verified(&claim, &params));
    let after = project(&claimed, &params);
    assert_eq!(
        after.decision, None,
        "no repetition, no fifty-move: ignored"
    );
    assert_eq!(after.ply, status.ply, "and the game is otherwise untouched");
}

#[test]
fn a_stale_claim_is_ignored() {
    let (state, params, w, _b) = play(THREEFOLD_LINE);
    let status = project(&state, &params);
    let head = status.chain.last().copied().expect("head");
    let claim = Record::sign(
        &w,
        &params,
        Body::DrawClaim {
            ply: status.ply,
            at: head,
        },
    );

    // The claimant moves instead of standing on the claim, advancing the head.
    // Only the claimant can do this -- which is precisely why a claim has no
    // race: the opponent cannot void it.
    let mut moved = state.clone();
    let mv = make_move(&moved, &params, &w, "e2e4").expect("legal");
    assert!(moved.insert_verified(&mv, &params));
    assert!(moved.insert_verified(&claim, &params));

    assert_eq!(
        project(&moved, &params).decision,
        None,
        "the head moved on, so the claim expired"
    );
}

#[test]
fn a_valid_claim_outranks_a_pending_draw_agreement() {
    let (state, params, w, b) = play(THREEFOLD_LINE);
    let status = project(&state, &params);
    let head = status.chain.last().copied().expect("head");

    // Black offers, white accepts -- a complete, live agreement.
    let offer = Record::sign(
        &b,
        &params,
        Body::DrawOffer {
            ply: status.ply,
            at: head,
        },
    );
    let accept = Record::sign(
        &w,
        &params,
        Body::DrawAccept {
            ply: status.ply,
            offer: offer.id(),
        },
    );
    // White, who is to move, also claims the threefold.
    let claim = Record::sign(
        &w,
        &params,
        Body::DrawClaim {
            ply: status.ply,
            at: head,
        },
    );

    let mut both = state.clone();
    for rec in [&offer, &accept, &claim] {
        assert!(both.insert_verified(rec, &params));
    }

    // Both are draws, so the winner is the same either way -- what the
    // precedence decides is which REASON a peer reports. Every peer must
    // report the same one.
    assert_eq!(
        project(&both, &params).decision.map(|d| d.reason),
        Some(Reason::ThreefoldClaim),
        "claim sits above agreement in the precedence"
    );
}

#[test]
fn only_the_player_to_move_may_claim() {
    let (state, params, _w, b) = play(THREEFOLD_LINE);
    let status = project(&state, &params);
    assert_eq!(status.turn, Color::White, "white is to move on this line");
    let head = status.chain.last().copied().expect("head");

    // Black is NOT to move, so black's claim must not count -- otherwise the
    // player to move could void it by moving, reintroducing a race.
    let claim = Record::sign(
        &b,
        &params,
        Body::DrawClaim {
            ply: status.ply,
            at: head,
        },
    );
    let mut claimed = state.clone();
    assert!(claimed.insert_verified(&claim, &params));
    assert_eq!(project(&claimed, &params).decision, None);
}

/// Note on what this does and does not prove. A position that is BOTH
/// checkmate and threefold/fifty-move is impractical to construct by hand, so
/// this asserts the weaker, still-useful property: a claim record at a
/// checkmate head does not disturb the mate. The strict board > claim ordering
/// is structural — `board_result.or_else(...)` short-circuits before
/// `draw_claimed` is ever called — and is documented at that call site.
#[test]
fn a_claim_does_not_disturb_a_checkmate() {
    let (state, params, _w, b) = play(SCHOLARS);
    let status = project(&state, &params);
    assert_eq!(
        status.decision.map(|d| d.reason),
        Some(Reason::Checkmate),
        "scholar's mate ends in checkmate"
    );
    let head = status.chain.last().copied().expect("head");

    // Black is the mated player, and therefore the player "to move".
    let claim = Record::sign(
        &b,
        &params,
        Body::DrawClaim {
            ply: status.ply,
            at: head,
        },
    );
    let mut claimed = state.clone();
    claimed.insert_verified(&claim, &params);

    assert_eq!(
        project(&claimed, &params).decision.map(|d| d.reason),
        Some(Reason::Checkmate),
        "a mated player cannot claim their way out of a loss"
    );
}

#[test]
fn ignored_counts_illegal_moves_but_not_resignations() {
    let (state, params, w, b) = play(&["e2e4", "e7e5"]);
    let base = project(&state, &params).ignored;
    assert_eq!(base, 0, "a clean game ignores nothing");

    // A resignation is a statement, not an ignored move.
    let mut with_resign = state.clone();
    assert!(with_resign.insert_verified(&Record::sign(&b, &params, Body::Resign), &params));
    assert_eq!(
        project(&with_resign, &params).ignored,
        0,
        "resignations are not ignored moves"
    );

    // A wrong-parent move IS an ignored move.
    let mut with_junk = state.clone();
    assert!(with_junk.insert_verified(
        &Record::sign(
            &w,
            &params,
            Body::Move {
                ply: 3,
                parent: [9u8; 32],
                uci: "g1f3".into()
            }
        ),
        &params
    ));
    assert_eq!(project(&with_junk, &params).ignored, 1);
}
