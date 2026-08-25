# Shared Client Crate and Streaming Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract the game flows into a crate a browser can compile, add the streaming update method the UI needs, and ship `adjourn watch` on top of it.

**Architecture:** `cli/src/session.rs` already holds every flow and is already generic over a `NodeClient` trait; it is unreachable from wasm only because the `cli` crate pulls `tokio-tungstenite`. Move the flows, the trait, the invite codec and `FakeNode` into a new `adjourn-client`; leave the tungstenite transport behind in `cli`. Then add one method for update notifications and use it for `watch`.

**Tech Stack:** Rust 1.97.1 (pinned), `freenet-stdlib` 0.8.5, `ciborium`, `shakmaty`, `ed25519-dalek`, `tokio` (CLI only).

**Spec:** `docs/superpowers/specs/2026-08-25-adjourn-web-ui-design.md`

This plan covers the spec's "A shared client crate" and "Streaming" sections. The Dioxus UI is a separate plan that builds on the interfaces produced here.

## Global Constraints

- **`adjourn-client` must not depend on anything without a wasm32 backend.** No `tokio-tungstenite`, no `tokio` runtime features, no `mio`. `freenet-stdlib` must be depended on **without** the `net` feature — `net` is what pulls the WebSocket client.
- **`FakeNode` sits behind a `fake` feature that is ON by default.** It depends on `adjourn-contract` and `adjourn-delegate`; the UI will set `default-features = false` to keep both out of its wasm build. This is not tidiness — co-building the contract and delegate in one cargo invocation can change the contract's emitted bytes through feature unification, which silently rotates the app's address.
- **The CLI's existing 13 tests must move with the crate and stay green.** They are the evidence the extraction changed no behaviour. Do not rewrite them beyond import paths.
- **Both players must derive byte-identical `GameParams`.** That is the whole reason for one shared implementation: different params mean different contract ids, two players on separate contracts, and no error anywhere.
- **`cargo test --workspace --locked` must be green before every commit.**
- **Never** run `cargo build --release` on the contract or delegate — use `scripts/build-contract.sh` / `scripts/build-delegate.sh`. A bare release build embeds home-directory paths and rotates the key.
- Do not add `rand` or `getrandom` to the contract or delegate dependency graphs. CI asserts they stay clean.

## Platform notes

- Windows host: `cargo test --workspace` fails at link time (`windows-sys` needs mingw binutils). Pre-existing and environmental. `cargo test -p adjourn-core --locked` works; the controller verifies the full suite on Linux.
- The CLI cannot be built for wasm32 (`tokio-tungstenite` → `mio`). Check it natively: `cargo check -p adjourn-cli`.
- Verify the delegate with `cargo check -p adjourn-delegate --target wasm32-unknown-unknown`.

---

### Task 1: Extract `adjourn-client`

**Files:**
- Create: `client/Cargo.toml`, `client/src/lib.rs`
- Move: `cli/src/session.rs` → `client/src/session.rs`
- Move: `cli/src/invite.rs` → `client/src/invite.rs`
- Move: `cli/src/fake.rs` → `client/src/fake.rs`
- Split: `cli/src/node.rs` → `client/src/node.rs` (trait + container helpers) and `cli/src/ws.rs` (`WsClient` only)
- Move: `cli/tests/*` → `client/tests/*`, except any test that drives the binary
- Modify: `Cargo.toml` (workspace members), `cli/Cargo.toml`, `cli/src/lib.rs`, `cli/src/main.rs`

**Interfaces:**
- Produces: crate `adjourn_client` exporting `session` (all flows), `invite::{Invite, GameOffer}`, `node::{NodeClient, contract_container, delegate_container}`, and `fake::{FakeNode, shared_world, World}` behind the default `fake` feature.
- Consumes: nothing.

- [ ] **Step 1: Create the crate manifest**

`client/Cargo.toml`. Note `freenet-stdlib` has **no** `features = ["net"]` — that is the line that keeps this crate wasm-compilable:

```toml
[package]
name = "adjourn-client"
version.workspace = true
edition.workspace = true

[features]
# ON by default so `cargo test --workspace` keeps working with no extra flags.
# The UI sets `default-features = false` to keep the contract and delegate
# crates out of its wasm build.
default = ["fake"]
fake = ["dep:adjourn-contract", "dep:adjourn-delegate"]

[dependencies]
adjourn-core.workspace = true
adjourn-contract = { workspace = true, optional = true }
adjourn-delegate = { workspace = true, optional = true }
freenet-stdlib.workspace = true
ciborium.workspace = true
serde.workspace = true
serde_bytes.workspace = true
bs58.workspace = true
anyhow.workspace = true
ed25519-dalek.workspace = true
rand.workspace = true
shakmaty.workspace = true

[dev-dependencies]
tokio = { workspace = true }
```

- [ ] **Step 2: Add the crate to the workspace**

In the root `Cargo.toml`, add `"client"` to `members`, and add to `[workspace.dependencies]`:

```toml
adjourn-client = { path = "client" }
```

- [ ] **Step 3: Move the modules**

```bash
git mv cli/src/session.rs client/src/session.rs
git mv cli/src/invite.rs  client/src/invite.rs
git mv cli/src/fake.rs    client/src/fake.rs
git mv cli/src/node.rs    client/src/node.rs
```

Then write `client/src/lib.rs`:

```rust
//! The game flows, independent of transport.
//!
//! Everything here is generic over [`node::NodeClient`], so the same code runs
//! against a real node over a WebSocket, against a browser's WebSocket, or
//! against [`fake::FakeNode`] in a test. That is not code reuse for its own
//! sake: both players must derive byte-identical `GameParams`, or they land on
//! different contract ids and each sees a game the other never joins, with no
//! error anywhere.

/// Test-facing only: runs the real contract and delegate code in memory so
/// integration tests can exercise the flows without a live Freenet node.
#[cfg(feature = "fake")]
#[doc(hidden)]
pub mod fake;
pub mod invite;
pub mod node;
pub mod session;
```

- [ ] **Step 4: Split the transport out of `node.rs`**

`client/src/node.rs` keeps the module docs, `NodeClient`, `contract_container` and `delegate_container`. Delete from it: the `WsClient` struct and its `impl` blocks, the `RESPONSE_TIMEOUT` constant, and the now-unused imports (`WebApi`, `ClientRequest`, `DelegateRequest`, `HostResponse`, `ContractResponse`, `std::time::Duration`, `anyhow::{anyhow, bail}` if unused).

Create `cli/src/ws.rs` with the deleted material, plus these imports:

```rust
//! The tungstenite transport. Lives in the CLI because `tokio-tungstenite`
//! pulls `mio`, which has no wasm32 backend — that is the whole reason
//! `adjourn-client` exists as a separate crate.

use adjourn_client::node::NodeClient;
use adjourn_core::delegate_api::{Request, Response};
use anyhow::{anyhow, bail, Context};
use freenet_stdlib::client_api::{
    ClientRequest, ContractRequest, ContractResponse, DelegateRequest, HostResponse, WebApi,
};
use freenet_stdlib::prelude::*;
use std::time::Duration;
```

- [ ] **Step 5: Point the CLI at the new crate**

`cli/src/lib.rs` becomes:

```rust
pub mod output;
pub mod ws;

// Re-exported so `main.rs` keeps one import path.
pub use adjourn_client::{invite, node, session};
```

Do **not** re-export `fake` here. The `cli` crate has no `fake` feature of its
own, so a `#[cfg(feature = "fake")]` in this crate is always false and the
re-export would silently vanish. Nothing in `cli` needs it any more either —
the tests that used it now live in `client`.

In `cli/Cargo.toml`, add `adjourn-client.workspace = true` and **remove** `adjourn-contract` and `adjourn-delegate` from `[dependencies]` — they now arrive through `adjourn-client`'s `fake` feature. Keep `freenet-stdlib = { workspace = true, features = ["net"] }`: the CLI still needs `net` for `WebApi`.

