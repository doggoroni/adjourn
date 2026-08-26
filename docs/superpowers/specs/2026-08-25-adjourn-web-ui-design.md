# adjourn: the web UI

**Status:** approved, not yet implemented
**Date:** 2026-08-25

## Goal

A browser app that lets two people play a complete correspondence game against a
local Freenet node: list games, create and accept an invite, see the board
update when the opponent moves, play a move, resign, offer and accept a draw,
and claim a threefold or fifty-move draw.

Publishing the app as a Freenet webapp container is **not** in this spec. It is
the next one.

## Why the browser changes things

Three facts came out of reading the code, not from assumption, and each one
shapes the design.

**Web-app games and CLI games are invisible to each other.**
`handle_list_games` skips any label where `load_owner(store, &label) != origin`.
The CLI presents `origin: None` — verified against a live node, because the node
does not populate `MessageOrigin` for a WS-API caller. A browser app gets a real
origin, so it starts with an empty game list and cannot continue a game started
in the CLI. This is the delegate's origin isolation working as designed: it is
what stops any web app on the node enumerating every key the user holds. The
consequence is that the CLI becomes a testing and debugging tool, not a second
way into the same games.

**The bundle must embed both WASM modules.** The CLI loads
`adjourn_contract.wasm` off disk to derive the contract id and PUT the
container, and `adjourn_delegate.wasm` to register the delegate. A browser
cannot read either. Both ship inside the UI bundle, so a UI build pins both
keys, and rebuilding either module requires rebuilding the UI.

**The UI forces `watch` to be solved.** A tab must show the opponent's move when
it arrives. The node's WebSocket API supports subscription and pushes
`UpdateNotification`, but `NodeClient` has no streaming method — the gap
deferred twice already. The UI cannot be built without it.

## Architecture

### A shared client crate

`cli/src/session.rs` already holds every flow that touches the delegate or
contract, and is already generic over the `NodeClient` trait. It is unreachable
from a browser only because it lives in the `cli` crate, which pulls
`tokio-tungstenite`, which has no wasm32 backend.

Extract **`client/` (`adjourn-client`)** holding `session.rs`, `invite.rs`, the
`NodeClient` trait, and `FakeNode`. It depends on `adjourn-core` and
`freenet-stdlib` **without** the `net` feature, and on nothing tokio-flavoured.

- `cli/` keeps `WsClient` (the tungstenite transport), argument parsing, and
  rendering.
- `ui/` supplies its own `NodeClient` over the browser WebSocket and inherits
  every flow: `invite_new`, `invite_accept`, `game_bind`, `show_label`,
  `play_move`, `resign`, `draw_offer`, `draw_accept` and `draw_claim`, plus the
  `#[doc(hidden)]` `sign_move_at_ply` bypass the tests use.

The reason is correctness, not tidiness: both sides must derive **byte-identical
`GameParams`**, or the two players land on different contract ids, sit on
separate contracts, and each sees a game the other never joins — with no error
anywhere. One implementation is the only way to be sure of that.

The CLI's 13 tests move with the crate and must stay green through the
extraction. That is the evidence the refactor changed no behaviour.

### Streaming

`NodeClient` gains one method, shaped so both transports can implement it:

```rust
async fn subscribe(&mut self, key: ContractKey) -> Result<impl Stream<Item = UpdateData<'static>>>;
```

`ContractResponse::UpdateNotification` carries `update: UpdateData<'static>`,
which may be a `State`, a `Delta`, or a `StateAndDelta` — the notification does
not promise which. So the UI holds a `GameState` and **merges** whatever arrives
into it rather than replacing it. That is what merge exists for, it means the
browser converges exactly as a peer does, and it makes the arrival order of
notifications relative to the initial GET irrelevant.

