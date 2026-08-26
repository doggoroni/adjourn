# Web UI Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `ui` crate that compiles for the browser, renders a chess board from a projected `Status`, and talks to a Freenet node over the browser's WebSocket.

**Architecture:** A Dioxus web crate depending on `adjourn-client` with `default-features = false` (keeping the contract and delegate crates out of the wasm build). The board is a pure `Status → Vec<Square>` function so it is testable natively with no browser. The transport wraps `freenet-stdlib`'s **callback-based** browser `WebApi` in an mpsc channel, so `NodeClient`'s request/response methods work over a push-based API.

**Tech Stack:** Rust 1.97.1 (pinned), Dioxus 0.7.9 (`web`), `freenet-stdlib` 0.8.5 with `net`, `web-sys`, `wasm-bindgen`, `futures`, `shakmaty` 0.30.1.

**Spec:** `docs/superpowers/specs/2026-08-25-adjourn-web-ui-design.md`

This plan covers the spec's crate setup, the board component, and the transport half of "Screens". **The five views are a separate plan**, written against the interfaces this one produces rather than guessed at.

## Global Constraints

- **The acceptance test for every task is `cargo check -p adjourn-ui --target wasm32-unknown-unknown --locked`.** A native check proves nothing here — the previous branch shipped a crate that passed `--no-default-features` natively and could not compile for the browser at all.
- **`adjourn-client` is depended on with `default-features = false`.** That drops its `fake` feature and with it `adjourn-contract` and `adjourn-delegate`. Co-building the contract and delegate in one cargo invocation can change the contract's emitted bytes through feature unification, which silently rotates the app's network address.
- **Do NOT add `rand` or `getrandom` to this crate.** Entropy comes from `web_sys` `crypto().get_random_values_with_u8_array()`. `adjourn-client` deliberately takes entropy as a parameter for exactly this reason. A `getrandom` in the graph emits wasm-bindgen placeholder imports, which is what makes a contract fail to instantiate (freenet/river#241).
- **`freenet-stdlib` here DOES use `features = ["net"]`.** On wasm that is what provides the browser `WebApi`; `tokio` and `tokio-tungstenite` are gated to `cfg(any(unix, windows))` and are not pulled on wasm. This is not a contradiction of `adjourn-client`, which must stay `net`-free because it is shared with the CLI.
- **Never** run `cargo build --release` on the contract or delegate — use `scripts/build-contract.sh` / `scripts/build-delegate.sh`. Both are package-scoped (`-p`), which is what keeps a `ui` crate in the same workspace from unifying features into the contract.
- `cargo test --workspace --locked` green, `cargo fmt --all -- --check` clean, and `cargo clippy --workspace --all-targets -- -D warnings` clean before every commit.

## Platform notes

- Windows host: `cargo test --workspace` fails at link time (`windows-sys` needs mingw binutils). Pre-existing and environmental. `cargo check --target wasm32-unknown-unknown` **does** work on Windows — so the acceptance test above is one you can and must run yourself.
- **`dx` (the Dioxus CLI) is not installed anywhere in this environment.** Nothing in this plan requires it: every task is verified by `cargo check`/`cargo test`. Actually serving the app in a browser needs `dx` and is out of scope here — do not install it, and do not claim the app has been loaded.
- The randomized law tests take ~85s. Expected, not a hang.

---

### Task 1: The `ui` crate, compiling for the browser

**Files:**
- Create: `ui/Cargo.toml`, `ui/src/lib.rs`, `ui/src/main.rs`, `ui/build.rs`, `ui/Dioxus.toml`, `ui/index.html`
- Modify: `Cargo.toml` (workspace members + `[workspace.dependencies]`), `.github/workflows/ci.yml`

**Interfaces:**
- Produces: crate `adjourn_ui` with `pub mod board;` and `pub mod node;` declared (bodies land in Tasks 2 and 3), plus `pub const CONTRACT_WASM: &[u8]` and `pub const DELEGATE_WASM: &[u8]`.
- Consumes: `adjourn-client` and `adjourn-core` from the workspace.

- [ ] **Step 1: Write the crate manifest**

`ui/Cargo.toml`. Note `adjourn-client` with `default-features = false`, and `freenet-stdlib` **with** `net`:

```toml
[package]
name = "adjourn-ui"
version.workspace = true
edition.workspace = true

[dependencies]
# `default-features = false` drops the `fake` feature, and with it the
# contract and delegate crates. They must not enter this wasm build.
adjourn-client = { workspace = true, default-features = false }
adjourn-core.workspace = true
# `net` is what provides the BROWSER WebApi on wasm. tokio and
# tokio-tungstenite are gated to cfg(any(unix, windows)) upstream and are not
# pulled here.
freenet-stdlib = { workspace = true, features = ["net"] }
dioxus = { version = "=0.7.9", features = ["web"] }
wasm-bindgen = "=0.2.104"
wasm-bindgen-futures = "=0.4.54"
futures = "=0.3.34"
web-sys = { version = "=0.3.81", features = [
    "Window",
    "Crypto",
    "WebSocket",
    "BinaryType",
    "MessageEvent",
    "ErrorEvent",
] }
ciborium.workspace = true
anyhow.workspace = true
shakmaty.workspace = true
```

- [ ] **Step 2: Add it to the workspace**

In the root `Cargo.toml`, add `"ui"` to `members`, and add to `[workspace.dependencies]`:

```toml
adjourn-ui = { path = "ui" }
```

- [ ] **Step 3: Guard the embedded WASM with a build script**

The bundle must carry both modules, because a browser cannot read them off disk. `include_bytes!` fails with an unhelpful "file not found" if they have not been built, so say what to do instead:

`ui/build.rs`:

```rust
//! Fail loudly if the WASM modules the bundle embeds have not been built.
//!
//! `include_bytes!` on a missing path reports only the path, which sends the
//! reader looking for a file that is *supposed* to be absent from a clean
//! checkout. The modules are build artifacts, deliberately not committed.

fn main() {
    for (what, path) in [
        ("contract", "../target/wasm32-unknown-unknown/release/adjourn_contract.wasm"),
        ("delegate", "../target/wasm32-unknown-unknown/release/adjourn_delegate.wasm"),
    ] {
        println!("cargo:rerun-if-changed={path}");
        if !std::path::Path::new(path).exists() {
            panic!(
                "the {what} WASM is missing at {path}.\n\
                 The UI bundle embeds it, so build it first:\n\
                 \n    ./scripts/build-{what}.sh\n\n\
                 Do NOT use a bare `cargo build --release` -- it embeds \
                 home-directory paths and produces a different, unshippable key."
            );
        }
    }
}
```

- [ ] **Step 4: Write the library root**

`ui/src/lib.rs`:

```rust
//! The adjourn web UI.
//!
//! Split lib/bin on purpose: everything with logic lives in the library so it
//! can be tested natively, and the binary is only the Dioxus entry point. The
//! board in particular is a pure function of a projected `Status`, so square
//! colours, orientation and legal-target marking are all testable with no
//! browser and no framework.

pub mod board;
pub mod node;

/// The compiled contract, embedded because a browser cannot read it off disk.
///
/// This pins the contract key into the bundle: rebuilding the contract means
/// rebuilding the UI. See `ui/build.rs` for the guard that says so when the
/// artifact is missing.
pub const CONTRACT_WASM: &[u8] =
    include_bytes!("../../target/wasm32-unknown-unknown/release/adjourn_contract.wasm");

/// The compiled delegate, embedded for the same reason. The UI registers it on
/// first run -- the browser's equivalent of `adjourn init`.
pub const DELEGATE_WASM: &[u8] =
    include_bytes!("../../target/wasm32-unknown-unknown/release/adjourn_delegate.wasm");
```

Create `ui/src/board.rs` and `ui/src/node.rs` as empty files for now; Tasks 2 and 3 fill them.

- [ ] **Step 5: Write the binary entry point**

`ui/src/main.rs`:

```rust
use dioxus::prelude::*;

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    rsx! {
        main { class: "app",
            h1 { "adjourn" }
            p { "untimed correspondence chess" }
        }
    }
}
```

- [ ] **Step 6: Add the Dioxus config and page shell**

`ui/Dioxus.toml`:

```toml
[application]
name = "adjourn"
default_platform = "web"
out_dir = "dist"
asset_dir = "assets"

[web.app]
title = "adjourn"
```

`ui/index.html`:

```html
<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>adjourn</title>
  </head>
  <body>
    <div id="main"></div>
  </body>
</html>
```

- [ ] **Step 7: Add the CI step**

In `.github/workflows/ci.yml`, immediately after the existing "Assert adjourn-client compiles for the browser" step (both WASM builds must already have run, because `ui` embeds them):

```yaml
      - name: Assert the UI compiles for the browser
        run: cargo check -p adjourn-ui --target wasm32-unknown-unknown --locked
```

- [ ] **Step 8: Verify**

Run, in this order:

```
./scripts/build-contract.sh
./scripts/build-delegate.sh
cargo check -p adjourn-ui --target wasm32-unknown-unknown --locked
```

Expected: the check passes. This is the task's whole point — a native check would prove nothing.

Then confirm the guard works: temporarily rename the contract WASM, re-run the check, and confirm the build script's message appears rather than a bare "file not found". Restore it. Report what you saw.

Also run `cargo test --workspace --locked` (Linux) or `cargo test -p adjourn-core --locked` (Windows), plus `cargo fmt --all -- --check`.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "feat(ui): crate scaffold compiling for wasm32"
```

---

### Task 2: The board as a pure function

**Files:**
- Modify: `ui/src/board.rs`
- Test: `ui/tests/board.rs` (new)

**Interfaces:**
- Consumes: `adjourn_core::{Status, legal_moves}`.
- Produces:
  - `pub enum Shade { Light, Dark }`
  - `pub enum Marker { None, Selected, LegalTarget }`
  - `pub struct Square { pub file: char, pub rank: u8, pub shade: Shade, pub piece: Option<(shakmaty::Color, shakmaty::Role)>, pub marker: Marker }`
  - `pub fn squares(status: &Status, orientation: shakmaty::Color, selected: Option<(char, u8)>) -> Vec<Square>` — 64 entries, reading order for `orientation`
  - `pub fn is_promotion(status: &Status, from: (char, u8), to: (char, u8)) -> bool`

- [ ] **Step 1: Write the failing tests**

`ui/tests/board.rs`:

```rust
use adjourn_core::{project, GameParams, GameState};
use adjourn_ui::board::{squares, Marker, Shade};
use shakmaty::{Color, Role};

fn start() -> (GameState, GameParams) {
    let w = ed25519_dalek::SigningKey::from_bytes(&[1u8; 32]);
    let b = ed25519_dalek::SigningKey::from_bytes(&[2u8; 32]);
    let params = GameParams {
        white: w.verifying_key().to_bytes(),
        black: b.verifying_key().to_bytes(),
        nonce: [7u8; 16],
    };
    (GameState::empty(), params)
}

#[test]
fn the_opening_position_is_laid_out_correctly() {
    let (state, params) = start();
    let status = project(&state, &params);
    let board = squares(&status, Color::White, None);

    assert_eq!(board.len(), 64, "a board has 64 squares");

    // White's view reads a8 first and h1 last.
    assert_eq!((board[0].file, board[0].rank), ('a', 8));
    assert_eq!((board[63].file, board[63].rank), ('h', 1));

    assert_eq!(board[0].piece, Some((Color::Black, Role::Rook)), "a8 is a black rook");
    assert_eq!(board[63].piece, Some((Color::White, Role::Rook)), "h1 is a white rook");

    // a8 is a light square; h1 is light too.
    assert_eq!(board[0].shade, Shade::Light);
    assert_eq!(board[63].shade, Shade::Light);
}

#[test]
fn black_sees_the_board_from_the_other_side() {
    let (state, params) = start();
    let status = project(&state, &params);
    let board = squares(&status, Color::Black, None);

    assert_eq!((board[0].file, board[0].rank), ('h', 1), "black reads h1 first");
    assert_eq!((board[63].file, board[63].rank), ('a', 8));

    // Orientation must not recolour a square: a8 is light from either side.
    let a8 = board.iter().find(|s| (s.file, s.rank) == ('a', 8)).expect("a8");
    assert_eq!(a8.shade, Shade::Light, "shade is a property of the square, not the viewer");
    assert_eq!(a8.piece, Some((Color::Black, Role::Rook)));
}

#[test]
fn selecting_a_piece_marks_its_legal_targets() {
    let (state, params) = start();
    let status = project(&state, &params);
    let board = squares(&status, Color::White, Some(('e', 2)));

    let selected: Vec<_> = board.iter().filter(|s| s.marker == Marker::Selected).collect();
    assert_eq!(selected.len(), 1, "exactly the selected square is marked");
    assert_eq!((selected[0].file, selected[0].rank), ('e', 2));

    let targets: Vec<(char, u8)> = board
        .iter()
        .filter(|s| s.marker == Marker::LegalTarget)
        .map(|s| (s.file, s.rank))
        .collect();
    assert_eq!(targets.len(), 2, "the e2 pawn has exactly two legal moves");
    assert!(targets.contains(&('e', 3)));
    assert!(targets.contains(&('e', 4)));
}

#[test]
fn selecting_a_square_with_no_legal_moves_marks_no_targets() {
    let (state, params) = start();
    let status = project(&state, &params);
    // a1 holds a rook that is completely blocked in the opening position.
    let board = squares(&status, Color::White, Some(('a', 1)));

    assert_eq!(
        board.iter().filter(|s| s.marker == Marker::LegalTarget).count(),
        0,
        "a blocked rook offers nothing"
    );
}

#[test]
fn nothing_is_marked_when_nothing_is_selected() {
    let (state, params) = start();
    let status = project(&state, &params);
    let board = squares(&status, Color::White, None);
    assert!(board.iter().all(|s| s.marker == Marker::None));
}

/// A UI that only ever queens cannot play some legal games, so the picker has
/// to know when to appear.
#[test]
fn promotion_is_detected_only_for_a_pawn_reaching_the_last_rank() {
    let (mut state, params) = start();
    let w = ed25519_dalek::SigningKey::from_bytes(&[1u8; 32]);
    let b = ed25519_dalek::SigningKey::from_bytes(&[2u8; 32]);

    // A line that leaves a white pawn on b7 with a promotion available.
    for (i, uci) in ["b2b4", "a7a5", "b4a5", "b7b6", "a5b6", "h7h6", "b6b7", "h6h5"]
        .iter()
        .enumerate()
    {
        let key = if i % 2 == 0 { &w } else { &b };
        let rec = adjourn_core::make_move(&state, &params, key, uci)
            .unwrap_or_else(|| panic!("move {} ({uci}) rejected", i + 1));
        assert!(state.insert_verified(&rec, &params));
    }
    let status = project(&state, &params);

    assert!(
        adjourn_ui::board::is_promotion(&status, ('b', 7), ('b', 8)),
        "a pawn reaching the last rank promotes"
    );
    assert!(
        !adjourn_ui::board::is_promotion(&status, ('h', 2), ('h', 3)),
        "an ordinary pawn push does not"
    );
}
```

Add `ui`'s dev-dependencies to `ui/Cargo.toml`:

```toml
[dev-dependencies]
adjourn-client.workspace = true
ed25519-dalek.workspace = true
```

Note these use `adjourn-client` **with** default features, which is correct: dev-dependencies do not affect the wasm build of the library.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p adjourn-ui --test board --locked`
Expected: compile failure — `squares`, `Shade`, `Marker`, `is_promotion` do not exist.

- [ ] **Step 3: Implement the board**

`ui/src/board.rs`:

```rust
//! The board, as a pure function of a projected `Status`.
//!
//! Nothing here touches Dioxus, the DOM, or a node. That is deliberate: it
//! makes square colours, orientation, legal-target marking and promotion
//! detection testable natively, with no browser and no framework. The view
//! layer's job is to render these descriptors and nothing else.

use adjourn_core::Status;
use shakmaty::fen::Fen;
use shakmaty::{CastlingMode, Chess, Color, Position, Role};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shade {
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marker {
    None,
    Selected,
    LegalTarget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Square {
    pub file: char,
    pub rank: u8,
    pub shade: Shade,
    pub piece: Option<(Color, Role)>,
    pub marker: Marker,
}

fn position_of(status: &Status) -> Option<Chess> {
    status
        .fen
        .parse::<Fen>()
        .ok()?
        .into_position(CastlingMode::Standard)
        .ok()
}

fn uci_of(file: char, rank: u8) -> String {
    format!("{file}{rank}")
}

/// The 64 squares in reading order for `orientation`: White reads a8 first,
/// Black reads h1 first.
///
/// `selected` marks that square and every square a legal move from it can
/// reach. The legal moves come from `legal_moves`, the same function the CLI
/// uses, so the browser cannot disagree with the projection about what is
/// playable.
pub fn squares(
    status: &Status,
    orientation: Color,
    selected: Option<(char, u8)>,
) -> Vec<Square> {
    let pos = position_of(status);

    let targets: Vec<(char, u8)> = match (selected, status.decision.is_none()) {
        (Some((f, r)), true) => {
            let from = uci_of(f, r);
            legal_moves_for(status)
                .into_iter()
                .filter(|m| m.starts_with(&from))
                .filter_map(|m| {
                    let mut cs = m.chars().skip(2);
                    let file = cs.next()?;
                    let rank = cs.next()?.to_digit(10)? as u8;
                    Some((file, rank))
                })
                .collect()
        }
        _ => Vec::new(),
    };

    let files: Vec<char> = ('a'..='h').collect();
    let mut out = Vec::with_capacity(64);
    let ranks: Vec<u8> = match orientation {
        Color::White => (1..=8).rev().collect(),
        Color::Black => (1..=8).collect(),
    };
    for rank in ranks {
        let row: Vec<char> = match orientation {
            Color::White => files.clone(),
            Color::Black => files.iter().rev().copied().collect(),
        };
        for file in row {
            // a1 is dark. Shade follows the square, never the viewer.
            let dark = ((file as u8 - b'a') + rank) % 2 == 0;
            let piece = pos.as_ref().and_then(|p| {
                let sq = shakmaty::Square::from_ascii(uci_of(file, rank).as_bytes()).ok()?;
                p.board().piece_at(sq).map(|pc| (pc.color, pc.role))
            });
            let marker = if selected == Some((file, rank)) {
                Marker::Selected
            } else if targets.contains(&(file, rank)) {
                Marker::LegalTarget
            } else {
                Marker::None
            };
            out.push(Square {
                file,
                rank,
                shade: if dark { Shade::Dark } else { Shade::Light },
                piece,
                marker,
            });
        }
    }
    out
}

/// `legal_moves` takes the state, but a rendered board only has the `Status`.
/// Re-deriving from the FEN keeps the board a pure function of what it is
/// handed.
fn legal_moves_for(status: &Status) -> Vec<String> {
    let Some(pos) = position_of(status) else {
        return Vec::new();
    };
    pos.legal_moves()
        .iter()
        .map(|m| shakmaty::uci::UciMove::from_move(*m, CastlingMode::Standard).to_string())
        .collect()
}

/// Would moving `from` -> `to` promote a pawn?
///
/// The picker must appear for this move and only this move: a UI that always
/// queens cannot play some legal games, and underpromotion is already
/// supported by the algebra.
pub fn is_promotion(status: &Status, from: (char, u8), to: (char, u8)) -> bool {
    let Some(pos) = position_of(status) else {
        return false;
    };
    let Ok(sq) = shakmaty::Square::from_ascii(uci_of(from.0, from.1).as_bytes()) else {
        return false;
    };
    let Some(piece) = pos.board().piece_at(sq) else {
        return false;
    };
    if piece.role != Role::Pawn {
        return false;
    }
    matches!((piece.color, to.1), (Color::White, 8) | (Color::Black, 1))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p adjourn-ui --test board --locked`
Expected: PASS, 6 tests.

Then re-run the wasm acceptance check — the board is library code and must still compile for the browser:

```
cargo check -p adjourn-ui --target wasm32-unknown-unknown --locked
```

- [ ] **Step 5: Prove the tests discriminate**

Every notification test on the previous branch passed for the wrong reason until this check was run, so run it here before committing.

Temporarily make `squares` ignore `selected` (return `Marker::None` always) and confirm `selecting_a_piece_marks_its_legal_targets` FAILS. Then temporarily invert the orientation branch and confirm `black_sees_the_board_from_the_other_side` FAILS. Restore both. Report what you saw for each.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(ui): board as a pure function of a projected Status"
```

---

### Task 3: The browser transport

**Files:**
- Modify: `ui/src/node.rs`
- Test: `ui/tests/routing.rs` (new)

**Interfaces:**
- Consumes: `adjourn_client::node::NodeClient`, `freenet_stdlib::client_api::WebApi` (the browser one).
- Produces:
  - `pub enum Routed { Response(HostResponse), Notification(ContractInstanceId, UpdateData<'static>), Ignored }`
  - `pub fn route(resp: HostResponse) -> Routed` — pure, natively testable
  - `pub struct BrowserClient` with `pub async fn connect(url: &str) -> anyhow::Result<Self>` and a `NodeClient` impl
  - `pub fn browser_entropy() -> anyhow::Result<[u8; 32]>`

**Why this shape.** The browser `WebApi` is **callback-based** — `WebApi::start(conn, result_handler, error_handler, onopen_handler)` and `send()`, with **no `recv()`**. The native one has `recv()`. So responses arrive by push, and `NodeClient`'s request/response methods need somewhere to put them. An unbounded mpsc channel is that somewhere: the result handler sends, the client receives. The classification of what arrived is pulled out as `route`, a pure function, because it is the only part with logic and therefore the only part worth testing natively.

- [ ] **Step 1: Write the failing tests**

`ui/tests/routing.rs`. This covers `route` only — everything else in the module needs a browser, and pretending otherwise is how the previous branch shipped a crate that did not compile for its target:

```rust
use adjourn_ui::node::{route, Routed};
use freenet_stdlib::client_api::{ContractResponse, HostResponse};
use freenet_stdlib::prelude::*;

fn an_id() -> ContractInstanceId {
    ContractInstanceId::new([9u8; 32])
}

#[test]
fn an_update_notification_is_routed_as_a_notification() {
    let resp = HostResponse::ContractResponse(ContractResponse::UpdateNotification {
        key: ContractKey::from(an_id()),
        update: UpdateData::State(State::from(vec![1, 2, 3])),
    });
    match route(resp) {
        Routed::Notification(id, UpdateData::State(bytes)) => {
            assert_eq!(id, an_id());
            assert_eq!(bytes.as_ref(), &[1, 2, 3]);
        }
        other => panic!("expected a notification, got {other:?}"),
    }
}

#[test]
fn an_ordinary_response_is_routed_as_a_response() {
    match route(HostResponse::Ok) {
        Routed::Response(HostResponse::Ok) => {}
        other => panic!("expected a response, got {other:?}"),
    }
}

/// A notification arriving while a request is in flight must not be mistaken
/// for that request's answer -- that is what makes `watch` miss moves.
#[test]
fn a_notification_is_never_mistaken_for_a_response() {
    let resp = HostResponse::ContractResponse(ContractResponse::UpdateNotification {
        key: ContractKey::from(an_id()),
        update: UpdateData::Delta(StateDelta::from(vec![4, 5])),
    });
    assert!(
        !matches!(route(resp), Routed::Response(_)),
        "a notification routed as a response would be consumed by whichever \
         request happened to be waiting, and lost"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p adjourn-ui --test routing --locked`
Expected: compile failure — `route` and `Routed` do not exist.

- [ ] **Step 3: Implement the transport**

`ui/src/node.rs`:

```rust
//! The browser transport.
//!
//! `freenet-stdlib`'s browser `WebApi` is callback-based: it takes a result
//! handler and pushes into it, with no `recv()` (the native one has one). But
//! `NodeClient`'s methods are request/response. An unbounded channel bridges
//! the two -- the handler sends, the client awaits.
//!
//! Update notifications arrive on the same channel as request answers, so they
//! are separated by [`route`] and parked in `pending`. Mistaking one for the
//! other would let whichever request is in flight swallow a move, which is
//! exactly the failure `watch` exists to avoid.

use adjourn_client::node::NodeClient;
use adjourn_core::delegate_api::{Request, Response};
use anyhow::{anyhow, bail, Context};
use freenet_stdlib::client_api::{
    ClientRequest, ContractRequest, ContractResponse, DelegateRequest, HostResponse, WebApi,
};
use freenet_stdlib::prelude::*;
use futures::channel::{mpsc, oneshot};
use futures::StreamExt;
use std::collections::VecDeque;

/// What a frame from the node turned out to be.
#[derive(Debug)]
pub enum Routed {
    Response(HostResponse),
    Notification(ContractInstanceId, UpdateData<'static>),
    Ignored,
}

/// Classify one frame. Pure, so it can be tested without a browser.
pub fn route(resp: HostResponse) -> Routed {
    match resp {
        HostResponse::ContractResponse(ContractResponse::UpdateNotification { key, update }) => {
            Routed::Notification(*key.id(), update)
        }
        other => Routed::Response(other),
    }
}

pub struct BrowserClient {
    api: WebApi,
    inbox: mpsc::UnboundedReceiver<HostResponse>,
    pending: VecDeque<(ContractInstanceId, UpdateData<'static>)>,
    delegate_key: Option<DelegateKey>,
}

impl BrowserClient {
    /// Open a WebSocket to the node and wait for it to be usable.
    ///
    /// Sending before `onopen` fires is silently dropped by the browser, so the
    /// connect future does not resolve until the socket is open.
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let socket = web_sys::WebSocket::new(url)
            .map_err(|e| anyhow!("could not open a WebSocket to {url}: {e:?}"))?;

        let (tx, inbox) = mpsc::unbounded();
        let (open_tx, open_rx) = oneshot::channel();
        let mut open_tx = Some(open_tx);

        let api = WebApi::start(
            socket,
            move |result| {
                if let Ok(resp) = result {
                    // A closed receiver means the client is gone; nothing to do.
                    let _ = tx.unbounded_send(resp);
                }
            },
            |_err| {},
            move || {
                if let Some(tx) = open_tx.take() {
                    let _ = tx.send(());
                }
            },
        );

        open_rx
            .await
            .map_err(|_| anyhow!("the WebSocket closed before it opened"))?;

        Ok(Self {
            api,
            inbox,
            pending: VecDeque::new(),
            delegate_key: None,
        })
    }

    /// The next request answer, parking any notification that arrives first.
    async fn next_response(&mut self, op: &str) -> anyhow::Result<HostResponse> {
        loop {
            let frame = self
                .inbox
                .next()
                .await
                .ok_or_else(|| anyhow!("the connection closed while waiting for {op}"))?;
            match route(frame) {
                Routed::Response(resp) => return Ok(resp),
                Routed::Notification(id, update) => self.pending.push_back((id, update)),
                Routed::Ignored => {}
            }
        }
    }

    pub async fn register_delegate(&mut self, container: DelegateContainer) -> anyhow::Result<()> {
        self.delegate_key = Some(container.key().clone());
        self.api
            .send(ClientRequest::DelegateOp(
                DelegateRequest::RegisterDelegate {
                    delegate: container,
                    cipher: [0u8; 32],
                    nonce: [0u8; 24],
                },
            ))
            .await
            .map_err(|e| anyhow!("sending RegisterDelegate: {e}"))?;
        match self.next_response("RegisterDelegate").await? {
            HostResponse::Ok | HostResponse::DelegateResponse { .. } => Ok(()),
            other => bail!("unexpected response to RegisterDelegate: {other:?}"),
        }
    }
}

```

**The four `NodeClient` methods are `cli/src/ws.rs`'s bodies with one
substitution.** Do not re-derive the requests — `ws.rs` makes exactly these
calls against a real node and is known to work. Open it and copy `get`, `put`,
`update` and `delegate` across, changing only:

- `self.recv_timeout("<op>")` becomes `self.next_response("<op>")`
- the `UpdateNotification` arms already push to `self.pending`, which exists
  here too and behaves the same

Details `ws.rs` will show you that are easy to get wrong from memory:
`ContractRequest::Get` takes `key: id` — the `ContractInstanceId` itself, not a
`ContractKey`; a missing contract answers `ContractResponse::NotFound`, not a
`PutResponse`; `Put` carries **both** `subscribe` and `blocking_subscribe`; a
`SubscribeResponse` can arrive before the answer you want and must be skipped;
and the delegate call is `ApplicationMessage::new(req.encode())` with a single
argument.

`next_update` is the one method that differs in substance, because there is no
`recv_timeout` to bypass. It goes in the same `impl` block as the four
copied methods:

```rust
impl NodeClient for BrowserClient {
    // get / put / update / delegate: copied from `cli/src/ws.rs` as above.

    async fn next_update(
        &mut self,
    ) -> anyhow::Result<Option<(ContractInstanceId, UpdateData<'static>)>> {
        if let Some(parked) = self.pending.pop_front() {
            return Ok(Some(parked));
        }
        loop {
            let Some(frame) = self.inbox.next().await else {
                return Ok(None);
            };
            match route(frame) {
                Routed::Notification(id, update) => return Ok(Some((id, update))),
                // A late answer to a request nobody is waiting on. Dropping the
                // connection over it would end a healthy session.
                Routed::Response(_) | Routed::Ignored => continue,
            }
        }
    }
}
```

Finally, the entropy helper:

```rust
/// 32 bytes from the browser's CSPRNG.
///
/// `adjourn-client` takes entropy as a parameter precisely so this crate needs
/// no `getrandom`: a `getrandom` in the graph emits wasm-bindgen placeholder
/// imports, which is what makes a contract fail to instantiate.
pub fn browser_entropy() -> anyhow::Result<[u8; 32]> {
    let mut bytes = [0u8; 32];
    web_sys::window()
        .ok_or_else(|| anyhow!("no window"))?
        .crypto()
        .map_err(|e| anyhow!("no crypto: {e:?}"))?
        .get_random_values_with_u8_array(&mut bytes)
        .map_err(|e| anyhow!("crypto.getRandomValues failed: {e:?}"))?;
    Ok(bytes)
}
```

If any `ClientRequest` / `ContractRequest` field name differs from the above, follow `cli/src/ws.rs` — it makes the identical calls against the native `WebApi` and is known to work against a real node.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p adjourn-ui --test routing --locked`
Expected: PASS, 3 tests.

Then the acceptance check, which is the one that matters:

```
cargo check -p adjourn-ui --target wasm32-unknown-unknown --locked
```

- [ ] **Step 5: Prove the routing test discriminates**

Temporarily change `route` so the `UpdateNotification` arm falls through to `Routed::Response(other)`, and confirm both `an_update_notification_is_routed_as_a_notification` and `a_notification_is_never_mistaken_for_a_response` FAIL. Restore. Report what you saw.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(ui): browser transport over the callback-based WebApi"
```

---

### Task 4: Documentation

**Files:**
- Modify: `CLAUDE.md`, `README.md`

- [ ] **Step 1: Add the crate to the table**

Add a `ui/` (`adjourn-ui`) row: the Dioxus web UI, depending on `adjourn-client` with `default-features = false` so the contract and delegate crates stay out of the wasm build. Note that the bundle embeds both compiled WASM modules, because a browser cannot read them off disk — which pins both keys into a UI build.

- [ ] **Step 2: Record the two feature facts, which look contradictory**

Write this up in the register the file uses, because the next reader will otherwise "fix" one of them:

`adjourn-client` must depend on `freenet-stdlib` **without** `net`, since it is shared with the CLI and `net` pulls `tokio-tungstenite` on native targets. `adjourn-ui` depends on it **with** `net`, because on wasm that is what provides the browser `WebApi` — upstream gates `tokio` and `tokio-tungstenite` to `cfg(any(unix, windows))` and `web-sys`/`wasm-bindgen` to `cfg(target_family = "wasm")`, so the same feature means different things per target.

Also record why a `ui` crate can sit in the same workspace as the contract: `scripts/build-contract.sh` and `build-delegate.sh` are package-scoped (`-p`), so no workspace-wide build ever unifies the UI's features into the contract's graph. River does the same.

- [ ] **Step 3: Record that the UI needs no `getrandom`**

`adjourn-client` takes entropy as a parameter, so the UI supplies it from `crypto.getRandomValues` via `web_sys` and adds no `getrandom` to the graph. That is the whole reason the entropy hoist was worth doing, and it is what keeps the wasm-bindgen placeholder imports that break contract instantiation out of this build.

- [ ] **Step 4: State the coverage honestly**

The board and the transport's `route` function are tested natively. **Nothing in this crate has been loaded in a browser** — `dx` (the Dioxus CLI) is not installed in this environment, and this plan does not install it. The wasm build is compile-checked in CI and nothing more. Say that plainly; do not let the repo's overall test count imply otherwise.

- [ ] **Step 5: Update the test counts**

Run `cargo test --workspace --locked`, count the per-file results, and update the summary line and per-file bullets. Do not guess — use the numbers the run prints.

- [ ] **Step 6: Commit**

```bash
git add CLAUDE.md README.md
git commit -m "docs: record the ui crate, its feature split, and its coverage"
```

---

## Notes for the executor

- **Build the contract and delegate before anything else.** `ui/build.rs` panics with instructions if either is missing, because the UI embeds them.
- **`cargo check --target wasm32-unknown-unknown` works on Windows** even though `cargo test --workspace` does not. The acceptance check is yours to run.
- If a test you wrote passes on the first run, be suspicious. Every task here has an explicit step for breaking the feature and confirming the test notices; the previous branch shipped three tests that passed for the wrong reason.
