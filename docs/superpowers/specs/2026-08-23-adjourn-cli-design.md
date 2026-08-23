# adjourn-cli — design (roadmap 3a + 3b)

**Date:** 2026-08-23
**Status:** approved, not yet implemented
**Roadmap:** item 3, sub-projects 3a (Freenet client layer) and 3b (game session flow)

## Purpose

A headless CLI that plays a complete game of correspondence chess against a
local Freenet node, using the real contract and the real delegate.

This is the first time any of this code runs against a node. Everything so far
is unit-tested against typechecked assumptions; the contract has never been
PUT, the delegate has never signed anything, and two peers have never actually
converged over a network.

## Goals

1. Play a full game end to end between two nodes, one player each.
2. Answer the two runtime questions the delegate spec left open (Task 9):
   is `freenet_rand` provided, and is `MessageOrigin` populated?
3. Keep the session flow testable in CI, which has no Freenet node.
4. Make a build mismatch between two players **loud**, not silent.

## Non-goals

Reconnection, backoff, a daemon, PGN import/export, matchmaking, and anything
visual. Presentation is 3c.

## Decisions

Four decisions shape everything below; each was taken deliberately.

**Two nodes, one side each.** A single delegate cannot hold both sides of one
game — `chess/game/<game_id>` is keyed by game id alone, and the second bind
would overwrite the first player's ply counter, so it is refused. Rather than
re-key per side, we run two `freenet local` instances with separate data dirs.
This is also the only arrangement that exercises real peer-to-peer sync; one
node talking to itself proves very little about a design whose entire point is
convergence between peers.

**One-shot commands.** Each invocation connects, sends, waits for its response
with a timeout, prints, exits. `watch` is the sole exception, holding a
subscription open and streaming notifications. No reconnection, no watchdog, no
subscription lifecycle — River needs 15k lines for that and we need none of it
yet. Failures stay visible instead of being silently retried.

**A `NodeClient` trait with an in-memory fake.** CI has no node and never will.
The fake runs the *real* contract code, so a merge or projection mistake is
caught in CI rather than only on a live node.

**Invite → accept, two blobs.** Both players must derive byte-identical
`GameParams` or they land on different contracts and silently never meet. The
initiator authors the nonce, so it has exactly one author and cannot disagree.

## Changes to the delegate

Both are forced by this work rather than invented for it.

### `origin` becomes `Option<[u8; 32]>`

`MessageOrigin::WebApp(ContractInstanceId)` means "sent by a web application
backed by the given contract". A CLI over the WebSocket API is not backed by a
contract, so its origin is expected to be `None` — and the current rule refuses
to bind or sign without one. **As it stands the CLI cannot sign a single move.**

`GameRecord.origin` becomes `Option<[u8; 32]>`, and `decide_bind` /
`decide_sign` require an **exact match** against what was recorded at bind time.

- A game bound by a web app records `Some(id)` and requires that same app
  forever — unchanged protection.
- A game bound by the CLI records `None` and requires `None`, so a web app
  cannot hijack a CLI-bound game either.

For CLI-bound games the origin check therefore provides no isolation, and the
protection is the node's own access control: the WS API binds loopback-only by
default and its own documentation warns that anything reaching it can read and
modify contract state, identities and keys. That is a defensible boundary for a
development tool and must be stated plainly rather than implied.

`Refusal::MissingOrigin` and `Refusal::ForeignOrigin` collapse into a single
`Refusal::WrongOrigin`: with `Option` equality there is one failure — the
caller is not who bound the game — and two variants for it would only invite
the reader to wonder what distinguishes them.

This changes `GameRecord`'s layout, so **`GAME_RECORD_FORMAT` becomes 2** — the
first real use of the version field added for exactly this.

### A `SecretStore` trait

The in-memory fake can run the real contract, whose code is pure over bytes.
It **cannot** run the real delegate: `DelegateCtx`'s secret methods are FFI
stubs off-wasm that always return `None`, so the delegate would never find a
key. Reimplementing the adapter inside the fake would let the two drift, and
the drift would be invisible.

Instead the delegate gains:

