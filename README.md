# adjourn — untimed correspondence chess on Freenet

An untimed correspondence chess app on Freenet.

`common/` (`adjourn-core`) holds the state algebra and has **no Freenet
dependencies** — pure Rust, testable standalone, so the consistency model is
verifiable on its own. `contracts/adjourn-contract/` is the `ContractInterface`
adapter over it: bytes in, bytes out, no logic of its own.

## The design in one paragraph

State is an unordered **set of signed records**, merged by **union**. That is
the entire consistency story — associative, commutative and idempotent by
construction, with no chess knowledge in it. Ordering comes from a
**parent-hash chain**: each move commits to the hash of its predecessor, so
strict sequencing is enforced by cryptography rather than by the network. The
board position is a **pure projection** of the merged set, never stored.

At any ply, only one player holds a key that can produce a valid record, so
concurrent writes are impossible rather than merely resolved.

## Layout

| file | role |
|---|---|
| `common/src/types.rs` | `Record`, `Body`, `GameParams`; signing and content-addressing |
| `common/src/state.rs` | `GameState`: merge, summarize, delta, apply_delta |
| `common/src/project.rs` | record set → position + outcome; `make_move`, `legal_moves` |
| `common/tests/algebra.rs` | monoid laws, order-independence, adversarial cases |
| `common/tests/adversarial.rs` | convergence attacks, outcome precedence, chess edges |
| `contracts/adjourn-contract/` | the `ContractInterface` adapter |
| `delegates/adjourn-delegate/` | holds the per-game signing key; enforces one signature per (game, ply) |
| `client/src/session.rs` | transport-independent game flows (`adjourn-client`), shared by the CLI and the UI |
| `ui/` | `adjourn-ui`: a Dioxus web frontend, compiled to `wasm32-unknown-unknown`. Compile-checked in CI only — see `CLAUDE.md`'s "UI" section for what is and is not tested |

## Five decisions worth keeping

1. **Record ids exclude the signature.** Content address is `H(signer ‖ body)`,
   so two encodings of the same statement collapse to one entry. Collisions
   tiebreak on `min(sig)` *among records that verify* — "first writer wins"
   would break commutativity, and a raw byte comparison would let a forged
   all-zero signature evict the honest move. Because ids exclude the signature,
   the summary must carry `H(sig)` alongside each id: otherwise two peers
   holding different valid signatures for one body are told they are already in
   sync, and never converge.

2. **Validity is permissive; projection is strict.** `verify()` only checks
   that a record is signed by one of the two players. Chess legality is
   checked at projection, where an illegal record is *ignored*. If illegality
   made the whole state invalid, either player could destroy the game by
   signing garbage.

3. **Double-signing is self-incriminating, and the proof is structural.** Two
   or more `Move` records from one signer at one ply is a fraud proof sitting
   in the state, and the projection forfeits deterministically — no arbiter
   needed. The rule *counts records*; it never asks whether any of them is a
   legal move. That matters because eviction (below) has to sort blind by id,
   so a legality-based proof could be dissolved by burying it under lower-id
   junk — which is exactly how a player would otherwise rewrite a ply their
   opponent had already answered. Counting cannot be dissolved: the burial
   records are themselves two records in one group. One illegal move alone is
   still merely ignored; only two or more at one ply are fatal.

4. **State is bounded by top-K eviction.** Merge keeps only the K smallest ids
   per `(signer, kind, ply)` group — K=2 for moves, K=1 for draw records — with
   `MAX_PLY = 4096` bounding the number of groups. Top-K distributes over
   union, so the monoid survives. Per-*signer* grouping is what makes it safe
   against an opponent: your records only ever compete with your own. K=2 is
   load-bearing rather than decorative — it floors a move group rather than
   emptying it, so an attacker cannot evict away their own fraud proof.

5. **Every signature is bound to `game_id`.** Prevents replaying a move from
   one game into a rematch between the same two players.

Fixed precedence, so all peers agree: forfeit > resignation > board result >
draw claim > draw agreement. Resignation outranks the board because `Resign` is
unanchored — otherwise a player could resign, play on to a mate, and be awarded
the win. The board outranks a draw claim because the claimant is by definition
the player to move, so a mated player must not be able to claim their way out.
`DrawClaim` is how FIDE 9.2 (threefold) and 9.3 (fifty-move) are cashed: both
are *claims*, not automatic results, and only the player to move may make one.
The genuinely automatic rules (fivefold 9.6.1, seventy-five-move 9.6.2) still
fire on their own.

## Verified

```
cargo test --workspace --locked     # 155 tests (99 adjourn-core + 17 contract + 9 delegate adapter + 16 adjourn-client + 14 adjourn-ui)
./scripts/build-contract.sh         # the canonical contract WASM
```

Build the contract **only** through that script: it applies the
`--remap-path-prefix` flags that keep the WASM — and therefore the contract key
— independent of who built it and where.

- `merge_is_a_monoid` — commutativity, associativity, idempotence, identity,
  over 200 random three-way partitions
- `projection_is_order_independent` — 500 random delivery orders with
  duplicate delivery; identical FEN, chain and outcome every time
