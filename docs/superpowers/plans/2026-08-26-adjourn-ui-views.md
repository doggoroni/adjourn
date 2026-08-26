# Web UI Views Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A browser app that plays a complete correspondence game against a local Freenet node — list games, create and accept an invite, watch the board update when the opponent moves, play a move, resign, offer/accept a draw, and claim a draw.

**Architecture:** A single **connection actor** (a Dioxus coroutine) owns the one `BrowserClient` and serves commands from the screens; results land in signals. The screens are thin renderers over `adjourn-client`'s existing flows and `ui/src/board.rs`'s pure square descriptors. No screen touches the node directly.

**Tech Stack:** Rust 1.97.1 (pinned), Dioxus 0.7.9 (`web`), `dx` 0.7.9, `freenet-stdlib` 0.8.5 with `net`, `shakmaty` 0.30.1.

**Spec:** `docs/superpowers/specs/2026-08-25-adjourn-web-ui-design.md`

This plan completes the spec. The foundation (crate, board, transport) and the bring-up page already exist and are verified against a live node.

## Global Constraints

- **Both build directions must pass, every task**: `cargo check -p adjourn-ui --target wasm32-unknown-unknown --locked` **and** `cargo check -p adjourn-ui --all-targets --locked`. A native check alone proved nothing before and still does not; a wasm check alone silently killed the test suite once. Anything touching `BrowserClient` needs `#[cfg(target_arch = "wasm32")]` with a native stub, as `ui/src/live.rs` already does.
- **Do NOT add `rand` or `getrandom` to `ui`.** Entropy comes from `browser_entropy()`, which uses `crypto.getRandomValues` via `web_sys`. A `getrandom` in the graph emits wasm-bindgen placeholder imports and breaks contract instantiation.
- **Do NOT modify anything under `common/`.** `adjourn-core` compiles into the contract, so a change there rotates the contract key and orphans every game. New shared helpers go in `client/` (`adjourn-client`), which is not in the contract's graph.
- **The nonce has exactly one author — the inviter.** `invite_new` takes it; `invite_accept` reads it from the invite and authors nothing. If both sides ever contribute, they derive different `GameParams`, land on different contract ids, and each sees a game the other never joins, with no error anywhere.
- **Never hold a `RefCell` borrow of the client across an `.await`.** That is why the connection actor exists — see Task 2.
- **`cargo test --workspace --locked` green, `cargo fmt --all -- --check` clean, `cargo clippy --workspace --all-targets -- -D warnings` clean** before every commit.
- **Never** run `cargo build --release` on the contract or delegate — use `scripts/build-contract.sh` / `scripts/build-delegate.sh`.

## Environment (all verified working)

- `dx` 0.7.9 is installed in WSL at `~/.cargo/bin/dx`. Build with `cd ui && dx build --platform web`; serve with `dx serve --platform web --addr 0.0.0.0 --port 8080`.
- A local node runs in WSL on port 7509 and is reachable from the Windows browser. Start it with:
  `setsid nohup freenet local --ws-api-port 7509 --data-dir ~/.adjourn-ui-node > ~/node-ui.log 2>&1 < /dev/null &`
  Plain `nohup` does **not** survive the shell; `setsid` does.
- `NODE_URL` is `ws://127.0.0.1:7509/v1/contract/command?encodingProtocol=native`.
- Windows cannot link `cargo test`; run the suite in WSL. After any mutate-and-restore cycle run `cargo clean -p adjourn-ui` before trusting a baseline — cargo's fingerprint survives a restore and will rerun the stale binary.

## File structure

| file | responsibility |
|---|---|
| `client/src/session.rs` (modify) | gains `GameView` + `open_game_view` + `moves_in_order` — the move list needs records, and `Status` carries only ids |
| `ui/src/node.rs` (modify) | gains `browser_nonce()` — the inviter's 16 nonce bytes |
| `ui/src/conn.rs` (new) | the connection actor: owns one `BrowserClient`, serves `Cmd`s, writes signals. Task 5 adds a second, dedicated watch coroutine here with its own client, so following a game never blocks resigning |
| `ui/src/app.rs` (new) | the shell: screen enum, routing, error banner |
| `ui/src/views/list.rs` (new) | game list — the landing screen |
| `ui/src/views/setup.rs` (new) | new game, accept invite, bind offer |
| `ui/src/views/game.rs` (new) | layout B: board left, move history right, status, actions |
| `ui/src/views/settings.rs` (new) | node URL |
| `ui/src/main.rs` (modify) | shrinks to `dioxus::launch(app::App)` |

---

### Task 1: Expose what the move list needs

**Files:**
- Modify: `client/src/session.rs`
- Modify: `ui/src/node.rs`
- Test: `client/tests/view.rs` (new)