```rust
pub trait SecretStore {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>>;
    fn set(&mut self, key: &[u8], value: &[u8]) -> bool;
    fn list(&self, prefix: &[u8]) -> Vec<Vec<u8>>;
}
```

with an impl for `DelegateCtx` and one for `BTreeMap<Vec<u8>, Vec<u8>>`. The
`handle_*` functions become generic over it. The fake then runs the *actual*
dispatch code, and the delegate's handlers become host-testable for the first
time — today only the policy layer has tests, while the adapter has four
covering key naming and nothing else.

`get_contract_state` stays on `DelegateCtx` and is passed separately as an
`Option<Vec<u8>>`, since it is already best-effort and `None` is a legitimate
answer.

## Architecture

```
cli/                       adjourn-cli — new workspace member
  src/main.rs              arg parsing, dispatch, exit codes
  src/node.rs              NodeClient trait + WsClient (real WebSocket)
  src/session.rs           3b: setup and move flows, generic over NodeClient
  src/invite.rs            Invite / GameOffer blob codec (base58 + CBOR)
  src/fake.rs              FakeNode: real contract + real delegate dispatch
  tests/full_game.rs       two sessions, invite to mate, in CI
```

The seam uses native `async fn` in traits, used generically (`<N: NodeClient>`)
rather than as `dyn`, so no `async-trait` dependency:

```rust
trait NodeClient {
    async fn get(&mut self, id: ContractInstanceId, subscribe: bool)
        -> Result<Option<Vec<u8>>>;          // None on NotFound
    async fn put(&mut self, container: ContractContainer, state: Vec<u8>)
        -> Result<()>;
    async fn update(&mut self, key: ContractKey, delta: Vec<u8>) -> Result<()>;
    async fn delegate(&mut self, req: Request) -> Result<Response>;
}
```

## WASM artifacts

The CLI needs both WASMs: the contract's code to derive a `ContractInstanceId`
via `ContractInstanceId::from_params_and_code`, and the delegate's to register
it.

They are read **from a path at runtime**, defaulting to
`target/wasm32-unknown-unknown/release/adjourn_{contract,delegate}.wasm` and
overridable with `--contract-wasm` / `--delegate-wasm`. River commits its WASM
and embeds it with `include_bytes!`; we do not, because committing a 244 KB
binary to get a build-order dependency is a poor trade for a development tool.

The cost is that two players can unknowingly run different builds. The offer
blob closes that (below).

## Command surface

```
adjourn init                              register the delegate on this node
adjourn key new    --label L [--entropy HEX]
adjourn key list
adjourn invite new --label L --side white|black
adjourn invite accept INVITE --label L
adjourn game bind  --label L OFFER
adjourn game list
adjourn show       --label L
adjourn move UCI   --label L
adjourn resign     --label L
adjourn draw offer|accept --label L
adjourn watch      --label L
```

The `DelegateKey` needed to address application messages is **derived at
runtime** from `--delegate-wasm` plus empty parameters, not stored. It is a
pure function of the code, so there is nothing to keep in sync and no way for a
stale cached key to point at a delegate generation that is no longer there.

Global: `--node URL`, defaulting to
`ws://127.0.0.1:7509/v1/contract/command?encodingProtocol=native` — the
endpoint the spike observed. Plus `--contract-wasm`, `--delegate-wasm`.

`init` is explicit rather than lazy: a development tool should say what it did
to the node, and registration is idempotent so re-running is harmless.

## Blob formats

Both are base58 of CBOR, versioned, with `bs58` as a new workspace dependency
(River uses it for the same purpose).

```rust
struct Invite {
    v: u8,                  // INVITE_FORMAT = 1
    side: Side,             // the INITIATOR's side
    public_key: KeyBytes,
    nonce: [u8; 16],        // authored once, by the initiator
}

struct GameOffer {
    v: u8,                  // OFFER_FORMAT = 1
    params: GameParams,
    contract: [u8; 32],     // the responder's derived ContractInstanceId
}
```

