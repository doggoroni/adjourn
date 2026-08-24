//! Projection: record set -> game position.
//!
//! This is a pure function of the merged set. All the ordering, legality and
//! termination logic lives here, where getting it wrong costs a wrong answer
//! rather than a convergence failure.

use crate::state::GameState;
use crate::types::{color_at_ply, Body, GameParams, Record, RecordId};
use shakmaty::fen::{Epd, Fen};
use shakmaty::uci::UciMove;
use shakmaty::{CastlingMode, Chess, Color, EnPassantMode, Position};
use std::collections::BTreeMap;

/// FIDE 9.6.1: the game is drawn as soon as a position occurs for the fifth
/// time. Threefold (9.2) is only a *claim*, so it is reported, not forced.
const FIVEFOLD: u32 = 5;

/// FIDE 9.6.2: the game is drawn after 75 moves by each player with no capture
/// and no pawn move — 150 halfmoves. Fifty (9.3) is likewise only a claim.
const SEVENTY_FIVE_MOVE_HALFMOVES: u32 = 150;

/// FIDE 9.2: a player may CLAIM a draw when a position occurs a third time.
const THREEFOLD: u32 = 3;

/// FIDE 9.3: a player may CLAIM a draw after 50 moves by each player with no
/// capture and no pawn move -- 100 halfmoves.
const FIFTY_MOVE_HALFMOVES: u32 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Checkmate,
    Stalemate,
    InsufficientMaterial,
    /// shakmaty's automatic 75-move / 5-fold rules.
    AutomaticDraw,
    Resignation,
    DrawAgreement,
    /// A player signed two different legal moves at the same ply. The record
    /// set contains its own fraud proof.
    DoubleSignForfeit,
    /// Both players resigned; no principled winner, so a draw.
    MutualResignation,
    /// A claimed threefold repetition (FIDE 9.2).
    ThreefoldClaim,
    /// A claimed fifty-move draw (FIDE 9.3).
    FiftyMoveClaim,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision {
    /// `None` means a draw.
    pub winner: Option<Color>,
    pub reason: Reason,
}

#[derive(Clone, Debug)]
pub struct Status {
    /// `None` while the game is still in progress.
    pub decision: Option<Decision>,
    /// Half-moves played so far.
    pub ply: u16,
    pub turn: Color,
    pub fen: String,
    /// Record ids of the accepted move chain, in order.
    pub chain: Vec<RecordId>,
    /// Move records present in state but not in the accepted chain: illegal
    /// moves, wrong-parent moves, moves past the point the chain stopped.
    /// Resignations and draw records are statements, not ignored moves, so
    /// they are never counted here. Useful for UI diagnostics.
    pub ignored: usize,
    /// How many times the current position has occurred, counting this one.
    /// At 3 a player may claim a draw; at [`FIVEFOLD`] it is automatic.
    pub repetitions: u32,
    /// Halfmoves since the last capture or pawn move. At 100 a player may claim
    /// a draw; at [`SEVENTY_FIVE_MOVE_HALFMOVES`] it is automatic.
    pub halfmove_clock: u32,
}

impl Status {
    pub fn is_over(&self) -> bool {
        self.decision.is_some()
    }
}

/// What walking the chain from genesis produced.
struct Walk {
    pos: Chess,
    chain: Vec<RecordId>,
    /// The colour of a player caught double-signing, if any.
    forfeit: Option<Color>,
    /// Set when a FIDE automatic draw fired, which also stops the chain.
    auto_draw: bool,
    /// Occurrences of the final position, counting itself.
    repetitions: u32,
}

/// The repetition key: placement, side to move, castling rights, and an en
/// passant square only where the capture is actually available. That is the
/// FIDE notion of "the same position", and it is what EPD encodes — a FEN
/// without the clocks.
fn repetition_key(pos: &Chess) -> String {
    Epd::from_position(pos, EnPassantMode::Legal).to_string()
}