**Interfaces:**
- Produces:
  - `pub struct GameView { pub label: String, pub side: Side, pub params: GameParams, pub contract: [u8; 32], pub state: GameState, pub status: Status }`
  - `pub async fn open_game_view<N: NodeClient>(node: &mut N, label: &str, contract_wasm: Vec<u8>) -> anyhow::Result<GameView>`
  - `pub fn moves_in_order(view: &GameView) -> Vec<String>`
  - `pub fn browser_nonce() -> anyhow::Result<[u8; 16]>` (in `ui/src/node.rs`, inside the wasm-gated module beside `browser_entropy`)

**Why.** `show_label` returns a `Status`, whose `chain` is `Vec<RecordId>` — ids, not moves. Rendering a move history needs the `Record`s those ids name, which live in the `GameState`. `session.rs` already computes exactly this in the private `open_game`; this makes it public without duplicating it.

- [ ] **Step 1: Write the failing test**

`client/tests/view.rs`:

```rust
mod common;

use adjourn_client::fake::{shared_world, FakeNode};
use adjourn_client::session::{
    game_bind, invite_accept, invite_new, moves_in_order, open_game_view, play_move,
};
use adjourn_core::delegate_api::Side;

#[tokio::test]
async fn the_view_carries_the_moves_in_order() {
    let Some(wasm) = common::contract_wasm() else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };
    let world = shared_world();
    let (mut alice, mut bob) = (FakeNode::new(world.clone()), FakeNode::new(world));
    let invite = invite_new(&mut alice, "alice", Side::White, [0xa1; 32], [0x11; 16])
        .await
        .unwrap();
    let offer = invite_accept(&mut bob, "bob", &invite, wasm.clone(), [0xb0; 32])
        .await
        .unwrap();
    game_bind(&mut alice, "alice", &offer, wasm.clone())
        .await
        .unwrap();

    for (node, label, uci) in [
        (&mut alice, "alice", "e2e4"),
        (&mut bob, "bob", "e7e5"),
        (&mut alice, "alice", "g1f3"),
    ] {
        play_move(node, label, uci, wasm.clone()).await.unwrap();
    }

    let view = open_game_view(&mut alice, "alice", wasm).await.unwrap();
    assert_eq!(view.status.ply, 3);
    assert_eq!(view.side, Side::White);
    assert_eq!(
        moves_in_order(&view),
        vec!["e2e4", "e7e5", "g1f3"],
        "the history is the chain in order, not the record set's id order"
    );
}

/// The chain is ordered; the record map is not. A history built by iterating
/// the map would come out in id order, which is effectively random.
#[tokio::test]
async fn the_move_order_is_the_chain_not_the_map() {
    let Some(wasm) = common::contract_wasm() else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };
    let world = shared_world();
    let (mut alice, mut bob) = (FakeNode::new(world.clone()), FakeNode::new(world));
    let invite = invite_new(&mut alice, "alice", Side::White, [0xa1; 32], [0x11; 16])
        .await
        .unwrap();
    let offer = invite_accept(&mut bob, "bob", &invite, wasm.clone(), [0xb0; 32])
        .await
        .unwrap();
    game_bind(&mut alice, "alice", &offer, wasm.clone())
        .await
        .unwrap();
    for (node, label, uci) in [
        (&mut alice, "alice", "e2e4"),
        (&mut bob, "bob", "e7e5"),
        (&mut alice, "alice", "g1f3"),
        (&mut bob, "bob", "b8c6"),
    ] {
        play_move(node, label, uci, wasm.clone()).await.unwrap();
    }
    let view = open_game_view(&mut alice, "alice", wasm).await.unwrap();

    let by_map: Vec<String> = view
        .state
        .records
        .values()
        .filter_map(|r| match &r.body {
            adjourn_core::Body::Move { uci, .. } => Some(uci.clone()),
            _ => None,
        })
        .collect();
    let by_chain = moves_in_order(&view);

    assert_eq!(by_chain, vec!["e2e4", "e7e5", "g1f3", "b8c6"]);
    assert_ne!(
        by_map, by_chain,
        "if these ever match, this fixture stopped proving anything -- pick \
         moves whose record ids sort differently from their play order"
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p adjourn-client --test view --locked`
Expected: compile failure — `open_game_view`, `moves_in_order`, `GameView` do not exist.

- [ ] **Step 3: Make the view public**

In `client/src/session.rs`, rename the private `struct OpenGame` to a public `GameView` with public fields, keeping every field it already has, and add the doc:

```rust
/// Everything a screen needs about one bound game.
///
/// `state` is here as well as `status` because `Status.chain` carries record
/// IDs, not moves: rendering a move history means looking those IDs up in the
/// record set. Nothing else needs the raw state.
#[derive(Clone, Debug)]
pub struct GameView {
    pub label: String,
    pub side: Side,
    pub params: GameParams,
    pub game_id: [u8; 32],
    pub contract: [u8; 32],
    pub state: GameState,
    pub status: Status,
}
```

`open_game` currently has no `label` field; add one and set it from its argument. Update the private callers (`play_move`, `resign`, `draw_offer`, `draw_accept`, `draw_claim`, `watch_label`, `show_label`) — they destructure or field-access `g`, so the rename is mechanical. **`container` stays out of `GameView`**: `ContractContainer` is not `Clone` in the way a UI signal needs, and only the submit path uses it. Keep `open_game` as the private function returning the container alongside, and have `open_game_view` call it:

```rust
/// The public half of `open_game`, for screens that only need to render.
pub async fn open_game_view<N: NodeClient>(
    node: &mut N,
    label: &str,
    contract_wasm: Vec<u8>,
) -> anyhow::Result<GameView> {
    Ok(open_game(node, label, contract_wasm).await?.view)
}
```

Restructure `open_game`'s return as a small private struct holding `view: GameView` plus `container: ContractContainer`, so there is exactly one place that opens a game.

- [ ] **Step 4: Add the move-list helper**

Also in `client/src/session.rs`:

```rust
/// The accepted moves, in play order, as UCI.
///
/// Driven by `status.chain` and NOT by iterating `state.records`: the record
/// set is a `BTreeMap` keyed by ID, so iterating it yields hash order, which
/// has nothing to do with the order the moves were played.
pub fn moves_in_order(view: &GameView) -> Vec<String> {
    view.status
        .chain
        .iter()
        .filter_map(|id| match &view.state.records.get(id)?.body {
            Body::Move { uci, .. } => Some(uci.clone()),
            _ => None,
        })
        .collect()
}
```

- [ ] **Step 5: Add the nonce helper**

In `ui/src/node.rs`, inside the `#[cfg(target_arch = "wasm32")] mod browser` block, beside `browser_entropy`:

```rust
/// 16 bytes from the browser's CSPRNG, for an invite's nonce.
///
/// The nonce distinguishes repeat matchups between the same two players and
/// has exactly one author, the inviter — that is what stops the two sides
/// deriving different `GameParams`.
pub fn browser_nonce() -> anyhow::Result<[u8; 16]> {
    let mut bytes = [0u8; 16];
    web_sys::window()
        .ok_or_else(|| anyhow!("no window"))?
        .crypto()
        .map_err(|e| anyhow!("no crypto: {e:?}"))?
        .get_random_values_with_u8_array(&mut bytes)
        .map_err(|e| anyhow!("crypto.getRandomValues failed: {e:?}"))?;
    Ok(bytes)
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p adjourn-client --test view --locked`, then `cargo test --workspace --locked`.
Expected: PASS, with two new tests.

Then both build directions for the UI:
`cargo check -p adjourn-ui --target wasm32-unknown-unknown --locked` and `cargo check -p adjourn-ui --all-targets --locked`.

- [ ] **Step 7: Prove the order test discriminates**

Temporarily rewrite `moves_in_order` to iterate `view.state.records.values()` instead of the chain, and confirm `the_view_carries_the_moves_in_order` FAILS. Restore, run `cargo clean -p adjourn-client`, confirm it passes. Report what you saw.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat(client): expose GameView and the ordered move list"
```

---

### Task 2: The connection actor and the app shell

**Files:**
- Create: `ui/src/conn.rs`, `ui/src/app.rs`, `ui/src/views/mod.rs`, `ui/src/views/settings.rs`
- Modify: `ui/src/main.rs`, `ui/src/lib.rs`

**Interfaces:**
- Consumes: `BrowserClient`, `browser_entropy`, `browser_nonce`, `adjourn_client::session::*`, `GameView`.
- Produces:
  - `pub enum Cmd { Connect, ListGames, NewGame { label: String, side: Side }, Accept { label: String, invite: String }, Bind { label: String, offer: String }, Open { label: String }, Play { label: String, uci: String }, Resign { label: String }, DrawOffer { label: String }, DrawAccept { label: String }, DrawClaim { label: String } }`
  - `pub struct Wires { pub tx: Coroutine<Cmd>, pub games: Signal<Vec<GameSummary>>, pub view: Signal<Option<GameView>>, pub blob: Signal<Option<String>>, pub error: Signal<Option<String>>, pub busy: Signal<bool>, pub connected: Signal<bool> }`
  - `pub fn use_conn(node_url: Signal<String>) -> Wires`
  - `pub enum Screen { List, New, Accept, Game(String), Settings }`

**Why an actor.** `BrowserClient` takes `&mut self` on every call and is not `Clone`. The obvious `Rc<RefCell<BrowserClient>>` in a context **panics at runtime**: a `RefCell` borrow cannot be held across an `.await`, and every one of these calls awaits. A coroutine owns the client outright and serialises commands, which removes the problem instead of managing it.

- [ ] **Step 1: Write the connection actor**

`ui/src/conn.rs`. The whole module is wasm-gated with a native stub, exactly as `live.rs` is, because `BrowserClient` does not exist off-wasm:

```rust
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
```

Then the coroutine itself, wasm-only:

```rust
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

    Wires { tx, games, view, blob, error, busy, connected }
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
```

- [ ] **Step 2: Write the shell**

`ui/src/app.rs`:

```rust
//! The shell: which screen is showing, and the one place errors surface.

