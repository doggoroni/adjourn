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

    // `invite_blob` is keyed by the label that produced it and outlives this
    // component's own `label` signal, which resets to `""` on remount --
    // navigate to "games" and back to "new game" and the blob for a
    // previously created invite would otherwise render here captioned as
    // whatever the (empty) label happens to be right now. Only render it, and
    // only build `BindOffer` with it, when the stored label still matches.
    let current_label = label().trim().to_string();
    let invite_for_this_label = wires
        .invite_blob
        .read()
        .clone()
        .filter(|(l, _)| *l == current_label);

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

            if let Some((_, b)) = invite_for_this_label {
                h3 { "send this invite to your opponent" }
                textarea { id: "invite-out", readonly: true, rows: 4, "{b}" }
                h3 { "then paste the offer they send back" }
                BindOffer { wires, label: current_label.clone() }
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

    // Same label-keying as `NewGame`'s `invite_for_this_label` -- see there.
    let current_label = label().trim().to_string();
    let offer_for_this_label = wires
        .offer_blob
        .read()
        .clone()
        .filter(|(l, _)| *l == current_label);

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
            if let Some((_, b)) = offer_for_this_label {
                h3 { "send this offer back to the inviter" }
                textarea { id: "offer-out", readonly: true, rows: 4, "{b}" }
            }
        }
    }
}