/// Walk the chain from genesis, applying exactly one legal move per ply.
///
/// `shakmaty::Chess` keeps no history, so the repetition count is accumulated
/// here. It stays a pure function of the record set: the chain is determined by
/// the parent-hash links, and the positions follow from the chain.
fn walk(state: &GameState, params: &GameParams) -> Walk {
    let mut pos = Chess::default();
    let mut chain: Vec<RecordId> = Vec::new();
    let mut parent = params.genesis();
    let mut ply: u16 = 1;

    let mut seen: BTreeMap<String, u32> = BTreeMap::new();
    seen.insert(repetition_key(&pos), 1);
    let mut repetitions = 1;

    loop {
        let expected = color_at_ply(ply);
        let expected_key = params.key_of(expected);

        // Candidates: well-formed, correctly-parented, legal continuations
        // signed by the player whose turn it is.
        let mut candidates: Vec<(RecordId, shakmaty::Move)> = Vec::new();
        for (id, rec) in &state.records {
            let Body::Move {
                ply: rec_ply,
                parent: rec_parent,
                uci,
            } = &rec.body
            else {
                continue;
            };
            if *rec_ply != ply || *rec_parent != parent || rec.signer != expected_key {
                continue;
            }
            let Ok(parsed) = UciMove::from_ascii(uci.as_bytes()) else {
                continue;
            };
            let Ok(mv) = parsed.to_move(&pos) else {
                continue; // illegal in this position: ignored, not fatal
            };
            candidates.push((*id, mv));
        }

        // Two spellings of one move are two bodies with two ids, but a single
        // move — `e1g1` and `e1h1` both castle. Collapsing them here is what
        // stops a notation mismatch reading as a double-sign and forfeiting an
        // honest player. Only genuinely different moves survive to be counted.
        //
        // Records iterate in id order, so the first spelling seen already holds
        // the smallest id; the comparison keeps that true regardless.
        let mut distinct: Vec<(RecordId, shakmaty::Move)> = Vec::new();
        for (id, mv) in candidates {
            match distinct.iter_mut().find(|(_, seen)| *seen == mv) {
                Some(slot) if id < slot.0 => slot.0 = id,
                Some(_) => {}
                None => distinct.push((id, mv)),
            }
        }
        let mut candidates = distinct;

        match candidates.len() {
            0 => break,
            1 => {
                let (id, mv) = candidates.pop().expect("len checked");
                pos = pos.play(mv).expect("move was validated against pos");
                chain.push(id);
                parent = id;
                ply += 1;

                repetitions = *seen
                    .entry(repetition_key(&pos))
                    .and_modify(|n| *n += 1)
                    .or_insert(1);

                // Both rules end the game the instant they are reached, so the
                // chain stops here and any later move is ignored.
                if repetitions >= FIVEFOLD || pos.halfmoves() >= SEVENTY_FIVE_MOVE_HALFMOVES {
                    return Walk {
                        pos,
                        chain,
                        forfeit: None,
                        auto_draw: true,
                        repetitions,
                    };
                }
            }
            _ => {
                // Two distinct legal moves at the same ply from the same
                // player. Only they hold the key, so authorship is not in
                // doubt. Deterministic forfeit.
                return Walk {
                    pos,
                    chain,
                    forfeit: Some(expected),
                    auto_draw: false,
                    repetitions,
                };
            }
        }
    }

    Walk {
        pos,
        chain,
        forfeit: None,
        auto_draw: false,
        repetitions,
    }
}