use crate::conn::{use_conn, Cmd, Wires};
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
            if wires.busy() {
                div { class: "busy", "working…" }
            }

            match screen() {
                Screen::List => rsx! { crate::views::list::GameList { wires, screen } },
                Screen::New => rsx! { crate::views::setup::NewGame { wires } },
                Screen::Accept => rsx! { crate::views::setup::AcceptInvite { wires } },
                Screen::Game(label) => rsx! { crate::views::game::GameScreen { wires, label } },
                Screen::Settings => rsx! { crate::views::settings::Settings { node_url } },
            }
        }
    }
}
```

- [ ] **Step 3: Write the settings screen**

`ui/src/views/settings.rs`:

```rust
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
```

`ui/src/views/mod.rs` — **only `settings` exists at this point**; Tasks 3 and 4
add their own lines. Declaring a module whose file does not exist is a compile
error, so do not write the others yet:

```rust
pub mod settings;
```

The shell's `match` in Step 2 therefore cannot reference `list`, `setup` or
`game` yet either. Until Task 3 lands, render a placeholder for those arms:

```rust
                Screen::List | Screen::New | Screen::Accept | Screen::Game(_) =>
                    rsx! { p { class: "hint", "screen lands in a later task" } },
                Screen::Settings => rsx! { crate::views::settings::Settings { node_url } },
```

- [ ] **Step 4: Shrink main.rs and declare the modules**

`ui/src/main.rs` becomes:

```rust
use dioxus::prelude::*;

fn main() {
    dioxus::launch(adjourn_ui::app::App);
}
```

Add to `ui/src/lib.rs`, beside the existing `pub mod board; pub mod live; pub mod node;`:

```rust
pub mod app;
pub mod conn;
pub mod views;
```

The bring-up page's board rendering moves into `views/game.rs` in Task 4; delete `ui/src/live.rs` and its `lib.rs` declaration **only after** Task 2 compiles, since the shell replaces what it demonstrated.

- [ ] **Step 5: Verify**

Both build directions, plus `cargo test --workspace --locked`, `cargo fmt --all -- --check`, and clippy.

Then build and load it: `cd ui && dx build --platform web`, `dx serve --platform web --addr 0.0.0.0 --port 8080`, and open `http://localhost:8080/`. With a node running you should see an empty game list and no error banner; with the node stopped you should see the error banner rather than a spinner. **Check both**, and report what you saw — the transport's two Criticals both presented as an endless spinner.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(ui): connection actor and app shell"
```

---

### Task 3: Game list, new game, accept invite

**Files:**
- Create: `ui/src/views/list.rs`, `ui/src/views/setup.rs`

**Interfaces:**
- Consumes: `Wires`, `Cmd`, `Screen`.
- Produces: components `GameList`, `NewGame`, `AcceptInvite`.

**The invite exchange is out of band, deliberately.** Two copy-pasteable blobs, exactly as the CLI does it — no lobby and no matchmaking, both named anti-goals. The inviter runs *new game* and sends the invite; the accepter runs *accept invite* and sends the offer back; the inviter pastes the offer to bind.

- [ ] **Step 1: Write the game list**

`ui/src/views/list.rs`:

```rust
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
```

- [ ] **Step 2: Write the setup screens**

`ui/src/views/setup.rs`:

```rust
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

            if let Some(b) = wires.blob.read().clone() {
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
            if let Some(b) = wires.blob.read().clone() {
                h3 { "send this offer back to the inviter" }
                textarea { id: "offer-out", readonly: true, rows: 4, "{b}" }
            }
        }
    }
}
```

- [ ] **Step 3: Verify**

Both build directions, the suite, `fmt`, clippy. Then build, serve, and with a node running create an invite — confirm a blob appears and no error banner. Report what you saw.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat(ui): game list and the invite exchange screens"
```

