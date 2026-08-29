//! The shell: which screen is showing, and the one place errors surface.

use crate::conn::{use_conn, Cmd};
use dioxus::prelude::*;

/// The node's WebSocket API, matching the CLI's default. Loopback-only by
/// design — that is the real access boundary for a locally-bound game.
pub const DEFAULT_NODE_URL: &str =
    "ws://127.0.0.1:7509/v1/contract/command?encodingProtocol=native";

#[derive(Clone, Debug, PartialEq)]
pub enum Screen {
    List,
    New,
    Accept,
    Game(String),
    Settings,
}

#[component]
pub fn App() -> Element {
    let node_url = use_signal(|| DEFAULT_NODE_URL.to_string());
    let wires = use_conn(node_url);
    let mut screen = use_signal(|| Screen::List);

    // One connect-and-list on mount; the actor registers the delegate as part
    // of connecting, which is the browser's `adjourn init`.
    use_effect(move || {
        wires.tx.send(Cmd::ListGames);
    });

    rsx! {
        main { class: "app",
            header {
                h1 { "adjourn" }
                nav {
                    button { onclick: move |_| screen.set(Screen::List), "games" }
                    button { onclick: move |_| screen.set(Screen::New), "new game" }
                    button { onclick: move |_| screen.set(Screen::Accept), "accept invite" }
                    button { onclick: move |_| screen.set(Screen::Settings), "settings" }
                }
            }

            if let Some(e) = wires.error.cloned() {
                div { class: "error", role: "alert",
                    span { "{e}" }
                    button { onclick: move |_| wires.tx.send(Cmd::ListGames), "retry" }
                }
            }
            // Kept in its own signal and rendered separately from `error`:
            // the main actor clears `error` at the top of every command it
            // runs, and a watch death handled the same way would be wiped out
            // by the very next unrelated success (e.g. this banner's own
            // sibling's "retry", or any click that happens to send
            // `ListGames`) with the board still frozen and no evidence
            // anything went wrong. Retrying here re-sends `Watch` for the
            // game screen currently open, which is what actually revives a
            // dead watch -- `ListGames` succeeding against a healthy main
            // actor proves nothing about the separate watch connection.
            if let Some(e) = wires.watch_error.cloned() {
                div { class: "error watch-error", role: "alert",
                    span { "{e}" }
                    if let Screen::Game(label) = screen() {
                        button {
                            onclick: move |_| wires.tx.send(Cmd::Watch { label: label.clone() }),
                            "retry"
                        }
                    }
                }
            }
            // Not an error: the transport and the watch are both healthy, but
            // the opponent is still signing moves against the contract this
            // game migrated away from, so those moves are not reaching this
            // build. There is nothing to retry here -- the fix is out of
            // band, on the opponent's side (upgrade) or this one's (go back
            // to the old build) -- so this gets its own presentation rather
            // than reusing `.error`'s alarm styling or its retry button.
            if let Some(msg) = wires.skew.cloned() {
                div { class: "skew", role: "status",
                    span { "{msg}" }
                }
            }
            if (wires.busy)() {
                div { class: "busy", "working…" }
            }

            match screen() {
                Screen::List => rsx! { crate::views::list::GameList { wires, screen } },
                Screen::New => rsx! { crate::views::setup::NewGame { wires } },
                Screen::Accept => rsx! { crate::views::setup::AcceptInvite { wires } },
                Screen::Game(label) => rsx! { crate::views::game::GameScreen { wires, label } },
                Screen::Settings =>
                    rsx! { crate::views::settings::Settings { node_url, tx: wires.tx } },
            }
        }
    }
}
