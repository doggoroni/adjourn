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

            if let Some(e) = wires.error.read().clone() {
                div { class: "error", role: "alert", "{e}" }
            }
            if (wires.busy)() {
                div { class: "busy", "working…" }
            }

            match screen() {
                Screen::List | Screen::New | Screen::Accept | Screen::Game(_) =>
                    rsx! { p { class: "hint", "screen lands in a later task" } },
                Screen::Settings => rsx! { crate::views::settings::Settings { node_url } },
            }
        }
    }
}
