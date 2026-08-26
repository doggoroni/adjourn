use crate::conn::{Cmd, Wires};
use adjourn_core::delegate_api::Side;
use dioxus::prelude::*;

/// Create a key and an invite. The nonce is authored here and only here — the
/// accepter reads it from the invite — which is what stops the two sides
/// deriving different `GameParams` and landing on different contracts.
#[component]
pub fn NewGame(wires: Wires) -> Element {
    let mut label = use_signal(String::new);
    let mut side = use_signal(|| Side::White);

    rsx! {
        section { class: "screen",
            h2 { "new game" }
            label { r#for: "new-label", "label" }
            input {
                id: "new-label",
                value: "{label}",
                oninput: move |e| label.set(e.value()),
            }
            fieldset {
                legend { "your side" }
                label {
                    input {
                        r#type: "radio", name: "side", checked: side() == Side::White,
                        onchange: move |_| side.set(Side::White),
                    }
                    "White"
                }
                label {
                    input {
                        r#type: "radio", name: "side", checked: side() == Side::Black,
                        onchange: move |_| side.set(Side::Black),
                    }
                    "Black"
                }
            }
            button {
                id: "create",
                disabled: label().trim().is_empty(),
                onclick: move |_| wires.tx.send(Cmd::NewGame {
                    label: label().trim().to_string(),
                    side: side(),
                }),
                "create invite"
            }

            if let Some(b) = wires.invite_blob.cloned() {
                h3 { "send this invite to your opponent" }
                textarea { id: "invite-out", readonly: true, rows: 4, "{b}" }
                h3 { "then paste the offer they send back" }
                BindOffer { wires, label: label().trim().to_string() }
            }
        }
    }
}

/// The inviter's second step: bind the offer that comes back.
#[component]
fn BindOffer(wires: Wires, label: String) -> Element {
    let mut offer = use_signal(String::new);
    rsx! {
        textarea {
            id: "offer-in",
            rows: 4,
            value: "{offer}",
            oninput: move |e| offer.set(e.value()),
        }
        button {
            id: "bind",
            disabled: offer().trim().is_empty(),
            onclick: {
                let label = label.clone();
                move |_| wires.tx.send(Cmd::Bind {
                    label: label.clone(),
                    offer: offer(),
                })
            },
            "bind game"
        }
    }
}

/// The accepter's single step: paste an invite, get an offer to send back.
#[component]
pub fn AcceptInvite(wires: Wires) -> Element {
    let mut label = use_signal(String::new);
    let mut invite = use_signal(String::new);

    rsx! {
        section { class: "screen",
            h2 { "accept invite" }
            label { r#for: "accept-label", "label" }
            input {
                id: "accept-label",
                value: "{label}",
                oninput: move |e| label.set(e.value()),
            }
            label { r#for: "invite-in", "the invite you were sent" }
            textarea {
                id: "invite-in",
                rows: 4,
                value: "{invite}",
                oninput: move |e| invite.set(e.value()),
            }
            button {
                id: "accept",
                disabled: label().trim().is_empty() || invite().trim().is_empty(),
                onclick: move |_| wires.tx.send(Cmd::Accept {
                    label: label().trim().to_string(),
                    invite: invite(),
                }),
                "accept"
            }
            if let Some(b) = wires.offer_blob.cloned() {
                h3 { "send this offer back to the inviter" }
                textarea { id: "offer-out", readonly: true, rows: 4, "{b}" }
            }
        }
    }
}
