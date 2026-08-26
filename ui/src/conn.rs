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
    /// Drop the client and clear `connected`, without attempting a fresh
    /// connection. Settings sends this when the URL changes -- editing the
    /// URL while a live socket is open must not silently keep talking to the
    /// old one, and it must not re-run delegate registration on its own,
    /// which is why this does not immediately reconnect. The next command
    /// (e.g. a retry-button `ListGames`) does that.
    Reconnect,
    ListGames,
    NewGame {
        label: String,
        side: Side,
    },
    Accept {
        label: String,
        invite: String,
    },
    Bind {
        label: String,
        offer: String,
    },
    Open {
        label: String,
    },
    Play {
        label: String,
        uci: String,
    },
    Resign {
        label: String,
    },
    DrawOffer {
        label: String,
    },
    DrawAccept {
        label: String,
    },
    DrawClaim {
        label: String,
    },
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
    use adjourn_core::delegate_api::{Request, Response};
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
            // `Reconnect` is a pure teardown, not a request: it must not go
            // through the generic connect-and-run machinery below (that would
            // immediately reconnect and re-register the delegate, which is
            // exactly the needless churn this command exists to avoid). The
            // next real command reconnects, once, when it actually needs to.
            if matches!(cmd, Cmd::Reconnect) {
                client = None;
                connected.set(false);
                error.set(None);
                continue;
            }

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
                    Cmd::Connect | Cmd::Reconnect => {}
                    Cmd::ListGames => {
                        use adjourn_client::node::NodeClient;
                        match c.delegate(Request::ListGames).await? {
                            Response::Games(g) => games.set(g),
                            // A refusal is not "no games" -- leaving `games`
                            // alone here would read as a healthy empty
                            // account. Surface it like every other failure.
                            Response::Refused(r) => {
                                anyhow::bail!("delegate refused ListGames: {r}")
                            }
                            other => {
                                anyhow::bail!("unexpected response to ListGames: {other:?}")
                            }
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
                // Only tear the client down -- and with it, force the next
                // command to reconnect and re-register the delegate -- when
                // the transport itself is actually gone. `BrowserClient`
                // already tells apart a genuine close from a node-reported
                // refusal or a client-side bail (a bad paste, a client-side
                // legality check): `is_disconnected()` is backed by the same
                // `CloseLatch` that only ever latches on a real close, never
                // on an ordinary error response. Everything else -- a
                // mistyped invite, an out-of-turn move -- is shown without
                // touching a socket that is still perfectly healthy.
                let dead = client.as_ref().is_none_or(BrowserClient::is_disconnected);
                if dead {
                    client = None;
                    connected.set(false);
                }
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
    // ever renders this app. The stub still answers honestly rather than
    // black-holing every command -- `live.rs`, the module this replaced, did
    // the same (`Err("the browser transport is wasm32-only")`), and a
    // silently-discarded command is exactly the failure mode `conn.rs`
    // exists to rule out.
    use futures::StreamExt;

    let games = use_signal(Vec::<GameSummary>::new);
    let view = use_signal(|| None::<GameView>);
    let blob = use_signal(|| None::<String>);
    let mut error = use_signal(|| None::<String>);
    let busy = use_signal(|| false);
    let connected = use_signal(|| false);

    let tx = use_coroutine(move |mut rx: UnboundedReceiver<Cmd>| async move {
        while let Some(_cmd) = rx.next().await {
            error.set(Some("the browser transport is wasm32-only".into()));
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