In `cli/src/main.rs`, change `use adjourn_cli::node::WsClient` (or equivalent) to `use adjourn_cli::ws::WsClient`.

- [ ] **Step 6: Move the tests**

```bash
git mv cli/tests/common     client/tests/common
git mv cli/tests/fake_node.rs client/tests/fake_node.rs
git mv cli/tests/full_game.rs client/tests/full_game.rs
git mv cli/tests/invite.rs    client/tests/invite.rs
git mv cli/tests/moves.rs     client/tests/moves.rs
git mv cli/tests/setup.rs     client/tests/setup.rs
```

In each moved test, change `use adjourn_cli::` to `use adjourn_client::`. Change nothing else — these tests are the evidence the extraction preserved behaviour, so any edit beyond an import path weakens that evidence.

`client/tests/common/mod.rs` locates the contract WASM by a path relative to the crate; check whether its path needs a `..` now that it sits one directory deeper, and fix it if so. Its CI guard (panicking rather than skipping when `CI` is set) must survive the move.

- [ ] **Step 7: Verify the extraction changed nothing**

Run: `cargo test --workspace --locked`
Expected: PASS, with the same 13 flow tests now reported under `adjourn-client` instead of `adjourn-cli`, and the total unchanged at 138.

Also run:
- `cargo check -p adjourn-cli` — the CLI still builds natively
- `cargo check -p adjourn-client --no-default-features` — **this is the point of the whole task**: the crate must compile without `adjourn-contract` or `adjourn-delegate`

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor: extract adjourn-client from the CLI crate"
```

---

### Task 2: The streaming update method

**Files:**
- Modify: `client/src/node.rs`, `client/src/fake.rs`, `cli/src/ws.rs`
- Test: `client/tests/updates.rs` (new)

**Interfaces:**
- Consumes: `NodeClient` from Task 1.
- Produces: `NodeClient::next_update(&mut self) -> anyhow::Result<Option<(ContractInstanceId, UpdateData<'static>)>>`; `FakeNode` gains the shared broadcast log so one fake's write is visible to another fake's `next_update`.

**A deliberate deviation from the spec, and why.** The spec sketches
`subscribe(...) -> Result<impl Stream<Item = UpdateData>>`. Implement
`next_update` instead. A `Stream` returned from a trait method has to borrow
`&mut self` for its whole life, which makes it a lending stream — awkward in
native Rust and worse in wasm, where there is no runtime to spawn it onto. A
plain `async fn` the caller loops on gives the same behaviour with none of that,
and both transports can implement it. The spec's intent (the UI reacts to
notifications rather than polling) is unchanged.

- [ ] **Step 1: Write the failing test**

Create `client/tests/updates.rs`. This test is the real point of the task: it
proves a notification from one peer reaches another and that **merging** it
(rather than replacing state) produces the right board.

