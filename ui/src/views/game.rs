//! The game screen: the board, the always-visible move history, status, and
//! the game actions.
//!
//! Layout B: board left, scrollable move history right, status beneath,
//! actions under that. The history is not collapsed behind a disclosure
//! because the outcome is not monotone -- a late-published double-sign fraud
//! proof forfeits a player, rewinds the board, and can flip the winner.
//! `CLAUDE.md` says the UI should show the full chain rather than the
//! truncated position after a forfeit, which a hidden history cannot do.

use crate::board::{is_promotion, squares, Marker, Shade};
use crate::conn::{Cmd, Wires};
use adjourn_client::session::moves_in_order;
use dioxus::prelude::*;
use shakmaty::{Color, Role};

#[component]
pub fn GameScreen(wires: Wires, label: String) -> Element {
    let mut selected = use_signal(|| None::<(char, u8)>);
    // Set when a click resolves to a pawn reaching the last rank: the move is
    // held until the user picks a piece. A UI that always queens cannot play
    // some legal games, and the algebra already supports underpromotion.
    let mut promoting = use_signal(|| None::<((char, u8), (char, u8))>);

    let Some(view) = wires.view.read().clone() else {
        return rsx! { section { class: "screen", p { "loading {label}…" } } };
    };

    // `From<Side> for Color` already exists in `delegate_api`; `session.rs`
    // uses the same `.into()` idiom.
    let orientation: Color = view.side.into();
    let board = squares(&view.status, orientation, selected());
    let history = moves_in_order(&view);
    let our_turn = orientation == view.status.turn;
    let over = view.status.is_over();
    let can_claim =
        !over && our_turn && (view.status.repetitions >= 3 || view.status.halfmove_clock >= 100);

    rsx! {
        section { class: "screen game",
            div { class: "left",
                div { class: "board",
                    for sq in board.iter().copied() {
                        div {
                            key: "{sq.file}{sq.rank}",
                            class: match (sq.shade, sq.marker) {
                                (_, Marker::Selected) => "sq selected",
                                (_, Marker::LegalTarget) => "sq target",
                                (Shade::Light, _) => "sq light",
                                (Shade::Dark, _) => "sq dark",
                            },
                            onclick: {
                                let label = label.clone();
                                let status = view.status.clone();
                                move |_| {
                                    let here = (sq.file, sq.rank);
                                    match selected() {
                                        // Second click: play it, unless it promotes.
                                        Some(from) if sq.marker == Marker::LegalTarget => {
                                            if is_promotion(&status, from, here) {
                                                promoting.set(Some((from, here)));
                                            } else {
                                                wires.tx.send(Cmd::Play {
                                                    label: label.clone(),
                                                    uci: format!("{}{}{}{}", from.0, from.1, here.0, here.1),
                                                });
                                            }
                                            selected.set(None);
                                        }
                                        // Clicking the selected square clears it.
                                        Some(from) if from == here => selected.set(None),
                                        _ => selected.set(Some(here)),
                                    }
                                }
                            },
                            if let Some((color, role)) = sq.piece {
                                span {
                                    class: if color == Color::White { "piece white" } else { "piece black" },
                                    "{glyph(color, role)}"
                                }
                            }
                        }
                    }
                }

                p { class: "status",
                    if over {
                        "{outcome(&view.status)}"
                    } else if our_turn {
                        "your move · ply {view.status.ply}"
                    } else {
                        "waiting for your opponent · ply {view.status.ply}"
                    }
                }

                div { class: "actions",
                    button {
                        id: "resign",
                        disabled: over,
                        onclick: {
                            let label = label.clone();
                            move |_| wires.tx.send(Cmd::Resign { label: label.clone() })
                        },
                        "resign"
                    }
                    button {
                        id: "draw-offer",
                        disabled: over,
                        onclick: {
                            let label = label.clone();
                            move |_| wires.tx.send(Cmd::DrawOffer { label: label.clone() })
                        },
                        "offer draw"
                    }
                    button {
                        id: "draw-accept",
                        disabled: over,
                        onclick: {
                            let label = label.clone();
                            move |_| wires.tx.send(Cmd::DrawAccept { label: label.clone() })
                        },
                        "accept draw"
                    }
                    // Only shown when a ground actually exists. A claim with no
                    // ground is ignored at projection, so offering the button
                    // would invite writing a dead record into contract state
                    // permanently.
                    if can_claim {
                        button {
                            id: "draw-claim",
                            onclick: {
                                let label = label.clone();
                                move |_| wires.tx.send(Cmd::DrawClaim { label: label.clone() })
                            },
                            "claim draw"
                        }
                    }
                }
            }

            aside { class: "right",
                h3 { "moves" }
                ol { class: "history",
                    for (i, m) in history.iter().enumerate() {
                        li { key: "{i}", "{i + 1}. {m}" }
                    }
                }
            }

            if let Some((from, to)) = promoting() {
                div { class: "promo", role: "dialog",
                    p { "promote to" }
                    for (piece, ch) in [("queen", 'q'), ("rook", 'r'), ("bishop", 'b'), ("knight", 'n')] {
                        button {
                            key: "{ch}",
                            onclick: {
                                let label = label.clone();
                                move |_| {
                                    wires.tx.send(Cmd::Play {
                                        label: label.clone(),
                                        uci: format!("{}{}{}{}{}", from.0, from.1, to.0, to.1, ch),
                                    });
                                    promoting.set(None);
                                }
                            },
                            "{piece}"
                        }
                    }
                }
            }
        }
    }
}

/// A finished game's result, in words.
fn outcome(status: &adjourn_core::Status) -> String {
    use adjourn_core::Reason;
    let Some(d) = status.decision else {
        return String::new();
    };
    let who = match d.winner {
        Some(Color::White) => "White wins",
        Some(Color::Black) => "Black wins",
        None => "Draw",
    };
    let why = match d.reason {
        Reason::Checkmate => "checkmate",
        Reason::Stalemate => "stalemate",
        Reason::InsufficientMaterial => "insufficient material",
        Reason::AutomaticDraw => "automatic draw",
        Reason::Resignation => "resignation",
        Reason::DrawAgreement => "draw agreement",
        Reason::DoubleSignForfeit => "double-sign forfeit",
        Reason::MutualResignation => "mutual resignation",
        Reason::ThreefoldClaim => "threefold repetition, claimed",
        Reason::FiftyMoveClaim => "fifty-move rule, claimed",
    };
    format!("{who} — {why}")
}

/// The Unicode glyph for a piece; colour carries the distinction so both sides
/// have the same visual weight.
fn glyph(color: Color, role: Role) -> &'static str {
    use shakmaty::Role::*;
    // Colour is carried by the CSS class, not the glyph, so both sides have
    // the same visual weight.
    let _ = color;
    match role {
        King => "\u{265A}",
        Queen => "\u{265B}",
        Rook => "\u{265C}",
        Bishop => "\u{265D}",
        Knight => "\u{265E}",
        Pawn => "\u{265F}",
    }
}
