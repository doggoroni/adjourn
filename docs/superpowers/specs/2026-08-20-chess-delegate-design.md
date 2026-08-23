# chess-delegate — design

**Date:** 2026-08-20
**Status:** approved, not yet implemented
**Roadmap item:** 2 of 3 — "Delegate holding the per-game signing key; UI never sees it"

## Purpose

Move the per-game ed25519 signing key out of the web UI and into a Freenet
delegate, so that a compromised UI cannot steal it — and, because the delegate
is the only thing that can sign, cannot forfeit the game either.

That second half is the reason this is worth building. The protocol has exactly
one self-inflicted loss condition: signing two different legal moves at the same
ply is a fraud proof that forfeits the signer (INVARIANT 3, `walk`). Today
nothing prevents a buggy or hostile client from doing it. A delegate that
records the highest ply it has signed makes it *unreachable*.

## Goals

1. The signing key is generated inside the delegate and never leaves it.
2. One signature per `(game, ply)`, ever — enforced by the delegate's own
   record, not by anything the caller claims.
3. Only the chess web app can ask the delegate to sign for its own games.
4. An identical retry after a dropped response still succeeds.
5. Key generation degrades honestly when host entropy is unavailable, and says
   so, rather than silently minting a weak key.

## Non-goals (v1)

Deliberately excluded; none of these require the message API to change later.

- Signing *and submitting* the update. Roadmap item 3 gives the UI the
  `update` path over the WebSocket API.
- Key export, backup, or multi-device sync.
- `RequestUserInput` confirmation prompts for destructive bodies (`Resign`,
  `DrawAccept`).
- Rate-limiting non-move bodies. A player spamming their own `DrawOffer`s only
  grows their own state; see the eviction rule in CLAUDE.md.

## Architecture

The split that already works for the contract: **all policy is a pure function
in `chess-core`; the delegate crate is a thin adapter.**

```
common/src/delegate_api.rs      wire types (UI <-> delegate). No Freenet deps.
common/src/delegate_policy.rs   the decision functions. Pure: no I/O, no clock,
                                no randomness. Fully unit-testable standalone.
delegates/chess-delegate/       adapter: secret-store I/O, host entropy,
                                message dispatch. Freenet-dependent.
```

River sets the precedent for putting delegate message types in the common crate
(`river/common/src/chat_delegate.rs`).

The payoff is testability. The delegate crate cannot be compiled on a Windows
host at all (`freenet-stdlib` depends unconditionally on `tracing-subscriber`,
which pulls `windows-sys`), so anything living there is CI-only. Everything
interesting therefore lives in `common`, where it runs everywhere.

`delegate_api` must not depend on `shakmaty`'s serde feature, so it defines its
own `Side` enum rather than serialising `shakmaty::Color`. This also keeps the
wire format ours, and stable across a shakmaty bump.

## Message API

```rust
pub enum Request {
    CreateGameKey { label: String, caller_entropy: Option<[u8; 32]> },
    BindGame      { label: String, params: GameParams },
    Sign          { game_id: [u8; 32], body: Body },
    ListGames,
}

pub enum Response {
    GameKey { label: String, public_key: KeyBytes, entropy: EntropyQuality },
    Bound   { game_id: [u8; 32] },
    Signed  { record: Record },
    Games(Vec<GameSummary>),
    Refused(Refusal),
}

pub enum EntropyQuality { HostBacked, Degraded }

pub struct GameSummary {
    pub label: String,
    pub public_key: KeyBytes,
    pub game_id: Option<[u8; 32]>,  // None until bound
    pub side: Option<Side>,          // None until bound
    pub last_signed_ply: u16,        // 0 = nothing signed yet
}
```

`ListGames` returns labels, public keys and ply counters. Never secrets.

### Why creation and binding are separate

`GameParams { white, black, nonce }` needs *both* players' public keys, and the
contract key derives from the params. So your key must exist before the game
does. `CreateGameKey` yields a public key to exchange out of band;
`BindGame` records which game that key now belongs to, once both halves are
known.

Binding could be folded into the first `Sign`, but keeping it explicit makes the
state machine (`created -> bound -> signing`) directly testable and gives the UI
a natural point to confirm registration before play starts.

## Policy: what the delegate enforces

Hard rules, evaluated against the delegate's own stored record and nothing the
caller asserts:

```rust
pub struct GameRecord {
    pub label: String,
    pub params: GameParams,
    pub side: Side,
    pub origin: [u8; 32],           // contract instance id that bound the game
    pub last_signed_ply: u16,       // 0 = none
    pub last_move_body_hash: [u8; 32],
}

pub fn decide_sign(record: &GameRecord, body: &Body, origin: Option<[u8; 32]>)
    -> SignDecision;

pub enum SignDecision {
    Sign { updated: GameRecord },
    Refuse(Refusal),
}
```