- `sync_soundness_two_step` — whitepaper Property 1: applying B's delta
  dominates merging B in full, and converges in one round
- `partial_state_projects_to_a_prefix` — a peer missing a middle record sees
  a short game, never a gap-skipped position
- `two_valid_signatures_on_one_body_converge_in_one_round` — a player running
  their own signer can sign one body twice, both valid; the summary
  distinguishes them and the peers converge in a single round
- `forged_signature_cannot_evict_the_valid_record` / `merge_and_filter_commute`
  — validation and merge commute, so peers that validate at different points in
  the pipeline agree
- `retroactive_move_substitution_forfeits` /
  `reviving_an_expired_draw_offer_by_rewinding_forfeits` — the two attacks the
  structural forfeit exists to close: rewriting a ply the opponent has already
  answered, and rewinding the head to cash an expired draw offer
- `a_buried_double_sign_still_forfeits` — burying a fraud proof under lower-id
  junk no longer evades it; the junk is the proof
- `eviction_distributes_over_merge` / `eviction_bounds_a_spammed_group` —
  top-K survives merge, and a spammed group stays bounded
- plus: forgery, wrong-turn, cross-game replay, illegal-move poisoning,
  signature malleability, canonical encoding, outcome precedence, head-bound
  draw offers and claims, `MAX_PLY`, promotion and underpromotion, castling
  notation, en passant, threefold and fivefold repetition

Full-game state is ~1.1 KB / 7 records for a 7-ply game (~157 bytes per move),
and a sync summary is exactly 64 bytes per record.

## Next

1. ~~Wrap in `ContractInterface`~~ — done, `contracts/adjourn-contract`.
2. ~~Delegate holding the per-game signing key; UI never sees it~~ — done,
   `delegates/adjourn-delegate`.
3. UI over the WebSocket API: `get`, `subscribe`, `update`, `watch` — all done.
   - ~~3a. Freenet client layer~~ — done, `client/src/node.rs` (`NodeClient`,
     `FakeNode`; `WsClient` is `cli/src/ws.rs`, the tungstenite transport).
   - ~~3b. Game session flow~~ — done, `client/src/session.rs`, a
     transport-independent crate (`adjourn-client`) driven by the `adjourn`
     headless CLI (`cli/`): `init`, `key`, `invite`, `game`, `move`, `show`,
     `resign`, `draw`, `watch`. The session flows moved out of `cli/` into
     their own crate specifically so a browser UI could reach them without
     pulling in `tokio-tungstenite` — see `CLAUDE.md`'s crate table for why.
   - ~~3c. `watch`~~ — done, backed by `NodeClient::next_update`. Streams
     updates rather than polling; see `docs/runbook-two-nodes.md` for how to
     run two nodes and play a game end-to-end, and `CLAUDE.md`'s "Client"
     section and "Runtime assumptions, verified" for what has actually been
     confirmed against a live node so far. `watch` has no automated test
     against a real node — the mechanism is covered by
     `client/tests/updates.rs` against `FakeNode` only.
   - 3d. Browser frontend — `ui/` (`adjourn-ui`), a Dioxus web UI over the
     same `adjourn-client` flows: a pure board projection and a browser
     `NodeClient` impl. It compiles for `wasm32-unknown-unknown` and is
     compile-checked in CI; **it has never been loaded in an actual browser**
     — `dx`, the Dioxus CLI, is not installed anywhere in this environment.
     See `CLAUDE.md`'s "UI" section for exactly what is and is not tested.

Deferred by design: timers, matchmaking, ratings.

## Status — pre-1.0, do not pin a contract key yet

The wire format is **not frozen**. Two changes still pending will rotate every
identifier in the system:

- **The wire format is now frozen-ish but unversioned.** The encoding pass is
  done (state 2494 -> 1100 bytes, summary ~110 -> 64 per record). But there is
  no format version field, so any future change is still a hard break rather
  than a negotiated one — and `Body::DrawClaim` plus the `ply` field on the
  draw bodies were exactly such a break, so the contract key has rotated again.
- **`fdev conformance` is current: 19 states, 817 cases, 817 held, 0
  violations**, run against the post-break wire format on 2026-08-25. The
  corpus generator is committed (`cargo run -p adjourn-core --example
  dump_corpus`), so it can be rebuilt the next time the format moves. See
  `CLAUDE.md`, "Check against the network's own verifier".

Treat published contract keys as ephemeral until this section says otherwise.

## Licence

LGPL-3.0-only, matching the Freenet ecosystem (`freenet-stdlib`,
`freenet-scaffold` and River are all LGPL-3.0-only).

`LICENSE` is the LGPL-3.0 text. LGPL-3.0 incorporates the GPL-3.0 by reference
rather than restating it, so the GPL-3.0 text is included as `LICENSE.GPL-3.0`;
you need both to have the full terms.

Note that the shipped contract and delegate WASM statically link
`freenet-stdlib`, so LGPL obligations attach to those binaries when you publish
them to the network — publishing a contract is distribution.

## Note on pinned versions

`Cargo.toml` pins exact versions because the contract key is the hash of the
compiled WASM — a dependency bump silently rotates your app's address.
Commit `Cargo.lock` and `rust-toolchain.toml`, build `--locked`.
