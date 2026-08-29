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
    /// Move a bound game onto the contract id this build derives. Sent from
    /// the game screen when `wires.error` reports a build mismatch for this
    /// label -- see `session::migrate_label`.
    Migrate {
        label: String,
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
    /// Follow a bound game live: run until the game ends, updating `view`'s
    /// `status` after every notification. Sent by the caller right after
    /// `Open` (see `views/list.rs`), never by a coroutine to itself -- a
    /// coroutine only gets its `Coroutine` handle back once its body has been
    /// constructed, so it cannot enqueue its own next command.
    ///
    /// This is long-lived by design: it does not return until the game is
    /// decided. Routed to its own dedicated coroutine below rather than run
    /// inline in the main actor, so a running watch never blocks resign,
    /// move, or list-games for the rest of the game.
    Watch {
        label: String,
    },
}

/// The handle every screen gets: one sender, and the signals results land in.
#[derive(Clone, Copy, PartialEq)]
pub struct Wires {
    pub tx: Coroutine<Cmd>,
    pub games: Signal<Vec<GameSummary>>,
    pub view: Signal<Option<GameView>>,
    /// The invite blob `NewGame` produces, shown only on the "new game"
    /// screen, and only for the label that produced it -- paired with that
    /// label so remounting `NewGame` with its local `label` signal reset to
    /// `""` (e.g. navigating to "games" and back) cannot render a surviving
    /// blob under the wrong label, and so `BindOffer` is never built with a
    /// label the blob does not match. Kept separate from `offer_blob` so
    /// navigating between the two setup screens never renders one command's
    /// output captioned as the other's -- see the fix-round-1 note in the
    /// task report.
    pub invite_blob: Signal<Option<(String, String)>>,
    /// The offer blob `Accept` produces, shown only on the "accept invite"
    /// screen, keyed by label for the same reason as `invite_blob`.
    pub offer_blob: Signal<Option<(String, String)>>,
    pub error: Signal<Option<String>>,
    /// Set only by the watch actor, and only on a genuine transport death
    /// while a watch was live -- never on the quiet, legitimate end of a
    /// decided game. Kept separate from `error` so an unrelated successful
    /// command in the main actor (e.g. the error banner's own "retry", which
    /// sends `ListGames`) cannot clear it out from under the user: `error` is
    /// cleared at the top of every main-actor command, and folding this into
    /// it would let that clear silently erase the only evidence the watch
    /// died.
    pub watch_error: Signal<Option<String>>,
    /// Set by the watch actor when `session::watch_label`'s `on_skew`
    /// callback fires: the opponent has moved on a contract this game
    /// migrated away from, and those moves are not reaching the contract
    /// this build derives (see `session::opponent_moved_on_previous`).
    /// Unlike `watch_error` this is NOT a transport failure -- the game is
    /// still fully readable and the watch is still running, which is why it
    /// is its own signal rather than reusing `watch_error` or `error`: a
    /// caller has to be able to tell "the node connection died" from
    /// "the connection is fine, but the other player is stuck on an old
    /// build" and react to the two differently (the latter never forces a
    /// reconnect). Cleared at the start of every fresh `Watch`, same as
    /// `watch_error`.
    pub skew: Signal<Option<String>>,
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
    use futures::{FutureExt, StreamExt};

    let mut games = use_signal(Vec::<GameSummary>::new);
    let mut view = use_signal(|| None::<GameView>);
    let mut invite_blob = use_signal(|| None::<(String, String)>);
    let mut offer_blob = use_signal(|| None::<(String, String)>);
    let mut error = use_signal(|| None::<String>);
    let mut watch_error = use_signal(|| None::<String>);
    let mut skew = use_signal(|| None::<String>);
    let mut busy = use_signal(|| false);
    let mut connected = use_signal(|| false);