`decide_sign` evaluates in this order:

1. **Origin.** `None` → `Refuse(MissingOrigin)`. `Some(o)` where
   `o != record.origin` → `Refuse(ForeignOrigin)`.
2. **`Body::Move { ply, .. }`:**
   - `color_at_ply(ply) != record.side` → `Refuse(WrongSide)`.
   - `ply < last_signed_ply` → `Refuse(PlyAlreadySigned { ply })`.
   - `ply == last_signed_ply`:
     - body hash equals `last_move_body_hash` → **Sign** (idempotent retry;
       `updated` is unchanged).
     - otherwise → `Refuse(PlyAlreadySigned { ply })`. **This is the rule the
       whole delegate exists for.**
   - `ply > last_signed_ply` → **Sign**, with `updated` advancing
     `last_signed_ply` and `last_move_body_hash`.
3. **`Resign` / `DrawOffer` / `DrawAccept`:** Sign, no ply update. These are
   idempotent by record id — signing the same statement twice yields the same
   id and collapses to one slot under merge (INVARIANT 2), so there is nothing
   to guard.

`Sign` always returns an `updated` record, even for a retry where it is
unchanged. Persisting an identical value is a no-op, and it removes a case from
the API.

### Binding

```rust
pub fn decide_bind(
    existing: Option<&GameRecord>,
    label: &str,
    public_key: KeyBytes,
    params: &GameParams,
    origin: Option<[u8; 32]>,
) -> BindDecision;

pub enum BindDecision {
    Bind { record: GameRecord },
    Refuse(Refusal),
}
```

1. `origin` absent → `Refuse(MissingOrigin)`.
2. `params.color_of(public_key)` is `None` — the key we hold for this label is
   neither player in these params → `Refuse(KeyNotInParams)`. This catches a UI
   that pairs the wrong key with the wrong game.
3. A record already exists for this label under a *different* `game_id` →
   `Refuse(AlreadyBound { game_id })`. Rebinding a label would orphan its ply
   counter, which is precisely the protection we are trying to keep.
4. Otherwise `Bind`, with `side` derived from `params`, `origin` recorded, and
   `last_signed_ply: 0`.

Re-binding the *same* label to the *same* `game_id` is allowed and idempotent,
for the same dropped-response reason as retried signing.

### Why the idempotent-retry case is load-bearing

If the UI sends a move, the response is dropped, and the UI resends, a naive
"never sign at a signed ply" rule refuses — and the game wedges with no way
forward. Storing the body hash distinguishes "the same move again" (safe, and
because ed25519 signing is deterministic the returned record is byte-identical)
from "a *different* move at that ply" (the fraud). Only the second is refused.

### Best-effort legality

When `ctx.get_contract_state` returns a local replica, the adapter additionally
projects it and checks the move is legal and that it is our turn. When it
returns `None` — the contract is not held locally — those checks are skipped and
the signature is still granted.

This is safe precisely because it is not the guarantee. `get_contract_state`
reads the local replica only; it can be stale or absent, so a stale state could
otherwise be used to induce a double-sign. The monotonic counter is what makes
that impossible, and it does not depend on reading any state at all.

## Secret store layout

```
chess/key/<label>    -> 32 raw signing-key bytes
chess/game/<game_id> -> CBOR(GameRecord)
```

`list_secrets(b"chess/key/")` backs `ListGames`.

## Entropy

The delicate part, and the one place where a mistake is silent.

### The trap

`freenet_stdlib::rand::rand_bytes` reads into a zero-initialised thread-local
buffer via a `freenet_rand` host import. Off-wasm the import is a **stub that
does nothing**:

```rust
thread_local! { static SMALL_BUF: RefCell<[u8; 512]> = const { RefCell::new([0u8; 512]) }; }

#[cfg(not(target_family = "wasm"))]
unsafe fn __frnt__rand__rand_bytes(_id: i64, _ptr: i64, _len: u32) { }
```

So off-wasm `rand_bytes(32)` returns **32 zero bytes, silently**, and
`SigningKey::from_bytes(&[0u8; 32])` is a valid, entirely known private key.
A missing import on wasm fails loudly at instantiation, which is the *good*
failure. The silent one lands exactly where tests run.

### Two properties, separated

1. **The UI cannot learn the key after generation** (later XSS, bad update,
   state leak).
2. **The UI cannot learn the key at generation** (already hostile).

Property 2 requires host entropy and is information-theoretically unattainable
without it: unpredictable-to-the-UI data cannot be built from material the UI
supplied. Property 1 is the realistic threat and is cheap.

### Design

```rust
pub enum HostEntropy { Live([u8; 32]), Dead }

/// Two draws. Identical results mean the source is dead — a real RNG collides
/// with negligible probability — and all-zeros catches the off-wasm stub.
pub fn classify_host_entropy(first: [u8; 32], second: [u8; 32]) -> HostEntropy;

pub fn derive_seed(host: HostEntropy, caller: Option<[u8; 32]>, label: &str)
    -> Result<([u8; 32], EntropyQuality), Refusal>;
```

