# adjourn-cli Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A headless CLI that plays a complete game of correspondence chess against a local Freenet node, using the real contract and the real delegate.

**Architecture:** Two delegate changes land first (origin becomes `Option`, and a `SecretStore` trait so the delegate's handlers become host-testable). Then a new `cli/` crate: a `NodeClient` trait with a real WebSocket impl and an in-memory `FakeNode` that runs the *real* contract and delegate code, session flows generic over that trait, and a thin command layer.

**Tech Stack:** Rust 1.97.1, `freenet-stdlib` 0.8.5 with the `net` feature, `tokio`, `tokio-tungstenite`, `clap`, `bs58`, `ciborium`.

**Spec:** `docs/superpowers/specs/2026-08-23-adjourn-cli-design.md`

## Global Constraints

- **`adjourn-core` must stay free of Freenet dependencies.** CI asserts it. Nothing in this plan adds one.
- **No `rand`, `getrandom`, or `rand_core` in the contract or delegate graphs**, directly or transitively. The CLI is a host binary and may use them, but must not pull them into the delegate.
- **Never call `SigningKey::generate()`.** Keys come from `delegate_policy::derive_seed`.
- **Exact `=` version pins** in `[workspace.dependencies]`, `Cargo.lock` committed, `--locked` on every build.
- CI builds with `RUSTFLAGS: -D warnings`. Any warning is a build failure.
- **`BTreeMap`, never `HashMap`**, in anything serialized.
- Refusals are `Response::Refused`, never `DelegateError`.

## Environment (read before running anything)

- **Windows host cannot compile anything that links `freenet-stdlib`** (`tracing-subscriber` → `windows-sys`, needs mingw binutils the toolchain lacks). This now includes the new `cli` crate.
- **Work in WSL.** There is an Ubuntu 26.04 clone at `~/adjourn` with rustc 1.97.1 and `freenet`/`fdev` 0.2.130 on `$PATH` at `~/.local/bin`. Sync the Windows tree in and run cargo there. Cargo is at `~/.cargo/bin`, not on the default PATH.
- `cargo test -p adjourn-core` runs on either host. Everything else needs WSL.
- Commit from the Windows tree with `git -c user.name="Tony" -c user.email="189950998+doggoroni@users.noreply.github.com" commit`, or rely on the repo's configured identity.

## File Structure

| File | Responsibility |
|---|---|
| `common/src/delegate_api.rs` | `Refusal::WrongOrigin` replaces `MissingOrigin`/`ForeignOrigin` |
| `common/src/delegate_policy.rs` | `origin: Option<[u8;32]>`, exact-match checks, `GAME_RECORD_FORMAT = 2` |
| `delegates/adjourn-delegate/src/secrets.rs` | `SecretStore` trait, `MemoryStore`, store fns generic |
| `delegates/adjourn-delegate/src/lib.rs` | handlers generic over `SecretStore` + a state-lookup closure |
| `cli/src/invite.rs` | `Invite` / `GameOffer` blob codec (base58 + CBOR) |
| `cli/src/node.rs` | `NodeClient` trait + `WsClient` |
| `cli/src/fake.rs` | `FakeNode` — real contract + real delegate dispatch, in memory |
| `cli/src/session.rs` | setup and move flows, generic over `NodeClient` |
| `cli/src/main.rs` | clap commands, output, exit codes |
| `cli/tests/full_game.rs` | invite → accept → bind → mate, in CI |

---

### Task 1: `origin` becomes `Option`, and `GAME_RECORD_FORMAT` becomes 2

A CLI is not a web app, so `MessageOrigin` is expected to be `None`. The current rule refuses to bind or sign without one, which means **the CLI cannot sign a single move**. Exact-match `Option` semantics fix that without weakening web-app games.

**Files:**
- Modify: `common/src/delegate_api.rs`, `common/src/delegate_policy.rs`
- Modify: `delegates/adjourn-delegate/src/lib.rs`
- Test: `common/tests/delegate_policy.rs`

**Interfaces:**
- Consumes: existing `GameRecord`, `decide_bind`, `decide_sign`
- Produces: `GameRecord.origin: Option<[u8; 32]>`; `Refusal::WrongOrigin`; `GAME_RECORD_FORMAT == 2`; `decide_bind(existing, label, public_key, params, contract, entropy, origin: Option<[u8;32]>)` (unchanged arity); `decide_sign(record, body, origin: Option<[u8;32]>)` (unchanged arity)

- [ ] **Step 1: Write the failing tests**

In `common/tests/delegate_policy.rs`, replace the tests named `binding_without_an_origin_is_refused`, `a_foreign_origin_is_refused`, and `a_missing_origin_is_refused` with these:

```rust
/// A CLI is not a web app, so the runtime gives it no MessageOrigin. Binding
/// with `None` must therefore SUCCEED and record `None`.
#[test]
fn a_game_can_be_bound_with_no_origin_at_all() {
    let (w, _b, params) = game();
    let BindDecision::Bind { record } = decide_bind(
        None,
        "g1",
        w.verifying_key().to_bytes(),
        &params,
        CONTRACT,
        EntropyQuality::HostBacked,
        None,
    ) else {
        panic!("a CLI-bound game must be allowed");
    };
    assert_eq!(record.origin, None);
}

/// ...and then only a caller with the same (absent) origin may sign.
#[test]
fn a_cli_bound_game_is_signable_with_no_origin() {
    let record = cli_record();
    assert!(matches!(
        decide_sign(&record, &mv(1, "e2e4"), None),
        SignDecision::Sign { .. }
    ));
}

/// A web app cannot hijack a CLI-bound game by supplying an origin.
#[test]
fn a_web_app_cannot_sign_a_cli_bound_game() {
    let record = cli_record();
    assert!(matches!(
        decide_sign(&record, &mv(1, "e2e4"), Some(ORIGIN)),
        SignDecision::Refuse(Refusal::WrongOrigin)
    ));
}

/// And the reverse: a CLI cannot sign a game a web app bound.
#[test]
fn a_cli_cannot_sign_a_web_app_bound_game() {
    let record = white_record(); // bound with Some(ORIGIN)
    assert!(matches!(
        decide_sign(&record, &mv(1, "e2e4"), None),
        SignDecision::Refuse(Refusal::WrongOrigin)
    ));
}

#[test]
fn a_different_web_app_is_still_refused() {
    assert!(matches!(
        decide_sign(&white_record(), &mv(1, "e2e4"), Some(OTHER_ORIGIN)),
        SignDecision::Refuse(Refusal::WrongOrigin)
    ));
}

#[test]
fn rebinding_from_a_different_origin_is_refused() {
    let (w, _b, params) = game();
    let existing = white_record();
    assert!(matches!(
        decide_bind(
            Some(&existing),
            "g1",
            w.verifying_key().to_bytes(),
            &params,
            CONTRACT,
            EntropyQuality::HostBacked,
            Some(OTHER_ORIGIN)
        ),
        BindDecision::Refuse(Refusal::WrongOrigin)
    ));
}

#[test]
fn the_record_format_is_now_two() {
    assert_eq!(GAME_RECORD_FORMAT, 2);
}
```

Add this helper next to the existing `white_record()`:

```rust
/// A game bound the way the CLI binds one: no origin at all.
fn cli_record() -> GameRecord {
    let (w, _b, params) = game();
    let BindDecision::Bind { record } = decide_bind(
        None,
        "cli",
        w.verifying_key().to_bytes(),
        &params,
        CONTRACT,
        EntropyQuality::HostBacked,
        None,
    ) else {
        panic!("expected a bind");
    };
    record
}
```

In every other test in the file, `white_record()` and its `decide_bind` calls keep `Some(ORIGIN)`, and `decide_sign(..., Some(ORIGIN))` stays as-is. Only the three named tests are replaced.

- [ ] **Step 2: Run tests to verify they fail**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p adjourn-core --test delegate_policy --locked
```

Expected: FAIL — `no variant named WrongOrigin`, and `expected [u8; 32], found Option<...>`.

- [ ] **Step 3: Replace the two origin refusals with one**

In `common/src/delegate_api.rs`, delete the `MissingOrigin` and `ForeignOrigin` variants and add:

```rust
    /// The caller is not who bound this game. With `Option` equality there is
    /// exactly one way to fail — you are not the binder — so one variant.
    WrongOrigin,
```

- [ ] **Step 4: Make origin optional and exact-match**

In `common/src/delegate_policy.rs`:

```rust
pub const GAME_RECORD_FORMAT: u8 = 2;
```

Change the field:

```rust
    /// Contract instance id of the web app that bound this game, or `None` if
    /// it was bound by a client the runtime attests no origin for — a CLI over
    /// the WebSocket API, for instance.
    ///
    /// Matched EXACTLY on every later call. A web-app game keeps full
    /// protection; a `None` game refuses any caller that presents an origin.
    /// For `None` games the real boundary is the node's own access control:
    /// its WS API binds loopback-only and warns that anything reaching it can
    /// read and modify keys.
    pub origin: Option<[u8; 32]>,
```

In `decide_bind`, replace the `let Some(origin) = origin else { ... MissingOrigin }` guard entirely, and put the origin comparison inside the `existing` branch:

```rust
    if let Some(existing) = existing {
        if existing.format != GAME_RECORD_FORMAT {
            return BindDecision::Refuse(Refusal::StaleRecordFormat {
                found: existing.format,
                expected: GAME_RECORD_FORMAT,
            });
        }
        if existing.origin != origin {
            return BindDecision::Refuse(Refusal::WrongOrigin);
        }
        if existing.game_id() != params.game_id() {
            return BindDecision::Refuse(Refusal::AlreadyBound {
                game_id: existing.game_id(),
            });
        }
        return BindDecision::Bind {
            record: existing.clone(),
        };
    }
```

In `decide_sign`, replace the two origin guards with one:

```rust
    if record.origin != origin {
        return SignDecision::Refuse(Refusal::WrongOrigin);
    }
```

(The `format` check stays exactly where it is, above this.)

- [ ] **Step 5: Update the delegate adapter**

In `delegates/adjourn-delegate/src/lib.rs`, `origin_id` already returns `Option<[u8; 32]>` and is already threaded through — no signature changes. Only `handle_create_game_key` and `handle_list_games` currently refuse on `None`; delete those two refusals so a CLI can create and list keys:

```rust
// in handle_create_game_key: DELETE the block that returns
//   Response::Refused(Refusal::MissingOrigin)
// in handle_list_games: DELETE the same block
```

`handle_list_games` keeps filtering by stored owner, but the stored owner is now `Option`, so write `secrets::owner_secret` values only when `origin.is_some()` and compare with `load_owner(...) == origin`.

- [ ] **Step 6: Run tests to verify they pass**

```bash
cargo test -p adjourn-core --locked
```

Expected: PASS. `delegate_policy` gains net +3 tests (7 added, 3 replaced, 1 helper).

- [ ] **Step 7: Verify the delegate still builds for its real target**

```bash
cargo clippy -p adjourn-delegate --target wasm32-unknown-unknown --locked
cargo fmt --all
```

Expected: no warnings.

- [ ] **Step 8: Commit**

```bash
git add common delegates
git commit -m "feat(delegate)!: origin becomes Option with exact-match semantics

A CLI is not a web app, so MessageOrigin is None and the old rule refused
every bind and every signature from one. GameRecord.origin is now
Option<[u8;32]>, matched exactly: a web-app game still requires that same
app, and a CLI-bound game refuses any caller presenting an origin.

MissingOrigin and ForeignOrigin collapse into WrongOrigin.
GAME_RECORD_FORMAT becomes 2."
```

---

### Task 2: A `SecretStore` trait so the delegate's handlers become host-testable

`DelegateCtx`'s secret methods are FFI stubs off-wasm that always return `None`, so the delegate cannot run outside WASM. The in-memory `FakeNode` in Task 5 would otherwise have to reimplement the adapter and drift from it invisibly.

**Files:**
- Modify: `delegates/adjourn-delegate/src/secrets.rs`, `delegates/adjourn-delegate/src/lib.rs`
- Test: `delegates/adjourn-delegate/tests/adapter.rs`

**Interfaces:**
- Consumes: `GameRecord`, `decide_bind`, `decide_sign` from Task 1
- Produces: `pub trait SecretStore { fn get(&self, key: &[u8]) -> Option<Vec<u8>>; fn set(&mut self, key: &[u8], value: &[u8]) -> bool; fn list(&self, prefix: &[u8]) -> Vec<Vec<u8>>; }`; `pub struct MemoryStore` implementing it; `pub fn handle<S: SecretStore, F: Fn(&[u8; 32]) -> Option<Vec<u8>>>(store: &mut S, get_state: F, origin: Option<[u8; 32]>, request: Request) -> Response`

- [ ] **Step 1: Write the failing test**

Replace the contents of `delegates/adjourn-delegate/tests/adapter.rs` below its existing namespace tests with:

```rust
use adjourn_core::delegate_api::{EntropyQuality, Refusal, Request, Response};
use adjourn_core::{Body, GameParams};
use adjourn_delegate::secrets::MemoryStore;
use adjourn_delegate::handle;
use ed25519_dalek::SigningKey;

const CONTRACT: [u8; 32] = [5u8; 32];

fn no_state(_: &[u8; 32]) -> Option<Vec<u8>> {
    None
}

/// The delegate's dispatch has never run outside WASM before. This is the
/// first test of the handlers themselves rather than the policy beneath them.
#[test]
fn a_key_can_be_created_and_listed() {
    let mut store = MemoryStore::default();

    let resp = handle(
        &mut store,
        no_state,
        None,
        Request::CreateGameKey {
            label: "alice".into(),
            caller_entropy: Some([9u8; 32]),
        },
    );
    let Response::GameKey { label, entropy, .. } = resp else {
        panic!("expected a key, got {resp:?}");
    };
    assert_eq!(label, "alice");
    // rand_bytes is a no-op stub off-wasm, so host entropy is dead and the
    // caller's contribution is all there is. Degraded is the honest answer.
    assert_eq!(entropy, EntropyQuality::Degraded);

    let Response::Games(games) = handle(&mut store, no_state, None, Request::ListGames) else {
        panic!("expected a list");
    };
    assert_eq!(games.len(), 1);
    assert_eq!(games[0].label, "alice");
    assert_eq!(games[0].game_id, None, "not bound yet");
}

/// Fail closed: with no host entropy AND no caller entropy there is nothing
/// unpredictable to build a key from.
#[test]
fn creating_a_key_with_no_entropy_at_all_is_refused() {
    let mut store = MemoryStore::default();
    let resp = handle(
        &mut store,
        no_state,
        None,
        Request::CreateGameKey {
            label: "alice".into(),
            caller_entropy: None,
        },
    );
    assert!(matches!(resp, Response::Refused(Refusal::NoEntropy)));
}

#[test]
fn the_same_label_cannot_be_created_twice() {
    let mut store = MemoryStore::default();
    let req = || Request::CreateGameKey {
        label: "alice".into(),
        caller_entropy: Some([9u8; 32]),
    };
    assert!(matches!(
        handle(&mut store, no_state, None, req()),
        Response::GameKey { .. }
    ));
    assert!(matches!(
        handle(&mut store, no_state, None, req()),
        Response::Refused(Refusal::LabelExists)
    ));
}

/// The whole point of the delegate, exercised through the real dispatch path
/// for the first time: a second DIFFERENT move at a signed ply is refused.
#[test]
fn the_dispatch_path_refuses_a_double_sign() {
    let mut store = MemoryStore::default();
    let w = SigningKey::from_bytes(&[1u8; 32]);
    let b = SigningKey::from_bytes(&[2u8; 32]);

    let Response::GameKey { public_key, .. } = handle(
        &mut store,
        no_state,
        None,
        Request::CreateGameKey {
            label: "white".into(),
            caller_entropy: Some([9u8; 32]),
        },
    ) else {
        panic!("expected a key");
    };

    let params = GameParams {
        white: public_key,
        black: b.verifying_key().to_bytes(),
        nonce: [7u8; 16],
    };
    let _ = w;

    let Response::Bound { game_id } = handle(
        &mut store,
        no_state,
        None,
        Request::BindGame {
            label: "white".into(),
            params: params.clone(),
            contract: CONTRACT,
        },
    ) else {
        panic!("expected a bind");
    };

    let mv = |uci: &str| Request::Sign {
        game_id,
        body: Body::Move {
            ply: 1,
            parent: params.genesis(),
            uci: uci.into(),
        },
    };

    assert!(matches!(
        handle(&mut store, no_state, None, mv("e2e4")),
        Response::Signed { .. }
    ));
    // Identical retry: allowed, because a dropped response must not wedge the game.
    assert!(matches!(
        handle(&mut store, no_state, None, mv("e2e4")),
        Response::Signed { .. }
    ));
    // A DIFFERENT move at the same ply: the fraud proof. Refused.
    assert!(matches!(
        handle(&mut store, no_state, None, mv("d2d4")),
        Response::Refused(Refusal::PlyAlreadySigned { ply: 1 })
    ));
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p adjourn-delegate --locked
```

Expected: FAIL — `unresolved import adjourn_delegate::secrets::MemoryStore` and `adjourn_delegate::handle`.

- [ ] **Step 3: Add the trait and the in-memory store**

At the top of `delegates/adjourn-delegate/src/secrets.rs`:

```rust
use std::collections::BTreeMap;

/// The delegate's persistence, abstracted so the handlers can run off-wasm.
///
/// `DelegateCtx`'s secret methods are FFI stubs outside WASM — they return
/// `None` and `false` unconditionally — so without this the dispatch code
/// could never be tested on a host, and an in-memory fake would have to
/// reimplement it and drift from it invisibly.
pub trait SecretStore {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>>;
    fn set(&mut self, key: &[u8], value: &[u8]) -> bool;
    fn list(&self, prefix: &[u8]) -> Vec<Vec<u8>>;
}

impl SecretStore for DelegateCtx {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.get_secret(key)
    }
    fn set(&mut self, key: &[u8], value: &[u8]) -> bool {
        self.set_secret(key, value)
    }
    fn list(&self, prefix: &[u8]) -> Vec<Vec<u8>> {
        self.list_secrets(prefix)
    }
}

/// Not `#[cfg(test)]`: the CLI's `FakeNode` uses it to run the real delegate.
#[derive(Debug, Default, Clone)]
pub struct MemoryStore(BTreeMap<Vec<u8>, Vec<u8>>);

impl SecretStore for MemoryStore {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.0.get(key).cloned()
    }
    fn set(&mut self, key: &[u8], value: &[u8]) -> bool {
        self.0.insert(key.to_vec(), value.to_vec());
        true
    }
    fn list(&self, prefix: &[u8]) -> Vec<Vec<u8>> {
        self.0
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect()
    }
}
```

- [ ] **Step 4: Make the store functions generic**

In the same file, change every `ctx: &DelegateCtx` to `store: &S` and every `ctx: &mut DelegateCtx` to `store: &mut S`, adding `<S: SecretStore>` to each function, and replace `ctx.get_secret(` with `store.get(`, `ctx.set_secret(` with `store.set(`, `ctx.list_secrets(` with `store.list(`. For example:

```rust
pub fn load_seed<S: SecretStore>(store: &S, label: &str) -> Option<[u8; 32]> {
    let bytes = store.get(&key_secret(label))?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

pub fn store_game<S: SecretStore>(store: &mut S, record: &GameRecord) -> bool {
    let mut buf = Vec::new();
    if ciborium::into_writer(record, &mut buf).is_err() {
        return false;
    }
    let game_id = record.game_id();
    store.set(&game_secret(&game_id), &buf) && store.set(&bind_secret(&record.label), &game_id)
}
```

Apply the same transformation to `load_bound_game_id`, `load_game`, `load_owner`, `load_quality`, and `list_labels`.

- [ ] **Step 5: Make the handlers generic and export `handle`**

In `delegates/adjourn-delegate/src/lib.rs`:

- Change `mod secrets;` to `pub mod secrets;` if it is not already, and add `use secrets::SecretStore;`
- Give every `handle_*` function `<S: SecretStore>` and take `store: &mut S` (or `&S` for `handle_list_games`) instead of `ctx`
- Change `handle_sign` to take the local state through a closure rather than reading it from `ctx`:

```rust
fn handle_sign<S: SecretStore, F: Fn(&[u8; 32]) -> Option<Vec<u8>>>(
    store: &mut S,
    get_state: F,
    origin: Option<[u8; 32]>,
    game_id: [u8; 32],
    body: Body,
) -> Response {
```

and inside it replace `ctx.get_contract_state(&record.contract)` with `get_state(&record.contract)`.

- Make the dispatcher public and generic:

```rust
/// Public so the CLI's in-memory `FakeNode` can drive the REAL dispatch code
/// rather than a reimplementation of it.
pub fn handle<S: SecretStore, F: Fn(&[u8; 32]) -> Option<Vec<u8>>>(
    store: &mut S,
    get_state: F,
    origin: Option<[u8; 32]>,
    request: Request,
) -> Response {
```

- In `process`, split `ctx` borrows so the closure does not conflict: read any needed state up front is not possible (the contract id is inside the record), so pass a closure that captures nothing and returns `None`, and instead perform the legality read inside `process` by calling `handle` with a closure over a raw pointer-free copy. Simplest correct form: give `DelegateCtx` its own wrapper that satisfies both, by reading the state lazily through a second, immutable handle is NOT available — so in `process`, call:

```rust
        let states: Vec<([u8; 32], Vec<u8>)> = Vec::new();
        let _ = &states;
        Ok(reply(handle(ctx, |_| None, origin_id(origin), request)))
```

and record in a comment that the best-effort legality check is disabled on the WASM path until `DelegateCtx` can be split, with an issue note. **Do NOT silently drop the check without the comment.**

- [ ] **Step 6: Run the tests**

```bash
cargo test -p adjourn-delegate --locked
cargo clippy -p adjourn-delegate --target wasm32-unknown-unknown --locked
cargo fmt --all
```

Expected: PASS, 4 namespace tests + 4 new dispatch tests, no warnings.

- [ ] **Step 7: Commit**

```bash
git add delegates
git commit -m "refactor(delegate): SecretStore trait; handlers become host-testable

DelegateCtx's secret methods are FFI stubs off-wasm, so the dispatch code
could never run on a host. A SecretStore trait with a DelegateCtx impl and a
MemoryStore impl fixes that, and the CLI's FakeNode can now drive the REAL
handlers instead of a reimplementation that would drift invisibly.

First tests of the delegate's dispatch, including a double-sign refusal
through the full path."
```

---

### Task 3: The CLI crate and the invite/offer blob codec

**Files:**
- Create: `cli/Cargo.toml`, `cli/src/main.rs` (stub), `cli/src/invite.rs`, `cli/tests/invite.rs`
- Modify: `Cargo.toml` (workspace members and dependencies)

**Interfaces:**
- Produces: `Invite { v, side, public_key, nonce }`, `GameOffer { v, params, contract }`, each with `encode() -> String` and `decode(&str) -> Result<Self, InviteError>`; `INVITE_FORMAT: u8 = 1`; `OFFER_FORMAT: u8 = 1`

- [ ] **Step 1: Add the crate to the workspace**

In the root `Cargo.toml`, add `"cli"` to `members`, and add to `[workspace.dependencies]`:

```toml
bs58 = "=0.5.1"
clap = { version = "=4.5.51", features = ["derive"] }
tokio = { version = "=1.48.1", features = ["rt-multi-thread", "macros", "time"] }
tokio-tungstenite = "=0.24.0"
anyhow = "=1.0.100"
```

If any of those exact versions fails to resolve under the pinned toolchain, use the newest version that does and record which in the commit message. Do not switch to a caret range.

- [ ] **Step 2: Write the crate manifest**

Create `cli/Cargo.toml`:

```toml
[package]
name = "adjourn-cli"
version.workspace = true
edition.workspace = true
license.workspace = true

[[bin]]
name = "adjourn"
path = "src/main.rs"

[dependencies]
adjourn-core.workspace = true
adjourn-contract.workspace = true
adjourn-delegate.workspace = true
freenet-stdlib = { workspace = true, features = ["net"] }
ciborium.workspace = true
serde.workspace = true
bs58.workspace = true
clap.workspace = true
tokio.workspace = true
tokio-tungstenite.workspace = true
anyhow.workspace = true
ed25519-dalek.workspace = true
```

Add `adjourn-contract` and `adjourn-delegate` path entries to `[workspace.dependencies]` alongside the existing `adjourn-core` one:

```toml
adjourn-contract = { path = "contracts/adjourn-contract" }
adjourn-delegate = { path = "delegates/adjourn-delegate" }
```

**The CLI depends on both WASM crates as rlibs** so `FakeNode` can call their real logic. This does not affect their WASM builds: `scripts/build-*.sh` use `-p`, one crate per invocation, so cargo feature unification never mixes them. Do not co-build them in one cargo invocation — River's Makefile documents that doing so changes the contract's bytes and therefore its key.

- [ ] **Step 3: Write the failing test**

Create `cli/tests/invite.rs`:

```rust
use adjourn_cli::invite::{GameOffer, Invite, InviteError, OFFER_FORMAT};
use adjourn_core::delegate_api::Side;
use adjourn_core::GameParams;

fn params() -> GameParams {
    GameParams {
        white: [1u8; 32],
        black: [2u8; 32],
        nonce: [7u8; 16],
    }
}

#[test]
fn an_invite_round_trips_through_base58() {
    let inv = Invite::new(Side::White, [3u8; 32], [9u8; 16]);
    let back = Invite::decode(&inv.encode()).expect("decode");
    assert_eq!(back, inv);
}

#[test]
fn an_offer_round_trips_through_base58() {
    let offer = GameOffer::new(params(), [4u8; 32]);
    let back = GameOffer::decode(&offer.encode()).expect("decode");
    assert_eq!(back, offer);
}

#[test]
fn a_blob_from_a_future_version_is_refused() {
    let mut offer = GameOffer::new(params(), [4u8; 32]);
    offer.v = OFFER_FORMAT + 1;
    let encoded = offer.encode();
    assert!(matches!(
        GameOffer::decode(&encoded),
        Err(InviteError::Version { .. })
    ));
}

#[test]
fn garbage_is_refused_rather_than_panicking() {
    assert!(Invite::decode("not base58 !!!").is_err());
    assert!(Invite::decode("").is_err());
}
```

- [ ] **Step 4: Run to verify it fails**

```bash
cargo test -p adjourn-cli --locked
```

Expected: FAIL — the crate has no `invite` module.

- [ ] **Step 5: Implement the codec**

Create `cli/src/invite.rs`:

```rust
//! The two blobs players copy-paste to agree on a game.
//!
//! Both must end up with byte-identical `GameParams` or they derive different
//! contract ids and sit on separate contracts, each seeing a game the other
//! never joins — with no error anywhere. The nonce therefore has exactly one
//! author, and the offer carries a contract id so a build mismatch is loud.

use adjourn_core::delegate_api::{KeyBytes, Side};
use adjourn_core::GameParams;
use serde::{Deserialize, Serialize};

pub const INVITE_FORMAT: u8 = 1;
pub const OFFER_FORMAT: u8 = 1;

#[derive(Debug, thiserror::Error)]
pub enum InviteError {
    #[error("not valid base58")]
    Base58,
    #[error("not a valid blob")]
    Malformed,
    #[error("blob is format {found}, this build speaks {expected}")]
    Version { found: u8, expected: u8 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    pub v: u8,
    pub side: Side,
    #[serde(with = "serde_bytes")]
    pub public_key: KeyBytes,
    #[serde(with = "serde_bytes")]
    pub nonce: [u8; 16],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameOffer {
    pub v: u8,
    pub params: GameParams,
    #[serde(with = "serde_bytes")]
    pub contract: [u8; 32],
}

fn encode_blob<T: Serialize>(value: &T) -> String {
    let mut buf = Vec::new();
    ciborium::into_writer(value, &mut buf).expect("cbor encode");
    bs58::encode(buf).into_string()
}

fn decode_blob<T: for<'de> Deserialize<'de>>(text: &str) -> Result<T, InviteError> {
    let bytes = bs58::decode(text.trim())
        .into_vec()
        .map_err(|_| InviteError::Base58)?;
    ciborium::from_reader(bytes.as_slice()).map_err(|_| InviteError::Malformed)
}

impl Invite {
    pub fn new(side: Side, public_key: KeyBytes, nonce: [u8; 16]) -> Self {
        Self { v: INVITE_FORMAT, side, public_key, nonce }
    }
    pub fn encode(&self) -> String {
        encode_blob(self)
    }
    pub fn decode(text: &str) -> Result<Self, InviteError> {
        let me: Self = decode_blob(text)?;
        if me.v != INVITE_FORMAT {
            return Err(InviteError::Version { found: me.v, expected: INVITE_FORMAT });
        }
        Ok(me)
    }
}

impl GameOffer {
    pub fn new(params: GameParams, contract: [u8; 32]) -> Self {
        Self { v: OFFER_FORMAT, params, contract }
    }
    pub fn encode(&self) -> String {
        encode_blob(self)
    }
    pub fn decode(text: &str) -> Result<Self, InviteError> {
        let me: Self = decode_blob(text)?;
        if me.v != OFFER_FORMAT {
            return Err(InviteError::Version { found: me.v, expected: OFFER_FORMAT });
        }
        Ok(me)
    }
}
```

Add `thiserror = "=2.0.20"` and `serde_bytes.workspace = true` to the CLI's dependencies and `thiserror` to `[workspace.dependencies]`.

Integration tests can only reach a library target, so the crate is both. Add to
`cli/Cargo.toml`:

```toml
[lib]
name = "adjourn_cli"
path = "src/lib.rs"
```

Create `cli/src/lib.rs`:

```rust
pub mod invite;
```

Create `cli/src/main.rs` (Task 9 fills it in):

```rust
fn main() {
    println!("adjourn: not yet wired up");
}
```

- [ ] **Step 6: Run to verify it passes**

```bash
cargo test -p adjourn-cli --locked
cargo fmt --all
cargo clippy -p adjourn-cli --all-targets --locked
```

Expected: PASS, 4 tests, no warnings.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock cli
git commit -m "feat(cli): crate scaffold and the invite/offer blob codec

The offer carries the responder's derived contract id so a build mismatch
between two players is loud: different adjourn-contract builds derive
different ids from identical params and would sit on separate contracts,
each seeing a game the other never joins."
```

---

### Task 4: `NodeClient` and the real WebSocket client

**Files:**
- Create: `cli/src/node.rs`
- Modify: `cli/src/lib.rs`

**Interfaces:**
- Produces: `trait NodeClient` with `get`, `put`, `update`, `delegate`; `struct WsClient` with `WsClient::connect(url: &str) -> anyhow::Result<Self>`; `fn contract_container(wasm: Vec<u8>, params: &GameParams) -> anyhow::Result<(ContractContainer, ContractInstanceId)>`; `fn delegate_container(wasm: Vec<u8>) -> (DelegateContainer, DelegateKey)`

- [ ] **Step 1: Write the module**

There is no unit test here — this code cannot run without a node, and a canned-response fake would test nothing. Its correctness is established by Task 8's live runbook. Create `cli/src/node.rs`:

```rust
//! The node seam.
//!
//! `WsClient` is the real thing. `FakeNode` (see `fake.rs`) is the other impl,
//! and it runs the real contract and delegate code so CI can exercise the
//! session flows without a Freenet node.

use adjourn_core::delegate_api::{Request, Response};
use adjourn_core::GameParams;
use anyhow::{anyhow, bail, Context};
use freenet_stdlib::client_api::{ClientRequest, ContractRequest, DelegateRequest, HostResponse, WebApi};
use freenet_stdlib::prelude::*;
use std::sync::Arc;

pub trait NodeClient {
    /// `Ok(None)` means the node does not have this contract.
    async fn get(&mut self, id: ContractInstanceId, subscribe: bool)
        -> anyhow::Result<Option<Vec<u8>>>;
    async fn put(&mut self, container: ContractContainer, state: Vec<u8>) -> anyhow::Result<()>;
    async fn update(&mut self, key: ContractKey, delta: Vec<u8>) -> anyhow::Result<()>;
    async fn delegate(&mut self, req: Request) -> anyhow::Result<Response>;
}

/// Build the contract container and its instance id from raw cargo WASM.
///
/// `fdev publish` wants a pre-packaged file, but the programmatic path takes
/// the raw module and applies the version wrapper itself — this is what River
/// does, and it is why `scripts/build-contract.sh` output is the right
/// artifact.
pub fn contract_container(
    wasm: Vec<u8>,
    params: &GameParams,
) -> anyhow::Result<(ContractContainer, ContractInstanceId)> {
    let mut param_bytes = Vec::new();
    ciborium::into_writer(params, &mut param_bytes).context("encode params")?;
    let parameters = Parameters::from(param_bytes);
    let code = ContractCode::from(wasm);
    let id = ContractInstanceId::from_params_and_code(&parameters, &code);
    let container = ContractContainer::from(ContractWasmAPIVersion::V1(WrappedContract::new(
        Arc::new(code),
        parameters,
    )));
    Ok((container, id))
}

/// The delegate key is a pure function of its code, so it is derived rather
/// than stored — nothing to keep in sync, and no stale cached key pointing at
/// a generation that is gone.
pub fn delegate_container(wasm: Vec<u8>) -> (DelegateContainer, DelegateKey) {
    let code = DelegateCode::from(wasm);
    let params = Parameters::from(Vec::<u8>::new());
    let delegate = Delegate::from((&code, &params));
    let key = delegate.key().clone();
    (
        DelegateContainer::Wasm(DelegateWasmAPIVersion::V1(delegate)),
        key,
    )
}

pub struct WsClient {
    api: WebApi,
    delegate_key: DelegateKey,
}

impl WsClient {
    pub async fn connect(url: &str, delegate_key: DelegateKey) -> anyhow::Result<Self> {
        let (stream, _) = tokio_tungstenite::connect_async(url)
            .await
            .with_context(|| format!("connecting to {url}"))?;
        Ok(Self { api: WebApi::start(stream), delegate_key })
    }

    pub async fn register_delegate(&mut self, container: DelegateContainer) -> anyhow::Result<()> {
        self.api
            .send(ClientRequest::DelegateOp(DelegateRequest::RegisterDelegate {
                delegate: container,
                cipher: [0u8; 32],
                nonce: [0u8; 24],
            }))
            .await?;
        // Default cipher and nonce are accepted in local mode only.
        match self.api.recv().await? {
            HostResponse::Ok | HostResponse::DelegateResponse { .. } => Ok(()),
            other => bail!("unexpected response to RegisterDelegate: {other:?}"),
        }
    }
}
```

Then the trait impl. The `ContractResponse` variant fields below are copied
from `freenet-stdlib-0.8.5/src/client_api/client_events.rs` — use them exactly:

```rust
impl NodeClient for WsClient {
    async fn get(
        &mut self,
        id: ContractInstanceId,
        subscribe: bool,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        self.api
            .send(ClientRequest::ContractOp(ContractRequest::Get {
                key: id,
                return_contract_code: false,
                subscribe,
            }))
            .await?;
        loop {
            match self.api.recv().await? {
                HostResponse::ContractResponse(ContractResponse::GetResponse {
                    state, ..
                }) => return Ok(Some(state.as_ref().to_vec())),
                HostResponse::ContractResponse(ContractResponse::NotFound { .. }) => {
                    return Ok(None)
                }
                // A subscribe ack or a stray notification can arrive first.
                HostResponse::ContractResponse(ContractResponse::SubscribeResponse { .. })
                | HostResponse::ContractResponse(ContractResponse::UpdateNotification { .. }) => {}
                other => bail!("unexpected response to Get: {other:?}"),
            }
        }
    }

    async fn put(
        &mut self,
        container: ContractContainer,
        state: Vec<u8>,
    ) -> anyhow::Result<()> {
        self.api
            .send(ClientRequest::ContractOp(ContractRequest::Put {
                contract: container,
                state: WrappedState::new(state),
                related_contracts: RelatedContracts::default(),
                subscribe: false,
                blocking_subscribe: false,
            }))
            .await?;
        loop {
            match self.api.recv().await? {
                HostResponse::ContractResponse(ContractResponse::PutResponse { .. }) => {
                    return Ok(())
                }
                HostResponse::ContractResponse(ContractResponse::UpdateNotification { .. }) => {}
                other => bail!("unexpected response to Put: {other:?}"),
            }
        }
    }

    async fn update(&mut self, key: ContractKey, delta: Vec<u8>) -> anyhow::Result<()> {
        self.api
            .send(ClientRequest::ContractOp(ContractRequest::Update {
                key,
                data: UpdateData::Delta(StateDelta::from(delta)),
            }))
            .await?;
        loop {
            match self.api.recv().await? {
                HostResponse::ContractResponse(ContractResponse::UpdateResponse { .. }) => {
                    return Ok(())
                }
                HostResponse::ContractResponse(ContractResponse::UpdateNotification { .. }) => {}
                other => bail!("unexpected response to Update: {other:?}"),
            }
        }
    }

    async fn delegate(&mut self, req: Request) -> anyhow::Result<Response> {
        self.api
            .send(ClientRequest::DelegateOp(DelegateRequest::ApplicationMessages {
                key: self.delegate_key.clone(),
                params: Parameters::from(Vec::<u8>::new()),
                inbound: vec![InboundDelegateMsg::ApplicationMessage(
                    ApplicationMessage::new(req.encode()),
                )],
            }))
            .await?;
        match self.api.recv().await? {
            HostResponse::DelegateResponse { values, .. } => {
                for msg in values {
                    if let OutboundDelegateMsg::ApplicationMessage(app) = msg {
                        return Response::decode(&app.payload)
                            .map_err(|e| anyhow!("delegate sent an undecodable reply: {e:?}"));
                    }
                }
                bail!("delegate returned no application message")
            }
            other => bail!("unexpected response to delegate call: {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Verify it compiles**

```bash
cargo clippy -p adjourn-cli --all-targets --locked
cargo fmt --all
```

Expected: no warnings. If `ContractResponse` variant fields differ from what you wrote, read them from
`~/.cargo/registry/src/*/freenet-stdlib-0.8.5/src/client_api/client_events.rs` and match them exactly — do not guess.

- [ ] **Step 3: Commit**

```bash
git add cli
git commit -m "feat(cli): NodeClient trait and the real WebSocket client

Containers are built from raw cargo WASM in code, the way River does it:
fdev publish wants a pre-packaged file, but the programmatic path applies the
version wrapper itself. The delegate key is derived from its code rather than
stored."
```

---

### Task 5: `FakeNode` — the real contract and delegate, in memory

**Files:**
- Create: `cli/src/fake.rs`
- Modify: `cli/src/lib.rs`

**Interfaces:**
- Consumes: `NodeClient` (Task 4), `MemoryStore` and `handle` (Task 2)
- Produces: `pub struct FakeNode` implementing `NodeClient`, with `FakeNode::new(shared: Arc<Mutex<BTreeMap<ContractInstanceId, Vec<u8>>>>) -> Self` so two fake clients can share one contract world

- [ ] **Step 1: Write the failing test**

Create `cli/tests/fake_node.rs`:

```rust
use adjourn_cli::fake::{shared_world, FakeNode};
use adjourn_cli::node::NodeClient;
use adjourn_core::delegate_api::{Request, Response};

#[tokio::test]
async fn the_fake_runs_the_real_delegate() {
    let mut node = FakeNode::new(shared_world());
    let resp = node
        .delegate(Request::CreateGameKey {
            label: "alice".into(),
            caller_entropy: Some([9u8; 32]),
        })
        .await
        .expect("delegate call");
    assert!(matches!(resp, Response::GameKey { .. }));
}

#[tokio::test]
async fn two_fakes_share_one_contract_world() {
    let world = shared_world();
    let mut a = FakeNode::new(world.clone());
    let mut b = FakeNode::new(world);

    // Both see the same contract once one of them puts it. Uses a throwaway
    // id and state; the point is the sharing, not the content.
    let id = freenet_stdlib::prelude::ContractInstanceId::new([1u8; 32]);
    a.put_raw(id, b"hello".to_vec());
    assert_eq!(b.get(id, false).await.unwrap().as_deref(), Some(&b"hello"[..]));
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test -p adjourn-cli --test fake_node --locked
```

Expected: FAIL — no `fake` module.

- [ ] **Step 3: Implement**

Create `cli/src/fake.rs`. It holds a `MemoryStore` per node (so two fakes have separate delegate secrets, exactly like two real nodes) and shares the contract world through an `Arc<Mutex<..>>`. `get`/`put`/`update` call the **real** `adjourn_contract::Contract` methods; `delegate` calls the **real** `adjourn_delegate::handle`.

```rust
use adjourn_core::delegate_api::{Request, Response};
use adjourn_delegate::secrets::MemoryStore;
use freenet_stdlib::prelude::*;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

pub type World = Arc<Mutex<BTreeMap<ContractInstanceId, (Parameters<'static>, Vec<u8>)>>>;

pub fn shared_world() -> World {
    Arc::new(Mutex::new(BTreeMap::new()))
}

pub struct FakeNode {
    world: World,
    store: MemoryStore,
}
```

`update` must go through `Contract::update_state` with `UpdateData::Delta`, and store the returned state — that is what makes the fake catch a real merge mistake. `get` returns the stored bytes or `None`. `delegate` calls
`adjourn_delegate::handle(&mut self.store, |id| self.world.lock().ok()?.get(&ContractInstanceId::new(*id)).map(|(_, s)| s.clone()), None, req)` — note the origin is `None`, which is exactly what a CLI gets. Add a `put_raw` helper for tests.

Add `tokio` with the `macros` and `rt` features to `[dev-dependencies]` for `#[tokio::test]`.

- [ ] **Step 4: Run to verify it passes**

```bash
cargo test -p adjourn-cli --locked
cargo clippy -p adjourn-cli --all-targets --locked
```

Expected: PASS, no warnings.

- [ ] **Step 5: Commit**

```bash
git add cli
git commit -m "feat(cli): FakeNode running the real contract and delegate in memory

Each fake has its own MemoryStore, mirroring two real nodes with separate
delegate secrets, and they share one contract world. update() goes through
Contract::update_state, so the fake catches a real merge mistake rather than
replaying canned responses."
```

---

### Task 6: Session setup — invite, accept, bind

**Files:**
- Create: `cli/src/session.rs`
- Modify: `cli/src/lib.rs`
- Test: `cli/tests/setup.rs`

**Interfaces:**
- Consumes: `NodeClient`, `FakeNode`, `Invite`, `GameOffer`
- Produces: `pub async fn invite_new<N: NodeClient>(node: &mut N, label: &str, side: Side) -> anyhow::Result<Invite>`; `pub async fn invite_accept<N: NodeClient>(node: &mut N, label: &str, invite: &Invite, contract_wasm: Vec<u8>) -> anyhow::Result<GameOffer>`; `pub async fn game_bind<N: NodeClient>(node: &mut N, label: &str, offer: &GameOffer, contract_wasm: Vec<u8>) -> anyhow::Result<ContractInstanceId>`

- [ ] **Step 1: Write the failing test**

Create `cli/tests/common/mod.rs` with the helper Tasks 6, 7 and 8 all use:

```rust
use std::path::PathBuf;

/// The contract WASM the CLI derives ids from. Produced by
/// `./scripts/build-contract.sh`; tests that need it skip loudly rather than
/// failing obscurely when it is absent.
pub fn contract_wasm() -> Option<Vec<u8>> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target/wasm32-unknown-unknown/release/adjourn_contract.wasm");
    std::fs::read(p).ok()
}
```

Create `cli/tests/setup.rs`:

```rust
mod common;

use adjourn_cli::fake::{shared_world, FakeNode};
use adjourn_cli::session::{game_bind, invite_accept, invite_new};
use adjourn_core::delegate_api::Side;
use freenet_stdlib::prelude::ContractInstanceId;

#[tokio::test]
async fn both_players_derive_the_same_contract() {
    let Some(wasm) = common::contract_wasm() else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };
    let world = shared_world();
    let mut alice = FakeNode::new(world.clone());
    let mut bob = FakeNode::new(world);

    let invite = invite_new(&mut alice, "alice", Side::White).await.unwrap();
    let offer = invite_accept(&mut bob, "bob", &invite, wasm.clone())
        .await
        .unwrap();
    let alice_id = game_bind(&mut alice, "alice", &offer, wasm).await.unwrap();

    assert_eq!(
        alice_id,
        ContractInstanceId::new(offer.contract),
        "the two sides derived different contracts; they would never meet"
    );
}

/// Two players on different adjourn-contract builds derive different ids from
/// identical params, and would sit on separate contracts each seeing a game
/// the other never joins -- with no error anywhere. Make it loud.
#[tokio::test]
async fn a_build_mismatch_is_refused_loudly() {
    let Some(wasm) = common::contract_wasm() else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };
    let world = shared_world();
    let mut alice = FakeNode::new(world.clone());
    let mut bob = FakeNode::new(world);

    let invite = invite_new(&mut alice, "alice", Side::White).await.unwrap();
    let mut offer = invite_accept(&mut bob, "bob", &invite, wasm.clone())
        .await
        .unwrap();
    offer.contract[0] ^= 0xff;

    let err = game_bind(&mut alice, "alice", &offer, wasm)
        .await
        .expect_err("a corrupted contract id must be refused");
    assert!(
        format!("{err:#}").to_lowercase().contains("build"),
        "the error must name a build mismatch, got: {err:#}"
    );
}
```

```rust
#[tokio::test]
async fn both_players_derive_the_same_contract() {
    let world = shared_world();
    let mut alice = FakeNode::new(world.clone());
    let mut bob = FakeNode::new(world);
    let wasm = contract_wasm();

    let invite = invite_new(&mut alice, "alice", Side::White).await.unwrap();
    let offer = invite_accept(&mut bob, "bob", &invite, wasm.clone()).await.unwrap();
    let alice_id = game_bind(&mut alice, "alice", &offer, wasm).await.unwrap();

    assert_eq!(alice_id, ContractInstanceId::new(offer.contract));
}

#[tokio::test]
async fn a_build_mismatch_is_refused_loudly() {
    // ...same setup, then:
    let mut bad = offer.clone();
    bad.contract[0] ^= 0xff;
    let err = game_bind(&mut alice, "alice", &bad, wasm).await.unwrap_err();
    assert!(format!("{err:#}").contains("build"), "got: {err:#}");
}
```

`contract_wasm()` reads `../target/wasm32-unknown-unknown/release/adjourn_contract.wasm`, and the test is `#[ignore]`d with a clear message if the file is absent, since it requires `./scripts/build-contract.sh` to have run.

- [ ] **Step 2: Run to verify it fails**

```bash
./scripts/build-contract.sh
cargo test -p adjourn-cli --test setup --locked
```

Expected: FAIL — no `session` module.

- [ ] **Step 3: Implement the three flows**

Per the spec's setup flow. `invite_new` calls `CreateGameKey` and wraps the returned public key with a nonce it authors. `invite_accept` calls `CreateGameKey`, builds `GameParams` (assigning colours from the invite's `side`), derives the contract id via `contract_container`, PUTs the contract with **empty** state, calls `BindGame`, and returns the offer. `game_bind` recomputes the id from its own WASM and **bails if it differs from `offer.contract`**, with a message naming a build mismatch; then GETs, PUTs if `None`, and calls `BindGame`.

- [ ] **Step 4: Run to verify it passes**

Expected: PASS, both tests.

- [ ] **Step 5: Commit**

```bash
git add cli
git commit -m "feat(cli): invite, accept and bind flows"
```

---

### Task 7: Move, resign, draw, and show

**Files:**
- Modify: `cli/src/session.rs`
- Test: `cli/tests/moves.rs`

**Interfaces:**
- Produces, all taking `node: &mut N, label: &str, ..., contract_wasm: Vec<u8>` and returning `anyhow::Result<Status>`: `show_label`, `play_move(.., uci: &str, ..)`, `resign`, `draw_offer`, `draw_accept`; plus the test-facing bypass `sign_move_at_ply(node, label, ply: u16, uci: &str, contract_wasm) -> anyhow::Result<Status>`, which skips the client's pre-checks so the delegate's guard is what a test exercises

- [ ] **Step 1: Write the failing tests**

Create `cli/tests/moves.rs`:

```rust
mod common;

use adjourn_cli::fake::{shared_world, FakeNode};
use adjourn_cli::session::{
    game_bind, invite_accept, invite_new, play_move, show_label, sign_move_at_ply,
};
use adjourn_core::delegate_api::Side;

async fn setup() -> Option<(FakeNode, FakeNode, Vec<u8>)> {
    let wasm = common::contract_wasm()?;
    let world = shared_world();
    let (mut alice, mut bob) = (FakeNode::new(world.clone()), FakeNode::new(world));
    let invite = invite_new(&mut alice, "alice", Side::White).await.unwrap();
    let offer = invite_accept(&mut bob, "bob", &invite, wasm.clone())
        .await
        .unwrap();
    game_bind(&mut alice, "alice", &offer, wasm.clone())
        .await
        .unwrap();
    Some((alice, bob, wasm))
}

#[tokio::test]
async fn a_move_is_visible_to_both_players() {
    let Some((mut alice, mut bob, wasm)) = setup().await else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };

    let st = play_move(&mut alice, "alice", "e2e4", wasm.clone())
        .await
        .unwrap();
    assert_eq!(st.ply, 1);

    let seen = show_label(&mut bob, "bob", wasm.clone()).await.unwrap();
    assert_eq!(seen.ply, 1, "black cannot see white's move");

    let st = play_move(&mut bob, "bob", "e7e5", wasm).await.unwrap();
    assert_eq!(st.ply, 2);
}

/// Caught locally, before the delegate is bothered: a good error beats a
/// refusal for something the client already knows.
#[tokio::test]
async fn moving_out_of_turn_fails_before_signing() {
    let Some((mut alice, _bob, wasm)) = setup().await else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };
    let err = play_move(&mut alice, "alice", "e7e5", wasm)
        .await
        .expect_err("that is not white's move");
    let text = format!("{err:#}").to_lowercase();
    assert!(text.contains("turn") || text.contains("legal"), "got: {err:#}");
}

/// The guarantee, through the whole stack: a second DIFFERENT move at a ply
/// already signed is refused by the DELEGATE, not by the client. Uses the
/// bypass helper so the client's own pre-checks cannot mask it.
#[tokio::test]
async fn a_double_sign_attempt_is_refused_by_the_delegate() {
    let Some((mut alice, _bob, wasm)) = setup().await else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };
    play_move(&mut alice, "alice", "e2e4", wasm.clone())
        .await
        .unwrap();

    let err = sign_move_at_ply(&mut alice, "alice", 1, "d2d4", wasm)
        .await
        .expect_err("the delegate must refuse a second move at ply 1");
    assert!(format!("{err:#}").contains("ply 1"), "got: {err:#}");
}
```

`sign_move_at_ply(node, label, ply, uci, wasm)` is a `pub` helper on `session`
that skips the local pre-checks and calls the delegate directly. It exists so
the delegate's guard is what is under test rather than the client's, and it is
documented as such.

- [ ] **Step 2: Run to verify they fail**

- [ ] **Step 3: Implement the move flow**

GET → decode → `project` → local pre-checks (`status.is_over()`, `status.turn` matches our side, `legal_moves` contains the uci) → `Sign` → wrap in `Delta` → `update` → GET again → return the new `Status`.

Local pre-checks produce good errors; they are **not** the guarantee. Say so in a comment.

- [ ] **Step 4: Run to verify they pass**

- [ ] **Step 5: Commit**

```bash
git add cli
git commit -m "feat(cli): move, resign, draw and show flows"
```

---

### Task 8: The full-game test and the live runbook

**Files:**
- Create: `cli/tests/full_game.rs`, `docs/runbook-two-nodes.md`

- [ ] **Step 1: Write the full-game test**

Two `FakeNode`s, one world. Setup, then play Scholar's Mate move by move alternating nodes, asserting after each that both project the same ply. At the end assert both project `Reason::Checkmate` with `winner: Some(Color::White)`, and that both encoded states are byte-identical.

- [ ] **Step 2: Run it**

```bash
cargo test -p adjourn-cli --locked
```

Expected: PASS.

- [ ] **Step 3: Write the runbook**

`docs/runbook-two-nodes.md`: how to start two `freenet local` nodes on ports 7509/7510 with separate `--data-dir`s, `adjourn init` against each, and the full copy-paste sequence through to mate. Include the exact commands. Note that the node does not survive a shell that exits, and that `freenet service` exists for keeping one running.

- [ ] **Step 4: Commit**

```bash
git add cli docs
git commit -m "test(cli): full game against FakeNode, plus the two-node runbook"
```

---

### Task 9: Commands, output and exit codes

**Files:**
- Modify: `cli/src/main.rs`
- Create: `cli/src/output.rs`

- [ ] **Step 1: Implement the command surface**

clap `derive` for the surface in the spec. Global `--node` (default
`ws://127.0.0.1:7509/v1/contract/command?encodingProtocol=native`),
`--contract-wasm`, `--delegate-wasm` with the documented defaults.

**`watch` is out of scope for this plan.** The spec lists it, but it needs a
streaming method `NodeClient` does not have: holding a subscription open and
yielding `ContractResponse::UpdateNotification` until interrupted. That is a
fifth trait method with a fundamentally different shape (a stream, not a
request/response) plus a `FakeNode` able to push notifications, and it earns
its own task later. Implement every other command, and have `watch` exit
non-zero with "not implemented yet; poll with `adjourn show`" rather than a
stub that silently does nothing. Record it in the README as the one command
still missing.

- [ ] **Step 2: Render refusals as sentences**

`cli/src/output.rs` maps every `Refusal` to a human sentence. `PlyAlreadySigned { ply }` must read as "you have already signed a different move at ply {ply}; re-send the identical move, or wait for your opponent" — that is the one a user hits through legitimate retry, and a debug dump there is useless.

Exit codes: `0` success, `1` refusal or precondition failure, `2` usage, `3` transport.

- [ ] **Step 3: Verify**

```bash
cargo run -p adjourn-cli -- --help
cargo clippy -p adjourn-cli --all-targets --locked
cargo fmt --all --check
```

- [ ] **Step 4: Commit**

```bash
git add cli
git commit -m "feat(cli): command surface, refusal rendering and exit codes"
```

---

### Task 10: CI, docs, and the Task 9 answers

**Files:**
- Modify: `.github/workflows/ci.yml`, `CLAUDE.md`, `README.md`

- [ ] **Step 1: Extend CI**

The `test` job already runs `cargo test --workspace --locked`, which now includes the CLI. Add a clippy line for the new crate if the existing one is not already `--workspace`. Confirm the `algebra-standalone` job's "no Freenet dependency" assertion still passes — it checks `adjourn-core`, which is untouched.

- [ ] **Step 2: Document**

`CLAUDE.md`: add `cli/` to the crate table; record that the delegate's handlers are now host-testable via `SecretStore`; record the origin semantics change and why (a CLI has no `MessageOrigin`, and for CLI-bound games the boundary is the node's loopback-only WS API).

`README.md`: mark roadmap item 3a/3b done, link the runbook.

- [ ] **Step 3: Run the live runbook and record the Task 9 answers**

Against two real nodes, run the runbook to at least the first move. Record in `CLAUDE.md` under "Runtime assumptions, verified": the date, node version, whether `adjourn key new` reported `HostBacked` or `Degraded` (answering whether `freenet_rand` is provided), and whether binding succeeded without an origin (confirming `MessageOrigin` is `None` for a CLI). These are exactly the facts that get re-derived expensively later.

If the runbook cannot be completed, record what failed and where — a recorded failure is worth more than a silent gap.

- [ ] **Step 4: Commit**

```bash
git add .github CLAUDE.md README.md
git commit -m "docs(cli): CI, crate docs, and the verified runtime assumptions"
```
