use crate::conn::Cmd;
use dioxus::prelude::*;

#[component]
pub fn Settings(node_url: Signal<String>, tx: Coroutine<Cmd>) -> Element {
    rsx! {
        section { class: "screen",
            h2 { "settings" }
            label { r#for: "node-url", "node WebSocket URL" }
            input {
                id: "node-url",
                value: "{node_url}",
                oninput: move |e| {
                    node_url.set(e.value());
                    // A live socket is talking to the OLD url; keeping it
                    // open would make this field a no-op until something
                    // else happened to fail. Drop it now so the next command
                    // reconnects against whatever is here.
                    tx.send(Cmd::Reconnect);
                },
            }
            p { class: "hint",
                "The node's API is loopback-only. Changing this disconnects immediately; the new URL is used on the next action."
            }
        }
    }
}