---

### Task 4: The game screen (layout B)

**Files:**
- Create: `ui/src/views/game.rs`
- Modify: `ui/index.html` (styles for the two-column layout and the promotion picker)
- Delete: `ui/src/live.rs` and its `lib.rs` declaration

**Interfaces:**
- Consumes: `Wires`, `Cmd`, `board::{squares, is_promotion, Marker, Shade, Square}`, `session::moves_in_order`.
- Produces: component `GameScreen`.

**Layout B, and why the history is always visible.** Board left, scrollable move history right, status beneath, actions under that. The history is not collapsed behind a disclosure because **the outcome is not monotone**: a late-published double-sign fraud proof forfeits a player, rewinds the board and can flip the winner. `CLAUDE.md` says the UI should show the full chain rather than the truncated position after a forfeit — which a hidden history cannot do.

- [ ] **Step 1: Write the game screen**

`ui/src/views/game.rs`:

```rust
use crate::board::{is_promotion, squares, Marker, Shade};
use crate::conn::{Cmd, Wires};
use adjourn_client::session::moves_in_order;
use dioxus::prelude::*;
use shakmaty::{Color, Role};
```

The component:

```rust
#[component]
pub fn GameScreen(wires: Wires, label: String) -> Element {
    let mut selected = use_signal(|| None::<(char, u8)>);
    // Set when a click resolves to a pawn reaching the last rank: the move is
    // held until the user picks a piece. A UI that always queens cannot play
    // some legal games, and the algebra already supports underpromotion.
    let mut promoting = use_signal(|| None::<((char, u8), (char, u8))>);

    let Some(view) = wires.view.read().clone() else {
        return rsx! { section { class: "screen", p { "loading {label}…" } } };
    };

    // `From<Side> for Color` already exists in `delegate_api`; `session.rs`
    // uses the same `.into()` idiom.
    let orientation: Color = view.side.into();
    let board = squares(&view.status, orientation, selected());
    let history = moves_in_order(&view);
    let our_turn = orientation == view.status.turn;
    let over = view.status.is_over();
    let can_claim = !over && our_turn
        && (view.status.repetitions >= 3 || view.status.halfmove_clock >= 100);

    rsx! {
        section { class: "screen game",
            div { class: "left",
                div { class: "board",
                    for sq in board.iter().copied() {
                        div {
                            key: "{sq.file}{sq.rank}",
                            class: match (sq.shade, sq.marker) {
                                (_, Marker::Selected) => "sq selected",
                                (_, Marker::LegalTarget) => "sq target",
                                (Shade::Light, _) => "sq light",
                                (Shade::Dark, _) => "sq dark",
                            },
                            onclick: {
                                let label = label.clone();
                                let status = view.status.clone();
                                move |_| {
                                    let here = (sq.file, sq.rank);
                                    match selected() {
                                        // Second click: play it, unless it promotes.
                                        Some(from) if sq.marker == Marker::LegalTarget => {
                                            if is_promotion(&status, from, here) {
                                                promoting.set(Some((from, here)));
                                            } else {
                                                wires.tx.send(Cmd::Play {
                                                    label: label.clone(),
                                                    uci: format!("{}{}{}{}", from.0, from.1, here.0, here.1),
                                                });
                                            }
                                            selected.set(None);
                                        }
                                        // Clicking the selected square clears it.
                                        Some(from) if from == here => selected.set(None),
                                        _ => selected.set(Some(here)),
                                    }
                                }
                            },
                            if let Some((color, role)) = sq.piece {
                                span {
                                    class: if color == Color::White { "piece white" } else { "piece black" },
                                    "{glyph(color, role)}"
                                }
                            }
                        }
                    }
                }

                p { class: "status",
                    if over {
                        "{outcome(&view.status)}"
                    } else if our_turn {
                        "your move · ply {view.status.ply}"
                    } else {
                        "waiting for your opponent · ply {view.status.ply}"
                    }
                }

                div { class: "actions",
                    button {
                        id: "resign",
                        disabled: over,
                        onclick: {
                            let label = label.clone();
                            move |_| wires.tx.send(Cmd::Resign { label: label.clone() })
                        },
                        "resign"
                    }
                    button {
                        id: "draw-offer",
                        disabled: over,
                        onclick: {
                            let label = label.clone();
                            move |_| wires.tx.send(Cmd::DrawOffer { label: label.clone() })
                        },
                        "offer draw"
                    }
                    button {
                        id: "draw-accept",
                        disabled: over,
                        onclick: {
                            let label = label.clone();
                            move |_| wires.tx.send(Cmd::DrawAccept { label: label.clone() })
                        },
                        "accept draw"
                    }
                    // Only shown when a ground actually exists. A claim with no
                    // ground is ignored at projection, so offering the button
                    // would invite writing a dead record into contract state
                    // permanently.
                    if can_claim {
                        button {
                            id: "draw-claim",
                            onclick: {
                                let label = label.clone();
                                move |_| wires.tx.send(Cmd::DrawClaim { label: label.clone() })
                            },
                            "claim draw"
                        }
                    }
                }
            }

            aside { class: "right",
                h3 { "moves" }
                ol { class: "history",
                    for (i, m) in history.iter().enumerate() {
                        li { key: "{i}", "{i + 1}. {m}" }
                    }
                }
            }

            if let Some((from, to)) = promoting() {
                div { class: "promo", role: "dialog",
                    p { "promote to" }
                    for (piece, ch) in [("queen", 'q'), ("rook", 'r'), ("bishop", 'b'), ("knight", 'n')] {
                        button {
                            key: "{ch}",
                            onclick: {
                                let label = label.clone();
                                move |_| {
                                    wires.tx.send(Cmd::Play {
                                        label: label.clone(),
                                        uci: format!("{}{}{}{}{}", from.0, from.1, to.0, to.1, ch),
                                    });
                                    promoting.set(None);
                                }
                            },
                            "{piece}"
                        }
                    }
                }
            }
        }
    }
}

/// A finished game's result, in words.
fn outcome(status: &adjourn_core::Status) -> String {
    use adjourn_core::Reason;
    let Some(d) = status.decision else {
        return String::new();
    };
    let who = match d.winner {
        Some(Color::White) => "White wins",
        Some(Color::Black) => "Black wins",
        None => "Draw",
    };
    let why = match d.reason {
        Reason::Checkmate => "checkmate",
        Reason::Stalemate => "stalemate",
        Reason::InsufficientMaterial => "insufficient material",
        Reason::AutomaticDraw => "automatic draw",
        Reason::Resignation => "resignation",
        Reason::DrawAgreement => "draw agreement",
        Reason::DoubleSignForfeit => "double-sign forfeit",
        Reason::MutualResignation => "mutual resignation",
        Reason::ThreefoldClaim => "threefold repetition, claimed",
        Reason::FiftyMoveClaim => "fifty-move rule, claimed",
    };
    format!("{who} — {why}")
}

/// The Unicode glyph for a piece; colour carries the distinction so both sides
/// have the same visual weight.
fn glyph(color: Color, role: Role) -> &'static str {
    use shakmaty::Role::*;
    // Colour is carried by the CSS class, not the glyph, so both sides have
    // the same visual weight.
    let _ = color;
    match role {
        King => "\u{265A}",
        Queen => "\u{265B}",
        Rook => "\u{265C}",
        Bishop => "\u{265D}",
        Knight => "\u{265E}",
        Pawn => "\u{265F}",
    }
}
```

