//! The adjourn web UI.
//!
//! Currently a bring-up page. It renders the opening position from a projected
//! `GameState` with click-to-select legal targets, and offers a one-button
//! probe that connects to a local node, registers the delegate and lists
//! games. No signing and no game flow yet — the point is to prove the
//! toolchain and the transport end to end, since nothing in this crate had
//! ever been loaded in a browser and `BrowserClient` had never talked to
//! anything. The five real screens land with the views work.
//!
//! Referencing `adjourn_ui` at all is load-bearing: until this file used the
//! library, the binary did not link it, so `dx build` emitted a bundle
//! containing neither the board nor the two embedded WASM modules.

use adjourn_core::{project, GameParams, GameState, Status};
use adjourn_ui::board::{squares, Marker, Shade, Square};
use dioxus::prelude::*;
use shakmaty::{Color, Role};

/// The node's WebSocket API, matching the CLI's default.
///
/// Loopback-only by design: `CLAUDE.md` records that the real boundary for a
/// CLI-bound game is that this API is not reachable off the machine.
const NODE_URL: &str = "ws://127.0.0.1:7509/v1/contract/command?encodingProtocol=native";

fn main() {
    dioxus::launch(App);
}

/// The opening position, projected from an empty record set.
///
/// The params carry placeholder keys: an empty state has no records to verify,
/// so projection never consults them. Real params come from an invite.
fn opening() -> (GameState, GameParams) {
    let params = GameParams {
        white: [1u8; 32],
        black: [2u8; 32],
        nonce: [7u8; 16],
    };
    (GameState::empty(), params)
}

/// The Unicode glyph for a piece. Rendering both colours from the filled
/// glyphs (rather than mixing filled and outlined) keeps them the same visual
/// weight; colour does the distinguishing.
fn glyph(color: Color, role: Role) -> &'static str {
    match (color, role) {
        (Color::White, Role::King) => "\u{265A}",
        (Color::White, Role::Queen) => "\u{265B}",
        (Color::White, Role::Rook) => "\u{265C}",
        (Color::White, Role::Bishop) => "\u{265D}",
        (Color::White, Role::Knight) => "\u{265E}",
        (Color::White, Role::Pawn) => "\u{265F}",
        (Color::Black, Role::King) => "\u{265A}",
        (Color::Black, Role::Queen) => "\u{265B}",
        (Color::Black, Role::Rook) => "\u{265C}",
        (Color::Black, Role::Bishop) => "\u{265D}",
        (Color::Black, Role::Knight) => "\u{265E}",
        (Color::Black, Role::Pawn) => "\u{265F}",
    }
}

/// Fold both embedded modules at runtime, so the bytes genuinely ship.
///
/// `std::hint::black_box` stops the optimiser proving the fold constant and
/// discarding the data — the whole point is that the modules are present in
/// the bundle, not that the number is interesting.
fn embedded_summary() -> String {
    fn checksum(bytes: &[u8]) -> u32 {
        std::hint::black_box(bytes)
            .iter()
            .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(*b as u32))
    }
    format!(
        "embedded: contract {} B (sum {:08x}) · delegate {} B (sum {:08x})",
        adjourn_ui::CONTRACT_WASM.len(),
        checksum(adjourn_ui::CONTRACT_WASM),
        adjourn_ui::DELEGATE_WASM.len(),
        checksum(adjourn_ui::DELEGATE_WASM),
    )
}

#[component]
fn App() -> Element {
    let (state, params) = opening();
    let status: Status = project(&state, &params);

    // Which square the user has clicked, if any. `squares` turns that into the
    // legal targets to highlight, using the same move generation the
    // projection uses.
    let mut selected = use_signal(|| None::<(char, u8)>);

    // The live-node probe's output, and whether one is in flight.
    let mut lines = use_signal(Vec::<String>::new);
    let mut probing = use_signal(|| false);

    let board: Vec<Square> = squares(&status, Color::White, selected());

    rsx! {
        main { class: "app",
            h1 { "adjourn" }
            p { class: "sub", "untimed correspondence chess — bring-up page" }

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
                        onclick: move |_| {
                            // Click the selected square again to clear it.
                            let here = (sq.file, sq.rank);
                            selected.set(if selected() == Some(here) { None } else { Some(here) });
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
                "ply {status.ply} · {status.turn:?} to move · click a piece to see its legal moves"
            }

            // A checksum, not a length. The modules are `const`, so they are
            // inlined at their use sites and an unreferenced `const` emits no
            // data at all -- `include_bytes!` by itself costs zero bytes. And
            // `.len()` does not help: it const-folds, so measuring them still
            // emits nothing. Only a use that reads the bytes at runtime puts
            // them in the bundle. The views will do that for real, PUTting the
            // contract and registering the delegate; until then this fold is
            // what makes the embedding true rather than nominal.
            p { class: "status", "{embedded_summary()}" }

            hr {}
            h2 { "node" }
            button {
                id: "connect",
                disabled: probing(),
                onclick: move |_| {
                    probing.set(true);
                    spawn(async move {
                        let result = adjourn_ui::live::probe(NODE_URL).await;
                        lines.set(match result {
                            Ok(ok) => ok,
                            // Show the failure rather than a spinner. Every
                            // Critical this transport has had presented as a
                            // hang, so a visible error is the point.
                            Err(e) => vec![format!("FAILED — {e}")],
                        });
                        probing.set(false);
                    });
                },
                if probing() { "connecting…" } else { "connect, register the delegate, list games" }
            }
            pre { id: "probe", class: "probe",
                if lines().is_empty() { "not attempted" } else { "{lines().join(\"\\n\")}" }
            }
        }
    }
}
