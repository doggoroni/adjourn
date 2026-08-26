use dioxus::prelude::*;

#[component]
pub fn Settings(node_url: Signal<String>) -> Element {
    rsx! {
        section { class: "screen",
            h2 { "settings" }
            label { r#for: "node-url", "node WebSocket URL" }
            input {
                id: "node-url",
                value: "{node_url}",
                oninput: move |e| node_url.set(e.value()),
            }
            p { class: "hint",
                "The node's API is loopback-only. Changing this takes effect on the next command."
            }
        }
    }
}