- [ ] **Step 3: Add the layout styles**

In `ui/index.html`'s `<style>` block, add — keeping the existing board and piece rules:

```css
.game { display: grid; grid-template-columns: minmax(0, 1fr) 14rem; gap: 1.5rem; align-items: start; }
.right { background: rgba(255,255,255,.04); border-radius: 6px; padding: .75rem 1rem; }
.history { max-height: 30rem; overflow-y: auto; margin: 0; padding-left: 1.5rem; font-variant-numeric: tabular-nums; }
.actions { display: flex; flex-wrap: wrap; gap: .5rem; margin-top: .75rem; }
.error { background: #7f1d1d; color: #fee; padding: .6rem .9rem; border-radius: 4px; margin-bottom: 1rem; }
.busy { opacity: .7; margin-bottom: 1rem; }
.promo { position: fixed; inset: 0; display: flex; flex-direction: column; align-items: center;
         justify-content: center; gap: .5rem; background: #000a; }
.promo button { font-size: 1.1rem; padding: .5rem 1.5rem; }
@media (max-width: 40rem) { .game { grid-template-columns: 1fr; } }
```

- [ ] **Step 4: Delete the bring-up module**

`ui/src/live.rs` demonstrated the transport before any screen existed; the connection actor supersedes it. Delete the file and its `pub mod live;` line.

- [ ] **Step 5: Verify**