`contract` in the offer is a **build-mismatch detector**. The contract id is
`hash(code, params)`, so two players running different `adjourn-contract`
builds derive different ids from identical params — and would sit on separate
contracts, each seeing a game the other never joins, with no error anywhere.
On `game bind` the initiator recomputes the id from its own WASM and refuses if
it disagrees, reporting that the two builds differ. Without this the failure is
silent and extremely confusing.

## Flows

### Setup

```
alice: invite new    -> delegate CreateGameKey(label)     -> pubkey
                     -> emit Invite{side, pubkey, nonce}

bob:   invite accept -> delegate CreateGameKey(label)     -> pubkey
                     -> build GameParams from both keys + nonce
                     -> derive contract id from params + contract WASM
                     -> PUT contract with EMPTY state
                     -> delegate BindGame{label, params, contract}
                     -> emit GameOffer{params, contract}

alice: game bind     -> parse offer; recompute contract id; compare
                     -> GET contract; PUT if NotFound
                     -> delegate BindGame{label, params, contract}
```

Empty state on PUT is deliberate and already supported: the contract treats
empty bytes as "nothing yet, not malformed", which exists precisely because a
contract is PUT before either player moves.

### Move

```
GET state -> decode -> project
  -> local pre-checks: game not over, our turn, move legal
  -> delegate Sign{game_id, body: Move{ply, parent, uci}}  -> Record
  -> UPDATE with UpdateData::Delta(cbor(vec![record]))
  -> await UpdateResponse
  -> GET again, print the new status
```

The local pre-checks exist to give a good error before bothering the delegate.
They are **not** the guarantee: the delegate's monotonic ply counter is, and it
does not trust anything the CLI claims.

## Error handling

Refusals from the delegate are expected outcomes and print as human sentences,
not debug output — `PlyAlreadySigned` in particular should read as "you have
already signed a move at ply 7; retry the identical move or wait for your
opponent", because that is the one a user will hit through legitimate retry.

Exit codes: `0` success, `1` refusal or precondition failure, `2` usage error,
`3` transport failure. A refusal is not a crash.

## Testing

**`tests/full_game.rs` (CI)** — two `FakeNode`s sharing one contract state,
each with its own delegate secret store; invite → accept → bind → play
Scholar's Mate → assert both sides converge on the same state and both project
the same mate. This is the first end-to-end exercise of contract and delegate
together.

Plus: build-mismatch detection (offer carrying a wrong contract id is refused),
a double-sign attempt through the CLI is refused by the delegate, and an
identical retry succeeds.

**Manual smoke tests** — the same flow against two live `freenet local` nodes.
Not in CI; recorded as a runbook in the repo.

## Task 9 falls out

`adjourn key new` prints the `EntropyQuality` the delegate reports, answering
whether `freenet_rand` is live. Whether `bind` succeeds answers whether
`MessageOrigin` is populated — and if it turns out to be populated after all,
the `Option` design works unchanged, recording `Some(id)` instead.

Both answers get recorded in CLAUDE.md as verified runtime facts.

## Sequencing

The delegate changes land **first**, with their own tests, before any CLI code
exists:

1. `origin` to `Option`, `WrongOrigin`, `GAME_RECORD_FORMAT` to 2.
2. The `SecretStore` trait, plus the first real tests of the delegate's
   handlers now that they are host-testable.
3. The CLI: `NodeClient` and `WsClient`, then `FakeNode`, then the session
   flows, then the commands.

Steps 1 and 2 are independently reviewable and leave the tree green. Building
the CLI on top of an unmodified delegate is not possible — step 1 is what makes
signing work at all from a non-web-app client.

## Risks

1. **Origin may behave differently than predicted.** The `Option` design
   handles both outcomes, so this is a documentation risk rather than a design
   one.
2. **The fake's contract state is shared in-process**, so it models convergence
   but not partition or latency. Real sync behaviour still needs the live
   two-node runbook.
3. **`freenet_rand` off-wasm returns zeros**, so every key the FakeNode creates
   is `Degraded`. That is correct and worth asserting in a test, but it means
   CI never exercises the `HostBacked` path.
4. **Two delegate changes touch code that was just reviewed.** Both bump
   `GAME_RECORD_FORMAT` and both need their own tests before the CLI is built
   on top of them.