**`UpdateData` is `#[non_exhaustive]`.** The contract already learned this the
hard way: its catch-all arm must return an error rather than `unreachable!()`,
because a panic inside contract WASM kills the runtime for that contract. The
same discipline applies here for a different reason — a panic in the UI kills
the tab. Unknown variants (`RelatedState`, `RelatedDelta`, anything added later)
are ignored, not asserted against.

`adjourn watch` falls out of this nearly free and should be added to the CLI in
the same pass, since the method is the thing that was missing.

## Screens

| screen | contents |
|---|---|
| Game list | every label the delegate reports for this origin, with whose turn it is; the landing screen |
| Game view | layout B — board left, move history right, status line, action buttons |
| New game | generates a key, shows the invite blob to copy |
| Accept invite | paste an invite blob, get an offer blob back to send to the inviter |
| Settings | node WebSocket URL, defaulting to the host the app was served from |

The invite exchange stays out of band — two copy-pasteable blobs, exactly as the
CLI does it. No lobby and no matchmaking; both are named anti-goals.

### The game view

Board left, scrollable move history right, status line beneath the board,
actions beneath that.

The move history is always visible rather than collapsed, and that is a
deliberate response to a documented property: **the outcome is not monotone.** A
late-published double-sign fraud proof forfeits a player, rewinds the board, and
can flip the winner. `CLAUDE.md` already states the UI should show the full
chain rather than the truncated position after a forfeit. A layout that hides
history behind a disclosure hides it exactly when it matters most.

Draw-claim appears only when a ground actually exists (`repetitions >= 3` or
`halfmove_clock >= 100`), mirroring the CLI's local pre-check — a claim with no
ground is ignored at projection, so offering the button would invite the user to
write a dead record into contract state permanently.

### The board

A pure function: `Status` in, 64 square descriptors out. Click a piece,
`legal_moves()` supplies the legal targets to highlight, click a target,
`play_move`. Board orientation follows the player's colour.

Promotion is the one interaction carrying real state: when the chosen move is a
pawn reaching the last rank, a picker selects the piece before the move is
signed. Underpromotion must be reachable — the algebra already covers it and a
UI that only ever queens is a UI that cannot play some legal games.

## What the browser forces

- **`getrandom` with the `js` feature** in the UI crate, for key generation.
  Safe: the CI assertions banning it apply to the contract and delegate
  dependency graphs, and a browser genuinely has `crypto.getRandomValues`. River
  does the same and documents why.
- **Both WASM modules embedded** in the bundle, as above.
- **The UI registers the delegate on first run** — the browser equivalent of
  `adjourn init`.
- **No clocks anywhere.** Untimed is a deliberate property, not an omission:
  self-reported timestamps are unenforceable when your parent's timestamp is set
  by your opponent.

## Testing

Weight goes in the shared client crate, not the UI layer.

- **`adjourn-client` carries the flow tests.** `FakeNode` already runs the real
  contract and delegate in memory, and the existing 13 CLI tests already cover
  every flow the browser uses. They move with the crate and keep running
  natively on every platform.
- **The board renders through a pure function**, so square colours, piece
  placement, orientation for Black, legal-target highlighting and the promotion
  picker's move construction are all testable natively — no browser, no
  framework.
- **Dioxus components stay thin**: wire events to flows, render descriptors.
- **One end-to-end smoke check** against a local node covers what unit tests
  structurally cannot — that the wasm build loads, the WebSocket transport
  connects, and a subscription delivers an update.

The UI layer's coverage will be thinner than the rest of this repo. That is
acceptable for a rendering layer over tested logic, and the documentation should
say so plainly rather than implying parity.

## Out of scope

- Publishing as a Freenet webapp container (its own spec, next).
- Drag-and-drop board interaction; click-to-move only.
- Any lobby, matchmaking, ratings, wagers or clocks — all named anti-goals.
- Mobile-specific layout beyond the board remaining responsive.
- Migrating CLI-created games into the browser. The origin partition is a
  security property, not a defect; a migration path would have to justify
  itself separately.