```rust
mod common;

use adjourn_client::fake::{shared_world, FakeNode};
use adjourn_client::session::{game_bind, invite_accept, invite_new, play_move, show_label};
use adjourn_core::delegate_api::Side;

/// Alice moves; Bob learns about it from a notification rather than a GET.
#[tokio::test]
async fn a_move_reaches_the_other_peer_as_an_update() {
    let Some(wasm) = common::contract_wasm() else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };
    let world = shared_world();
    let (mut alice, mut bob) = (FakeNode::new(world.clone()), FakeNode::new(world));
    let invite = invite_new(&mut alice, "alice", Side::White).await.unwrap();
    let offer = invite_accept(&mut bob, "bob", &invite, wasm.clone())
        .await
        .unwrap();
    game_bind(&mut alice, "alice", &offer, wasm.clone())
        .await
        .unwrap();

    // Bob reads the game once, so he is subscribed and holds a baseline.
    let before = show_label(&mut bob, "bob", wasm.clone()).await.unwrap();
    assert_eq!(before.ply, 0);

    play_move(&mut alice, "alice", "e2e4", wasm.clone())
        .await
        .unwrap();

    let update = bob
        .next_update()
        .await
        .expect("next_update failed")
        .expect("expected an update after the opponent moved");
    assert_eq!(
        *update.0,
        contract_bytes_of(&mut bob, "bob").await,
        "the update names our game"
    );

    let after = show_label(&mut bob, "bob", wasm).await.unwrap();
    assert_eq!(after.ply, 1, "bob sees the move");
}

/// The contract id the delegate recorded for `label` at bind time.
///
/// Compared as raw bytes rather than as a `ContractInstanceId`, because that is
/// what `GameSummary.contract` stores and what the id derefs to.
async fn contract_bytes_of(node: &mut FakeNode, label: &str) -> [u8; 32] {
    use adjourn_client::node::NodeClient;
    use adjourn_core::delegate_api::{Request, Response};
    let Response::Games(games) = node.delegate(Request::ListGames).await.unwrap() else {
        panic!("expected a games list");
    };
    games
        .into_iter()
        .find(|g| g.label == label)
        .expect("label is bound")
        .contract
        .expect("a bound game carries its contract id")
}

/// With nothing happening, `next_update` must not invent one.
#[tokio::test]
async fn next_update_is_empty_when_nothing_changed() {
    let Some(wasm) = common::contract_wasm() else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };
    let world = shared_world();
    let (mut alice, mut bob) = (FakeNode::new(world.clone()), FakeNode::new(world));
    let invite = invite_new(&mut alice, "alice", Side::White).await.unwrap();
    let offer = invite_accept(&mut bob, "bob", &invite, wasm.clone())
        .await
        .unwrap();
    game_bind(&mut alice, "alice", &offer, wasm.clone())
        .await
        .unwrap();
    let _ = show_label(&mut bob, "bob", wasm).await.unwrap();

    assert!(
        bob.next_update().await.unwrap().is_none(),
        "no write happened, so there is nothing to report"
    );
}
```

- [ ] **Step 3: Add the trait method**

In `client/src/node.rs`, add to `trait NodeClient`:

```rust
    /// The next update notification for a contract this client subscribed to,
    /// or `None` if there is nothing waiting.
    ///
    /// Deliberately NOT bounded by a request timeout. A correspondence move can
    /// legitimately take days, so a timeout here would report a healthy idle
    /// game as a failure — the opposite of what the per-request timeout on the
    /// other methods is for.
    ///
    /// The payload is `UpdateData`, which may be a `State`, a `Delta`, or a
    /// `StateAndDelta` — the notification does not promise which. Callers hold
    /// a `GameState` and MERGE whatever arrives rather than replacing, which is
    /// what makes arrival order irrelevant and lets a browser converge exactly
    /// as a peer does. `UpdateData` is `#[non_exhaustive]`: ignore variants you
    /// do not recognise rather than panicking on them.
    async fn next_update(
        &mut self,
    ) -> anyhow::Result<Option<(ContractInstanceId, UpdateData<'static>)>>;
```

- [ ] **Step 4: Implement it on `FakeNode`**

In `client/src/fake.rs`. **`World` is currently a bare type alias over a
`BTreeMap`** — `Arc<Mutex<BTreeMap<[u8; 32], (Parameters<'static>, Vec<u8>)>>>` —
so there is no struct to add a field to. Give it one:

```rust
/// The shared contract world: current state per contract, plus an ordered log
/// of every write so a second fake can observe the first's writes the way a
/// subscribed peer would.
#[derive(Default)]
pub struct WorldInner {
    pub contracts: BTreeMap<[u8; 32], (Parameters<'static>, Vec<u8>)>,
    pub log: Vec<([u8; 32], Vec<u8>)>,
}

pub type World = Arc<Mutex<WorldInner>>;

pub fn shared_world() -> World {
    Arc::new(Mutex::new(WorldInner::default()))
}
```

Every existing `world.lock()` call site in this file then indexes
`.contracts` where it previously indexed the map directly. That is a mechanical
change the compiler will walk you through — do not change any behaviour while
making it.

`FakeNode` gains `cursor: usize`, initialised to `0` in `FakeNode::new`. In
`FakeNode`'s `update` and `put` implementations, push `(id, state_bytes)` onto
`world.log` after the write succeeds. Then:

```rust
    async fn next_update(
        &mut self,
    ) -> anyhow::Result<Option<(ContractInstanceId, UpdateData<'static>)>> {
        let entry = {
            let world = self.world.lock().expect("world lock");
            world.log.get(self.cursor).cloned()
        };
        let Some((id, bytes)) = entry else {
            return Ok(None);
        };
        self.cursor += 1;
        Ok(Some((
            ContractInstanceId::from(id),
            UpdateData::State(State::from(bytes)),
        )))
    }
```

Note this yields the node's *own* writes back to it as well. That is deliberate:
merging your own state is idempotent, and a fake that hid them would be
modelling a guarantee the real node does not make.

- [ ] **Step 5: Implement it on `WsClient`**

In `cli/src/ws.rs`. The existing `recv_timeout` helper skips
`UpdateNotification`; this method is the one place that returns them. It must
not use `RESPONSE_TIMEOUT`:

```rust
    async fn next_update(
        &mut self,
    ) -> anyhow::Result<Option<(ContractInstanceId, UpdateData<'static>)>> {
        loop {
            match self.api.recv().await? {
                HostResponse::ContractResponse(ContractResponse::UpdateNotification {
                    key,
                    update,
                }) => return Ok(Some((*key.id(), update))),
                // Anything else on this socket is a response to a request we
                // are no longer waiting on. Skip rather than fail: dropping the
                // connection over a late reply would end a healthy session.
                _ => continue,
            }
        }
    }
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --workspace --locked`
Expected: PASS, 140 tests (the 138 from before, plus the two new ones).

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(client): add next_update for subscribed contracts"
```

---

### Task 3: `adjourn watch`

**Files:**
- Modify: `cli/src/main.rs`, `cli/src/output.rs`
- Modify: `client/src/session.rs`

**Interfaces:**
- Consumes: `NodeClient::next_update` from Task 2.
- Produces: `session::watch_label<N: NodeClient>(node, label, contract_wasm, on_status) -> anyhow::Result<()>`, where `on_status: impl FnMut(&Status)`.

- [ ] **Step 1: Add the flow**

In `client/src/session.rs`, after `show_label`. The callback keeps this
transport- and terminal-agnostic, so the UI can reuse it:

```rust
/// Follow a game, calling `on_status` with the projection after every update.
///
/// Merges each notification into the held state rather than replacing it: the
/// payload may be a `State`, a `Delta`, or a `StateAndDelta`, and merge is what
/// makes all three land on the same answer regardless of arrival order.
/// Unrecognised `UpdateData` variants are ignored -- the enum is
/// `#[non_exhaustive]`, and a panic here would end a healthy session.
pub async fn watch_label<N: NodeClient>(
    node: &mut N,
    label: &str,
    contract_wasm: Vec<u8>,
    mut on_status: impl FnMut(&Status),
) -> anyhow::Result<()> {
    let g = open_game(node, label, contract_wasm).await?;
    let mut state = g.state;
    on_status(&project(&state, &g.params));

    loop {
        let Some((id, update)) = node.next_update().await? else {
            continue;
        };
        // `OpenGame.contract` is a raw `[u8; 32]`; `ContractInstanceId` derefs
        // to the same, so compare through the deref rather than by type.
        if *id != g.contract {
            continue; // a different game on the same connection
        }
        // A `State` payload is an encoded `GameState`; a `Delta` payload is an
        // encoded `Delta`, which is `Vec<Record>` -- a DIFFERENT type with a
        // different encoding. Decoding one as the other fails silently and the
        // board simply never updates, so the two arms must not be merged.
        match update {
            UpdateData::State(bytes) => {
                if let Some(incoming) = GameState::decode(bytes.as_ref()) {
                    state.merge(&incoming, &g.params);
                }
            }
            UpdateData::Delta(bytes) => {
                if let Ok(delta) = ciborium::from_reader::<Delta, &[u8]>(bytes.as_ref()) {
                    state.apply_delta(&delta, &g.params);
                }
            }
            UpdateData::StateAndDelta { state: s, delta } => {
                if let Some(incoming) = GameState::decode(s.as_ref()) {
                    state.merge(&incoming, &g.params);
                }
                if let Ok(delta) = ciborium::from_reader::<Delta, &[u8]>(delta.as_ref()) {
                    state.apply_delta(&delta, &g.params);
                }
            }
            // `UpdateData` is `#[non_exhaustive]`. Ignore what we do not know.
            _ => continue,
        }
        let status = project(&state, &g.params);
        on_status(&status);
        if status.is_over() {
            return Ok(());
        }
    }
}
```

Add both of these to the file's imports — `Delta` is `Vec<Record>`, a
different type from `GameState` with a different encoding, and the file
does not import it today:

```rust
use adjourn_core::state::Delta;
use freenet_stdlib::prelude::UpdateData;
```

- [ ] **Step 2: Replace the `watch` stub**

`cli/src/main.rs` currently bails with `"watch: not implemented yet; poll with
`adjourn show`"`. Replace that arm's body with a call to `session::watch_label`,
rendering each status with the same renderer `show` uses:

```rust
        Command::Watch { label } => {
            session::watch_label(&mut client, &label, contract_wasm()?, |status| {
                output::render_status(&label, status);
            })
            .await?;
        }
```

Follow the surrounding arms for how the client and WASM are obtained if they
differ from this sketch.

- [ ] **Step 3: Verify**

Run: `cargo check -p adjourn-cli` and `cargo test --workspace --locked`
Expected: both clean. There is no automated test for `watch` against a real
node; say so in the report rather than implying coverage. The flow itself is
covered by Task 2's tests, which exercise `next_update` and the merge.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat(cli): adjourn watch, following a game live"
```

---

### Task 4: Documentation

**Files:**
- Modify: `CLAUDE.md`, `README.md`

- [ ] **Step 1: Update the crate table**

`CLAUDE.md` opens with a table of crates and their roles. Add a row for
`client/` (`adjourn-client`) describing it as the transport-independent game
flows, generic over `NodeClient`, with `FakeNode` behind a default-on `fake`
feature that the UI disables. Amend the `cli/` row: it is now the tungstenite
transport, argument parsing and rendering, and no longer holds the flows.

- [ ] **Step 2: Record why the crate exists**

Add a short paragraph near the crate table. The reason is not code reuse: both
players must derive byte-identical `GameParams` or they land on different
contract ids and each sees a game the other never joins, with no error anywhere.
One implementation is the only way to be sure of that, and a browser cannot
reach the flows while they live in a crate that pulls `tokio-tungstenite`.

- [ ] **Step 3: Close the `watch` gap in the roadmap**

`CLAUDE.md`'s roadmap item 3 says `watch` "needs a streaming `NodeClient` method
that does not exist yet." It exists now. Rewrite the item to say `watch` is
done, and note that `next_update` deliberately has no timeout because a
correspondence move can legitimately take days.

- [ ] **Step 4: Update the test counts**

Run `cargo test --workspace --locked`, count the per-file results, and update
the summary line and per-file bullets in the "Testing" section. The flow tests
now live under `adjourn-client`, not `adjourn-cli`. Do not guess — use the
numbers the run prints.

- [ ] **Step 5: Commit**

```bash
git add CLAUDE.md README.md
git commit -m "docs: record adjourn-client, next_update, and watch"
```

---

## Notes for the executor

- **`merge` verifies signatures**, so the randomized law tests take ~85s. Expected, not a hang.
- The contract WASM must exist on disk for the flow tests to run; several skip
  without it locally but **panic** if `CI` is set, so a skip can never
  masquerade as a pass in CI.
- If a test you moved fails after the move, the extraction changed behaviour.
  Do not adjust the test to match — find what moved.
