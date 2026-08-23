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

## Four decisions worth keeping

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

3. **Double-signing is self-incriminating.** Two distinct legal moves at the
   same ply from the same player is a fraud proof sitting in the state.
   The projection forfeits deterministically — no arbiter needed.

4. **Every signature is bound to `game_id`.** Prevents replaying a move from
   one game into a rematch between the same two players.

Fixed precedence, so all peers agree: forfeit > resignation > board result >
draw agreement. Resignation outranks the board because `Resign` is unanchored —
otherwise a player could resign, play on to a mate, and be awarded the win.

## Verified

```
cargo test --workspace --locked     # 76 tests (59 adjourn-core + 13 contract + 4 delegate adapter)
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
- plus: forgery, wrong-turn, cross-game replay, illegal-move poisoning,
  signature malleability, canonical encoding, outcome precedence, head-bound
  draw offers, promotion and underpromotion, castling notation, en passant,
  fivefold repetition

Full-game state is ~1.1 KB / 7 records for a 7-ply game (~157 bytes per move),
and a sync summary is exactly 64 bytes per record.

## Next

1. ~~Wrap in `ContractInterface`~~ — done, `contracts/adjourn-contract`.
2. ~~Delegate holding the per-game signing key; UI never sees it~~ — done,
   `delegates/adjourn-delegate`.
3. UI over the WebSocket API: `get`, `subscribe`, `update`.

Deferred by design: timers, matchmaking, ratings.

## Status — pre-1.0, do not pin a contract key yet

The wire format is **not frozen**. Two changes still pending will rotate every
identifier in the system:

- **The wire format is now frozen-ish but unversioned.** The encoding pass is
  done (state 2494 -> 1100 bytes, summary ~110 -> 64 per record), and
  `fdev conformance` reports 403/403 cases held. But there is no format version
  field, so any future change is still a hard break rather than a negotiated
  one.

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
