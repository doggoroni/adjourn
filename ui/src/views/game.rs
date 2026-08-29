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

    // Re-establish the watch whenever this screen mounts on a label, or the
    // label changes -- `GameList`'s row click sends the first `Watch`, but
    // nothing else ever sends another one. A watch that ends any way other
    // than being raced out by a fresh `Watch` (the game decides, or the
    // socket dies -- see `conn.rs`) leaves the screen sitting on a dead watch
    // for as long as the user stays here. `label` is a plain prop, not a
    // signal, so `use_reactive!` is what makes this effect re-run when it
    // changes rather than only once on the very first mount.
    use_effect(use_reactive!(|label| {
        wires.tx.send(Cmd::Watch { label });
    }));

    // `session::open_game`'s `expected_container` refuses to even GET when
    // this build derives a different contract id than the delegate recorded
    // for this label -- see `CLAUDE.md`'s "Client" section and
    // `expected_container`'s doc comment. That refusal lands in `wires.error`
    // as a message naming this exact label (it spells out the very
    // `adjourn game migrate --label <label>` command this button sends), so
    // matching on that text -- rather than showing the button unconditionally
    // -- is what keeps it hidden for every game that is not actually affected.
    let build_mismatch =
        wires.error.read().as_ref().is_some_and(|e| {
            e.contains("build mismatch") && e.contains(&format!("--label {label}"))
        });
    if build_mismatch {
        return rsx! {
            section { class: "screen",
                p { "{label} cannot be opened: this build's contract WASM no longer matches the one this game was bound with." }
                button {
                    id: "migrate",
                    class: "migrate",
                    onclick: {
                        let label = label.clone();
                        move |_| wires.tx.send(Cmd::Migrate { label: label.clone() })
                    },
                    "migrate this game"
                }
            }
        };
    }

    // `view` is one signal shared by every bound game. A stale view left over
    // from a different game (or one still in flight after `Cmd::Open` cleared
    // it to `None`) must never render under this screen's label -- treat a
    // mismatch exactly like "nothing loaded yet".
    let Some(view) = wires.view.read().clone().filter(|v| v.label == label) else {
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
                div {
                    class: if over || !our_turn { "board inactive" } else { "board" },
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
                                    // `squares()` highlights legal targets on
                                    // game-over alone; it never looks at whose
                                    // turn it is (see `board.rs`, deliberately
                                    // left untouched). Gate here instead: on
                                    // the opponent's turn, or once the game is
                                    // over, a click must not select a piece or
                                    // submit a move -- the board would only
                                    // offer moves `play_move` refuses.
                                    if over || !our_turn {
                                        return;
                                    }
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
