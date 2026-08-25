//! Build a conformance corpus for `fdev conformance`.
//!
//! `fdev conformance` runs the *same* verifier freenet-core runs (RFC #5320),
//! so a finding there means what it would mean on the network. It takes a set
//! of observed states and checks the contract's laws across every pairing, so
//! **the corpus is the experiment**: a set of tidy, complete games proves
//! almost nothing, because merge only has work to do when two peers hold
//! different fragments of one record set.
//!
//! This generator is therefore weighted toward partial and adversarial states.
//! It is committed rather than kept as scratch because the corpus has to be
//! regenerated on every wire-format change — the previous one lived only in a
//! throwaway working copy and did not survive, which is why the last recorded
//! result could not be reproduced.
//!
//! ```sh
//! cargo run -p adjourn-core --example dump_corpus -- corpus/
//! fdev conformance \
//!   --wasm target/wasm32-unknown-unknown/release/adjourn_contract.wasm \
//!   --params corpus/params.bin \
//!   $(for s in corpus/state_*.bin; do printf -- "--state %s " "$s"; done)
//! ```

use adjourn_core::state::GameState;
use adjourn_core::types::signing_payload;
use adjourn_core::*;
use ed25519_dalek::SigningKey;
use std::path::PathBuf;

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

/// Play a legal line through the real `make_move`, so the states are ones the
/// honest client could actually have produced.
fn play(moves: &[&str]) -> (GameState, GameParams, SigningKey, SigningKey) {
    let (w, b, params) = keys();
    let mut state = GameState::empty();
    for (i, uci) in moves.iter().enumerate() {
        let key = if i % 2 == 0 { &w } else { &b };
        let rec = make_move(&state, &params, key, uci)
            .unwrap_or_else(|| panic!("move {} ({uci}) rejected", i + 1));
        assert!(state.insert_verified(&rec, &params));
    }
    (state, params, w, b)
}

/// A state holding exactly these records, with NO verification and NO eviction.
///
/// This is the crafted-bytes path: what a hostile peer can PUT, as distinct
/// from what our own `merge` would ever construct.
fn raw(records: &[Record]) -> GameState {
    let mut s = GameState::empty();
    for r in records {
        s.absorb_for_test(r);
    }
    s
}

/// A second, different, but fully VALID signature over one body. ed25519 does
/// not pin the nonce, so a player running their own signer can produce this;
/// both records share an id, because ids exclude the signature.
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

/// A `Move` record at `ply` whose id sorts below `below`, found by grinding the
/// parent link. Wrong-parent records are never walk candidates, so this is the
/// shape an attacker uses to push their own real move out of a group.
fn junk_below(
    key: &SigningKey,
    params: &GameParams,
    ply: u16,
    below: RecordId,
    salt: u64,
) -> Record {
    for i in 0..200_000u64 {
        let mut parent = [0u8; 32];
        parent[..8].copy_from_slice(&(i ^ salt).to_le_bytes());
        let rec = Record::sign(
            key,
            params,
            Body::Move {
                ply,
                parent,
                uci: "e2e4".into(),
            },
        );
        if rec.id() < below {
            return rec;
        }
    }
    panic!("could not grind a junk id below the target");
}

const SCHOLARS: &[&str] = &["e2e4", "e7e5", "f1c4", "b8c6", "d1h5", "g8f6", "h5f7"];

/// Knights out and back: the start position occurs at plies 0, 4 and 8.
const THREEFOLD_LINE: &[&str] = &[
    "g1f3", "g8f6", "f3g1", "f6g8", "g1f3", "g8f6", "f3g1", "f6g8",
];

