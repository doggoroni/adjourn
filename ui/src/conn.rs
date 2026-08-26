//! The single owner of the node connection.
//!
//! Every screen sends a [`Cmd`] and reads signals; nothing else touches the
//! client. That is not tidiness: `BrowserClient` takes `&mut self` and is not
//! `Clone`, and the obvious `Rc<RefCell<_>>` in a context panics at runtime,
//! because a `RefCell` borrow cannot be held across an `.await` and every call
//! here awaits. A coroutine owns the client outright and serialises commands,
//! which removes the hazard rather than managing it.

use adjourn_client::session::GameView;
use adjourn_core::delegate_api::{GameSummary, Side};
use dioxus::prelude::*;

#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    Connect,
    ListGames,
    NewGame { label: String, side: Side },
    Accept { label: String, invite: String },
    Bind { label: String, offer: String },
    Open { label: String },
    Play { label: String, uci: String },
    Resign { label: String },
    DrawOffer { label: String },
    DrawAccept { label: String },
    DrawClaim { label: String },
}

/// The handle every screen gets: one sender, and the signals results land in.
#[derive(Clone, Copy)]
pub struct Wires {
    pub tx: Coroutine<Cmd>,
    pub games: Signal<Vec<GameSummary>>,
    pub view: Signal<Option<GameView>>,
    /// An invite or offer blob to show the user for copying.
    pub blob: Signal<Option<String>>,
    pub error: Signal<Option<String>>,
    pub busy: Signal<bool>,
    pub connected: Signal<bool>,
}

#[cfg(target_arch = "wasm32")]
pub fn use_conn(node_url: Signal<String>) -> Wires {
    use crate::node::{browser_entropy, browser_nonce, BrowserClient};
    use adjourn_client::invite::{GameOffer, Invite};
    use adjourn_client::node::delegate_container;
    use adjourn_client::session;
    use futures::StreamExt;

    let mut games = use_signal(Vec::<GameSummary>::new);
    let mut view = use_signal(|| None::<GameView>);
    let mut blob = use_signal(|| None::<String>);
    let mut error = use_signal(|| None::<String>);
    let mut busy = use_signal(|| false);
    let mut connected = use_signal(|| false);

    let tx = use_coroutine(move |mut rx: UnboundedReceiver<Cmd>| async move {
        let mut client: Option<BrowserClient> = None;

        while let Some(cmd) = rx.next().await {
            busy.set(true);
            error.set(None);

            // Every arm is fallible and every failure is shown. A silent
            // failure here reads exactly like a healthy idle game, which is
            // the defect this transport has already had twice.
            let outcome: anyhow::Result<()> = async {
                if client.is_none() {
                    let mut fresh = BrowserClient::connect(&node_url()).await?;
                    let (container, _key) = delegate_container(crate::DELEGATE_WASM.to_vec());
                    fresh.register_delegate(container).await?;
                    client = Some(fresh);
                    connected.set(true);
                }
                let c = client.as_mut().expect("just connected");
                let wasm = crate::CONTRACT_WASM.to_vec();

                match cmd.clone() {
                    Cmd::Connect => {}
                    Cmd::ListGames => {
                        use adjourn_client::node::NodeClient;
                        use adjourn_core::delegate_api::{Request, Response};
                        if let Response::Games(g) = c.delegate(Request::ListGames).await? {
                            games.set(g);
                        }
                    }
                    Cmd::NewGame { label, side } => {
                        let inv = session::invite_new(
                            c,
                            &label,
                            side,
                            browser_entropy()?,
                            browser_nonce()?,
                        )
                        .await?;
                        blob.set(Some(inv.encode()));
                    }
                    Cmd::Accept { label, invite } => {
                        let inv = Invite::decode(invite.trim())?;
                        let offer =
                            session::invite_accept(c, &label, &inv, wasm, browser_entropy()?)
                                .await?;
                        blob.set(Some(offer.encode()));
                    }
                    Cmd::Bind { label, offer } => {
                        let off = GameOffer::decode(offer.trim())?;
                        session::game_bind(c, &label, &off, wasm).await?;
                        blob.set(None);
                    }
                    Cmd::Open { label } => {
                        view.set(Some(session::open_game_view(c, &label, wasm).await?));
                    }
                    Cmd::Play { label, uci } => {
                        session::play_move(c, &label, &uci, wasm.clone()).await?;
                        view.set(Some(session::open_game_view(c, &label, wasm).await?));
                    }
                    Cmd::Resign { label } => {
                        session::resign(c, &label, wasm.clone()).await?;
                        view.set(Some(session::open_game_view(c, &label, wasm).await?));
                    }
                    Cmd::DrawOffer { label } => {
                        session::draw_offer(c, &label, wasm).await?;
                    }
                    Cmd::DrawAccept { label } => {
                        session::draw_accept(c, &label, wasm.clone()).await?;
                        view.set(Some(session::open_game_view(c, &label, wasm).await?));
                    }
                    Cmd::DrawClaim { label } => {
                        session::draw_claim(c, &label, wasm.clone()).await?;
                        view.set(Some(session::open_game_view(c, &label, wasm).await?));
                    }
                }
                Ok(())
            }
            .await;

            if let Err(e) = outcome {
                // Drop the client on failure so the next command reconnects.
                // A dead socket cannot be revived, and holding it would make
                // every later command fail for a reason the user cannot see.
                client = None;
                connected.set(false);
                error.set(Some(format!("{e:#}")));
            }
            busy.set(false);
        }
    });

    Wires {
        tx,
        games,
        view,
        blob,
        error,
        busy,
        connected,
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn use_conn(_node_url: Signal<String>) -> Wires {
    // Native builds exist only so the crate's tests can run; nothing native
    // ever renders this app.
    Wires {
        tx: use_coroutine(|_rx: UnboundedReceiver<Cmd>| async move {}),
        games: use_signal(Vec::new),
        view: use_signal(|| None),
        blob: use_signal(|| None),
        error: use_signal(|| None),
        busy: use_signal(|| false),
        connected: use_signal(|| false),
    }
}