Both build directions, the suite, `fmt`, clippy. Then play a real game: two browser profiles (or two tabs with different labels) against one node — create, accept, bind, and make a move each way. Report what worked and what did not. **Do not claim a flow works that you did not run.**

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(ui): the game screen, layout B"
```

---

### Task 5: Live updates

**Files:**
- Modify: `ui/src/conn.rs`

**Interfaces:**
- Consumes: `NodeClient::next_update`, `GameState::merge`, `apply_delta`.
- Produces: `Cmd::Watch { label: String }` on a **second, dedicated coroutine** — not the command actor, which must stay free to serve resign and draw while a watch runs.

**Why this is not just a poll.** `UpdateNotification` carries `UpdateData`, which may be a `State`, a `Delta`, or a `StateAndDelta` — the notification does not promise which. So the held state is **merged** with whatever arrives rather than replaced, which is what makes arrival order irrelevant and lets the browser converge exactly as a peer does. `UpdateData` is also `#[non_exhaustive]`: ignore variants you do not recognise rather than panicking, because a panic here kills the tab.

**A `Delta` is `Vec<Record>`, not a `GameState`** — a different type with a different encoding. Decoding one as the other fails silently and the board simply never updates. `client/src/session.rs`'s `watch_label` already handles all three arms correctly; **call it rather than reimplementing it.**

- [ ] **Step 1: Add the command and the loop**

Add `Watch { label: String }` to `Cmd`, and in the actor:

```rust
                    Cmd::Watch { label } => {
                        // `watch_label` runs until the game ends, calling back
                        // after every update. It merges rather than replaces,
                        // and it subscribes -- `open_game_view`'s GET does not,
                        // which is why a watcher needs its own command.
                        let mut view_sig = view;
                        let l = label.clone();
                        session::watch_label(c, &label, wasm, move |status| {
                            view_sig.with_mut(|v| {
                                if let Some(v) = v.as_mut() {
                                    if v.label == l {
                                        v.status = status.clone();
                                    }
                                }
                            });
                        })
                        .await?;
                    }
```

- [ ] **Step 2: Send it when a game opens**

A coroutine cannot send to itself from inside its own body — `use_coroutine`
returns the handle only after the body is constructed. So the **caller**
follows the game: in `ui/src/views/list.rs`, the row's click handler sends both,
in order. The actor serialises commands, so `Watch` queues behind `Open` rather
than racing it:

```rust
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
```

Note this makes `Watch` long-lived: it runs until the game ends, so the actor
is busy for its duration and later commands queue behind it. That is wrong for
a UI where the user must still be able to resign mid-game. **Give `Watch` its
own coroutine** rather than putting it in the command actor: a second
`use_coroutine` that owns its own `BrowserClient` connected to the same node,
so watching never blocks acting. Two sockets to one local node is cheap, and it
keeps the "one owner per client" rule that makes the actor safe.

- [ ] **Step 3: Verify against two peers**

With a node running, open the same game in two browser contexts under different labels, play a move in one, and confirm the other's board advances **without a manual refresh**. That is the entire point of this task; if it does not happen, report that rather than the code compiling.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat(ui): follow a game live"
```

---

### Task 6: A browser test for the transport

**Files:**
- Modify: `ui/Cargo.toml`
- Create: `ui/tests/browser.rs`

**Interfaces:**
- Consumes: `BrowserClient`, `route`.
- Produces: `wasm-bindgen-test` cases that run in a headless browser.

**Why.** `BrowserClient` has **no automated test of any kind**. Its two Critical defects — every node-reported error discarded, and a socket death being undetectable — were both found by reading the source, and the only thing that has ever exercised it is a manual click. The views are built directly on it. Nothing currently stops those defects regressing.

- [ ] **Step 1: Add the dev-dependency**

In `ui/Cargo.toml`:

```toml
[dev-dependencies]
ed25519-dalek.workspace = true
wasm-bindgen-test = "=0.3.54"
```

**Do not add `adjourn-client` here.** It re-enables the `fake` feature and puts the contract and delegate back into the graph the documented `cargo tree ... -e normal` check reads — which is how that guard lost its ability to tell the guarded state from the broken one once already.

- [ ] **Step 2: Re-export what the test needs**

The test needs `delegate_container`, which lives in `adjourn-client` — and
Step 1 forbids that as a dev-dependency, because it re-enables `fake` and puts
the contract and delegate back into the graph the documented `cargo tree ...
-e normal` guard reads. Re-export it from the crate instead, inside
`ui/src/node.rs`'s `#[cfg(target_arch = "wasm32")] mod browser`:

```rust
    /// Re-exported for `ui/tests/browser.rs`, which cannot take an
    /// `adjourn-client` dev-dependency without switching `fake` back on and
    /// blinding the dependency-graph guard.
    pub use adjourn_client::node::delegate_container;
```

- [ ] **Step 3: Write the tests**

`ui/tests/browser.rs`:

