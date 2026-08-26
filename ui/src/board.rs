//! The board, as a pure function of a projected `Status`.
//!
//! Nothing here touches Dioxus, the DOM, or a node. That is deliberate: it
//! makes square colours, orientation, legal-target marking and promotion
//! detection testable natively, with no browser and no framework. The view
//! layer's job is to render these descriptors and nothing else.

use adjourn_core::Status;
use shakmaty::fen::Fen;
use shakmaty::{CastlingMode, Chess, Color, Position, Role};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shade {
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marker {
    None,
    Selected,
    LegalTarget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Square {
    pub file: char,
    pub rank: u8,
    pub shade: Shade,
    pub piece: Option<(Color, Role)>,
    pub marker: Marker,
}

fn position_of(status: &Status) -> Option<Chess> {
    status
        .fen
        .parse::<Fen>()
        .ok()?
        .into_position(CastlingMode::Standard)
        .ok()
}

fn uci_of(file: char, rank: u8) -> String {
    format!("{file}{rank}")
}

/// The 64 squares in reading order for `orientation`: White reads a8 first,
/// Black reads h1 first.
///
/// `selected` marks that square and every square a legal move from it can
/// reach, via `legal_moves_for` -- a deliberate duplicate of
/// `common/src/project.rs`'s `legal_moves`, NOT a shared call. See that
/// function's own note for why, and for the standing obligation to keep the
/// two in step by hand.
pub fn squares(status: &Status, orientation: Color, selected: Option<(char, u8)>) -> Vec<Square> {
    let pos = position_of(status);

    let targets: Vec<(char, u8)> = match (selected, status.decision.is_none()) {
        (Some((f, r)), true) => {
            let from = uci_of(f, r);
            legal_moves_for(status)
                .into_iter()
                .filter(|m| m.starts_with(&from))
                .filter_map(|m| {
                    let mut cs = m.chars().skip(2);
                    let file = cs.next()?;
                    let rank = cs.next()?.to_digit(10)? as u8;
                    Some((file, rank))
                })
                .collect()
        }
        _ => Vec::new(),
    };

    let files: Vec<char> = ('a'..='h').collect();
    let mut out = Vec::with_capacity(64);
    let ranks: Vec<u8> = match orientation {
        Color::White => (1..=8).rev().collect(),
        Color::Black => (1..=8).collect(),
    };
    for rank in ranks {
        let row: Vec<char> = match orientation {
            Color::White => files.clone(),
            Color::Black => files.iter().rev().copied().collect(),
        };
        for file in row {
            // a1 is dark. Shade follows the square, never the viewer.
            let dark = ((file as u8 - b'a') + rank) % 2 == 1;
            let piece = pos.as_ref().and_then(|p| {
                let sq = shakmaty::Square::from_ascii(uci_of(file, rank).as_bytes()).ok()?;
                p.board().piece_at(sq).map(|pc| (pc.color, pc.role))
            });
            let marker = if selected == Some((file, rank)) {
                Marker::Selected
            } else if targets.contains(&(file, rank)) {
                Marker::LegalTarget
            } else {
                Marker::None
            };
            out.push(Square {
                file,
                rank,
                shade: if dark { Shade::Dark } else { Shade::Light },
                piece,
                marker,
            });
        }
    }
    out
}

/// The legal moves in `status`'s position.
///
/// This is a hand copy of `adjourn_core::project::legal_moves` (both FEN-parse
/// then ask shakmaty), and the duplication is deliberate rather than
/// accidental: `legal_moves` takes the whole state, while [`squares`] is
/// handed only a `&Status`, which is what keeps the board a pure function of
/// what it renders. Factoring the shared half into `adjourn-core` was
/// considered and REJECTED -- that crate compiles into the contract, so a new
/// public item there rotates the contract key and orphans every game in
/// progress. Two lines of shakmaty are not worth that.
///
/// The obligation this leaves: if move generation ever changes -- a different
/// `CastlingMode`, a different UCI spelling -- change it in BOTH places, or
/// the browser will disagree with the projection about what is playable.
fn legal_moves_for(status: &Status) -> Vec<String> {
    let Some(pos) = position_of(status) else {
        return Vec::new();
    };
    pos.legal_moves()
        .iter()
        .map(|m| shakmaty::uci::UciMove::from_move(*m, CastlingMode::Standard).to_string())
        .collect()
}

/// Would moving `from` -> `to` promote a pawn?
///
/// The picker must appear for this move and only this move: a UI that always
/// queens cannot play some legal games, and underpromotion is already
/// supported by the algebra.
///
/// Answers a question about the PIECE, not about the move: it checks neither
/// legality nor side to move, so it says `true` for a black pawn's promotion
/// while White is to play. Callers must therefore only ask about a `to` square
/// that [`squares`] has already marked [`Marker::LegalTarget`] for this `from`
/// -- that is where legality and turn are decided.
pub fn is_promotion(status: &Status, from: (char, u8), to: (char, u8)) -> bool {
    let Some(pos) = position_of(status) else {
        return false;
    };
    let Ok(sq) = shakmaty::Square::from_ascii(uci_of(from.0, from.1).as_bytes()) else {
        return false;
    };
    let Some(piece) = pos.board().piece_at(sq) else {
        return false;
    };
    if piece.role != Role::Pawn {
        return false;
    }
    matches!((piece.color, to.1), (Color::White, 8) | (Color::Black, 1))
}