fn main() {
    let out: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "corpus".into())
        .into();
    std::fs::create_dir_all(&out).expect("create corpus dir");

    let (w, b, params) = keys();

    let mut params_bytes = Vec::new();
    ciborium::into_writer(&params, &mut params_bytes).expect("encode params");
    std::fs::write(out.join("params.bin"), &params_bytes).expect("write params");

    let mut states: Vec<(&str, GameState)> = Vec::new();

    // --- ordinary shapes -------------------------------------------------
    states.push(("empty", GameState::empty()));

    // Deliberately NOT e2e4: ed25519 signing is deterministic, so a one-move
    // e2e4 state is byte-identical to the `malleable_sig_a` state built by
    // hand below, and `fdev` would silently deduplicate the two — shrinking
    // the corpus without saying so.
    let (one, ..) = play(&["d2d4"]);
    states.push(("one_move", one));

    let (mate, ..) = play(SCHOLARS);

    // --- fragments: where the merge laws actually bite --------------------
    //
    // Overlapping subsets of ONE record set. Two peers holding different
    // fragments is the only situation in which commutativity and
    // associativity have anything to prove.
    let all: Vec<Record> = mate.records.values().cloned().collect();
    states.push(("mate_full", mate.clone()));
    states.push(("mate_prefix", raw(&all[..3])));
    states.push(("mate_suffix", raw(&all[3..])));
    states.push((
        "mate_alternating",
        raw(&all.iter().step_by(2).cloned().collect::<Vec<_>>()),
    ));
    states.push(("mate_single", raw(&all[..1])));

    // --- crafted bytes: over-K, which our own merge would never build -----
    let mut over_k = GameState::empty();
    for i in 0..30u64 {
        let mut parent = [0u8; 32];
        parent[..8].copy_from_slice(&i.to_le_bytes());
        over_k.absorb_for_test(&Record::sign(
            &w,
            &params,
            Body::Move {
                ply: 1,
                parent,
                uci: "e2e4".into(),
            },
        ));
    }
    states.push(("over_k_group", over_k));

    // --- the fraud shapes -------------------------------------------------
    let (two_ply, ..) = play(&["e2e4", "e7e5"]);
    let head = project(&two_ply, &params).chain.last().copied().unwrap();

    // Two distinct legal moves at one ply: the structural forfeit.
    let mut double_sign = two_ply.clone();
    for uci in ["g1f3", "b1c3"] {
        double_sign.absorb_for_test(&Record::sign(
            &w,
            &params,
            Body::Move {
                ply: 3,
                parent: head,
                uci: uci.into(),
            },
        ));
    }
    states.push(("double_sign_forfeit", double_sign));

    // The substitution shape: real move, plus a wrong-parent junk record and a
    // different legal move, both grinding below it.
    let (five, ..) = play(&["e2e4", "e7e5", "g1f3", "b8c6", "f1b5"]);
    let real_id = *project(&five, &params).chain.last().unwrap();
    let parent4 = project(&five, &params).chain[3];
    let mut substitution = five.clone();
    substitution.absorb_for_test(&junk_below(&w, &params, 5, real_id, 0));
    for uci in [
        "f1c4", "f1e2", "f1d3", "d2d4", "b1c3", "h2h3", "a2a3", "c2c3",
    ] {
        let alt = Record::sign(
            &w,
            &params,
            Body::Move {
                ply: 5,
                parent: parent4,
                uci: uci.into(),
            },
        );
        if alt.id() < real_id {
            substitution.absorb_for_test(&alt);
            break;
        }
    }
    states.push(("substitution_attempt", substitution));

    // --- draw records -----------------------------------------------------
    let st = project(&two_ply, &params);
    let offer = Record::sign(
        &b,
        &params,
        Body::DrawOffer {
            ply: st.ply,
            at: head,
        },
    );
    let accept = Record::sign(
        &w,
        &params,
        Body::DrawAccept {
            ply: st.ply,
            offer: offer.id(),
        },
    );
    states.push(("draw_agreed", raw(&[all[0].clone(), offer.clone(), accept])));
    states.push(("draw_offer_only", raw(&[offer])));

    // A stale offer: anchored at a head the game has since moved past.
    let stale = Record::sign(
        &b,
        &params,
        Body::DrawOffer {
            ply: 1,
            at: params.genesis(),
        },
    );
    states.push(("stale_draw_offer", raw(&[stale])));

    // A threefold claim, live at the head.
    let (rep, rep_params, rep_w, _) = play(THREEFOLD_LINE);
    let rep_status = project(&rep, &rep_params);
    let claim = Record::sign(
        &rep_w,
        &rep_params,
        Body::DrawClaim {
            ply: rep_status.ply,
            at: rep_status.chain.last().copied().unwrap(),
        },
    );
    let mut claimed = rep.clone();
    claimed.absorb_for_test(&claim);
    states.push(("threefold_claim", claimed));
    states.push(("threefold_line", rep));

    // --- resignation, and the ply bound -----------------------------------
    states.push((
        "resigned",
        raw(&[all[0].clone(), Record::sign(&b, &params, Body::Resign)]),
    ));

    states.push((
        "at_max_ply",
        raw(&[Record::sign(
            &w,
            &params,
            Body::Move {
                ply: MAX_PLY,
                parent: params.genesis(),
                uci: "e2e4".into(),
            },
        )]),
    ));

    // --- the signature collision -----------------------------------------
    //
    // Two peers holding the SAME statement under different valid signatures.
    // Ids exclude the signature, so these two states look identical to a
    // set-of-ids summary and must still converge. This is invariant 4's case,
    // and it needs two separate states to express.
    let body = Body::Move {
        ply: 1,
        parent: params.genesis(),
        uci: "e2e4".into(),
    };
    let sig_a = Record::sign(&w, &params, body.clone());
    let sig_b = second_valid_signature(&w, &params, &body);
    assert_eq!(sig_a.id(), sig_b.id(), "malleable pair must share an id");
    assert_ne!(sig_a.sig, sig_b.sig, "signatures must actually differ");
    states.push(("malleable_sig_a", raw(&[sig_a])));
    states.push(("malleable_sig_b", raw(&[sig_b])));

    // --- write, and prove every file is what we think it is ---------------
    //
    // A corpus of malformed bytes would produce meaningless conformance
    // results, so each state is decoded back before it counts as written.
    let mut seen: std::collections::BTreeMap<Vec<u8>, &str> = std::collections::BTreeMap::new();
    for (i, (name, state)) in states.iter().enumerate() {
        let bytes = state.encode();
        let back = GameState::decode(&bytes)
            .unwrap_or_else(|| panic!("state {name} does not decode: the corpus would be junk"));
        assert_eq!(
            &back, state,
            "state {name} does not round-trip; conformance would be testing the wrong bytes"
        );
        // `fdev` deduplicates identical states without saying so, which would
        // quietly shrink the corpus below what this file appears to build.
        // Signing is deterministic, so two states written different ways can
        // easily collide.
        if let Some(prev) = seen.insert(bytes.clone(), name) {
            panic!("state {name} is byte-identical to {prev}; fdev would silently drop one");
        }
        let path = out.join(format!("state_{i:02}_{name}.bin"));
        std::fs::write(&path, &bytes).expect("write state");
        println!(
            "{:>3} records  {:>6} bytes  {}",
            state.len(),
            bytes.len(),
            path.display()
        );
    }

    println!(
        "\n{} states + params.bin written to {}",
        states.len(),
        out.display()
    );
}