```rust
//! Tests that need a real browser.
//!
//! Run with a node listening on 7509:
//!   wasm-pack test --headless --firefox ui
//! These are NOT part of `cargo test --workspace`; they need a browser and a
//! node, and CI has neither.

#![cfg(target_arch = "wasm32")]

use adjourn_ui::node::BrowserClient;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

const NODE_URL: &str = "ws://127.0.0.1:7509/v1/contract/command?encodingProtocol=native";
const DEAD_URL: &str = "ws://127.0.0.1:7599/v1/contract/command?encodingProtocol=native";

/// The failure that presented as an endless spinner. It must resolve.
#[wasm_bindgen_test]
async fn connecting_to_a_dead_port_fails_rather_than_hanging() {
    let result = BrowserClient::connect(DEAD_URL).await;
    assert!(
        result.is_err(),
        "a refused connection must surface as an error, not hang"
    );
}

/// The success path, end to end against a live node.
#[wasm_bindgen_test]
async fn a_live_node_registers_the_delegate_and_lists_games() {
    use adjourn_ui::node::{delegate_container, NodeClient};
    use adjourn_core::delegate_api::{Request, Response};

    let mut client = BrowserClient::connect(NODE_URL)
        .await
        .expect("a node must be listening on 7509 for this test");
    let (container, _key) = delegate_container(adjourn_ui::DELEGATE_WASM.to_vec());
    client
        .register_delegate(container)
        .await
        .expect("register_delegate");
    match client.delegate(Request::ListGames).await.expect("ListGames") {
        Response::Games(_) => {}
        other => panic!("unexpected reply: {other:?}"),
    }
}
```



- [ ] **Step 4: Run them**

Install the runner if absent (`cargo install wasm-pack --locked`), start a node, and run:

```
wasm-pack test --headless --firefox ui
```

Report the result. **If the runner cannot be installed or the browser is unavailable, say so plainly and do not claim the tests passed** — an unrun test is worth less than no test, because it advertises coverage.

- [ ] **Step 5: Prove the dead-port test discriminates**

Temporarily make `BrowserClient::connect` ignore the error path (drop the `err_tx` send), confirm `connecting_to_a_dead_port_fails_rather_than_hanging` hangs or fails, then restore and re-run. Run `cargo clean -p adjourn-ui` before trusting the restored baseline. Report what you saw.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "test(ui): browser tests for the transport"
```

---

### Task 7: Documentation

**Files:**
- Modify: `CLAUDE.md`, `README.md`

- [ ] **Step 1: Describe the UI as it now is**

Replace the "UI" section's coverage statement. What was true — "nothing has ever been loaded in a browser", "`BrowserClient` has no automated test" — is no longer true, and leaving it would be its own kind of lie. Say what is true now: the app plays a game against a local node, `dx` 0.7.9 builds and serves it, and the transport has browser tests that need a node and are not part of `cargo test --workspace`.

- [ ] **Step 2: Record the connection actor and why**

`BrowserClient` takes `&mut self` and is not `Clone`, and a `RefCell` borrow cannot be held across an `.await` — so a shared-context client panics at runtime. One coroutine owns it and serialises commands. This is the kind of "looks like indirection for its own sake" decision the file exists to explain.

- [ ] **Step 3: Record the `include_bytes!` finding**

An `include_bytes!` into a `const` costs **zero bytes** until something reads it at runtime, and `.len()` does not count because it const-folds. The bring-up measured this: the app wasm was byte-identical with and without a length read, and grew by exactly 1,376,063 bytes — contract 267,003 plus delegate 1,101,953 — only once a runtime fold touched the data. Anyone checking whether the modules ship must test for the bytes, not for the constant.

- [ ] **Step 4: Record the dx title quirk**

`dx` **appends** `[web.app] title` into whatever `<title>` it finds and does nothing when there is none. Text in `index.html` gets prefixed ("adjournadjourn"), deleting the element yields no title at all, and an empty `<title></title>` with the value in `Dioxus.toml` is the only combination that produces the configured title.

- [ ] **Step 5: Update the test counts**

Run `cargo test --workspace --locked` and use the numbers it prints. Note separately that `ui/tests/browser.rs` is **not** in that count and why.

- [ ] **Step 6: Commit**

```bash
git add CLAUDE.md README.md
git commit -m "docs: record the UI, the connection actor, and the bring-up findings"
```

---

## Notes for the executor

- **Two browser contexts, not two tabs**, when testing two players: the delegate partitions labels by origin, and two tabs of the same origin share one. Use two browser profiles, or one browser plus one private window.
- **The delegate's origin partition is deliberate.** A browser cannot see games created in the CLI. That is the isolation working, not a bug — do not "fix" it.
- **`cargo test --workspace` cannot link on Windows.** Run the suite in WSL.
- If a test passes on the first run, be suspicious. Every task with a test has a step that breaks the behaviour and confirms the test notices; this repo has shipped several tests that passed for the wrong reason.
