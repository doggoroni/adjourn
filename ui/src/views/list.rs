use crate::app::Screen;
use crate::conn::{Cmd, Wires};
use adjourn_core::delegate_api::Side;
use dioxus::prelude::*;

#[component]
pub fn GameList(wires: Wires, screen: Signal<Screen>) -> Element {
    let games = wires.games.read().clone();
    rsx! {
        section { class: "screen",
            h2 { "games" }
            if games.is_empty() {
                p { class: "hint",
                    "No games yet. Games created in the CLI will not appear here: the \
                     delegate partitions labels by origin, so a browser cannot see or \
                     continue them."
                }
            }
            ul { class: "games",
                for g in games {
                    li {
                        key: "{g.label}",
                        button {
                            class: "game-row",
                            onclick: {
                                let label = g.label.clone();
                                move |_| {
                                    wires.tx.send(Cmd::Open { label: label.clone() });
                                    // Following is a separate command because
                                    // `open_game_view`'s GET deliberately does
                                    // not subscribe -- the one-shot flows share
                                    // it and must not leave subscriptions behind.
                                    wires.tx.send(Cmd::Watch { label: label.clone() });
                                    screen.set(Screen::Game(label.clone()));
                                }
                            },
                            span { class: "label", "{g.label}" }
                            span { class: "side",
                                match g.side {
                                    Some(Side::White) => "White",
                                    Some(Side::Black) => "Black",
                                    None => "unbound",
                                }
                            }
                            span { class: "ply", "last signed ply {g.last_signed_ply}" }
                        }
                    }
                }
            }
            button { onclick: move |_| wires.tx.send(Cmd::ListGames), "refresh" }
        }
    }
}