`derive_seed` mixes with domain-separated, length-prefixed SHA-256 (`sha2` is
already a dependency; blake3 would be a new one):

| host | caller | result |
|---|---|---|
| `Live` | any | seed, `HostBacked` |
| `Dead` | `Some` | seed, `Degraded` |
| `Dead` | `None` | `Err(Refusal::NoEntropy)` — **fail closed** |

An all-zero `caller_entropy` is treated as absent, so a lazy caller cannot
manufacture a false sense of safety.

Mixing never loses: the result is at least as unpredictable as the strongest
input. `EntropyQuality` is returned to the UI so it can warn once that a key was
created without hardware randomness, rather than the system quietly pretending
otherwise.

The adapter calls `rand_bytes` twice and passes both draws in. The policy layer
never touches `rand_bytes`, so the tests are real.

## Error handling

```rust
pub enum Refusal {
    UnknownLabel, LabelExists, UnknownGame,
    AlreadyBound { game_id: [u8; 32] },
    KeyNotInParams,
    WrongSide { ours: Side, ply_needs: Side },
    PlyAlreadySigned { ply: u16 },
    MissingOrigin, ForeignOrigin,
    NoEntropy,
    Malformed(String),
}
```

Refusals are returned as `Response::Refused` — an ordinary application message,
not a `DelegateError`. A refusal is an expected outcome, and the UI needs to
render it.

`InboundDelegateMsg` is `#[non_exhaustive]`. The catch-all arm returns an error,
never `unreachable!()` — the same lesson as the contract: a panic inside
delegate WASM kills the runtime for the delegate and surfaces as an opaque
execution error.

## Testing

**`common/tests/delegate_policy.rs`** — runs on every platform:

- a second, different move at a signed ply is refused
- an identical move at a signed ply is signed, and the record is byte-identical
- a move at a lower ply than one already signed is refused
- signing for the wrong side is refused
- a foreign or missing origin is refused
- binding params that do not contain our key is refused
- rebinding a label to a different game is refused
- `classify_host_entropy` reports `Dead` for all-zeros and for two equal draws
- `derive_seed` fails closed when host entropy is dead and no caller entropy
- `derive_seed` is deterministic in its inputs and changes with the label

**`delegates/chess-delegate/tests/`** — adapter dispatch and secret-store round
trips. CI-only, same `windows-sys` limitation as the contract; typechecked
locally by compiling for wasm32.

## Build

Mirrors the contract exactly.

```toml
[features]
default = ["freenet-main-delegate"]
freenet-main-delegate = []   # the #[delegate] macro expands to code gated on it

[lib]
crate-type = ["cdylib", "rlib"]
```

Dependencies: `chess-core`, `freenet-stdlib`, `ciborium`, `serde`,
`ed25519-dalek` — **no `rand`, no `getrandom`, no `rand_core`**, directly or
transitively. We never call `SigningKey::generate()`; keys are built with
`SigningKey::from_bytes` from a seed we derived ourselves.

The delegate key is `BLAKE3(BLAKE3(wasm) ‖ params)`, so it has the same
reproducibility exposure as the contract key. `scripts/build-delegate.sh`
mirrors `build-contract.sh`: `--locked`, `--remap-path-prefix`, and a hard
failure if a build path leaked. CI builds it, asserts no `getrandom` in the
wasm32 graph, and checks the rebuild is byte-identical.

## Risks and open questions

1. **Is `freenet_rand` provided by the running node?** Worth a cheap spike
   before implementation. It decides whether we ship with property 2 or without
   it. It is no longer a blocker — the design is correct either way — but the
   answer changes what we tell users. Note that River deliberately avoids host
   randomness entirely, deriving from blake3 of delegate-controlled inputs
   instead, which may be evidence the import is not dependable.

2. **Is `MessageOrigin` always populated for web-app calls?** The design refuses
   to sign without it. That is the correct default, but if the runtime leaves it
   `None` in some path, every signature fails. Verify in the same spike; if it
   is unreliable, the fallback is to bind-on-first-origin and refuse only on a
   *mismatch*, which is weaker but still blocks a second app.

3. **`last_signed_ply` is per game, not per chain.** After a double-sign forfeit
   by the *opponent*, `walk` truncates the chain and the projected ply rewinds
   (see "The outcome is not monotone" in CLAUDE.md). Our counter does not
   rewind, so the delegate would refuse to re-sign at the rewound plies. This is
   the correct conservative behaviour — the game is over, the opponent forfeited
   — but the UI must render it as "this game is decided", not "signing broken".

4. **The delegate cannot be host-tested on Windows.** Same constraint as the
   contract, mitigated by keeping policy in `common`.