/// Is there a matching offer/accept pair from opposite players, on an offer
/// that is still live?
///
/// An offer is live only while the head it names is still the head. A player
/// offers a draw straight after their own move, so it is the OPPONENT's turn
/// while the offer stands — which means the acceptor is the only player who can
/// advance the head. An acceptance can therefore be voided only by the
/// acceptor's own subsequent move, never by the offerer. No race, and the whole
/// thing stays a pure function of the merged set.
///
/// Without this, an offer made at ply 2 could be banked and cashed at ply 40 by
/// a player who had since walked into a losing position.
fn draw_agreed(state: &GameState, params: &GameParams, head: RecordId) -> bool {
    for rec in state.records.values() {
        let Body::DrawAccept { offer, .. } = &rec.body else {
            continue;
        };
        let Some(accepter) = rec.color(params) else {
            continue;
        };
        let Some(offer_rec) = state.records.get(offer) else {
            continue;
        };
        let Body::DrawOffer { at, .. } = &offer_rec.body else {
            continue;
        };
        if *at != head {
            continue; // the game moved on: this offer has expired
        }
        let Some(offerer) = offer_rec.color(params) else {
            continue;
        };
        if offerer != accepter {
            return true;
        }
    }
    false
}

/// Is there a live, well-founded draw claim at the head?
///
/// Anchored to the head exactly like `DrawOffer`, which is what keeps it free
/// of races: the claimant must be the player to move, and the player to move is
/// the only party who can advance the head. The opponent therefore cannot void
/// a valid claim, and a claim withheld and published later simply does nothing.
///
/// A claim with no valid ground is ignored, never fatal -- invariant 1 applies
/// to claims exactly as it applies to illegal moves.
fn draw_claimed(
    state: &GameState,
    params: &GameParams,
    head: RecordId,
    turn: Color,
    repetitions: u32,
    halfmoves: u32,
) -> Option<Reason> {
    for rec in state.records.values() {
        let Body::DrawClaim { at, .. } = &rec.body else {
            continue;
        };
        if *at != head {
            continue; // the game moved on: this claim has expired
        }
        // Only the player to move may claim. Letting the idle player claim
        // would let the player to move void it by moving -- a race.
        if rec.color(params) != Some(turn) {
            continue;
        }
        if repetitions >= THREEFOLD {
            return Some(Reason::ThreefoldClaim);
        }
        if halfmoves >= FIFTY_MOVE_HALFMOVES {
            return Some(Reason::FiftyMoveClaim);
        }
    }
    None
}

fn resigned(state: &GameState, params: &GameParams) -> (bool, bool) {
    let mut white = false;
    let mut black = false;
    for rec in state.records.values() {
        if matches!(rec.body, Body::Resign) {
            match rec.color(params) {
                Some(Color::White) => white = true,
                Some(Color::Black) => black = true,
                None => {}
            }
        }
    }
    (white, black)
}