    // A dedicated actor for `Watch`, with its own `BrowserClient` connected
    // to the same node. `next_update` has no request timeout -- a
    // correspondence move can legitimately take days -- so a watch parks this
    // coroutine for the whole game. Running it here rather than in the main
    // actor below is what keeps resign, move, and list-games free for the
    // whole time a watch is running: two sockets to one local node is cheap,
    // and each still has exactly one owner, which is the rule that makes
    // `BrowserClient` (not `Clone`, `&mut self` throughout) safe to hold
    // across an `.await` at all.
    let watch_tx = use_coroutine(move |mut rx: UnboundedReceiver<Cmd>| async move {
        let mut client: Option<BrowserClient> = None;
        // The command that should be processed next. Populated either by a
        // fresh `rx.next().await` at the bottom of the loop, or by the
        // command that raced an in-flight watch and won -- see below.
        let mut next_cmd = rx.next().await;

        while let Some(cmd) = next_cmd.take() {
            match cmd {
                // Only `Watch` and `Reconnect` ever arrive here -- see the
                // forwarding arms in the main actor below -- but ignoring
                // anything else is cheaper than asserting it can't happen.
                Cmd::Reconnect => {
                    client = None;
                    watch_error.set(None);
                    skew.set(None);
                }
                Cmd::Watch { label } => {
                    watch_error.set(None);
                    skew.set(None);

                    // `watch_label` does not return until the game is
                    // decided or the transport dies -- there is no timeout,
                    // by design (a correspondence move can take days). So it
                    // has to run raced against the next incoming command:
                    // opening a second game, or a reconnect. Whichever
                    // resolves first wins; the other future is simply
                    // dropped, and dropping a future cancels it -- no
                    // cancellation hook needed in `session::watch_label`.
                    let outcome: anyhow::Result<Option<Cmd>> = async {
                        if client.is_none() {
                            let mut fresh = BrowserClient::connect(&node_url()).await?;
                            let (container, _key) =
                                delegate_container(crate::DELEGATE_WASM.to_vec());
                            fresh.register_delegate(container).await?;
                            client = Some(fresh);
                        }
                        let c = client.as_mut().expect("just connected");
                        let wasm = crate::CONTRACT_WASM.to_vec();

                        // `watch_label` runs until the game ends, calling back
                        // after every update. It merges rather than replaces,
                        // and it subscribes -- `open_game_view`'s GET does not,
                        // which is why a watcher needs its own command.
                        let mut view_sig = view;
                        let mut skew_sig = skew;
                        let l = label.clone();
                        let watch_fut = session::watch_label(
                            c,
                            &label,
                            wasm,
                            move |state, status| {
                                view_sig.with_mut(|v| {
                                    if let Some(v) = v.as_mut() {
                                        if v.label == l {
                                            // Update together: `moves_in_order`
                                            // resolves `status.chain` against
                                            // `state.records`, so updating
                                            // `status` alone advances the board
                                            // and the status line while the
                                            // move history stays frozen at
                                            // whatever `state` the screen
                                            // opened with -- forever, since
                                            // nothing else ever touches it.
                                            v.state = state.clone();
                                            v.status = status.clone();
                                        }
                                    }
                                });
                            },
                            // Never fatal, never a teardown -- see the doc
                            // comment on `Wires::skew`. Latched here rather
                            // than appended to, matching `session.rs`'s own
                            // `reported_skew` latch: the message is static
                            // (`SKEW_WARNING`), so there is nothing to
                            // accumulate.
                            move |msg: &str| skew_sig.set(Some(msg.to_string())),
                        )
                        .fuse();
                        futures::pin_mut!(watch_fut);
                        let next = rx.next().fuse();
                        futures::pin_mut!(next);

                        futures::select! {
                            res = watch_fut => res.map(|()| None),
                            next_cmd = next => Ok(next_cmd),
                        }
                    }
                    .await;

                    match outcome {
                        // `watch_label` returns `Ok(())` in two situations
                        // that look identical from here and are NOT the same:
                        // the game ended (quiet, legitimate -- a decided game
                        // never produces another notification) and the
                        // socket died (`next_update` returned `Ok(None)`
                        // because `BrowserClient`'s `CloseLatch` fired). Tell
                        // them apart the same way the main actor does: a
                        // genuine transport death is exactly what
                        // `is_disconnected()` latches, and nothing else does.
                        // Treating both as quiet would freeze the board with
                        // no banner and no spinner on a restarted node -- the
                        // exact silent failure this file exists to rule out.
                        Ok(None) => {
                            let dead = client.as_ref().is_none_or(BrowserClient::is_disconnected);
                            if dead {
                                client = None;
                                watch_error.set(Some(
                                    "the connection to the node was lost while watching this \
                                     game"
                                        .to_string(),
                                ));
                            }
                        }
                        // A new command raced in and won while the watch was
                        // still live. It has already cancelled that watch
                        // (the future was dropped); process it next time
                        // round the loop, without an intervening
                        // `rx.next().await` that would eat a second command.
                        //
                        // Drop the client with it. `BrowserClient` correlates
                        // nothing: `next_response` hands back the next Result
                        // frame off the socket, whichever request it belongs
                        // to. Cancelling mid-request does not un-send that
                        // request, so its answer still arrives and is consumed
                        // by whatever asks next -- every later response is then
                        // off by one. That is not theoretical: it was observed
                        // in a browser as a GET failing with the delegate's
                        // ListGames reply pasted into "unexpected response to
                        // Get". A cancelled watch therefore leaves a socket
                        // whose stream cannot be trusted, and the honest thing
                        // is to reconnect rather than to reason about how many
                        // answers are outstanding. Reconnecting re-runs
                        // `register_delegate`, which is safe here: the
                        // container sets no `predecessors` and its key derives
                        // from the code alone, so re-registering byte-identical
                        // code addresses the same generation and does not
                        // rotate the per-game secret namespace.
                        Ok(Some(won)) => {
                            client = None;
                            next_cmd = Some(won);
                        }
                        Err(e) => {
                            // Same rule as the main actor: only a genuine
                            // transport death tears the client down and
                            // forces a reconnect (and a fresh
                            // `RegisterDelegate`) on the next watch.
                            let dead = client.as_ref().is_none_or(BrowserClient::is_disconnected);
                            if dead {
                                client = None;
                            }
                            watch_error.set(Some(format!("{e:#}")));
                        }
                    }
                }
                _ => {}
            }

            if next_cmd.is_none() {
                next_cmd = rx.next().await;
            }
        }
    });

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
                // The watch coroutine holds its own client to the same node
                // URL and must not keep talking to the old one -- forward the
                // teardown so it cancels any in-flight watch and drops its
                // client too.
                watch_tx.send(Cmd::Reconnect);
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
                        invite_blob.set(Some((label, inv.encode())));
                    }
                    Cmd::Accept { label, invite } => {
                        let inv = Invite::decode(invite.trim())?;
                        let offer =
                            session::invite_accept(c, &label, &inv, wasm, browser_entropy()?)
                                .await?;
                        offer_blob.set(Some((label, offer.encode())));
                    }
                    Cmd::Bind { label, offer } => {
                        let off = GameOffer::decode(offer.trim())?;
                        session::game_bind(c, &label, &off, wasm).await?;
                        // Clear only the invite blob this bind produced --
                        // this is the inviter's flow, finalising the offer
                        // their opponent sent back. `offer_blob` belongs to a
                        // wholly separate flow (this user as the ACCEPTER of
                        // some other game), and clearing it here wiped it out
                        // from under a user who happened to be both an
                        // inviter and an accepter at once.
                        invite_blob.with_mut(|b| {
                            if b.as_ref().is_some_and(|(l, _)| *l == label) {
                                *b = None;
                            }
                        });
                    }
                    Cmd::Migrate { label } => {
                        session::migrate_label(c, &label, wasm.clone()).await?;
                        view.set(Some(session::open_game_view(c, &label, wasm).await?));
                    }
                    Cmd::Open { label } => {
                        // Clear the shared view before awaiting the open, not
                        // after: otherwise a stale game's board keeps
                        // rendering under the new label for the entire
                        // request, and forever if the open then errors --
                        // the label guard in `GameScreen` alone would leave
                        // that case showing "loading" forever rather than
                        // the honest stale-cleared state.
                        view.set(None);
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
                        session::draw_offer(c, &label, wasm.clone()).await?;
                        view.set(Some(session::open_game_view(c, &label, wasm).await?));
                    }
                    Cmd::DrawAccept { label } => {
                        session::draw_accept(c, &label, wasm.clone()).await?;
                        view.set(Some(session::open_game_view(c, &label, wasm).await?));
                    }
                    Cmd::DrawClaim { label } => {
                        session::draw_claim(c, &label, wasm.clone()).await?;
                        view.set(Some(session::open_game_view(c, &label, wasm).await?));
                    }
                    Cmd::Watch { label } => {
                        // Forward, don't run: `watch_label` does not return
                        // until the game ends, and this actor must stay free
                        // to serve resign, move, and list-games while a watch
                        // is in progress. The dedicated coroutine above owns
                        // the actual client and loop.
                        watch_tx.send(Cmd::Watch { label });
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
        invite_blob,
        offer_blob,
        error,
        watch_error,
        skew,
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
    let invite_blob = use_signal(|| None::<(String, String)>);
    let offer_blob = use_signal(|| None::<(String, String)>);
    let mut error = use_signal(|| None::<String>);
    let watch_error = use_signal(|| None::<String>);
    let skew = use_signal(|| None::<String>);
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
        invite_blob,
        offer_blob,
        error,
        watch_error,
        skew,
        busy,
        connected,
    }
}
