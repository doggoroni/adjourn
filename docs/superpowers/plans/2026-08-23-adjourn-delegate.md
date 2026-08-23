# adjourn-delegate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the per-game ed25519 signing key into a Freenet delegate so the UI never sees it, and so a compromised UI cannot forfeit the game by double-signing a ply.

**Architecture:** Three layers. `common/src/delegate_api.rs` holds plain serde wire types. `common/src/delegate_policy.rs` holds pure decision functions — no I/O, no clock, no randomness — which is where every interesting rule lives and where every meaningful test runs. `delegates/adjourn-delegate/` is a thin adapter doing secret-store I/O, host entropy, and message dispatch. This mirrors the `common` / `contracts` split already working in this repo, and exists because the delegate crate cannot be compiled on a Windows host at all.

**Tech Stack:** Rust 1.97.1, `freenet-stdlib` 0.8.5, `ed25519-dalek` 2.2.0 (verify + deterministic sign only), `sha2`, `ciborium`, `shakmaty` 0.30.1.

**Spec:** `docs/superpowers/specs/2026-08-20-adjourn-delegate-design.md`

## Global Constraints

Every task's requirements implicitly include these.

- **No `rand`, `getrandom`, or `rand_core` in the delegate dependency graph**, directly or transitively. wasmtime has no getrandom backend on `wasm32-unknown-unknown`; those crates emit wasm-bindgen placeholder imports that cannot be resolved, and the delegate fails to instantiate (freenet/river#241).
- **Never call `SigningKey::generate()`.** Keys are always built with `SigningKey::from_bytes(&seed)` from a seed derived by `delegate_policy::derive_seed`.
- **`adjourn-core` must stay free of Freenet dependencies.** CI asserts this. `delegate_api` and `delegate_policy` use only `serde`, `sha2`, `ciborium`, `shakmaty`.
- **Exact `=` version pins**, `Cargo.lock` committed, `--locked` on every build. The delegate key is `BLAKE3(BLAKE3(wasm) ‖ params)`; a drifting dependency rotates it.
- **`InboundDelegateMsg` is `#[non_exhaustive]`.** The catch-all arm returns `Err(DelegateError::Other(..))` — never `unreachable!()`, never a panic. A panic inside delegate WASM kills the runtime for the delegate and surfaces as an opaque execution error.
- **Refusals are `Response::Refused`, not `DelegateError`.** A refusal is an expected outcome the UI must render. `DelegateError` is only for malformed input and store failures.
- **`BTreeMap`, never `HashMap`**, in anything serialized.

## File Structure

| File | Responsibility |
|---|---|
| `common/src/delegate_api.rs` | Wire types: `Side`, `Request`, `Response`, `Refusal`, `EntropyQuality`, `GameSummary`. CBOR codec. |
| `common/src/delegate_policy.rs` | `GameRecord`, `decide_bind`, `decide_sign`, entropy derivation. Pure. |
| `common/src/lib.rs` | Add the two modules. |
| `common/tests/delegate_policy.rs` | Every rule that matters. Runs on all platforms. |
| `delegates/adjourn-delegate/Cargo.toml` | Crate config, `freenet-main-delegate` feature, `cdylib`+`rlib`. |
| `delegates/adjourn-delegate/src/secrets.rs` | Secret-store key naming and typed load/store helpers. |
| `delegates/adjourn-delegate/src/lib.rs` | `#[delegate]` impl, dispatch, the four handlers. |
| `delegates/adjourn-delegate/tests/adapter.rs` | Dispatch and store round-trips. CI-only. |
| `scripts/build-delegate.sh` | Canonical reproducible build. |
| `Cargo.toml`, `.github/workflows/ci.yml`, `CLAUDE.md`, `README.md` | Wiring and docs. |

**Store layout** (one addition to the spec, found while planning: the spec keyed games by `game_id` only, but `decide_bind` needs to find an existing record *by label*, so a label→game_id index is required):

```
chess/key/<label>     -> 32 raw signing-key bytes
chess/bind/<label>    -> 32-byte game_id
chess/game/<game_id>  -> CBOR(GameRecord)
```

---

### Task 1: Repository setup and wire types

This repo is not yet under version control, which every later "commit" step needs. Fold that in here.

**Files:**
- Create: `.git` (via `git init`)
- Create: `common/src/delegate_api.rs`
- Modify: `common/src/lib.rs`
- Test: `common/tests/delegate_policy.rs`

**Interfaces:**
- Consumes: `adjourn_core::types::{Body, GameParams, KeyBytes, Record}`
- Produces: `Side`, `GameId`, `Request`, `Response`, `EntropyQuality`, `GameSummary`, `Refusal`, and `Request::{encode,decode}` / `Response::{encode,decode}`

- [ ] **Step 1: Initialise the repository**

```bash
git init
git add -A
git commit -m "chore: initial commit — algebra, contract, reproducible builds"
```

- [ ] **Step 2: Write the failing test**

Create `common/tests/delegate_policy.rs`:

```rust
use adjourn_core::delegate_api::{Refusal, Request, Response, Side};
use adjourn_core::Body;

#[test]
fn requests_round_trip_through_cbor() {
    let req = Request::Sign {
        game_id: [7u8; 32],
        body: Body::Move {
            ply: 3,
            parent: [9u8; 32],
            uci: "e2e4".into(),
        },
    };
    let back = Request::decode(&req.encode()).expect("decode");
    assert_eq!(back, req);
}

#[test]
fn refusals_round_trip_through_cbor() {
    let resp = Response::Refused(Refusal::WrongSide {
        ours: Side::White,
        ply_needs: Side::Black,
    });
    let back = Response::decode(&resp.encode()).expect("decode");
    assert_eq!(back, resp);
}

#[test]
fn malformed_bytes_decode_to_a_refusal_not_a_panic() {
    assert!(matches!(
        Request::decode(&[0xff, 0xff, 0xff]),
        Err(Refusal::Malformed(_))
    ));
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p adjourn-core --test delegate_policy --locked`
Expected: FAIL — `unresolved import adjourn_core::delegate_api`

- [ ] **Step 4: Write the wire types**

Create `common/src/delegate_api.rs`:

```rust
//! Wire types for the UI <-> delegate protocol.
//!
//! No Freenet dependencies: plain serde types, so the policy layer and its
//! tests build standalone. Deliberately does NOT serialize `shakmaty::Color`
//! (which would need shakmaty's `serde` feature). `Side` is ours, which also
//! keeps the wire format stable across a shakmaty bump.

use crate::types::{Body, GameParams, KeyBytes, Record};
use serde::{Deserialize, Serialize};
use shakmaty::Color;

/// `GameParams::game_id()`.
pub type GameId = [u8; 32];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    White,
    Black,
}

impl From<Color> for Side {
    fn from(c: Color) -> Self {
        match c {
            Color::White => Side::White,
            Color::Black => Side::Black,
        }
    }
}

impl From<Side> for Color {
    fn from(s: Side) -> Self {
        match s {
            Side::White => Color::White,
            Side::Black => Color::Black,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// Create a signing key for a game that does not exist yet. `GameParams`
    /// needs BOTH players' public keys, so your key must exist before the game
    /// does; this returns the half you exchange out of band.
    CreateGameKey {
        label: String,
        caller_entropy: Option<[u8; 32]>,
    },
    /// Record which game the key for `label` belongs to, once both halves are
    /// known.
    ///
    /// `contract` is the game contract's instance id. The delegate cannot
    /// derive it (it is `hash(code, params)`, and the delegate does not have
    /// the contract code), and it is NOT the same as `params.game_id()`. The
    /// UI knows it because it computed that key to PUT the contract. Without
    /// it the delegate has no way to read the game's local state.
    BindGame {
        label: String,
        params: GameParams,
        contract: [u8; 32],
    },
    Sign { game_id: GameId, body: Body },
    ListGames,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    GameKey {
        label: String,
        public_key: KeyBytes,
        entropy: EntropyQuality,
    },
    Bound {
        game_id: GameId,
    },
    Signed {
        record: Record,
    },
    Games(Vec<GameSummary>),
    Refused(Refusal),
}

/// Whether a key was generated with host-backed randomness. Returned so the UI
/// can warn once, rather than the system quietly pretending all keys are equal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntropyQuality {
    HostBacked,
    Degraded,
}

/// Never contains secrets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameSummary {
    pub label: String,
    pub public_key: KeyBytes,
    /// `None` until bound.
    pub game_id: Option<GameId>,
    /// `None` until bound.
    pub side: Option<Side>,
    /// 0 means nothing signed yet; plies are 1-indexed.
    pub last_signed_ply: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
    UnknownLabel,
    LabelExists,
    UnknownGame,
    AlreadyBound { game_id: GameId },
    KeyNotInParams,
    WrongSide { ours: Side, ply_needs: Side },
    PlyAlreadySigned { ply: u16 },
    MissingOrigin,
    ForeignOrigin,
    NoEntropy,
    Malformed(String),
}

fn encode_cbor<T: Serialize>(value: &T) -> Vec<u8> {
    let mut buf = Vec::new();
    ciborium::into_writer(value, &mut buf).expect("cbor encode");
    buf
}

fn decode_cbor<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, Refusal> {
    ciborium::from_reader(bytes).map_err(|e| Refusal::Malformed(e.to_string()))
}

impl Request {
    pub fn encode(&self) -> Vec<u8> {
        encode_cbor(self)
    }
    pub fn decode(bytes: &[u8]) -> Result<Request, Refusal> {
        decode_cbor(bytes)
    }
}

impl Response {
    pub fn encode(&self) -> Vec<u8> {
        encode_cbor(self)
    }
    pub fn decode(bytes: &[u8]) -> Result<Response, Refusal> {
        decode_cbor(bytes)
    }
}
```

- [ ] **Step 5: Wire the module in**

In `common/src/lib.rs`, add after the existing `pub mod` lines:

```rust
pub mod delegate_api;
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p adjourn-core --test delegate_policy --locked`
Expected: PASS, 3 tests

- [ ] **Step 7: Commit**

```bash
git add common/src/delegate_api.rs common/src/lib.rs common/tests/delegate_policy.rs
git commit -m "feat(delegate): wire types for the UI <-> delegate protocol"
```

---

### Task 2: Entropy derivation

The security-critical piece. `freenet_stdlib::rand::rand_bytes` reads into a zero-initialised buffer through a host import that is a **no-op stub off-wasm**, so it silently returns 32 zero bytes there — and `SigningKey::from_bytes(&[0u8; 32])` is a valid, entirely known private key. This task makes that condition detectable and fails closed on it.

**Files:**
- Create: `common/src/delegate_policy.rs`
- Modify: `common/src/lib.rs`
- Test: `common/tests/delegate_policy.rs`

**Interfaces:**
- Consumes: `Refusal`, `EntropyQuality` from Task 1
- Produces: `HostEntropy::{Live, Dead}`, `classify_host_entropy(first: [u8;32], second: [u8;32]) -> HostEntropy`, `derive_seed(host: HostEntropy, caller: Option<[u8;32]>, label: &str) -> Result<([u8;32], EntropyQuality), Refusal>`

- [ ] **Step 1: Write the failing tests**

Append to `common/tests/delegate_policy.rs`:

```rust
use adjourn_core::delegate_api::EntropyQuality;
use adjourn_core::delegate_policy::{classify_host_entropy, derive_seed, HostEntropy};

#[test]
fn all_zero_host_entropy_is_dead() {
    // This is exactly what the off-wasm stub returns.
    assert!(matches!(
        classify_host_entropy([0u8; 32], [0u8; 32]),
        HostEntropy::Dead
    ));
}

#[test]
fn two_identical_draws_mean_the_source_is_dead() {
    // A live CSPRNG repeats 32 bytes with negligible probability.
    assert!(matches!(
        classify_host_entropy([5u8; 32], [5u8; 32]),
        HostEntropy::Dead
    ));
}

#[test]
fn two_different_draws_are_live() {
    let mut second = [5u8; 32];
    second[0] = 6;
    assert!(matches!(
        classify_host_entropy([5u8; 32], second),
        HostEntropy::Live(_)
    ));
}

#[test]
fn dead_host_and_no_caller_entropy_fails_closed() {
    assert_eq!(
        derive_seed(HostEntropy::Dead, None, "g1").unwrap_err(),
        Refusal::NoEntropy
    );
}

#[test]
fn all_zero_caller_entropy_counts_as_absent() {
    assert_eq!(
        derive_seed(HostEntropy::Dead, Some([0u8; 32]), "g1").unwrap_err(),
        Refusal::NoEntropy
    );
}

#[test]
fn dead_host_with_caller_entropy_is_degraded_not_fatal() {
    let (seed, quality) = derive_seed(HostEntropy::Dead, Some([1u8; 32]), "g1").expect("seed");
    assert_eq!(quality, EntropyQuality::Degraded);
    assert_ne!(seed, [0u8; 32]);
}

#[test]
fn live_host_is_host_backed() {
    let (_, quality) =
        derive_seed(HostEntropy::Live([2u8; 32]), None, "g1").expect("seed");
    assert_eq!(quality, EntropyQuality::HostBacked);
}

#[test]
fn seeds_are_deterministic_and_label_separated() {
    let a = derive_seed(HostEntropy::Live([2u8; 32]), Some([1u8; 32]), "g1").unwrap();
    let b = derive_seed(HostEntropy::Live([2u8; 32]), Some([1u8; 32]), "g1").unwrap();
    let c = derive_seed(HostEntropy::Live([2u8; 32]), Some([1u8; 32]), "g2").unwrap();
    assert_eq!(a.0, b.0, "derivation must be deterministic in its inputs");
    assert_ne!(a.0, c.0, "a different label must give a different key");
}

#[test]
fn caller_entropy_changes_the_seed_even_with_the_same_host_draw() {
    let a = derive_seed(HostEntropy::Live([2u8; 32]), Some([1u8; 32]), "g1").unwrap();
    let b = derive_seed(HostEntropy::Live([2u8; 32]), Some([9u8; 32]), "g1").unwrap();
    assert_ne!(a.0, b.0, "caller entropy must be mixed in, not ignored");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p adjourn-core --test delegate_policy --locked`
Expected: FAIL — `unresolved import adjourn_core::delegate_policy`

- [ ] **Step 3: Write the implementation**

Create `common/src/delegate_policy.rs`:

```rust
//! The delegate's decision functions.
//!
//! Pure: no I/O, no clock, no randomness. Everything the delegate decides is
//! decided here, so it can be tested on any platform — the delegate crate
//! itself cannot even be compiled on a Windows host.

use crate::delegate_api::{EntropyQuality, Refusal};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const DOMAIN_KEYGEN: &[u8] = b"freenet-chess-v1/keygen";

/// The result of probing the host RNG.
///
/// `freenet_stdlib::rand::rand_bytes` reads into a zero-initialised buffer via
/// a host import that is a no-op stub off-wasm, so it returns all zeros there
/// with no error. Treating that as entropy would mint a known private key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostEntropy {
    Live([u8; 32]),
    Dead,
}

/// Classify two independent draws from the host RNG.
///
/// All-zeros catches the off-wasm stub. Two identical draws catch a dead or
/// missing host source generally — a live CSPRNG repeats 32 bytes with
/// negligible probability.
pub fn classify_host_entropy(first: [u8; 32], second: [u8; 32]) -> HostEntropy {
    if first == [0u8; 32] || first == second {
        HostEntropy::Dead
    } else {
        HostEntropy::Live(first)
    }
}

/// Mix available entropy sources into a signing-key seed.
///
/// Mixing never loses: the result is at least as unpredictable as the
/// strongest input. Host entropy is the only source the UI does not control,
/// so it alone gives "the UI cannot learn the key at generation time"; caller
/// entropy still gives "the UI cannot learn it afterwards". With neither, this
/// fails closed rather than producing a guessable key.
pub fn derive_seed(
    host: HostEntropy,
    caller: Option<[u8; 32]>,
    label: &str,
) -> Result<([u8; 32], EntropyQuality), Refusal> {
    // A caller sending zeros is not contributing entropy, whatever it thinks.
    let caller = caller.filter(|c| c != &[0u8; 32]);

    let (host_bytes, quality) = match host {
        HostEntropy::Live(h) => (h, EntropyQuality::HostBacked),
        HostEntropy::Dead => {
            if caller.is_none() {
                return Err(Refusal::NoEntropy);
            }
            ([0u8; 32], EntropyQuality::Degraded)
        }
    };

    let mut h = Sha256::new();
    h.update(DOMAIN_KEYGEN);
    h.update(host_bytes);
    h.update(caller.unwrap_or([0u8; 32]));
    h.update((label.len() as u32).to_le_bytes());
    h.update(label.as_bytes());
    Ok((h.finalize().into(), quality))
}
```

- [ ] **Step 4: Wire the module in**

In `common/src/lib.rs`:

```rust
pub mod delegate_policy;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p adjourn-core --test delegate_policy --locked`
Expected: PASS, 12 tests

- [ ] **Step 6: Commit**

```bash
git add common/src/delegate_policy.rs common/src/lib.rs common/tests/delegate_policy.rs
git commit -m "feat(delegate): entropy derivation that fails closed on a dead host RNG"
```

---

### Task 3: GameRecord and decide_bind

**Files:**
- Modify: `common/src/delegate_policy.rs`
- Test: `common/tests/delegate_policy.rs`

**Interfaces:**
- Consumes: `Side`, `Refusal`, `GameId` from Task 1
- Produces: `GameRecord { label, params, side, origin, contract, last_signed_ply, last_move_body_hash }`, `BindDecision::{Bind, Refuse}`, `decide_bind(existing: Option<&GameRecord>, label: &str, public_key: KeyBytes, params: &GameParams, contract: [u8;32], origin: Option<[u8;32]>) -> BindDecision`, `body_hash(body: &Body) -> [u8;32]`

- [ ] **Step 1: Write the failing tests**

Append to `common/tests/delegate_policy.rs`:

```rust
use adjourn_core::delegate_policy::{decide_bind, BindDecision, GameRecord};
use adjourn_core::GameParams;
use ed25519_dalek::SigningKey;

const ORIGIN: [u8; 32] = [3u8; 32];
const OTHER_ORIGIN: [u8; 32] = [4u8; 32];
const CONTRACT: [u8; 32] = [5u8; 32];

fn game() -> (SigningKey, SigningKey, GameParams) {
    let w = SigningKey::from_bytes(&[1u8; 32]);
    let b = SigningKey::from_bytes(&[2u8; 32]);
    let params = GameParams {
        white: w.verifying_key().to_bytes(),
        black: b.verifying_key().to_bytes(),
        nonce: [7u8; 16],
    };
    (w, b, params)
}

#[test]
fn binding_without_an_origin_is_refused() {
    let (w, _b, params) = game();
    assert!(matches!(
        decide_bind(None, "g1", w.verifying_key().to_bytes(), &params, CONTRACT, None),
        BindDecision::Refuse(Refusal::MissingOrigin)
    ));
}

#[test]
fn binding_a_key_that_is_not_a_player_is_refused() {
    let (_w, _b, params) = game();
    let stranger = SigningKey::from_bytes(&[9u8; 32]);
    assert!(matches!(
        decide_bind(
            None,
            "g1",
            stranger.verifying_key().to_bytes(),
            &params,
            CONTRACT,
            Some(ORIGIN)
        ),
        BindDecision::Refuse(Refusal::KeyNotInParams)
    ));
}

#[test]
fn binding_records_the_side_and_starts_the_ply_counter_at_zero() {
    let (w, _b, params) = game();
    let BindDecision::Bind { record } =
        decide_bind(
        None,
        "g1",
        w.verifying_key().to_bytes(),
        &params,
        CONTRACT,
        Some(ORIGIN),
    )
    else {
        panic!("expected a bind");
    };
    assert_eq!(record.side, Side::White);
    assert_eq!(record.origin, ORIGIN);
    assert_eq!(record.last_signed_ply, 0);
    assert_eq!(record.label, "g1");
}

#[test]
fn rebinding_a_label_to_a_different_game_is_refused() {
    // Rebinding would orphan the ply counter, which is the whole protection.
    let (w, _b, params) = game();
    let BindDecision::Bind { record } =
        decide_bind(
        None,
        "g1",
        w.verifying_key().to_bytes(),
        &params,
        CONTRACT,
        Some(ORIGIN),
    )
    else {
        panic!("expected a bind");
    };

    let mut other = params.clone();
    other.nonce = [8u8; 16]; // a different game between the same two players
    assert!(matches!(
        decide_bind(
            Some(&record),
            "g1",
            w.verifying_key().to_bytes(),
            &other,
            CONTRACT,
            Some(ORIGIN)
        ),
        BindDecision::Refuse(Refusal::AlreadyBound { .. })
    ));
}

#[test]
fn rebinding_the_same_label_to_the_same_game_is_idempotent() {
    // A dropped response must not wedge setup.
    let (w, _b, params) = game();
    let BindDecision::Bind { record } =
        decide_bind(
        None,
        "g1",
        w.verifying_key().to_bytes(),
        &params,
        CONTRACT,
        Some(ORIGIN),
    )
    else {
        panic!("expected a bind");
    };
    let BindDecision::Bind { record: again } = decide_bind(
        Some(&record),
        "g1",
        w.verifying_key().to_bytes(),
        &params,
        CONTRACT,
        Some(ORIGIN),
    ) else {
        panic!("expected an idempotent re-bind");
    };
    assert_eq!(record, again);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p adjourn-core --test delegate_policy --locked`
Expected: FAIL — `cannot find function decide_bind`

- [ ] **Step 3: Write the implementation**

Append to `common/src/delegate_policy.rs`:

```rust
use crate::delegate_api::{GameId, Side};
use crate::types::{Body, GameParams, KeyBytes};

const DOMAIN_BODY: &[u8] = b"freenet-chess-v1/delegate-body";

/// What the delegate knows about one game. Persisted in the secret store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameRecord {
    pub label: String,
    pub params: GameParams,
    pub side: Side,
    /// Contract instance id of the WEB APP that bound this game. Only that app
    /// may ask for signatures on it. Note this is the app's own contract, not
    /// the game's.
    pub origin: [u8; 32],
    /// Contract instance id of the GAME, supplied at bind time. Used only to
    /// read local state for the best-effort legality check.
    pub contract: [u8; 32],
    /// Highest ply signed so far. 0 means none; plies are 1-indexed.
    pub last_signed_ply: u16,
    /// Body hash of the move signed at `last_signed_ply`, so an identical
    /// retry can be told apart from a different move at the same ply.
    pub last_move_body_hash: [u8; 32],
}

impl GameRecord {
    pub fn game_id(&self) -> GameId {
        self.params.game_id()
    }
}

/// Domain-separated hash of a body, used only to recognise an identical retry.
pub fn body_hash(body: &Body) -> [u8; 32] {
    let mut buf = Vec::new();
    ciborium::into_writer(body, &mut buf).expect("cbor encode");
    let mut h = Sha256::new();
    h.update(DOMAIN_BODY);
    h.update(&buf);
    h.finalize().into()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindDecision {
    Bind { record: GameRecord },
    Refuse(Refusal),
}

/// Associate the key held for `label` with a game.
///
/// `existing` is the record currently bound to `label`, if any.
pub fn decide_bind(
    existing: Option<&GameRecord>,
    label: &str,
    public_key: KeyBytes,
    params: &GameParams,
    contract: [u8; 32],
    origin: Option<[u8; 32]>,
) -> BindDecision {
    let Some(origin) = origin else {
        return BindDecision::Refuse(Refusal::MissingOrigin);
    };
    // Catches a UI pairing the wrong key with the wrong game.
    let Some(color) = params.color_of(&public_key) else {
        return BindDecision::Refuse(Refusal::KeyNotInParams);
    };

    if let Some(existing) = existing {
        if existing.game_id() != params.game_id() {
            // Rebinding would orphan the ply counter and reopen the
            // double-sign hole this delegate exists to close.
            return BindDecision::Refuse(Refusal::AlreadyBound {
                game_id: existing.game_id(),
            });
        }
        // Same label, same game: idempotent, for dropped responses.
        return BindDecision::Bind {
            record: existing.clone(),
        };
    }

    BindDecision::Bind {
        record: GameRecord {
            label: label.to_string(),
            params: params.clone(),
            side: color.into(),
            origin,
            contract,
            last_signed_ply: 0,
            last_move_body_hash: [0u8; 32],
        },
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p adjourn-core --test delegate_policy --locked`
Expected: PASS, 17 tests

- [ ] **Step 5: Commit**

```bash
git add common/src/delegate_policy.rs common/tests/delegate_policy.rs
git commit -m "feat(delegate): GameRecord and binding rules"
```

---

### Task 4: decide_sign — the rule the delegate exists for

**Files:**
- Modify: `common/src/delegate_policy.rs`
- Test: `common/tests/delegate_policy.rs`

**Interfaces:**
- Consumes: `GameRecord`, `body_hash` from Task 3; `color_at_ply` from `adjourn_core::types`
- Produces: `SignDecision::{Sign, Refuse}`, `decide_sign(record: &GameRecord, body: &Body, origin: Option<[u8;32]>) -> SignDecision`

- [ ] **Step 1: Write the failing tests**

Append to `common/tests/delegate_policy.rs`:

```rust
use adjourn_core::delegate_policy::{decide_sign, SignDecision};
use adjourn_core::Body;

fn white_record() -> GameRecord {
    let (w, _b, params) = game();
    let BindDecision::Bind { record } =
        decide_bind(
        None,
        "g1",
        w.verifying_key().to_bytes(),
        &params,
        CONTRACT,
        Some(ORIGIN),
    )
    else {
        panic!("expected a bind");
    };
    record
}

fn mv(ply: u16, uci: &str) -> Body {
    Body::Move {
        ply,
        parent: [9u8; 32],
        uci: uci.into(),
    }
}

fn sign(record: &GameRecord, body: &Body) -> GameRecord {
    match decide_sign(record, body, Some(ORIGIN)) {
        SignDecision::Sign { updated } => updated,
        other => panic!("expected a signature, got {other:?}"),
    }
}

#[test]
fn a_second_different_move_at_a_signed_ply_is_refused() {
    // The one self-inflicted loss in the protocol, made unreachable.
    let record = sign(&white_record(), &mv(1, "e2e4"));
    assert_eq!(record.last_signed_ply, 1);

    assert!(matches!(
        decide_sign(&record, &mv(1, "d2d4"), Some(ORIGIN)),
        SignDecision::Refuse(Refusal::PlyAlreadySigned { ply: 1 })
    ));
}

#[test]
fn an_identical_move_at_a_signed_ply_is_signed_again() {
    // A dropped response must not wedge the game. ed25519 signing is
    // deterministic, so the record the UI gets back is byte-identical.
    let record = sign(&white_record(), &mv(1, "e2e4"));
    let again = sign(&record, &mv(1, "e2e4"));
    assert_eq!(record, again, "an identical retry must not change state");
}

#[test]
fn signing_one_body_twice_produces_byte_identical_records() {
    // The whole retry story rests on this: ed25519-dalek signing is
    // deterministic, so re-signing an identical body returns the same record
    // and the peer sees no new state. If this ever stopped holding, an
    // idempotent retry would start splitting into two records.
    use adjourn_core::Record;
    let (w, _b, params) = game();
    let body = mv(1, "e2e4");
    let a = Record::sign(&w, &params, body.clone());
    let b = Record::sign(&w, &params, body);
    assert_eq!(a, b);
}

#[test]
fn a_move_at_a_lower_ply_than_one_already_signed_is_refused() {
    let mut record = sign(&white_record(), &mv(1, "e2e4"));
    record = sign(&record, &mv(3, "g1f3"));
    assert!(matches!(
        decide_sign(&record, &mv(1, "e2e4"), Some(ORIGIN)),
        SignDecision::Refuse(Refusal::PlyAlreadySigned { ply: 1 })
    ));
}

#[test]
fn signing_for_the_wrong_side_is_refused() {
    // Ply 2 is Black's; this record holds White's key.
    assert!(matches!(
        decide_sign(&white_record(), &mv(2, "e7e5"), Some(ORIGIN)),
        SignDecision::Refuse(Refusal::WrongSide {
            ours: Side::White,
            ply_needs: Side::Black
        })
    ));
}

#[test]
fn a_foreign_origin_is_refused() {
    assert!(matches!(
        decide_sign(&white_record(), &mv(1, "e2e4"), Some(OTHER_ORIGIN)),
        SignDecision::Refuse(Refusal::ForeignOrigin)
    ));
}

#[test]
fn a_missing_origin_is_refused() {
    assert!(matches!(
        decide_sign(&white_record(), &mv(1, "e2e4"), None),
        SignDecision::Refuse(Refusal::MissingOrigin)
    ));
}

#[test]
fn resign_and_draw_bodies_sign_without_touching_the_ply_counter() {
    // These are idempotent by record id (INVARIANT 2), so there is nothing to
    // guard: signing the same statement twice collapses to one slot on merge.
    let record = sign(&white_record(), &mv(1, "e2e4"));
    for body in [
        Body::Resign,
        Body::DrawOffer { at: [1u8; 32] },
        Body::DrawAccept { offer: [2u8; 32] },
    ] {
        let after = sign(&record, &body);
        assert_eq!(after.last_signed_ply, record.last_signed_ply);
        assert_eq!(after.last_move_body_hash, record.last_move_body_hash);
    }
}

#[test]
fn advancing_plies_updates_the_counter() {
    let mut record = white_record();
    for ply in [1u16, 3, 5, 7] {
        record = sign(&record, &mv(ply, "e2e4"));
        assert_eq!(record.last_signed_ply, ply);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p adjourn-core --test delegate_policy --locked`
Expected: FAIL — `cannot find function decide_sign`

- [ ] **Step 3: Write the implementation**

Append to `common/src/delegate_policy.rs`:

```rust
use crate::types::color_at_ply;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignDecision {
    /// Sign the body and persist `updated`. For an identical retry `updated`
    /// equals the record passed in, so persisting it is a no-op — that keeps
    /// one case out of this API.
    Sign { updated: GameRecord },
    Refuse(Refusal),
}

/// Decide whether to sign, using only what the delegate itself has recorded.
///
/// Nothing here trusts the caller's view of the game. That is deliberate: the
/// caller may be replaying a stale position, and the ply counter is what makes
/// that harmless.
pub fn decide_sign(
    record: &GameRecord,
    body: &Body,
    origin: Option<[u8; 32]>,
) -> SignDecision {
    let Some(origin) = origin else {
        return SignDecision::Refuse(Refusal::MissingOrigin);
    };
    if origin != record.origin {
        return SignDecision::Refuse(Refusal::ForeignOrigin);
    }

    match body {
        Body::Move { ply, .. } => {
            let needs: Side = color_at_ply(*ply).into();
            if needs != record.side {
                return SignDecision::Refuse(Refusal::WrongSide {
                    ours: record.side,
                    ply_needs: needs,
                });
            }
            if *ply < record.last_signed_ply {
                return SignDecision::Refuse(Refusal::PlyAlreadySigned { ply: *ply });
            }
            if *ply == record.last_signed_ply {
                // Only an identical retry may pass. A DIFFERENT move at a ply
                // already signed is the double-sign fraud proof: signing it
                // would forfeit us.
                if record.last_signed_ply != 0 && body_hash(body) == record.last_move_body_hash
                {
                    return SignDecision::Sign {
                        updated: record.clone(),
                    };
                }
                return SignDecision::Refuse(Refusal::PlyAlreadySigned { ply: *ply });
            }
            let mut updated = record.clone();
            updated.last_signed_ply = *ply;
            updated.last_move_body_hash = body_hash(body);
            SignDecision::Sign { updated }
        }
        // Idempotent by record id, so no guard is needed.
        Body::Resign | Body::DrawOffer { .. } | Body::DrawAccept { .. } => SignDecision::Sign {
            updated: record.clone(),
        },
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p adjourn-core --test delegate_policy --locked`
Expected: PASS, 26 tests

- [ ] **Step 5: Verify the whole common suite is still green**

Run: `cargo test -p adjourn-core --locked`
Expected: PASS — 31 pre-existing tests plus the new delegate policy tests

- [ ] **Step 6: Commit**

```bash
git add common/src/delegate_policy.rs common/tests/delegate_policy.rs
git commit -m "feat(delegate): decide_sign — one signature per (game, ply), ever"
```

---

### Task 5: Delegate crate scaffold, secret store, and CreateGameKey

The delegate crate cannot be compiled on a Windows host (`freenet-stdlib` depends unconditionally on `tracing-subscriber`, which pulls `windows-sys`, which needs a full mingw binutils). Verify with `cargo check --target wasm32-unknown-unknown` throughout; the tests in Task 8 run on CI.

**Files:**
- Create: `delegates/adjourn-delegate/Cargo.toml`
- Create: `delegates/adjourn-delegate/src/secrets.rs`
- Create: `delegates/adjourn-delegate/src/lib.rs`
- Create: `scripts/build-delegate.sh`
- Modify: `Cargo.toml` (workspace members)

**Interfaces:**
- Consumes: everything from Tasks 1–4
- Produces: `ChessDelegate`, `secrets::{key_secret, bind_secret, game_secret, load_seed, load_bound_game_id, load_game, store_game, list_labels}`

- [ ] **Step 1: Add the crate to the workspace**

In the root `Cargo.toml`, change the members line to:

```toml
members = ["common", "contracts/adjourn-contract", "delegates/adjourn-delegate"]
```

- [ ] **Step 2: Write the crate manifest**

Create `delegates/adjourn-delegate/Cargo.toml`:

```toml
[package]
name = "adjourn-delegate"
version.workspace = true
edition.workspace = true

[dependencies]
adjourn-core.workspace = true
freenet-stdlib.workspace = true
ciborium.workspace = true
serde.workspace = true
ed25519-dalek.workspace = true

# NOTE: do NOT add `rand`, `getrandom`, or `rand_core` here, directly or
# transitively. The delegate WASM runs in wasmtime, which has no getrandom
# backend on wasm32-unknown-unknown; those crates produce wasm-bindgen
# placeholder imports that cannot be resolved at instantiation
# (freenet/river#241). Key material comes from `freenet_stdlib::rand::rand_bytes`
# via a host import, mixed by `adjourn_core::delegate_policy::derive_seed`, and
# keys are always built with `SigningKey::from_bytes` — never `generate()`.

[lib]
crate-type = ["cdylib", "rlib"]

[features]
default = ["freenet-main-delegate"]
# The `#[delegate]` macro expands to code gated on this cfg; without the
# feature declared the generated exports are compiled out and the WASM has no
# entry points. River declares it for the same reason.
freenet-main-delegate = []
```

- [ ] **Step 3: Write the secret-store helpers**

Create `delegates/adjourn-delegate/src/secrets.rs`:

```rust
//! Secret-store key naming and typed access.
//!
//! Layout:
//! ```text
//! chess/key/<label>     -> 32 raw signing-key bytes
//! chess/bind/<label>    -> 32-byte game_id
//! chess/game/<game_id>  -> CBOR(GameRecord)
//! ```
//!
//! `chess/bind/` exists because binding is looked up by LABEL while game
//! records are keyed by game id.

use adjourn_core::delegate_api::GameId;
use adjourn_core::delegate_policy::GameRecord;
use freenet_stdlib::prelude::DelegateCtx;

pub const KEY_PREFIX: &[u8] = b"chess/key/";
pub const BIND_PREFIX: &[u8] = b"chess/bind/";
pub const GAME_PREFIX: &[u8] = b"chess/game/";

pub fn key_secret(label: &str) -> Vec<u8> {
    [KEY_PREFIX, label.as_bytes()].concat()
}

pub fn bind_secret(label: &str) -> Vec<u8> {
    [BIND_PREFIX, label.as_bytes()].concat()
}

pub fn game_secret(game_id: &GameId) -> Vec<u8> {
    [GAME_PREFIX, game_id.as_slice()].concat()
}

/// The 32 raw signing-key bytes for `label`, if we hold them.
pub fn load_seed(ctx: &DelegateCtx, label: &str) -> Option<[u8; 32]> {
    let bytes = ctx.get_secret(&key_secret(label))?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

pub fn load_bound_game_id(ctx: &DelegateCtx, label: &str) -> Option<GameId> {
    let bytes = ctx.get_secret(&bind_secret(label))?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

pub fn load_game(ctx: &DelegateCtx, game_id: &GameId) -> Option<GameRecord> {
    let bytes = ctx.get_secret(&game_secret(game_id))?;
    ciborium::from_reader(bytes.as_slice()).ok()
}

/// Writes the game record and the label -> game_id index together. Returns
/// false if either write fails.
pub fn store_game(ctx: &mut DelegateCtx, record: &GameRecord) -> bool {
    let mut buf = Vec::new();
    if ciborium::into_writer(record, &mut buf).is_err() {
        return false;
    }
    let game_id = record.game_id();
    ctx.set_secret(&game_secret(&game_id), &buf)
        && ctx.set_secret(&bind_secret(&record.label), &game_id)
}

/// Labels we hold a key for, recovered from the `chess/key/` prefix.
pub fn list_labels(ctx: &DelegateCtx) -> Vec<String> {
    ctx.list_secrets(KEY_PREFIX)
        .into_iter()
        .filter_map(|k| {
            let suffix = k.strip_prefix(KEY_PREFIX)?;
            String::from_utf8(suffix.to_vec()).ok()
        })
        .collect()
}
```

- [ ] **Step 4: Write the delegate with CreateGameKey only**

Create `delegates/adjourn-delegate/src/lib.rs`:

```rust
//! The Freenet delegate that holds per-game signing keys.
//!
//! All policy lives in `adjourn_core::delegate_policy`, which is pure and tested
//! standalone. This crate is the adapter: secret-store I/O, host entropy, and
//! message dispatch.

mod secrets;

use adjourn_core::delegate_api::{Refusal, Request, Response};
use adjourn_core::delegate_policy::{classify_host_entropy, derive_seed};
use ed25519_dalek::SigningKey;
use freenet_stdlib::prelude::*;
use freenet_stdlib::rand::rand_bytes;

pub struct ChessDelegate;

/// The contract instance id of the calling web app, which the runtime attests.
fn origin_id(origin: Option<MessageOrigin>) -> Option<[u8; 32]> {
    match origin {
        Some(MessageOrigin::WebApp(id)) => <[u8; 32]>::try_from(id.as_bytes()).ok(),
        _ => None,
    }
}

/// Two independent draws, so `classify_host_entropy` can spot a dead source.
fn probe_host_entropy() -> adjourn_core::delegate_policy::HostEntropy {
    let first = <[u8; 32]>::try_from(rand_bytes(32).as_slice()).unwrap_or([0u8; 32]);
    let second = <[u8; 32]>::try_from(rand_bytes(32).as_slice()).unwrap_or([0u8; 32]);
    classify_host_entropy(first, second)
}

fn reply(response: Response) -> Vec<OutboundDelegateMsg> {
    vec![OutboundDelegateMsg::ApplicationMessage(
        ApplicationMessage::new(response.encode()).processed(true),
    )]
}

fn handle_create_game_key(
    ctx: &mut DelegateCtx,
    label: String,
    caller_entropy: Option<[u8; 32]>,
) -> Response {
    if ctx.get_secret(&secrets::key_secret(&label)).is_some() {
        return Response::Refused(Refusal::LabelExists);
    }
    let (seed, quality) = match derive_seed(probe_host_entropy(), caller_entropy, &label) {
        Ok(v) => v,
        Err(refusal) => return Response::Refused(refusal),
    };
    // Never `SigningKey::generate()` — that would pull an RNG crate in.
    let key = SigningKey::from_bytes(&seed);
    let public_key = key.verifying_key().to_bytes();

    if !ctx.set_secret(&secrets::key_secret(&label), &seed) {
        return Response::Refused(Refusal::Malformed("secret store write failed".into()));
    }
    Response::GameKey {
        label,
        public_key,
        entropy: quality,
    }
}

fn handle(ctx: &mut DelegateCtx, origin: Option<[u8; 32]>, request: Request) -> Response {
    let _ = origin;
    match request {
        Request::CreateGameKey {
            label,
            caller_entropy,
        } => handle_create_game_key(ctx, label, caller_entropy),
        _ => Response::Refused(Refusal::Malformed("not yet implemented".into())),
    }
}

#[delegate]
impl DelegateInterface for ChessDelegate {
    fn process(
        ctx: &mut DelegateCtx,
        _parameters: Parameters<'static>,
        origin: Option<MessageOrigin>,
        message: InboundDelegateMsg,
    ) -> Result<Vec<OutboundDelegateMsg>, DelegateError> {
        match message {
            InboundDelegateMsg::ApplicationMessage(app) => {
                let request = match Request::decode(&app.payload) {
                    Ok(r) => r,
                    Err(refusal) => return Ok(reply(Response::Refused(refusal))),
                };
                Ok(reply(handle(ctx, origin_id(origin), request)))
            }
            // `InboundDelegateMsg` is `#[non_exhaustive]`. Reject unknown
            // variants rather than panicking: a panic inside delegate WASM
            // kills the runtime for this delegate and surfaces as an opaque
            // execution error.
            _ => Err(DelegateError::Other(
                "unsupported inbound delegate message".into(),
            )),
        }
    }
}
```

- [ ] **Step 5: Verify it compiles for the real target**

Run: `cargo check -p adjourn-delegate --target wasm32-unknown-unknown --locked`
Expected: no errors, no warnings

- [ ] **Step 6: Write the canonical build script**

Derive it from the contract script so the reproducibility machinery cannot drift between the two:

```bash
sed -e 's/-p adjourn-contract/-p adjourn-delegate/'     -e 's/adjourn_contract\.wasm/adjourn_delegate.wasm/'     -e 's/contract key input/delegate key input/'     -e 's/^# The canonical contract build\./# The canonical delegate build./'     scripts/build-contract.sh > scripts/build-delegate.sh
chmod +x scripts/build-delegate.sh
grep -n "adjourn-delegate\|adjourn_delegate\|delegate key input" scripts/build-delegate.sh
```

Expected: three substitutions applied. Everything else — `--locked`, the `cygpath` native-path conversion, the `--remap-path-prefix` flags, and the leak check over both path spellings — is inherited verbatim. The delegate key is `BLAKE3(BLAKE3(wasm) ‖ params)`, so it has exactly the same exposure as the contract key.

- [ ] **Step 7: Verify the build is reproducible**

```bash
./scripts/build-delegate.sh
FIRST=$(sha256sum target/wasm32-unknown-unknown/release/adjourn_delegate.wasm | cut -d' ' -f1)
cargo clean
./scripts/build-delegate.sh
SECOND=$(sha256sum target/wasm32-unknown-unknown/release/adjourn_delegate.wasm | cut -d' ' -f1)
[ "$FIRST" = "$SECOND" ] && echo REPRODUCIBLE || echo NOT REPRODUCIBLE
```

Expected: `REPRODUCIBLE`, and the script reports no leaked build paths

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock delegates/ scripts/build-delegate.sh
git commit -m "feat(delegate): crate scaffold, secret store, and key creation"
```

---

### Task 6: BindGame, Sign, and ListGames handlers

**Files:**
- Modify: `delegates/adjourn-delegate/src/lib.rs`

**Interfaces:**
- Consumes: `decide_bind`, `decide_sign`, `BindDecision`, `SignDecision` from Tasks 3–4; `secrets::*` from Task 5
- Produces: complete `handle` dispatch for all four requests

- [ ] **Step 1: Replace the `handle` stub with the full dispatch**

In `delegates/adjourn-delegate/src/lib.rs`, add the imports:

```rust
use adjourn_core::delegate_api::{GameSummary, Side};
use adjourn_core::delegate_policy::{decide_bind, decide_sign, BindDecision, SignDecision};
use adjourn_core::types::{GameParams, Record};
use adjourn_core::Body;
```

and replace `handle` with:

```rust
fn handle_bind_game(
    ctx: &mut DelegateCtx,
    origin: Option<[u8; 32]>,
    label: String,
    params: GameParams,
    contract: [u8; 32],
) -> Response {
    let Some(seed) = secrets::load_seed(ctx, &label) else {
        return Response::Refused(Refusal::UnknownLabel);
    };
    let public_key = SigningKey::from_bytes(&seed).verifying_key().to_bytes();

    let existing = secrets::load_bound_game_id(ctx, &label)
        .and_then(|id| secrets::load_game(ctx, &id));

    match decide_bind(existing.as_ref(), &label, public_key, &params, contract, origin) {
        BindDecision::Refuse(refusal) => Response::Refused(refusal),
        BindDecision::Bind { record } => {
            let game_id = record.game_id();
            if !secrets::store_game(ctx, &record) {
                return Response::Refused(Refusal::Malformed(
                    "secret store write failed".into(),
                ));
            }
            Response::Bound { game_id }
        }
    }
}

fn handle_sign(
    ctx: &mut DelegateCtx,
    origin: Option<[u8; 32]>,
    game_id: [u8; 32],
    body: Body,
) -> Response {
    let Some(record) = secrets::load_game(ctx, &game_id) else {
        return Response::Refused(Refusal::UnknownGame);
    };
    let Some(seed) = secrets::load_seed(ctx, &record.label) else {
        return Response::Refused(Refusal::UnknownLabel);
    };

    match decide_sign(&record, &body, origin) {
        SignDecision::Refuse(refusal) => Response::Refused(refusal),
        SignDecision::Sign { updated } => {
            // Persist BEFORE handing out the signature. If the store write
            // fails we must not release a signature whose ply we did not
            // record, or a retry could produce a different move at that ply.
            if !secrets::store_game(ctx, &updated) {
                return Response::Refused(Refusal::Malformed(
                    "secret store write failed".into(),
                ));
            }
            let key = SigningKey::from_bytes(&seed);
            Response::Signed {
                record: Record::sign(&key, &record.params, body),
            }
        }
    }
}

fn handle_list_games(ctx: &DelegateCtx) -> Response {
    let mut games = Vec::new();
    for label in secrets::list_labels(ctx) {
        let Some(seed) = secrets::load_seed(ctx, &label) else {
            continue;
        };
        let public_key = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
        let bound = secrets::load_bound_game_id(ctx, &label)
            .and_then(|id| secrets::load_game(ctx, &id));
        games.push(match bound {
            Some(record) => GameSummary {
                label,
                public_key,
                game_id: Some(record.game_id()),
                side: Some(record.side),
                last_signed_ply: record.last_signed_ply,
            },
            None => GameSummary {
                label,
                public_key,
                game_id: None,
                side: None,
                last_signed_ply: 0,
            },
        });
    }
    Response::Games(games)
}

fn handle(ctx: &mut DelegateCtx, origin: Option<[u8; 32]>, request: Request) -> Response {
    match request {
        Request::CreateGameKey {
            label,
            caller_entropy,
        } => handle_create_game_key(ctx, label, caller_entropy),
        Request::BindGame {
            label,
            params,
            contract,
        } => handle_bind_game(ctx, origin, label, params, contract),
        Request::Sign { game_id, body } => handle_sign(ctx, origin, game_id, body),
        Request::ListGames => handle_list_games(ctx),
    }
}
```

Remove the now-unused `use adjourn_core::delegate_api::Side;` if the compiler reports it as unused.

- [ ] **Step 2: Verify it compiles for the real target**

Run: `cargo check -p adjourn-delegate --target wasm32-unknown-unknown --locked`
Expected: no errors, no warnings

- [ ] **Step 3: Verify clippy is clean**

Run: `cargo clippy -p adjourn-delegate --target wasm32-unknown-unknown --locked`
Expected: no warnings

- [ ] **Step 4: Commit**

```bash
git add delegates/adjourn-delegate/src/lib.rs
git commit -m "feat(delegate): bind, sign, and list handlers"
```

---

### Task 7: Best-effort legality against local contract state

The guarantee is the ply counter; this only catches honest client bugs. `get_contract_state` reads the **local replica only** — it returns `None` when the contract is not held locally and can be stale — so it must never be required.

**Files:**
- Modify: `delegates/adjourn-delegate/src/lib.rs`

**Interfaces:**
- Consumes: `adjourn_core::{GameState, project}`
- Produces: no new public surface; `handle_sign` gains a pre-check

- [ ] **Step 1: Add the check**

In `delegates/adjourn-delegate/src/lib.rs` add:

```rust
use adjourn_core::{project, GameState};

/// Best-effort only. Returns `None` when we cannot tell — no local replica, or
/// it does not decode — and the signature is granted anyway. The monotonic ply
/// counter in `decide_sign` is the actual guarantee; requiring state here would
/// let a cold cache lock a player out of their own game.
fn locally_known_to_be_illegal(
    ctx: &DelegateCtx,
    record: &adjourn_core::delegate_policy::GameRecord,
    body: &Body,
) -> bool {
    let Body::Move { ply, uci, .. } = body else {
        return false;
    };
    // `record.contract`, NOT `record.game_id()`: a contract instance id is
    // hash(code, params) and is a different value from our game id.
    let Some(bytes) = ctx.get_contract_state(&record.contract) else {
        return false;
    };
    let Some(state) = GameState::decode(&bytes) else {
        return false;
    };
    let status = project(&state, &record.params);
    if status.is_over() {
        return true;
    }
    // Only judge when the local replica agrees about which ply is next; if it
    // is behind, we have nothing useful to say.
    if status.ply + 1 != *ply {
        return false;
    }
    !adjourn_core::legal_moves(&state, &record.params).iter().any(|m| m == uci)
}
```

and in `handle_sign`, immediately after loading `seed`:

```rust
    if locally_known_to_be_illegal(ctx, &record, &body) {
        return Response::Refused(Refusal::Malformed(
            "move is illegal in the locally known position".into(),
        ));
    }
```

- [ ] **Step 2: Verify it compiles for the real target**

Run: `cargo check -p adjourn-delegate --target wasm32-unknown-unknown --locked`
Expected: no errors, no warnings

- [ ] **Step 3: Commit**

```bash
git add delegates/adjourn-delegate/src/lib.rs
git commit -m "feat(delegate): best-effort legality check against local state"
```

---

### Task 8: Adapter tests, CI, and documentation

**Files:**
- Create: `delegates/adjourn-delegate/tests/adapter.rs`
- Modify: `.github/workflows/ci.yml`
- Modify: `CLAUDE.md`, `README.md`

**Interfaces:**
- Consumes: everything above

- [ ] **Step 1: Write the adapter tests**

Create `delegates/adjourn-delegate/tests/adapter.rs`:

```rust
//! Adapter-level tests. CI-only: this crate cannot be compiled on a Windows
//! host, because `freenet-stdlib` depends unconditionally on
//! `tracing-subscriber`, which pulls `windows-sys`.
//!
//! Every rule that matters is tested in `adjourn-core`; these cover message
//! dispatch and the secret-store round trip.

#![cfg(not(target_arch = "wasm32"))]

use adjourn_core::delegate_api::{Refusal, Request, Response};

#[test]
fn an_undecodable_payload_is_refused_rather_than_panicking() {
    assert!(matches!(
        Request::decode(&[0xff, 0xff]),
        Err(Refusal::Malformed(_))
    ));
}

#[test]
fn responses_survive_the_wire() {
    let resp = Response::Bound { game_id: [4u8; 32] };
    assert_eq!(Response::decode(&resp.encode()).unwrap(), resp);
}
```

- [ ] **Step 2: Verify the tests typecheck**

The cfg gate hides them from a wasm check, so lift it temporarily:

```bash
sed -i 's/^#!\[cfg(not(target_arch = "wasm32"))\]$/\/\/ typecheck/' delegates/adjourn-delegate/tests/adapter.rs
cargo check -p adjourn-delegate --tests --target wasm32-unknown-unknown --locked
sed -i 's|^// typecheck$|#!\[cfg(not(target_arch = "wasm32"))\]|' delegates/adjourn-delegate/tests/adapter.rs
```

Expected: no errors, and the gate restored afterwards

- [ ] **Step 3: Extend CI**

In `.github/workflows/ci.yml`, after the contract reproducibility step, add:

```yaml
      - name: Build delegate WASM (canonical)
        run: ./scripts/build-delegate.sh

      - name: Assert no getrandom in the delegate graph
        run: |
          if cargo tree -p adjourn-delegate --target wasm32-unknown-unknown -i getrandom 2>/dev/null | grep -q getrandom; then
            echo "getrandom reached the delegate dependency graph; the WASM will not instantiate under wasmtime"
            exit 1
          fi

      - name: Assert the delegate build is reproducible
        run: |
          WASM=target/wasm32-unknown-unknown/release/adjourn_delegate.wasm
          FIRST=$(sha256sum "$WASM" | cut -d' ' -f1)
          cargo clean
          ./scripts/build-delegate.sh > /dev/null
          SECOND=$(sha256sum "$WASM" | cut -d' ' -f1)
          if [ "$FIRST" != "$SECOND" ]; then
            echo "delegate WASM is not reproducible; the delegate key would be unstable"
            exit 1
          fi
```

- [ ] **Step 4: Update the docs**

In `CLAUDE.md`, add `delegates/adjourn-delegate/` to the crate table with the role "holds per-game signing keys; enforces one signature per (game, ply)". Add a short **Delegate** section recording:
- policy lives in `common/src/delegate_policy.rs` and is pure, because the delegate crate cannot be host-compiled on Windows
- the `rand_bytes` off-wasm stub returns zeros silently, which is why `classify_host_entropy` takes two draws
- `freenet-main-delegate` must be declared, exactly like `freenet-main-contract`
- build only via `scripts/build-delegate.sh`

In `README.md`, mark roadmap item 2 done and add the delegate to the layout table.

- [ ] **Step 5: Verify everything**

```bash
cargo fmt --all --check
cargo clippy -p adjourn-core --all-targets --locked
cargo clippy -p adjourn-delegate --target wasm32-unknown-unknown --locked
cargo test -p adjourn-core --locked
./scripts/build-delegate.sh
```

Expected: all clean; the delegate build reports a hash and no leaked paths

- [ ] **Step 6: Commit**

```bash
git add delegates/adjourn-delegate/tests .github/workflows/ci.yml CLAUDE.md README.md
git commit -m "test(delegate): adapter tests, CI wiring, and docs"
```

---

### Task 9: Runtime spike — verify the two host assumptions

**Requires a running Freenet node.** Not blocking: the design is correct either way. What this changes is what we *tell users*, and whether the strict origin rule is viable.

- [ ] **Step 1: Register the delegate against a local node and call `CreateGameKey`**

Send a `Request::CreateGameKey { label: "spike", caller_entropy: Some([1u8; 32]) }` and read the `EntropyQuality` in the response.

- `HostBacked` → `freenet_rand` is provided; we ship with the strong property. Record this in `CLAUDE.md`.
- `Degraded` → the host import is absent or dead. The key is still safe against later UI compromise but not against a UI hostile at creation time. Record it, and surface the warning in the UI when roadmap item 3 lands.

- [ ] **Step 2: Confirm `MessageOrigin` is populated for web-app calls**

Call `BindGame` from the web app and check the response is `Bound`, not `Refused(MissingOrigin)`.

If it is `MissingOrigin`, the strict rule is not viable. Apply the fallback recorded in the spec: bind on first observed origin and refuse only on a *mismatch*. That is weaker — it cannot distinguish "no origin" from "the right origin" — but still blocks a second app from signing.

- [ ] **Step 3: Record the findings**

Add a short "Runtime assumptions, verified" section to `CLAUDE.md` with the date, node version, and both answers. These are exactly the sort of facts that get re-derived expensively later.

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md
git commit -m "docs(delegate): record verified runtime assumptions"
```