/// The whole game, as a function of the record set.
///
/// Precedence is fixed and documented so that every peer reaches the same
/// answer: forfeit > resignation > board result > draw claim > draw
/// agreement.
///
/// Resignation sits ABOVE the board result because `Resign` is unanchored and
/// unconditional — it names no position, so there is no ply at which it stops
/// applying. Ranking the board first let a player resign and then play on to a
/// mate, and be awarded the win by their own resigned game.
pub fn project(state: &GameState, params: &GameParams) -> Status {
    let Walk {
        pos,
        chain,
        forfeit,
        auto_draw,
        repetitions,
    } = walk(state, params);

    let board_result = if pos.is_checkmate() {
        Some(Decision {
            winner: Some(!pos.turn()),
            reason: Reason::Checkmate,
        })
    } else if pos.is_stalemate() {
        Some(Decision {
            winner: None,
            reason: Reason::Stalemate,
        })
    } else if pos.is_insufficient_material() {
        Some(Decision {
            winner: None,
            reason: Reason::InsufficientMaterial,
        })
    } else if auto_draw {
        Some(Decision {
            winner: None,
            reason: Reason::AutomaticDraw,
        })
    } else {
        None
    };

    let decision = if let Some(cheater) = forfeit {
        Some(Decision {
            winner: Some(!cheater),
            reason: Reason::DoubleSignForfeit,
        })
    } else {
        match resigned(state, params) {
            (true, true) => Some(Decision {
                winner: None,
                reason: Reason::MutualResignation,
            }),
            (true, false) => Some(Decision {
                winner: Some(Color::Black),
                reason: Reason::Resignation,
            }),
            (false, true) => Some(Decision {
                winner: Some(Color::White),
                reason: Reason::Resignation,
            }),
            (false, false) => board_result.or_else(|| {
                let head = chain.last().copied().unwrap_or_else(|| params.genesis());
                // Board first: the claimant is by definition the player to
                // move, so if that position is checkmate the claimant is the
                // player who has been mated. Ranking the claim above the board
                // would let a mated player draw their way out of a loss.
                draw_claimed(
                    state,
                    params,
                    head,
                    pos.turn(),
                    repetitions,
                    pos.halfmoves(),
                )
                .map(|reason| Decision { winner: None, reason })
                .or_else(|| {
                    if draw_agreed(state, params, head) {
                        Some(Decision {
                            winner: None,
                            reason: Reason::DrawAgreement,
                        })
                    } else {
                        None
                    }
                })
            }),
        }
    };

    let ply = chain.len() as u16;

    // Only MOVE records can be "ignored" -- illegal moves, wrong-parent moves,
    // and moves past the point the chain stopped. A resignation or a draw
    // record is a statement in its own right, not a move the projection
    // skipped.
    let in_chain: std::collections::BTreeSet<RecordId> = chain.iter().copied().collect();
    let ignored = state
        .records
        .iter()
        .filter(|(id, rec)| matches!(rec.body, Body::Move { .. }) && !in_chain.contains(*id))
        .count();

    Status {
        decision,
        ply,
        turn: pos.turn(),
        halfmove_clock: pos.halfmoves(),
        fen: Fen::from_position(&pos, EnPassantMode::Legal).to_string(),
        ignored,
        repetitions,
        chain,
    }
}

/// Build the next move record for `key`, or `None` if it isn't their turn,
/// the game is over, or the move is illegal.
pub fn make_move(
    state: &GameState,
    params: &GameParams,
    key: &ed25519_dalek::SigningKey,
    uci: &str,
) -> Option<Record> {
    let status = project(state, params);
    if status.is_over() {
        return None;
    }
    let signer = key.verifying_key().to_bytes();
    if params.color_of(&signer)? != status.turn {
        return None;
    }

    // Legality check against the projected position.
    let pos: Chess = status
        .fen
        .parse::<Fen>()
        .ok()?
        .into_position(CastlingMode::Standard)
        .ok()?;
    let parsed = UciMove::from_ascii(uci.as_bytes()).ok()?;
    let mv = parsed.to_move(&pos).ok()?;

    // Sign the canonical spelling, not whatever the caller typed. shakmaty
    // accepts both `e1g1` and `e1h1` for the same castling move; signing both
    // would put two bodies in state for one move.
    let canonical = UciMove::from_move(mv, CastlingMode::Standard).to_string();

    let parent = status
        .chain
        .last()
        .copied()
        .unwrap_or_else(|| params.genesis());
    Some(Record::sign(
        key,
        params,
        Body::Move {
            ply: status.ply + 1,
            parent,
            uci: canonical,
        },
    ))
}

/// Legal moves in the current position, in UCI notation. For the UI.
pub fn legal_moves(state: &GameState, params: &GameParams) -> Vec<String> {
    let status = project(state, params);
    if status.is_over() {
        return Vec::new();
    }
    let Ok(fen) = status.fen.parse::<Fen>() else {
        return Vec::new();
    };
    let Ok(pos) = fen.into_position::<Chess>(CastlingMode::Standard) else {
        return Vec::new();
    };
    pos.legal_moves()
        .iter()
        .map(|m| UciMove::from_move(*m, CastlingMode::Standard).to_string())
        .collect()
}
